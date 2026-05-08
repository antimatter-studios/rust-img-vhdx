//! End-to-end test on a hand-crafted VHDX image.
//!
//! Layout (1 MiB virtual; 1 MiB block_size; 1 block on disk):
//!
//!   offset 0 ............ file identifier ("vhdxfile" + creator)
//!   offset 64 KiB ....... header 1 (sequence=1, valid)
//!   offset 128 KiB ...... header 2 (zero — invalid, reader picks h1)
//!   offset 192 KiB ...... region table 1 (BAT + metadata entries)
//!   offset 256 KiB ...... region table 2 (zero — invalid, reader uses #1)
//!   offset 1 MiB ........ metadata region (64 KiB; FileParameters,
//!                         VirtualDiskSize, LogicalSectorSize)
//!   offset 2 MiB ........ BAT region (one 8-byte entry)
//!   offset 3 MiB ........ data block 0 (1 MiB of pattern)
//!   total file size ..... 4 MiB

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;

use vhdx::VhdxReader;

const ONE_MIB: u64 = 1024 * 1024;
const HEADER_SIZE: usize = 4096;
const REGION_TABLE_SIZE: usize = 64 * 1024;
const METADATA_REGION_SIZE: usize = 64 * 1024;
const TOTAL_FILE_SIZE: u64 = 4 * ONE_MIB;

const HEADER1_OFFSET: u64 = 64 * 1024;
const REGION_TABLE1_OFFSET: u64 = 192 * 1024;
const METADATA_REGION_OFFSET: u64 = ONE_MIB;
const BAT_REGION_OFFSET: u64 = 2 * ONE_MIB;
const DATA_BLOCK_OFFSET: u64 = 3 * ONE_MIB;

const BAT_REGION_LEN: u32 = 8; // one entry
const BLOCK_SIZE: u32 = ONE_MIB as u32;
const SECTOR_SIZE: u32 = 512;
const VIRTUAL_DISK_SIZE: u64 = ONE_MIB;

// GUIDs in mixed-endian on-disk form, copied from `region_table::guids`
// and `metadata::item_ids` so the test stays self-contained.
const BAT_GUID: [u8; 16] = [
    0x66, 0x77, 0xC2, 0x2D, 0x23, 0xF6, 0x00, 0x42,
    0x9D, 0x64, 0x11, 0x5E, 0x9B, 0xFD, 0x4A, 0x08,
];
const METADATA_GUID: [u8; 16] = [
    0x06, 0xA2, 0x7C, 0x8B, 0x90, 0x47, 0x9A, 0x4B,
    0xB8, 0xFE, 0x57, 0x5F, 0x05, 0x0F, 0x88, 0x6E,
];
const FILE_PARAMS_ID: [u8; 16] = [
    0x37, 0x67, 0xA1, 0xCA, 0x36, 0xFA, 0x43, 0x4D,
    0xB3, 0xB6, 0x33, 0xF0, 0xAA, 0x44, 0xE7, 0x6B,
];
const VIRT_SIZE_ID: [u8; 16] = [
    0x24, 0x42, 0xA5, 0x2F, 0x1B, 0xCD, 0x76, 0x48,
    0xB2, 0x11, 0x5D, 0xBE, 0xD8, 0x3B, 0xF4, 0xB8,
];
const LOGICAL_SECTOR_ID: [u8; 16] = [
    0x1D, 0xBF, 0x41, 0x81, 0x6F, 0xA9, 0x09, 0x47,
    0xBA, 0x47, 0xF2, 0x33, 0xA8, 0xFA, 0xAB, 0x5F,
];

trait WriteAt {
    fn write_all_at(&mut self, buf: &[u8], offset: u64) -> std::io::Result<()>;
}
impl WriteAt for File {
    fn write_all_at(&mut self, buf: &[u8], offset: u64) -> std::io::Result<()> {
        self.seek(SeekFrom::Start(offset))?;
        self.write_all(buf)
    }
}

fn tmp_path(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "vhdx_synth_{}_{n}_{name}.vhdx",
        std::process::id()
    ));
    p
}

fn build_vhdx(path: &PathBuf, data: &[u8; BLOCK_SIZE as usize]) {
    let mut f = File::create(path).unwrap();
    f.set_len(TOTAL_FILE_SIZE).unwrap();

    // 1. File identifier.
    let mut id = vec![0u8; 64 * 1024];
    id[0..8].copy_from_slice(b"vhdxfile");
    // creator (UTF-16 LE) — optional, leave zeros.
    f.write_all_at(&id, 0).unwrap();

    // 2. Header 1 with proper CRC-32C.
    let mut hdr = vec![0u8; HEADER_SIZE];
    hdr[0..4].copy_from_slice(b"head");
    hdr[8..16].copy_from_slice(&1u64.to_le_bytes()); // sequence_number = 1
    // file_write_guid + data_write_guid + log_guid: leave zeros
    hdr[64..66].copy_from_slice(&0u16.to_le_bytes()); // log_version
    hdr[66..68].copy_from_slice(&1u16.to_le_bytes()); // version = 1.0
    hdr[68..72].copy_from_slice(&0u32.to_le_bytes()); // log_length = 0
    hdr[72..80].copy_from_slice(&0u64.to_le_bytes()); // log_offset = 0
    let hdr_crc = {
        let mut tmp = hdr.clone();
        tmp[4..8].fill(0);
        crc32c::crc32c(&tmp)
    };
    hdr[4..8].copy_from_slice(&hdr_crc.to_le_bytes());
    f.write_all_at(&hdr, HEADER1_OFFSET).unwrap();

    // 3. Region table 1.
    let mut rt = vec![0u8; REGION_TABLE_SIZE];
    rt[0..4].copy_from_slice(b"regi");
    rt[8..12].copy_from_slice(&2u32.to_le_bytes()); // entry_count = 2

    // Entry 0: BAT region.
    let off = 16;
    rt[off..off + 16].copy_from_slice(&BAT_GUID);
    rt[off + 16..off + 24].copy_from_slice(&BAT_REGION_OFFSET.to_le_bytes());
    rt[off + 24..off + 28].copy_from_slice(&BAT_REGION_LEN.to_le_bytes());
    rt[off + 28..off + 32].copy_from_slice(&1u32.to_le_bytes()); // required

    // Entry 1: Metadata region.
    let off = 16 + 32;
    rt[off..off + 16].copy_from_slice(&METADATA_GUID);
    rt[off + 16..off + 24].copy_from_slice(&METADATA_REGION_OFFSET.to_le_bytes());
    rt[off + 24..off + 28].copy_from_slice(&(METADATA_REGION_SIZE as u32).to_le_bytes());
    rt[off + 28..off + 32].copy_from_slice(&1u32.to_le_bytes()); // required

    let rt_crc = {
        let mut tmp = rt.clone();
        tmp[4..8].fill(0);
        crc32c::crc32c(&tmp)
    };
    rt[4..8].copy_from_slice(&rt_crc.to_le_bytes());
    f.write_all_at(&rt, REGION_TABLE1_OFFSET).unwrap();

    // 4. Metadata region.
    //    Header (32 bytes) + 3 entries (32 bytes each) + items.
    let mut meta = vec![0u8; METADATA_REGION_SIZE];
    meta[0..8].copy_from_slice(b"metadata");
    meta[10..12].copy_from_slice(&3u16.to_le_bytes()); // entry_count = 3

    // Items live just past the entry table.
    let items_start = 32 + 3 * 32; // 128
    let file_params_off = items_start as u32;
    let file_params_len = 8u32;
    let virt_size_off = file_params_off + file_params_len;
    let virt_size_len = 8u32;
    let sector_size_off = virt_size_off + virt_size_len;
    let sector_size_len = 4u32;

    // Entry 0: File Parameters.
    let off = 32;
    meta[off..off + 16].copy_from_slice(&FILE_PARAMS_ID);
    meta[off + 16..off + 20].copy_from_slice(&file_params_off.to_le_bytes());
    meta[off + 20..off + 24].copy_from_slice(&file_params_len.to_le_bytes());
    // flags = 0x06 (virtual_disk + required)
    meta[off + 24..off + 28].copy_from_slice(&0x6u32.to_le_bytes());

    // Entry 1: Virtual Disk Size.
    let off = 32 + 32;
    meta[off..off + 16].copy_from_slice(&VIRT_SIZE_ID);
    meta[off + 16..off + 20].copy_from_slice(&virt_size_off.to_le_bytes());
    meta[off + 20..off + 24].copy_from_slice(&virt_size_len.to_le_bytes());
    meta[off + 24..off + 28].copy_from_slice(&0x6u32.to_le_bytes());

    // Entry 2: Logical Sector Size.
    let off = 32 + 64;
    meta[off..off + 16].copy_from_slice(&LOGICAL_SECTOR_ID);
    meta[off + 16..off + 20].copy_from_slice(&sector_size_off.to_le_bytes());
    meta[off + 20..off + 24].copy_from_slice(&sector_size_len.to_le_bytes());
    meta[off + 24..off + 28].copy_from_slice(&0x6u32.to_le_bytes());

    // Items.
    meta[file_params_off as usize..(file_params_off + 4) as usize]
        .copy_from_slice(&BLOCK_SIZE.to_le_bytes());
    meta[(file_params_off + 4) as usize..(file_params_off + 8) as usize]
        .copy_from_slice(&0u32.to_le_bytes()); // flags = 0 (no parent)
    meta[virt_size_off as usize..(virt_size_off + 8) as usize]
        .copy_from_slice(&VIRTUAL_DISK_SIZE.to_le_bytes());
    meta[sector_size_off as usize..(sector_size_off + 4) as usize]
        .copy_from_slice(&SECTOR_SIZE.to_le_bytes());

    f.write_all_at(&meta, METADATA_REGION_OFFSET).unwrap();

    // 5. BAT region — single entry, FullyPresent (state=6), file_offset = 3 MiB.
    let bat_entry: u64 = (3u64 << 20) | 6; // file_offset_mb = 3, state = 6
    f.write_all_at(&bat_entry.to_le_bytes(), BAT_REGION_OFFSET)
        .unwrap();

    // 6. Data block 0.
    f.write_all_at(data, DATA_BLOCK_OFFSET).unwrap();
}

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
