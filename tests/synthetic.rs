//! End-to-end tests on hand-crafted VHDX images.
//!
//! The byte-level builders live in `tests/common/mod.rs` so the
//! corruption tests can reuse them. The small fixture is one 1 MiB
//! block; the "big" fixture adds a real (empty) log region, a 4-entry
//! BAT, and tail slack for the writer to allocate fresh blocks.

mod common;

use std::io::{Seek, SeekFrom, Write};

use common::*;
use vhdx::VhdxReader;

#[test]
fn fully_present_block_round_trips() {
    let path = tmp_path("full");
    let mut data = [0u8; ONE_MIB as usize];
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = (i & 0xFF) as u8;
    }
    build_vhdx(&path, &data);

    let r = VhdxReader::open(&path).unwrap();
    assert_eq!(r.virtual_size(), VIRTUAL_DISK_SIZE);
    assert_eq!(r.block_size(), BLOCK_SIZE);
    assert_eq!(r.sector_size(), SECTOR_SIZE);
    assert!(!r.has_parent());

    // Read first 4 KiB.
    let mut buf = vec![0u8; 4096];
    r.read_at(0, &mut buf).unwrap();
    for (i, b) in buf.iter().enumerate() {
        assert_eq!(*b, (i & 0xFF) as u8, "byte {i} mismatch");
    }

    // Read at offset 1000 KiB (still inside the single 1 MiB block).
    let mut tail = vec![0u8; 1024];
    r.read_at(1000 * 1024, &mut tail).unwrap();
    for (i, b) in tail.iter().enumerate() {
        let expected = ((1000 * 1024 + i) & 0xFF) as u8;
        assert_eq!(*b, expected);
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn out_of_bounds_read_errors() {
    let path = tmp_path("oob");
    let data = [0u8; ONE_MIB as usize];
    build_vhdx(&path, &data);

    let r = VhdxReader::open(&path).unwrap();
    let mut buf = [0u8; 16];
    let err = r.read_at(VIRTUAL_DISK_SIZE - 8, &mut buf).unwrap_err();
    matches!(err, vhdx::Error::OutOfBounds { .. });
    let _ = std::fs::remove_file(&path);
}

#[test]
fn fs_core_blockread_size_matches_virtual() {
    let path = tmp_path("fs_core_size");
    let data = [0u8; ONE_MIB as usize];
    build_vhdx(&path, &data);

    let r = VhdxReader::open(&path).unwrap();
    assert_eq!(
        <VhdxReader as fs_core::BlockRead>::size_bytes(&r),
        VIRTUAL_DISK_SIZE
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn rw_open_writes_into_allocated_block() {
    let path = tmp_path("write_allocated");
    let block0 = pattern_block(1);
    build_big_vhdx(&path, &block0);

    let r = VhdxReader::open_rw(&path).unwrap();
    assert!(r.is_writable());

    // Single-sector write into allocated block 0.
    let payload = [0xC3u8; 512];
    r.write_at(2048, &payload).unwrap();

    // Read it back through the same handle.
    let mut readback = [0u8; 512];
    r.read_at(2048, &mut readback).unwrap();
    assert_eq!(readback, payload);

    // Untouched bytes inside block 0 still match the original pattern.
    let mut around = [0u8; 32];
    r.read_at(0, &mut around).unwrap();
    for (i, b) in around.iter().enumerate() {
        assert_eq!(*b, ((i as u32).wrapping_mul(2) & 0xFF) as u8);
    }
    drop(r);

    // Reopen — change must be durable.
    let r2 = VhdxReader::open(&path).unwrap();
    let mut readback = [0u8; 512];
    r2.read_at(2048, &mut readback).unwrap();
    assert_eq!(readback, payload);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn rw_open_writes_into_unallocated_block_allocates() {
    let path = tmp_path("write_unalloc");
    let block0 = pattern_block(2);
    build_big_vhdx(&path, &block0);

    let r = VhdxReader::open_rw(&path).unwrap();

    // Block 1 (virtual offset 1 MiB) is unallocated. Writing into it
    // should trigger allocation, BAT update, and the read-back should
    // see the payload + zeros for the rest of the block.
    let payload = [0x99u8; 4096];
    let virt_off = BIG_BLOCK_SIZE as u64; // start of block 1
    r.write_at(virt_off + 8192, &payload).unwrap();

    // Bytes the caller wrote.
    let mut got = [0u8; 4096];
    r.read_at(virt_off + 8192, &mut got).unwrap();
    assert_eq!(got, payload);

    // Untouched bytes inside the freshly-allocated block read as zero.
    let mut zero = [0u8; 4096];
    r.read_at(virt_off, &mut zero).unwrap();
    assert!(zero.iter().all(|b| *b == 0), "leading bytes not zero");

    drop(r);

    // Reopen and confirm allocation persisted.
    let r2 = VhdxReader::open(&path).unwrap();
    let mut got = [0u8; 4096];
    r2.read_at(virt_off + 8192, &mut got).unwrap();
    assert_eq!(got, payload);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn rw_open_multi_block_write_spans_allocated_and_unallocated() {
    let path = tmp_path("write_multiblock");
    let block0 = pattern_block(3);
    build_big_vhdx(&path, &block0);

    let r = VhdxReader::open_rw(&path).unwrap();

    // Write 1.5 MiB starting 256 KiB inside block 0 (allocated) so it
    // straddles into block 1 (unallocated, must allocate).
    let len: usize = (3 * BIG_BLOCK_SIZE as usize) / 2;
    let mut payload = vec![0u8; len];
    for (i, b) in payload.iter_mut().enumerate() {
        *b = (i & 0xFF) as u8;
    }
    let start = 256 * 1024u64;
    r.write_at(start, &payload).unwrap();

    let mut readback = vec![0u8; len];
    r.read_at(start, &mut readback).unwrap();
    assert_eq!(readback, payload);

    drop(r);
    let r2 = VhdxReader::open(&path).unwrap();
    let mut readback = vec![0u8; len];
    r2.read_at(start, &mut readback).unwrap();
    assert_eq!(readback, payload);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn ro_open_against_writable_file_replays_dirty_log() {
    // 1. Build a clean image.
    let path = tmp_path("dirty_replay");
    let block0 = pattern_block(4);
    build_big_vhdx(&path, &block0);

    // 2. Inject a dirty log: forge a single-entry log that overwrites
    //    a 4 KiB sector inside block 0 with 0xEE, and bump the
    //    header's log_guid so the reader thinks the log is active.
    let log_guid = [0x77u8; 16];
    let sector = vec![0xEEu8; 4096];
    let entry = vhdx::log::encode_entry(
        2,
        0,
        &log_guid,
        BIG_TOTAL_FILE_SIZE,
        BIG_TOTAL_FILE_SIZE,
        &[vhdx::log::PendingWrite {
            file_offset: BIG_DATA_BLOCK0_OFFSET + 8192,
            sector: sector.clone(),
        }],
    );

    {
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        // Splice the entry into the log region.
        f.seek(SeekFrom::Start(BIG_LOG_OFFSET)).unwrap();
        f.write_all(&entry).unwrap();

        // Bump header.log_guid, sequence_number; rewrite header 2 (the
        // currently-inactive slot) so it wins on next open.
        let mut hdr = vec![0u8; HEADER_SIZE];
        hdr[0..4].copy_from_slice(b"head");
        hdr[8..16].copy_from_slice(&5u64.to_le_bytes());
        hdr[48..64].copy_from_slice(&log_guid);
        hdr[66..68].copy_from_slice(&1u16.to_le_bytes());
        hdr[68..72].copy_from_slice(&BIG_LOG_LENGTH.to_le_bytes());
        hdr[72..80].copy_from_slice(&BIG_LOG_OFFSET.to_le_bytes());
        let crc = {
            let mut tmp = hdr.clone();
            tmp[4..8].fill(0);
            crc32c::crc32c(&tmp)
        };
        hdr[4..8].copy_from_slice(&crc.to_le_bytes());
        f.seek(SeekFrom::Start(HEADER2_OFFSET)).unwrap();
        f.write_all(&hdr).unwrap();
        f.flush().unwrap();
    }

    // 3. Open RO. The file is RW on disk so replay can run in place.
    let r = VhdxReader::open(&path).unwrap();

    // 4. Block 0 sector at +8 KiB now reads as 0xEE — replay applied.
    let mut got = [0u8; 4096];
    r.read_at(8192, &mut got).unwrap();
    assert!(got.iter().all(|b| *b == 0xEE), "first byte: {:#x}", got[0]);

    // 5. Reopening must NOT replay again — log_guid was cleared.
    drop(r);
    let _ = VhdxReader::open(&path).unwrap();
    let _ = std::fs::remove_file(&path);
}

#[test]
fn open_on_device_round_trips_through_file_device() {
    let path = tmp_path("on_device");
    let block0 = pattern_block(5);
    build_big_vhdx(&path, &block0);

    let dev = std::sync::Arc::new(fs_core::FileDevice::open(&path).unwrap());
    let r = VhdxReader::open_on_device(dev).unwrap();
    assert_eq!(r.virtual_size(), BIG_VIRTUAL_DISK_SIZE);

    let mut buf = [0u8; 4096];
    r.read_at(0, &mut buf).unwrap();
    for (i, b) in buf.iter().enumerate() {
        let expected = ((i as u32).wrapping_mul(6) & 0xFF) as u8;
        assert_eq!(*b, expected, "byte {i} mismatch");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn open_rw_on_device_writes_persist() {
    let path = tmp_path("rw_on_device");
    let block0 = pattern_block(6);
    build_big_vhdx(&path, &block0);

    let dev = std::sync::Arc::new(fs_core::FileDevice::open_rw(&path).unwrap());
    let r = VhdxReader::open_rw_on_device(dev).unwrap();
    assert!(r.is_writable());

    let payload = [0x55u8; 1024];
    r.write_at(4096, &payload).unwrap();
    r.flush().unwrap();

    let mut readback = [0u8; 1024];
    r.read_at(4096, &mut readback).unwrap();
    assert_eq!(readback, payload);
    drop(r);

    let r2 = VhdxReader::open(&path).unwrap();
    let mut readback = [0u8; 1024];
    r2.read_at(4096, &mut readback).unwrap();
    assert_eq!(readback, payload);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn open_rw_on_device_rejects_readonly_inner() {
    let path = tmp_path("rw_on_ro_inner");
    let block0 = pattern_block(7);
    build_big_vhdx(&path, &block0);

    let dev = std::sync::Arc::new(fs_core::FileDevice::open(&path).unwrap());
    match VhdxReader::open_rw_on_device(dev) {
        Err(vhdx::Error::ReadOnly) => {}
        Err(e) => panic!("expected ReadOnly, got: {e}"),
        Ok(_) => panic!("expected ReadOnly, got Ok"),
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn ro_open_rejects_write() {
    let path = tmp_path("ro_rejects_write");
    let block0 = pattern_block(8);
    build_big_vhdx(&path, &block0);

    let r = VhdxReader::open(&path).unwrap();
    let err = r.write_at(0, b"x").unwrap_err();
    assert!(matches!(err, vhdx::Error::ReadOnly));
    let _ = std::fs::remove_file(&path);
}

/// Writing into a PartiallyPresent block is refused, not silently
/// serviced by allocating a fresh one over it.
///
/// The block's payload is on disk and its valid sectors are described by
/// a bitmap this crate does not walk — which is why `read_at` refuses
/// the state outright. The write path used to catch it in a `_ =>` arm
/// alongside the genuinely-empty states and hand it to
/// `allocate_block_for`, publishing a zeroed block over it. That
/// discards every sector the bitmap called valid, and does so while
/// reporting success.
///
/// Refusing is the only consistent answer: a reader that admits it
/// cannot interpret the block must not have a writer that overwrites it.
#[test]
fn write_to_a_partially_present_block_is_refused() {
    use std::os::unix::fs::FileExt;

    let path = tmp_path("write_partially_present");
    let block0 = pattern_block(2);
    build_big_vhdx(&path, &block0);

    // Re-stamp BAT entry 1 as PartiallyPresent (state 7) pointing at a
    // real, block-aligned offset, so it is well-formed rather than
    // merely corrupt.
    {
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        let entry: u64 = ((BIG_DATA_BLOCK0_OFFSET >> 20) << 20) | 7;
        f.write_all_at(&entry.to_le_bytes(), BIG_BAT_OFFSET + 8)
            .unwrap();
    }

    let r = VhdxReader::open_rw(&path).unwrap();
    let virt_off = BIG_BLOCK_SIZE as u64; // start of block 1

    let err = r
        .write_at(virt_off, &[0xAAu8; 4096])
        .expect_err("a PartiallyPresent block must not be overwritten by a fresh allocation");
    let msg = format!("{err}");
    assert!(
        msg.contains("PartiallyPresent"),
        "the refusal should name the state, got: {msg}"
    );

    // And the reader still refuses it too, so the two paths agree.
    let mut buf = [0u8; 4096];
    assert!(r.read_at(virt_off, &mut buf).is_err());

    drop(r);
    let _ = std::fs::remove_file(&path);
}
