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
    let path = tmp_path("write_partially_present");
    let block0 = pattern_block(2);
    build_big_vhdx(&path, &block0);

    // Re-stamp BAT entry 1 as PartiallyPresent (state 7) pointing at a
    // real, block-aligned offset, so it is well-formed rather than
    // merely corrupt.
    {
        // `WriteAt` is the portable seek-then-write in `tests/common`.
        // The positional syscall it stands in for is Unix-only, and CI
        // runs this on Windows too.
        let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
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

/// The in-block offset reaches the host file, and is not silently
/// collapsed to the start of the block.
///
/// Every other write test here reads back through the same reader, so
/// an error in the virtual→host offset arithmetic that applies equally
/// to `read_at` and `write_at` cancels out and the round-trip still
/// passes. That is a real risk now the arithmetic is one shared
/// definition (`block_chunks`) rather than two copies: one bug can no
/// longer disagree with itself.
///
/// So this asserts against the raw file instead — an oracle the reader
/// has no hand in. The payload must land at `host_block + in_block`,
/// and the bytes at the block's start must still be the original
/// pattern.
#[test]
fn a_write_lands_at_its_in_block_offset_in_the_host_file() {
    let path = tmp_path("in_block_offset");
    let block0 = pattern_block(1);
    build_big_vhdx(&path, &block0);

    // Deliberately not sector 0, and not sector-table-aligned either.
    const IN_BLOCK: u64 = 3 * 4096 + 512;
    let payload = [0x5Au8; 512];

    let r = VhdxReader::open_rw(&path).unwrap();
    r.write_at(IN_BLOCK, &payload).unwrap();
    drop(r);

    let raw = std::fs::read(&path).unwrap();
    let at = |off: u64| &raw[off as usize..off as usize + payload.len()];

    assert_eq!(
        at(BIG_DATA_BLOCK0_OFFSET + IN_BLOCK),
        &payload,
        "payload must be written at the block's host offset plus the in-block offset"
    );
    assert_eq!(
        at(BIG_DATA_BLOCK0_OFFSET),
        &block0[..payload.len()],
        "the start of the block must be untouched — an in-block offset \
         collapsed to zero would have overwritten exactly this"
    );
    let _ = std::fs::remove_file(&path);
}

/// Build a log entry carrying a single "zero" descriptor.
///
/// The encoder only emits "desc" descriptors, so the zeroing half of
/// replay has to be laid down byte by byte. Layout is signature(4) +
/// checksum(4) + entry_length(4) + tail(4) + sequence(8) +
/// descriptor_count(4) + reserved(4) + log_guid(16), then the
/// descriptor: "zero"(4) + reserved(4) + zero_length(8) +
/// file_offset(8) + sequence(8).
fn zero_descriptor_entry(seq: u64, guid: &[u8; 16], file_offset: u64, len: u64) -> Vec<u8> {
    const SECTOR: usize = 4096;
    const ENTRY_HEADER: usize = 64;
    let mut buf = vec![0u8; SECTOR];
    buf[0..4].copy_from_slice(b"loge");
    buf[8..12].copy_from_slice(&(SECTOR as u32).to_le_bytes());
    buf[16..24].copy_from_slice(&seq.to_le_bytes());
    buf[24..28].copy_from_slice(&1u32.to_le_bytes());
    buf[32..48].copy_from_slice(guid);
    let d = ENTRY_HEADER;
    buf[d..d + 4].copy_from_slice(b"zero");
    buf[d + 8..d + 16].copy_from_slice(&len.to_le_bytes());
    buf[d + 16..d + 24].copy_from_slice(&file_offset.to_le_bytes());
    buf[d + 24..d + 32].copy_from_slice(&seq.to_le_bytes());
    let crc = {
        let mut tmp = buf.clone();
        tmp[4..8].fill(0);
        crc32c::crc32c(&tmp)
    };
    buf[4..8].copy_from_slice(&crc.to_le_bytes());
    buf
}

/// Point the header at a live log so the next open replays it.
fn arm_the_log(path: &std::path::PathBuf, entry: &[u8], log_guid: &[u8; 16]) {
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    f.seek(SeekFrom::Start(BIG_LOG_OFFSET)).unwrap();
    f.write_all(entry).unwrap();

    let mut hdr = vec![0u8; HEADER_SIZE];
    hdr[0..4].copy_from_slice(b"head");
    hdr[8..16].copy_from_slice(&5u64.to_le_bytes());
    hdr[48..64].copy_from_slice(log_guid);
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

/// Opening a file must not damage it.
///
/// `open` is the read-only entry point, and it takes the host file
/// read-write when it can, because a dirty VHDX has to be replayed
/// before its region table, metadata and BAT mean anything. That makes
/// every log descriptor in the file a write request, and the two
/// numbers a "zero" descriptor carries -- where to start and how much
/// to erase -- came out of the same file.
///
/// What the reader did with them: erased from byte 0 for as far as the
/// descriptor asked, growing the file to make room, and then returned
/// "no valid VHDX region table found" -- so the caller was told the
/// file was not a VHDX, and not that it had just been overwritten.
#[test]
fn a_read_only_open_does_not_let_the_log_erase_the_file() {
    let path = tmp_path("hostile_zero_span");
    let block0 = pattern_block(9);
    build_big_vhdx(&path, &block0);

    let size_before = std::fs::metadata(&path).unwrap().len();
    let mut before = vec![0u8; 4096];
    {
        use std::io::Read;
        let mut f = std::fs::File::open(&path).unwrap();
        f.seek(SeekFrom::Start(BIG_DATA_BLOCK0_OFFSET)).unwrap();
        f.read_exact(&mut before).unwrap();
    }

    let log_guid = [0x99u8; 16];
    // From the very start of the file, for eight times its length.
    arm_the_log(
        &path,
        &zero_descriptor_entry(2, &log_guid, 0, BIG_TOTAL_FILE_SIZE * 8),
        &log_guid,
    );

    let outcome = VhdxReader::open(&path);
    assert!(
        outcome.is_err(),
        "a log naming a span eight times the length of the file was accepted"
    );

    let size_after = std::fs::metadata(&path).unwrap().len();
    assert_eq!(
        size_before, size_after,
        "the file grew from {size_before} to {size_after} bytes -- the \
         replay wrote past the end of it"
    );

    let mut after = vec![0u8; 4096];
    {
        use std::io::Read;
        let mut f = std::fs::File::open(&path).unwrap();
        f.seek(SeekFrom::Start(BIG_DATA_BLOCK0_OFFSET)).unwrap();
        f.read_exact(&mut after).unwrap();
    }
    assert_eq!(before, after, "the image's first data block was erased");

    let _ = std::fs::remove_file(&path);
}

/// The log, the metadata table and the BAT are each located by an
/// offset and a `u32` length that came out of the image, and each is
/// read whole into a buffer sized from that length alone. A file of a
/// few kilobytes could ask for 4 GiB three times over.
///
/// The sibling VHD reader has always checked that its BAT fits inside
/// the file it lives in. These are the same kind of number.
#[test]
fn a_region_claiming_more_than_the_file_is_refused() {
    for (which, entry_offset) in [("BAT", 16u64), ("metadata", 16 + 32)] {
        let path = tmp_path(&format!("oversized_{which}"));
        let block0 = pattern_block(11);
        build_big_vhdx(&path, &block0);

        {
            let mut f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            let mut rt = vec![0u8; REGION_TABLE_SIZE];
            {
                use std::io::Read;
                f.seek(SeekFrom::Start(REGION_TABLE1_OFFSET)).unwrap();
                f.read_exact(&mut rt).unwrap();
            }
            let len_at = (entry_offset + 24) as usize;
            rt[len_at..len_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            let crc = {
                let mut tmp = rt.clone();
                tmp[4..8].fill(0);
                crc32c::crc32c(&tmp)
            };
            rt[4..8].copy_from_slice(&crc.to_le_bytes());
            for at in [REGION_TABLE1_OFFSET, REGION_TABLE2_OFFSET] {
                f.seek(SeekFrom::Start(at)).unwrap();
                f.write_all(&rt).unwrap();
            }
            f.flush().unwrap();
        }

        // The open fails either way -- reading 4 GiB out of a 64 MiB
        // file comes up short. What is being asserted is that it is
        // refused by name, before the buffer is allocated and the read
        // is issued, rather than by the read running out of file.
        let outcome = VhdxReader::open(&path);
        let why = format!("{:?}", outcome.as_ref().err());
        assert!(
            why.contains("past the end of the file"),
            "a {which} region of 4 GiB in a 64 MiB file was refused as {why}, \
             which means the buffer was allocated and read first"
        );
        let _ = std::fs::remove_file(&path);
    }
}

/// A BAT entry is one 64-bit word off the disk. `BatEntry::from_u64`
/// keeps its top 44 bits as a megabyte count, so the largest host
/// offset it can name is just under 2^64, and the reader added the
/// offset within the block to it and read there.
///
/// Past the end of the file that produced a short read, which at least
/// failed. Above `2^64 - block_size` it wrapped, and a wrapped offset
/// does not fail: it reads somewhere else in the file and hands the
/// caller those bytes as the guest's. (The wrap needs a block larger
/// than 1 MiB to reach; the bound below is what stops both.)
#[test]
fn a_bat_entry_pointing_past_the_file_is_refused_by_name() {
    let path = tmp_path("bat_past_file");
    let block0 = pattern_block(12);
    build_big_vhdx(&path, &block0);

    {
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        // FullyPresent (state 6) at the highest megabyte-aligned offset
        // the encoding can express.
        let entry: u64 = 0xFFFF_FFFF_FFF0_0000 | 6;
        f.seek(SeekFrom::Start(BIG_BAT_OFFSET)).unwrap();
        f.write_all(&entry.to_le_bytes()).unwrap();
        f.flush().unwrap();
    }

    let r = VhdxReader::open(&path).unwrap();
    let mut buf = vec![0u8; 4096];
    let why = format!("{:?}", r.read_at(0, &mut buf).err());
    assert!(
        why.contains("past the file"),
        "a BAT entry at offset 2^64 - 1 MiB was refused as {why}, which means \
         the read was issued at that offset rather than refused"
    );

    drop(r);
    let _ = std::fs::remove_file(&path);
}
