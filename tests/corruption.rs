//! Reader-level corruption and recovery tests.
//!
//! These exercise the `open()` path's resilience: dual-slot header /
//! region-table selection, fallback when one slot is invalid, and clean
//! rejection of malformed images. The byte builders come from
//! `tests/common/mod.rs`; here we build a valid image and then surgically
//! corrupt one structure to observe the reader's response.

mod common;

use std::io::{Read, Seek, SeekFrom, Write};

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

fn patch_header_version(path: &std::path::Path, version: u16) {
    let mut f = open_file_rw(path);
    let mut header = vec![0u8; HEADER_SIZE];
    f.seek(SeekFrom::Start(HEADER1_OFFSET)).unwrap();
    f.read_exact(&mut header).unwrap();
    header[66..68].copy_from_slice(&version.to_le_bytes());
    let crc = vhdx::header::compute_crc(&header);
    header[4..8].copy_from_slice(&crc.to_le_bytes());
    drop(f);
    patch(path, HEADER1_OFFSET, &header);
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

#[test]
fn unsupported_header_version_is_rejected_after_crc_recompute() {
    let path = tmp_path("unsupported_header_version");
    build_vhdx(&path, &ramp_block());
    patch_header_version(&path, 2);

    let err = VhdxReader::open(&path)
        .err()
        .expect("expected unsupported header version to be rejected");
    assert!(matches!(err, Error::NoValidHeader), "got {err:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn unsupported_logical_sector_size_is_rejected() {
    let path = tmp_path("unsupported_sector_size");
    build_vhdx(&path, &ramp_block());
    patch(&path, LOGICAL_SECTOR_SIZE_OFFSET, &1024u32.to_le_bytes());

    let err = VhdxReader::open(&path)
        .err()
        .expect("expected unsupported sector size to be rejected");
    assert!(matches!(
        err,
        Error::Corrupt("sector_size must be 512 or 4096")
    ));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn accepts_4096_logical_sector_size() {
    let path = tmp_path("sector_size_4096");
    build_vhdx(&path, &ramp_block());
    patch(&path, LOGICAL_SECTOR_SIZE_OFFSET, &4096u32.to_le_bytes());

    let reader = VhdxReader::open(&path).expect("4096-byte sectors are valid VHDX");
    assert_eq!(reader.sector_size(), 4096);
    let _ = std::fs::remove_file(&path);
}
