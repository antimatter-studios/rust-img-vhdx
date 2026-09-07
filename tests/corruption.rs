//! Reader-level corruption and recovery tests.
//!
//! These exercise the `open()` path's resilience: dual-slot header /
//! region-table selection, fallback when one slot is invalid, and clean
//! rejection of malformed images. The byte builders come from
//! `tests/common/mod.rs`; here we build a valid image and then surgically
//! corrupt one structure to observe the reader's response.

mod common;

use std::io::{Seek, SeekFrom, Write};

use common::*;
use vhdx::{Error, VhdxReader};

/// A 1 MiB block whose bytes are `i & 0xFF`, so reads are verifiable.
fn ramp_block() -> Box<[u8; BLOCK_SIZE as usize]> {
    let mut data = Box::new([0u8; BLOCK_SIZE as usize]);
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i & 0xFF) as u8;
    }
    data
}

fn open_file_rw(path: &std::path::Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap()
}

fn patch(path: &std::path::Path, offset: u64, bytes: &[u8]) {
    let mut f = open_file_rw(path);
    f.seek(SeekFrom::Start(offset)).unwrap();
    f.write_all(bytes).unwrap();
    f.flush().unwrap();
}

/// Patch a field inside a header slot and repair that slot's CRC-32C.
///
/// Most tests here want a structure the reader should reject, and a
/// deliberately broken checksum is enough. Some want the opposite: a
/// header that is entirely well-formed and simply says something the
/// reader does not know how to honour. Repairing the CRC is what makes
/// the second kind possible — otherwise the file fails as a checksum
/// mismatch and never reaches the check under test.
fn patch_header_field(path: &std::path::Path, slot: u64, field_offset: usize, bytes: &[u8]) {
    let mut image = std::fs::read(path).unwrap();
    let at = slot as usize;
    image[at + field_offset..at + field_offset + bytes.len()].copy_from_slice(bytes);
    let crc = vhdx::header::compute_crc(&image[at..at + HEADER_SIZE]);
    image[at + 4..at + 8].copy_from_slice(&crc.to_le_bytes());
    std::fs::write(path, &image).unwrap();
}

#[test]
fn valid_image_opens_as_baseline() {
    // Guards the corruption tests below: if this fails the fixture is
    // broken, not the reader's error handling.
    let path = tmp_path("corruption_baseline");
    build_vhdx(&path, &ramp_block());
    let r = VhdxReader::open(&path).unwrap();
    let mut buf = [0u8; 16];
    r.read_at(0, &mut buf).unwrap();
    assert_eq!(buf[1], 1);
    let _ = std::fs::remove_file(&path);
}

/// `log_version` says which log format the file uses. Version 0 is the
/// only one defined, and a reader that meets another value has to stop:
/// the log region is then not one it can reason about, and replaying it
/// is a *write* — the version-0 parser's idea of the descriptors gets
/// applied on top of real data.
///
/// The header CRC covers the field, so this fixture is not a corrupt
/// image. It is exactly the shape a future revision of the format has.
#[test]
fn an_unknown_log_version_is_unsupported_not_replayed() {
    let path = tmp_path("log_version_1");
    build_vhdx(&path, &ramp_block());
    // Baseline: the fixture opens before the patch, so the refusal below
    // is attributable to the field and not to the fixture.
    VhdxReader::open(&path).expect("fixture precondition");

    patch_header_field(&path, HEADER1_OFFSET, 64, &1u16.to_le_bytes());

    match VhdxReader::open(&path) {
        Err(Error::Unsupported(msg)) => assert!(
            msg.contains("log version"),
            "the refusal must name the log version, got {msg:?}"
        ),
        Err(other) => panic!("expected Unsupported, got {other:?}"),
        Ok(_) => panic!("opened an image whose log format we cannot parse"),
    }
    let _ = std::fs::remove_file(&path);
}

/// The refusal is not gated on the log being dirty. This fixture's
/// `log_guid` is zero, so nothing would be replayed today — and it is
/// still refused, because a later write would append to a log region
/// whose format this crate does not know.
#[test]
fn an_unknown_log_version_is_refused_even_with_an_empty_log() {
    let path = tmp_path("log_version_clean");
    build_vhdx(&path, &ramp_block());
    patch_header_field(&path, HEADER1_OFFSET, 64, &0xFFFFu16.to_le_bytes());

    let err = VhdxReader::open(&path)
        .err()
        .expect("expected Unsupported, got Ok");
    assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn not_a_vhdx_when_file_identifier_is_wrong() {
    let path = tmp_path("not_vhdx");
    build_vhdx(&path, &ramp_block());
    patch(&path, 0, b"NOTvhdxf");
    let err = VhdxReader::open(&path)
        .err()
        .expect("expected NotVhdx, got Ok");
    assert!(matches!(err, Error::NotVhdx), "got {err:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn both_header_slots_invalid_yields_no_valid_header() {
    let path = tmp_path("both_headers_bad");
    build_vhdx(&path, &ramp_block());
    // Header 1 is valid, header 2 is zero (invalid). Corrupt header 1's
    // signature so neither slot parses.
    patch(&path, HEADER1_OFFSET, b"junk");
    let err = VhdxReader::open(&path)
        .err()
        .expect("expected NoValidHeader, got Ok");
    assert!(matches!(err, Error::NoValidHeader), "got {err:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn falls_back_to_header2_when_header1_crc_is_invalid() {
    let path = tmp_path("header2_fallback");
    build_vhdx(&path, &ramp_block());

    // Write a *valid* header 2 (sequence 2) matching the no-log layout,
    // then corrupt header 1's CRC by flipping a payload byte without
    // fixing its checksum. The reader must fall back to header 2.
    patch(&path, HEADER2_OFFSET, &encode_header(2, [0u8; 16], 0, 0));
    // Flip one byte of header 1's sequence_number field (offset +8).
    let mut f = open_file_rw(&path);
    f.seek(SeekFrom::Start(HEADER1_OFFSET + 8)).unwrap();
    f.write_all(&[0xFF]).unwrap();
    f.flush().unwrap();
    drop(f);

    let r = VhdxReader::open(&path).expect("should recover via header 2");
    let mut buf = [0u8; 4];
    r.read_at(0, &mut buf).unwrap();
    assert_eq!(buf, [0, 1, 2, 3]);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn opens_with_two_valid_header_slots() {
    let path = tmp_path("two_headers");
    build_vhdx(&path, &ramp_block());
    // Header 1 has sequence 1 (from build_vhdx). Add a second valid header
    // with a much higher sequence so both slots parse. The image must
    // still open and read correctly. (Both slots carry identical
    // log-less content, so this asserts robustness to two valid headers,
    // not which one wins — the higher-sequence *selection* is proven
    // end-to-end by `ro_open_against_writable_file_replays_dirty_log` in
    // tests/synthetic.rs, whose replay only fires when the higher-sequence
    // slot-2 header is chosen.)
    patch(&path, HEADER2_OFFSET, &encode_header(999, [0u8; 16], 0, 0));
    let r = VhdxReader::open(&path).unwrap();
    let mut buf = [0u8; 8];
    r.read_at(0, &mut buf).unwrap();
    assert_eq!(buf, [0, 1, 2, 3, 4, 5, 6, 7]);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn both_region_tables_invalid_yields_no_valid_region_table() {
    let path = tmp_path("both_region_tables_bad");
    build_vhdx(&path, &ramp_block());
    // build_vhdx writes region table 1 only; table 2 is zero (invalid).
    // Corrupt table 1's signature so neither parses.
    patch(&path, REGION_TABLE1_OFFSET, b"xxxx");
    let err = VhdxReader::open(&path)
        .err()
        .expect("expected NoValidRegionTable, got Ok");
    assert!(matches!(err, Error::NoValidRegionTable), "got {err:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn bad_metadata_signature_is_rejected() {
    let path = tmp_path("bad_metadata");
    build_vhdx(&path, &ramp_block());
    patch(&path, METADATA_REGION_OFFSET, b"NOTmeta!");
    let err = VhdxReader::open(&path)
        .err()
        .expect("expected BadMetadata, got Ok");
    assert!(matches!(err, Error::BadMetadata(_)), "got {err:?}");
    let _ = std::fs::remove_file(&path);
}
