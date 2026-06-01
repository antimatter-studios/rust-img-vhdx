//! Shared synthetic-image builders for integration tests.
//!
//! Each integration test binary in `tests/` (`synthetic.rs`,
//! `corruption.rs`, …) includes this module via `mod common;`. Builders
//! here hand-write the on-disk VHDX byte layout per the spec — the
//! reader/writer is never invoked when producing fixtures.
//!
//! `tests/qemu_validation.rs` does NOT use these builders: it relies on
//! qemu-img to emit real images, so it carries its own helpers.

#![allow(dead_code)] // not every consumer uses every helper

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;

pub const ONE_MIB: u64 = 1024 * 1024;
pub const HEADER_SIZE: usize = 4096;
pub const REGION_TABLE_SIZE: usize = 64 * 1024;
pub const METADATA_REGION_SIZE: usize = 64 * 1024;
pub const TOTAL_FILE_SIZE: u64 = 4 * ONE_MIB;

pub const HEADER1_OFFSET: u64 = 64 * 1024;
pub const HEADER2_OFFSET: u64 = 128 * 1024;
pub const REGION_TABLE1_OFFSET: u64 = 192 * 1024;
pub const REGION_TABLE2_OFFSET: u64 = 256 * 1024;
pub const METADATA_REGION_OFFSET: u64 = ONE_MIB;
pub const BAT_REGION_OFFSET: u64 = 2 * ONE_MIB;
pub const DATA_BLOCK_OFFSET: u64 = 3 * ONE_MIB;

pub const BAT_REGION_LEN: u32 = 8; // one entry
pub const BLOCK_SIZE: u32 = ONE_MIB as u32;
pub const SECTOR_SIZE: u32 = 512;
pub const VIRTUAL_DISK_SIZE: u64 = ONE_MIB;

// GUIDs in mixed-endian on-disk form, copied from `region_table::guids`
// and `metadata::item_ids` so the tests stay self-contained.
pub const BAT_GUID: [u8; 16] = [
    0x66, 0x77, 0xC2, 0x2D, 0x23, 0xF6, 0x00, 0x42, 0x9D, 0x64, 0x11, 0x5E, 0x9B, 0xFD, 0x4A, 0x08,
];
pub const METADATA_GUID: [u8; 16] = [
    0x06, 0xA2, 0x7C, 0x8B, 0x90, 0x47, 0x9A, 0x4B, 0xB8, 0xFE, 0x57, 0x5F, 0x05, 0x0F, 0x88, 0x6E,
];
pub const FILE_PARAMS_ID: [u8; 16] = [
    0x37, 0x67, 0xA1, 0xCA, 0x36, 0xFA, 0x43, 0x4D, 0xB3, 0xB6, 0x33, 0xF0, 0xAA, 0x44, 0xE7, 0x6B,
];
pub const VIRT_SIZE_ID: [u8; 16] = [
    0x24, 0x42, 0xA5, 0x2F, 0x1B, 0xCD, 0x76, 0x48, 0xB2, 0x11, 0x5D, 0xBE, 0xD8, 0x3B, 0xF4, 0xB8,
];
pub const LOGICAL_SECTOR_ID: [u8; 16] = [
    0x1D, 0xBF, 0x41, 0x81, 0x6F, 0xA9, 0x09, 0x47, 0xBA, 0x47, 0xF2, 0x33, 0xA8, 0xFA, 0xAB, 0x5F,
];

pub trait WriteAt {
    fn write_all_at(&mut self, buf: &[u8], offset: u64) -> std::io::Result<()>;
}
impl WriteAt for File {
    fn write_all_at(&mut self, buf: &[u8], offset: u64) -> std::io::Result<()> {
        self.seek(SeekFrom::Start(offset))?;
        self.write_all(buf)
    }
}

pub fn tmp_path(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!("vhdx_synth_{}_{n}_{name}.vhdx", std::process::id()));
    p
}

/// Encode a 4 KiB header with a valid CRC-32C. `seq` is the sequence
/// number (the reader picks the higher of the two valid headers).
pub fn encode_header(seq: u64, log_guid: [u8; 16], log_length: u32, log_offset: u64) -> Vec<u8> {
    let mut hdr = vec![0u8; HEADER_SIZE];
    hdr[0..4].copy_from_slice(b"head");
    hdr[8..16].copy_from_slice(&seq.to_le_bytes());
    hdr[48..64].copy_from_slice(&log_guid);
    hdr[64..66].copy_from_slice(&0u16.to_le_bytes()); // log_version
    hdr[66..68].copy_from_slice(&1u16.to_le_bytes()); // version = 1.0
    hdr[68..72].copy_from_slice(&log_length.to_le_bytes());
    hdr[72..80].copy_from_slice(&log_offset.to_le_bytes());
    let crc = {
        let mut tmp = hdr.clone();
        tmp[4..8].fill(0);
        crc32c::crc32c(&tmp)
    };
    hdr[4..8].copy_from_slice(&crc.to_le_bytes());
    hdr
}

/// Encode the region table with a BAT entry and a metadata entry.
pub fn encode_region_table(bat_offset: u64, bat_len: u32, meta_offset: u64) -> Vec<u8> {
    let mut rt = vec![0u8; REGION_TABLE_SIZE];
    rt[0..4].copy_from_slice(b"regi");
    rt[8..12].copy_from_slice(&2u32.to_le_bytes()); // entry_count = 2

    let off = 16;
    rt[off..off + 16].copy_from_slice(&BAT_GUID);
    rt[off + 16..off + 24].copy_from_slice(&bat_offset.to_le_bytes());
    rt[off + 24..off + 28].copy_from_slice(&bat_len.to_le_bytes());
    rt[off + 28..off + 32].copy_from_slice(&1u32.to_le_bytes()); // required

    let off = 16 + 32;
    rt[off..off + 16].copy_from_slice(&METADATA_GUID);
    rt[off + 16..off + 24].copy_from_slice(&meta_offset.to_le_bytes());
    rt[off + 24..off + 28].copy_from_slice(&(METADATA_REGION_SIZE as u32).to_le_bytes());
    rt[off + 28..off + 32].copy_from_slice(&1u32.to_le_bytes()); // required

    let crc = {
        let mut tmp = rt.clone();
        tmp[4..8].fill(0);
        crc32c::crc32c(&tmp)
    };
    rt[4..8].copy_from_slice(&crc.to_le_bytes());
    rt
}

/// Encode the metadata region (FileParameters, VirtualDiskSize,
/// LogicalSectorSize).
pub fn encode_metadata(block_size: u32, virtual_disk_size: u64, sector_size: u32) -> Vec<u8> {
    let mut meta = vec![0u8; METADATA_REGION_SIZE];
    meta[0..8].copy_from_slice(b"metadata");
    meta[10..12].copy_from_slice(&3u16.to_le_bytes()); // entry_count = 3

    let items_start = 32 + 3 * 32; // 128
    let file_params_off = items_start as u32;
    let file_params_len = 8u32;
    let virt_size_off = file_params_off + file_params_len;
    let virt_size_len = 8u32;
    let sector_size_off = virt_size_off + virt_size_len;
    let sector_size_len = 4u32;

    let off = 32;
    meta[off..off + 16].copy_from_slice(&FILE_PARAMS_ID);
    meta[off + 16..off + 20].copy_from_slice(&file_params_off.to_le_bytes());
    meta[off + 20..off + 24].copy_from_slice(&file_params_len.to_le_bytes());
    meta[off + 24..off + 28].copy_from_slice(&0x6u32.to_le_bytes());

    let off = 32 + 32;
    meta[off..off + 16].copy_from_slice(&VIRT_SIZE_ID);
    meta[off + 16..off + 20].copy_from_slice(&virt_size_off.to_le_bytes());
    meta[off + 20..off + 24].copy_from_slice(&virt_size_len.to_le_bytes());
    meta[off + 24..off + 28].copy_from_slice(&0x6u32.to_le_bytes());

    let off = 32 + 64;
    meta[off..off + 16].copy_from_slice(&LOGICAL_SECTOR_ID);
    meta[off + 16..off + 20].copy_from_slice(&sector_size_off.to_le_bytes());
    meta[off + 20..off + 24].copy_from_slice(&sector_size_len.to_le_bytes());
    meta[off + 24..off + 28].copy_from_slice(&0x6u32.to_le_bytes());

    meta[file_params_off as usize..(file_params_off + 4) as usize]
        .copy_from_slice(&block_size.to_le_bytes());
    meta[(file_params_off + 4) as usize..(file_params_off + 8) as usize]
        .copy_from_slice(&0u32.to_le_bytes()); // flags = 0 (no parent)
    meta[virt_size_off as usize..(virt_size_off + 8) as usize]
        .copy_from_slice(&virtual_disk_size.to_le_bytes());
    meta[sector_size_off as usize..(sector_size_off + 4) as usize]
        .copy_from_slice(&sector_size.to_le_bytes());
    meta
}

/// Build a minimal 1-block VHDX. Header 1 is valid (sequence=1),
/// header 2 is zero (invalid), so the reader picks header 1.
pub fn build_vhdx(path: &PathBuf, data: &[u8; BLOCK_SIZE as usize]) {
    let mut f = File::create(path).unwrap();
    f.set_len(TOTAL_FILE_SIZE).unwrap();

    let mut id = vec![0u8; 64 * 1024];
    id[0..8].copy_from_slice(b"vhdxfile");
    f.write_all_at(&id, 0).unwrap();

    f.write_all_at(&encode_header(1, [0u8; 16], 0, 0), HEADER1_OFFSET)
        .unwrap();
    f.write_all_at(
        &encode_region_table(BAT_REGION_OFFSET, BAT_REGION_LEN, METADATA_REGION_OFFSET),
        REGION_TABLE1_OFFSET,
    )
    .unwrap();
    f.write_all_at(
        &encode_metadata(BLOCK_SIZE, VIRTUAL_DISK_SIZE, SECTOR_SIZE),
        METADATA_REGION_OFFSET,
    )
    .unwrap();

    // BAT — single entry, FullyPresent (state=6), file_offset = 3 MiB.
    let bat_entry: u64 = (3u64 << 20) | 6;
    f.write_all_at(&bat_entry.to_le_bytes(), BAT_REGION_OFFSET)
        .unwrap();

    f.write_all_at(data, DATA_BLOCK_OFFSET).unwrap();
}

// ---------------------------------------------------------------------------
// Larger fixture for write + log-replay tests.
// ---------------------------------------------------------------------------

pub const BIG_BLOCK_SIZE: u32 = ONE_MIB as u32;
pub const BIG_BAT_ENTRIES: u64 = 4;
pub const BIG_VIRTUAL_DISK_SIZE: u64 = BIG_BLOCK_SIZE as u64 * BIG_BAT_ENTRIES;
pub const BIG_LOG_OFFSET: u64 = 4 * ONE_MIB;
pub const BIG_LOG_LENGTH: u32 = ONE_MIB as u32;
pub const BIG_METADATA_OFFSET: u64 = 5 * ONE_MIB;
pub const BIG_BAT_OFFSET: u64 = 6 * ONE_MIB;
pub const BIG_DATA_BLOCK0_OFFSET: u64 = 7 * ONE_MIB;
pub const BIG_TOTAL_FILE_SIZE: u64 = 64 * ONE_MIB;

/// Build a VHDX image with a real (empty) log region, a 4-entry BAT, and
/// only the first block on disk. Used by the write-path and log-replay
/// tests.
pub fn build_big_vhdx(path: &PathBuf, block0: &[u8; BIG_BLOCK_SIZE as usize]) {
    let mut f = File::create(path).unwrap();
    f.set_len(BIG_TOTAL_FILE_SIZE).unwrap();

    let mut id = vec![0u8; 64 * 1024];
    id[0..8].copy_from_slice(b"vhdxfile");
    f.write_all_at(&id, 0).unwrap();

    // Header 1 — sequence=1, log region declared but empty (log_guid
    // all-zero so the reader skips replay).
    f.write_all_at(
        &encode_header(1, [0u8; 16], BIG_LOG_LENGTH, BIG_LOG_OFFSET),
        HEADER1_OFFSET,
    )
    .unwrap();

    let bat_region_len = (BIG_BAT_ENTRIES * 8) as u32;
    f.write_all_at(
        &encode_region_table(BIG_BAT_OFFSET, bat_region_len, BIG_METADATA_OFFSET),
        REGION_TABLE1_OFFSET,
    )
    .unwrap();
    f.write_all_at(
        &encode_metadata(BIG_BLOCK_SIZE, BIG_VIRTUAL_DISK_SIZE, SECTOR_SIZE),
        BIG_METADATA_OFFSET,
    )
    .unwrap();

    // BAT — 4 entries; only entry 0 is FullyPresent at 7 MiB.
    let mut bat = vec![0u8; (BIG_BAT_ENTRIES * 8) as usize];
    let entry0: u64 = ((BIG_DATA_BLOCK0_OFFSET >> 20) << 20) | 6;
    bat[0..8].copy_from_slice(&entry0.to_le_bytes());
    f.write_all_at(&bat, BIG_BAT_OFFSET).unwrap();

    f.write_all_at(block0, BIG_DATA_BLOCK0_OFFSET).unwrap();
}

pub fn pattern_block(seed: u8) -> Box<[u8; BIG_BLOCK_SIZE as usize]> {
    let mut data = Box::new([0u8; BIG_BLOCK_SIZE as usize]);
    for (i, b) in data.iter_mut().enumerate() {
        *b = ((i as u32).wrapping_mul(seed as u32 + 1) & 0xFF) as u8;
    }
    data
}
