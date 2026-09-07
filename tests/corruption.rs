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

/// The header's `version` field, at offset 66, patched in the first
/// header slot with that slot's CRC repaired.
fn patch_header_version(path: &std::path::Path, version: u16) {
    patch_header_field(path, HEADER1_OFFSET, 66, &version.to_le_bytes());
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

/// A region entry's `Required` flag is a hard gate: a region whose GUID
/// this reader does not know, with the flag set, means the file must not
/// be loaded. It is how the format reserves room for a region that
/// *transforms* the payload — an encryption region, a dedup map —
/// without an older reader quietly handing back the untransformed bytes.
///
/// The flag was parsed onto `RegionEntry` and read by nothing outside
/// the module's own tests, so such an image was read as though the
/// region were not there: BAT found, metadata found, payload returned
/// raw, and no error, because nothing looked.
#[test]
fn an_unknown_required_region_is_unsupported() {
    let path = tmp_path("required_region");
    build_vhdx(&path, &ramp_block());
    VhdxReader::open(&path).expect("fixture precondition");

    append_region_entry(&path, REGION_TABLE1_OFFSET, [0xDE; 16], true);

    match VhdxReader::open(&path) {
        Err(Error::Unsupported(msg)) => assert!(
            msg.contains("region"),
            "the refusal must name the region, got {msg:?}"
        ),
        Err(other) => panic!("expected Unsupported, got {other:?}"),
        Ok(_) => panic!("opened an image carrying a region we cannot honour"),
    }
    let _ = std::fs::remove_file(&path);
}

/// The same region with the flag clear is the format saying "ignore me
/// if you do not know me". Honouring that is what makes the check a gate
/// rather than a blanket refusal of every unknown region — and a blanket
/// refusal would reject vendor regions that are none of our business.
#[test]
fn an_unknown_optional_region_is_ignored() {
    let path = tmp_path("optional_region");
    build_vhdx(&path, &ramp_block());
    append_region_entry(&path, REGION_TABLE1_OFFSET, [0xDE; 16], false);

    let r = VhdxReader::open(&path).expect("an unknown optional region must be ignored");
    let mut buf = [0u8; 16];
    r.read_at(0, &mut buf).unwrap();
    assert_eq!(buf[1], 1);
    drop(r);
    let _ = std::fs::remove_file(&path);
}

/// Add a region entry to the table at `table_offset` and repair its
/// CRC-32C, so the image fails for the reason the test is about rather
/// than for a checksum.
fn append_region_entry(path: &std::path::Path, table_offset: u64, guid: [u8; 16], required: bool) {
    let mut image = std::fs::read(path).unwrap();
    let at = table_offset as usize;
    let count = u32::from_le_bytes(image[at + 8..at + 12].try_into().unwrap()) as usize;
    let off = at + 16 + count * 32;
    image[off..off + 16].copy_from_slice(&guid);
    image[off + 16..off + 24].copy_from_slice(&0u64.to_le_bytes());
    image[off + 24..off + 28].copy_from_slice(&0u32.to_le_bytes());
    image[off + 28..off + 32].copy_from_slice(&u32::from(required).to_le_bytes());
    image[at + 8..at + 12].copy_from_slice(&((count + 1) as u32).to_le_bytes());
    image[at + 4..at + 8].fill(0);
    let crc = vhdx::region_table::compute_crc(&image[at..at + REGION_TABLE_SIZE]);
    image[at + 4..at + 8].copy_from_slice(&crc.to_le_bytes());
    std::fs::write(path, &image).unwrap();
}

#[test]
fn falls_back_to_header2_when_header1_version_is_unsupported() {
    let path = tmp_path("header2_fallback_unsupported_version");
    build_vhdx(&path, &ramp_block());

    // Give header 2 a lower sequence number but an active log. This makes
    // the selected slot observable: header 2 must be selected after header 1
    // is rejected, otherwise the log replay below never happens.
    let log_guid = [0x77u8; 16];
    let log_offset = 4 * ONE_MIB;
    let log_length = ONE_MIB as u32;
    let entry = vhdx::log::encode_entry(
        2,
        0,
        &log_guid,
        8 * ONE_MIB,
        8 * ONE_MIB,
        &[vhdx::log::PendingWrite {
            file_offset: DATA_BLOCK_OFFSET,
            sector: vec![0xEE; 4096],
        }],
    );
    {
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        f.set_len(8 * ONE_MIB).unwrap();
        f.seek(SeekFrom::Start(log_offset)).unwrap();
        f.write_all(&entry).unwrap();
        f.seek(SeekFrom::Start(HEADER2_OFFSET)).unwrap();
        f.write_all(&encode_header(0, log_guid, log_length, log_offset))
            .unwrap();
        f.flush().unwrap();
    }
    patch_header_version(&path, 2);

    let reader = VhdxReader::open(&path).expect("should recover via header 2");
    let mut buf = [0u8; 4096];
    reader.read_at(0, &mut buf).unwrap();
    assert!(buf.iter().all(|byte| *byte == 0xEE));
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

// ---------------------------------------------------------------------------
// The log region has to be somewhere a region may go
//
// `log_offset` and `log_length` were checked only inside the branch that
// runs when the log is dirty. A clean image — `log_guid` all zeros,
// which is what every properly-closed image looks like — never had them
// read at all, and `journal_sector_write` then zeroed `log_length` bytes
// at `log_offset` before splicing its entry in. So the first write to
// such an image erased whatever those two fields named.
// ---------------------------------------------------------------------------

/// Build the four-block fixture with `log_offset` and `log_length`
/// replaced, leaving `log_guid` at zero so the image looks clean.
fn big_vhdx_with_log_region(name: &str, log_offset: u64, log_length: u32) -> std::path::PathBuf {
    let path = tmp_path(name);
    let block = pattern_block(3);
    build_big_vhdx(&path, &block);
    let mut f = open_file_rw(&path);
    f.write_all_at(
        &encode_header(1, [0u8; 16], log_length, log_offset),
        HEADER1_OFFSET,
    )
    .unwrap();
    path
}

#[test]
fn a_log_region_over_the_metadata_region_is_refused_at_open() {
    let path = big_vhdx_with_log_region("log_over_meta", BIG_METADATA_OFFSET, BIG_LOG_LENGTH);

    let before = std::fs::read(&path).unwrap();
    let err = VhdxReader::open_rw(&path)
        .err()
        .expect("a log region on top of the metadata region must be refused");
    assert!(matches!(err, Error::Corrupt(_)), "got {err:?}");

    // And nothing was written on the way to the refusal. Before this,
    // opening read-write and writing one 4 KiB sector turned the
    // metadata table's "metadata" signature into "loge" and the next
    // open into BadMetadata("signature mismatch").
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "the refusal must not have touched the file"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_log_region_over_the_bat_region_is_refused_at_open() {
    let path = big_vhdx_with_log_region("log_over_bat", BIG_BAT_OFFSET, BIG_LOG_LENGTH);
    let err = VhdxReader::open_rw(&path)
        .err()
        .expect("a log region on top of the BAT must be refused");
    assert!(matches!(err, Error::Corrupt(_)), "got {err:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_log_region_inside_the_header_section_is_refused_at_open() {
    // The first megabyte holds the file identifier, both headers and
    // both region tables. A log there would erase the reader's own way
    // back in.
    let path = big_vhdx_with_log_region("log_in_headers", 0, BIG_LOG_LENGTH);
    let err = VhdxReader::open(&path)
        .err()
        .expect("a log region inside the header section must be refused");
    assert!(matches!(err, Error::Corrupt(_)), "got {err:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_log_region_that_is_not_megabyte_aligned_is_refused_at_open() {
    // 10 MiB + 4 KiB, in a 64 MiB file whose declared regions are at 5
    // and 6 MiB. The offset overlaps nothing and is inside the file, so
    // the alignment rule is the only thing that can refuse it — a
    // test whose subject is caught by a neighbouring check proves
    // nothing about its own.
    let path = big_vhdx_with_log_region("log_unaligned", 10 * ONE_MIB + 4096, BIG_LOG_LENGTH);
    let err = VhdxReader::open(&path)
        .err()
        .expect("a log region off the megabyte grid must be refused");
    assert!(matches!(err, Error::Corrupt(_)), "got {err:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_log_region_whose_length_is_not_a_whole_megabyte_is_refused_at_open() {
    // Same reasoning: aligned start, no overlap, inside the file. Only
    // the length rule can refuse this.
    let path = big_vhdx_with_log_region("log_len_unaligned", 10 * ONE_MIB, BIG_LOG_LENGTH + 4096);
    let err = VhdxReader::open(&path)
        .err()
        .expect("a log length that is not a whole number of megabytes must be refused");
    assert!(matches!(err, Error::Corrupt(_)), "got {err:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_sound_log_region_somewhere_else_in_the_file_still_opens() {
    // The positive control for the two above: the same 10 MiB address,
    // aligned and a whole megabyte long, is legal.
    let path = big_vhdx_with_log_region("log_elsewhere", 10 * ONE_MIB, BIG_LOG_LENGTH);
    VhdxReader::open(&path).expect("an aligned log region that overlaps nothing is legal");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_log_region_reaching_past_the_end_of_the_file_is_refused_at_open() {
    let path = big_vhdx_with_log_region("log_past_end", BIG_TOTAL_FILE_SIZE, BIG_LOG_LENGTH);
    let err = VhdxReader::open(&path)
        .err()
        .expect("a log region past the end of the file must be refused");
    assert!(matches!(err, Error::Corrupt(_)), "got {err:?}");
    let _ = std::fs::remove_file(&path);
}

/// The positive control: the fixture's own log region is legal, and an
/// image that declares no log at all is legal too.
///
/// Without these, a check that simply refused every image would pass
/// every test above.
#[test]
fn a_sound_log_region_still_opens_and_writes() {
    let path = big_vhdx_with_log_region("log_sound", BIG_LOG_OFFSET, BIG_LOG_LENGTH);
    {
        let r = VhdxReader::open_rw(&path).expect("the fixture's own log region is legal");
        r.write_at(BIG_BLOCK_SIZE as u64, &[0xABu8; 4096])
            .expect("and a write into an unallocated block still allocates");
        r.flush().unwrap();
    }
    let r = VhdxReader::open(&path).expect("and the image is still readable afterwards");
    let mut buf = vec![0u8; 4096];
    r.read_at(BIG_BLOCK_SIZE as u64, &mut buf).unwrap();
    assert_eq!(buf, vec![0xABu8; 4096]);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn an_image_declaring_no_log_at_all_still_opens() {
    let path = big_vhdx_with_log_region("log_absent", 0, 0);
    VhdxReader::open(&path).expect("log_length = 0 means there is no log, which is legal");
    let _ = std::fs::remove_file(&path);
}

/// A *dirty* image whose log region names a declared region is refused
/// with that region intact.
///
/// This is the half the deferred erase exists for. `zero_log_region` used
/// to run inside the replay branch, above the point where the region
/// table can say whether the log region is somewhere it may be erased —
/// so opening such a file read-write zeroed the region as part of
/// "repairing" it, and the reader destroyed the image while reporting
/// that it had fixed one.
#[test]
fn a_dirty_log_over_the_metadata_region_is_refused_with_the_region_intact() {
    let path = tmp_path("dirty_log_over_meta");
    let block = pattern_block(3);
    build_big_vhdx(&path, &block);
    {
        let mut f = open_file_rw(&path);
        // A non-zero log_guid says a writer left something pending.
        f.write_all_at(
            &encode_header(1, [0x11u8; 16], BIG_LOG_LENGTH, BIG_METADATA_OFFSET),
            HEADER1_OFFSET,
        )
        .unwrap();
    }
    let before = std::fs::read(&path).unwrap();

    let err = VhdxReader::open_rw(&path)
        .err()
        .expect("a dirty log on top of the metadata region must be refused");
    assert!(matches!(err, Error::Corrupt(_)), "got {err:?}");

    let after = std::fs::read(&path).unwrap();
    let meta = BIG_METADATA_OFFSET as usize;
    assert_eq!(
        &after[meta..meta + 16],
        &before[meta..meta + 16],
        "the metadata region must be untouched"
    );
    assert_eq!(after, before, "nothing at all should have been written");
    let _ = std::fs::remove_file(&path);
}

/// The erase is deferred until the region table has said the log region
/// may be erased — and this is the fixture that proves the deferral,
/// rather than the refusal that happens to come first.
///
/// `a_dirty_log_over_the_metadata_region_is_refused_with_the_region_intact`
/// above passes on the overlap check alone: `collect_replay_chain` finds
/// nothing in the metadata region's bytes, so `replayed` stays false and
/// step 4c never runs. The deferral only bites when a chain really is
/// replayed AND the region overlaps, which is the issue's own worst
/// case — a write plants a real log entry over the metadata table, and
/// the next open replays it and zeroes the rest of the region.
///
/// So this one forges a genuine single-entry chain at the hostile
/// offset, and asserts on a marker 512 KiB into the log region: far past
/// the 8 KiB the entry occupies, and inside the megabyte
/// `zero_log_region` would erase. The entry cannot touch it and the
/// erase cannot miss it.
#[test]
fn a_replayable_chain_at_a_hostile_log_offset_does_not_erase_the_region() {
    let path = tmp_path("replayable_hostile_log");
    let block0 = pattern_block(4);
    build_big_vhdx(&path, &block0);

    const MARKER_AT: u64 = BIG_METADATA_OFFSET + 512 * 1024;
    let log_guid = [0x77u8; 16];
    let entry = vhdx::log::encode_entry(
        2,
        0,
        &log_guid,
        BIG_TOTAL_FILE_SIZE,
        BIG_TOTAL_FILE_SIZE,
        &[vhdx::log::PendingWrite {
            file_offset: BIG_DATA_BLOCK0_OFFSET + 8192,
            sector: vec![0xEEu8; 4096],
        }],
    );

    {
        let mut f = open_file_rw(&path);
        // The log region is declared over the metadata region, and the
        // chain really is there.
        f.write_all_at(&entry, BIG_METADATA_OFFSET).unwrap();
        f.write_all_at(&[0xC5u8; 4096], MARKER_AT).unwrap();
        // Header 2 wins on sequence number and declares the hostile log.
        let mut hdr = encode_header(5, log_guid, BIG_LOG_LENGTH, BIG_METADATA_OFFSET);
        hdr.truncate(HEADER_SIZE);
        f.write_all_at(&hdr, HEADER2_OFFSET).unwrap();
    }

    // Precondition: the chain is genuinely replayable, so `replayed`
    // becomes true and step 4c is reached. Without this the test would
    // pass on the overlap check the way its neighbour does.
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        &before[BIG_METADATA_OFFSET as usize..BIG_METADATA_OFFSET as usize + 4],
        b"loge",
        "the fixture must actually contain a log entry"
    );

    let err = VhdxReader::open_rw(&path)
        .err()
        .expect("a log region over a declared region must be refused");
    assert!(matches!(err, Error::Corrupt(_)), "got {err:?}");

    let after = std::fs::read(&path).unwrap();
    assert_eq!(
        &after[MARKER_AT as usize..MARKER_AT as usize + 16],
        &[0xC5u8; 16],
        "the log region was erased before anything said it could be"
    );
    let _ = std::fs::remove_file(&path);
}

/// A refused image is an unmodified image, including the descriptor's
/// own target.
///
/// `#43` moved the log-region *erase* below the region-table check, so
/// a hostile `log_offset` can no longer wipe a megabyte. `apply_chain`
/// still ran above that check, so a replayed chain's descriptors landed
/// on a file the reader then refused — and a caller handed
/// `Err(Corrupt)` has no reason to suspect the file changed underneath
/// it.
///
/// Bounded is not the same as small. A descriptor's target is checked
/// against `allowed_extent`, which is at least the file's current size,
/// so one forged descriptor naming offset 0 with a length covering the
/// whole file passes it — the image is erased and the open then blames
/// the image. That is the case this asserts: whole-file bytes, before
/// and after, not just the four kilobytes the older fixture watched.
#[test]
fn a_refused_image_is_not_written_to_by_the_replay_that_precedes_the_refusal() {
    let path = tmp_path("replay_before_refusal");
    let block0 = pattern_block(6);
    build_big_vhdx(&path, &block0);

    const TARGET: u64 = BIG_DATA_BLOCK0_OFFSET + 8192;
    let log_guid = [0x77u8; 16];
    let entry = vhdx::log::encode_entry(
        2,
        0,
        &log_guid,
        BIG_TOTAL_FILE_SIZE,
        BIG_TOTAL_FILE_SIZE,
        &[vhdx::log::PendingWrite {
            file_offset: TARGET,
            sector: vec![0xEEu8; 4096],
        }],
    );

    {
        let mut f = open_file_rw(&path);
        // The log is declared over the metadata region — somewhere it
        // may not be — and the chain really is there.
        f.write_all_at(&entry, BIG_METADATA_OFFSET).unwrap();
        let mut hdr = encode_header(5, log_guid, BIG_LOG_LENGTH, BIG_METADATA_OFFSET);
        hdr.truncate(HEADER_SIZE);
        f.write_all_at(&hdr, HEADER2_OFFSET).unwrap();
    }

    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        &before[BIG_METADATA_OFFSET as usize..BIG_METADATA_OFFSET as usize + 4],
        b"loge",
        "the fixture must actually contain a log entry"
    );
    assert_ne!(
        &before[TARGET as usize..TARGET as usize + 8],
        &[0xEEu8; 8],
        "the target already holds what replay would write, so this test could not tell"
    );

    let err = VhdxReader::open_rw(&path)
        .err()
        .expect("a log region over a declared region must be refused");
    // The message as well as the variant. A fix that prevented the
    // write but left the diagnostic saying something else would pass a
    // test written only against the bytes — and what a caller is told
    // is half of what is wrong here: "this file is bad" is only honest
    // once it is also true that the reader left it alone.
    match &err {
        Error::Corrupt(m) => assert!(
            m.contains("log region overlaps"),
            "refused, but not for the overlap: {m}"
        ),
        other => panic!("got {other:?}"),
    }

    let after = std::fs::read(&path).unwrap();
    assert_eq!(
        &after[TARGET as usize..TARGET as usize + 8],
        &before[TARGET as usize..TARGET as usize + 8],
        "the chain was applied before the refusal, so a caller told the file is bad \
         is holding a file this reader has just modified"
    );
    assert_eq!(
        after.len(),
        before.len(),
        "the file changed size on an open that failed"
    );
    assert!(
        after == before,
        "the refused open modified the image somewhere"
    );
    let _ = std::fs::remove_file(&path);
}

/// The same refusal, with the descriptor that erases everything.
///
/// This is the version that makes the previous test's bound worth
/// stating: `allowed_extent` is at least the file's current size, so a
/// descriptor naming offset 0 is inside it, and a chain of them covers
/// the image. The refusal has to come first or there is nothing left to
/// refuse.
#[test]
fn a_forged_chain_that_would_erase_the_image_is_refused_before_it_runs() {
    let path = tmp_path("replay_erases_everything");
    let block0 = pattern_block(7);
    build_big_vhdx(&path, &block0);

    let log_guid = [0x77u8; 16];
    // Eight sectors of zeros starting at offset 0: the file identifier,
    // both headers, and the region tables.
    let zeros: Vec<vhdx::log::PendingWrite> = (0..8u64)
        .map(|i| vhdx::log::PendingWrite {
            file_offset: i * 4096,
            sector: vec![0u8; 4096],
        })
        .collect();
    let entry = vhdx::log::encode_entry(
        2,
        0,
        &log_guid,
        BIG_TOTAL_FILE_SIZE,
        BIG_TOTAL_FILE_SIZE,
        &zeros,
    );

    {
        let mut f = open_file_rw(&path);
        f.write_all_at(&entry, BIG_METADATA_OFFSET).unwrap();
        let mut hdr = encode_header(5, log_guid, BIG_LOG_LENGTH, BIG_METADATA_OFFSET);
        hdr.truncate(HEADER_SIZE);
        f.write_all_at(&hdr, HEADER2_OFFSET).unwrap();
    }

    let before = std::fs::read(&path).unwrap();
    let err = VhdxReader::open_rw(&path)
        .err()
        .expect("a log region over a declared region must be refused");
    match &err {
        Error::Corrupt(m) => assert!(
            m.contains("log region overlaps"),
            "refused, but not for the overlap: {m}"
        ),
        other => panic!("got {other:?}"),
    }

    let after = std::fs::read(&path).unwrap();
    assert_eq!(
        &after[..8],
        b"vhdxfile",
        "the file identifier was erased by a replay the reader then refused"
    );
    assert!(after == before, "the refused open modified the image");
    let _ = std::fs::remove_file(&path);
}

/// The check after replay is not a duplicate of the one before it.
///
/// A replayed chain may rewrite the region table itself — that is the
/// whole reason the module reads the table again at step 4 and treats
/// the second read as authoritative. So a file can pass the check
/// before replay and fail it after: the table on disk beforehand
/// declares regions clear of the log, and the chain replaces it with
/// one that does not.
///
/// That is the case the second call exists for, and without this test
/// it is unwitnessed: with the pre-replay check in place, every other
/// hostile-log fixture here is refused before the second call is
/// reached.
#[test]
fn a_replay_that_moves_a_region_onto_the_log_is_refused_after_it_runs() {
    let path = tmp_path("replay_moves_a_region");
    let block0 = pattern_block(8);
    build_big_vhdx(&path, &block0);

    // A region table declaring the BAT exactly where the log lives.
    // Innocuous on disk beforehand — it is not there yet.
    let hostile_table = encode_region_table(BIG_LOG_OFFSET, 4096, BIG_METADATA_OFFSET);
    let log_guid = [0x77u8; 16];
    let writes: Vec<vhdx::log::PendingWrite> = hostile_table
        .chunks(4096)
        .enumerate()
        .map(|(i, chunk)| vhdx::log::PendingWrite {
            file_offset: REGION_TABLE1_OFFSET + (i as u64) * 4096,
            sector: chunk.to_vec(),
        })
        .collect();
    let entry = vhdx::log::encode_entry(
        2,
        0,
        &log_guid,
        BIG_TOTAL_FILE_SIZE,
        BIG_TOTAL_FILE_SIZE,
        &writes,
    );

    {
        let mut f = open_file_rw(&path);
        // The log is where the header says it is, and where the table
        // as it stands says nothing lives.
        f.write_all_at(&entry, BIG_LOG_OFFSET).unwrap();
        let mut hdr = encode_header(5, log_guid, BIG_LOG_LENGTH, BIG_LOG_OFFSET);
        hdr.truncate(HEADER_SIZE);
        f.write_all_at(&hdr, HEADER2_OFFSET).unwrap();
    }

    let err = VhdxReader::open_rw(&path)
        .err()
        .expect("after replay the BAT sits on the log region, which must be refused");
    match &err {
        Error::Corrupt(m) => assert!(
            m.contains("log region overlaps"),
            "refused, but not for the overlap: {m}"
        ),
        other => panic!("got {other:?}"),
    }

    // And the replay really did happen — otherwise this would be the
    // pre-replay check firing and the test would prove nothing about
    // the second one.
    let after = std::fs::read(&path).unwrap();
    assert_eq!(
        &after[REGION_TABLE1_OFFSET as usize..REGION_TABLE1_OFFSET as usize + 4096],
        &hostile_table[..4096],
        "the chain was not applied, so this test did not reach the check it is about"
    );
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// The BAT region against the disk it describes
// ---------------------------------------------------------------------------

/// Rewrite both region-table copies with a BAT region of `bat_len`
/// bytes, leaving everything else as the fixture built it.
fn declare_bat_region_length(path: &std::path::Path, bat_len: u32) {
    let table = encode_region_table(BIG_BAT_OFFSET, bat_len, BIG_METADATA_OFFSET);
    let mut f = open_file_rw(path);
    for off in [REGION_TABLE1_OFFSET, REGION_TABLE2_OFFSET] {
        f.seek(SeekFrom::Start(off)).unwrap();
        f.write_all(&table).unwrap();
    }
    f.flush().unwrap();
}

/// A BAT region too short for the disk is refused at open, naming the
/// region.
///
/// The region table declares a length and the metadata declares a
/// virtual size; the two are independent statements about the same
/// table and nothing compared them. The shortfall surfaced on a read
/// instead, as "BAT index out of range", the first time a caller
/// touched a block past the end of the table — blaming the entry rather
/// than the declaration, and only for a caller that reached that far.
///
/// The fixture's disk needs four entries and this declares two, so
/// blocks 0 and 1 still read correctly: an image can be half-addressable
/// and look healthy from the front, which is why the check belongs at
/// open.
#[test]
fn a_bat_region_too_short_for_the_disk_is_refused_at_open() {
    let path = tmp_path("bat_region_short");
    build_big_vhdx(&path, &pattern_block(3));
    declare_bat_region_length(&path, 2 * 8);

    match VhdxReader::open(&path) {
        Err(Error::Corrupt(m)) => assert!(
            m.contains("BAT region is too short"),
            "refused, but not for the region's length: {m}"
        ),
        Ok(_) => panic!("a BAT region half the size the disk needs was accepted"),
        Err(e) => panic!("a short BAT region gave {e:?}"),
    }
}

/// Exactly enough is enough.
///
/// The bound needs both ends: a check written one entry too strict
/// refuses this image, and one too lax accepts the one above. The
/// fixture's four blocks need four entries — `chunk_ratio` is large
/// enough here that no sector-bitmap entry falls inside the range, so
/// the required count is the block count.
#[test]
fn a_bat_region_of_exactly_the_required_length_is_accepted() {
    let path = tmp_path("bat_region_exact");
    build_big_vhdx(&path, &pattern_block(3));
    declare_bat_region_length(&path, (BIG_BAT_ENTRIES * 8) as u32);

    let r = VhdxReader::open(&path).expect("the region holds exactly what the disk needs");
    let mut buf = [0u8; 16];
    r.read_at(0, &mut buf).unwrap();
    assert_eq!(buf[0], 0, "block 0 did not read back");
}

/// A length that is not a whole number of entries is refused rather
/// than rounded down.
///
/// `chunks_exact(8)` drops a trailing partial entry silently, so a
/// region declaring four entries and four spare bytes loaded as four
/// entries and nobody looked at the remainder. Nobody writes that on
/// purpose, which is the point: it is a declaration that disagrees with
/// itself, and hearing about it is worth more than tolerating it.
#[test]
fn a_bat_region_length_that_is_not_whole_entries_is_refused() {
    let path = tmp_path("bat_region_ragged");
    build_big_vhdx(&path, &pattern_block(3));
    declare_bat_region_length(&path, (BIG_BAT_ENTRIES * 8) as u32 + 4);

    match VhdxReader::open(&path) {
        Err(Error::Corrupt(m)) => assert!(
            m.contains("whole number of 8-byte entries"),
            "refused, but not for the ragged length: {m}"
        ),
        Ok(_) => panic!("a BAT region length of 8n+4 was accepted"),
        Err(e) => panic!("a ragged BAT region gave {e:?}"),
    }
}

// ---------------------------------------------------------------------------
// Differencing images
// ---------------------------------------------------------------------------

/// A differencing image is refused at open, not at the first read.
///
/// The refusal used to live in `transfer_end`, so `open` succeeded and
/// the geometry accessors all answered — and then every read failed. A
/// `VhdxReader` is handed out as an `fs_core::BlockDevice`, and the
/// stack above it takes a successful open as "this is a usable
/// device": what it got was a device reporting an 8 GiB size and
/// failing every read, which a partition probe cannot tell from an I/O
/// error on a real disk.
///
/// The message has to name differencing rather than the metadata item,
/// because a differencing image carries a required ParentLocator and a
/// required-item check would otherwise refuse it as an unrecognised
/// GUID — a true statement that tells a user nothing.
#[test]
fn a_differencing_image_is_refused_at_open() {
    let path = tmp_path("differencing");
    build_vhdx_with_file_params_flags(&path, &pattern_block(12), 0x2);

    match VhdxReader::open(&path) {
        Err(Error::Unsupported(m)) => assert!(
            m.contains("differencing"),
            "refused, but not as a differencing image: {m}"
        ),
        Ok(_) => panic!(
            "a differencing image opened; every read would then fail and the caller \
             would be told about its hardware"
        ),
        Err(e) => panic!("a differencing image gave {e:?}"),
    }
}

/// The flags word is read rather than assumed: bit 0 is a different
/// flag and must not refuse anything.
///
/// `leave_blocks_allocated` says how the writer treats freed blocks;
/// it has nothing to do with parents. A check written against the whole
/// word rather than bit 1 would refuse an ordinary image that carries
/// it, which is the too-strict failure and the worse one.
#[test]
fn the_leave_blocks_allocated_flag_is_not_a_parent() {
    let path = tmp_path("leave_blocks_allocated");
    build_vhdx_with_file_params_flags(&path, &pattern_block(13), 0x1);

    let r = VhdxReader::open(&path).expect("bit 0 is not a parent");
    assert!(!r.has_parent());
    let mut buf = [0u8; 16];
    r.read_at(0, &mut buf).expect("and the image still reads");
}
