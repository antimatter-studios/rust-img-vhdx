//! VHDX read path. Walks the file identifier → header → region table →
//! metadata → BAT pipeline once at `open()` time, then resolves
//! `read_at` by indexing the cached BAT.
//!
//! Limitations (logged in README):
//!
//! - No log replay. The reader assumes a clean shutdown; a dirty image
//!   may decode to inconsistent state. Run `vhdxtool repair` (a future
//!   tool) or convert via `qemu-img` first if needed.
//! - No sector-bitmap (PARTIALLY_PRESENT) support. Such blocks are
//!   surfaced as `Error::Unsupported`. Most dynamic VHDX images
//!   without snapshots never hit this state.
//! - No differencing-chain resolution.

use crate::bat::{chunk_ratio as compute_chunk_ratio, data_bat_index, BatEntry, PayloadState};
use crate::error::{Error, Result};
use crate::header::{Header, HEADER1_OFFSET, HEADER2_OFFSET, HEADER_SIZE};
use crate::metadata::{
    item_ids, read_sector_size, read_virtual_disk_size, FileParameters, MetadataTable,
};
use crate::region_table::{
    guids, RegionTable, REGION_TABLE1_OFFSET, REGION_TABLE2_OFFSET, REGION_TABLE_SIZE,
};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

const FILE_IDENTIFIER_OFFSET: u64 = 0;
const FILE_IDENTIFIER_SIGNATURE: &[u8; 8] = b"vhdxfile";

pub struct VhdxReader {
    file: Mutex<File>,
    virtual_size: u64,
    block_size: u32,
    sector_size: u32,
    chunk_ratio: u64,
    /// In-memory BAT — cached at open. Always small enough for practical
    /// images (e.g. a 64 GiB virtual disk with 32 MiB blocks needs
    /// 2049 BAT entries × 8 bytes = 16 KiB).
    bat: Vec<BatEntry>,
    has_parent: bool,
}

impl VhdxReader {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let mut file = File::open(path.as_ref())?;
        let file_len = file.metadata()?.len();

        // 1. File identifier.
        let mut sig = [0u8; 8];
        file.seek(SeekFrom::Start(FILE_IDENTIFIER_OFFSET))?;
        file.read_exact(&mut sig)?;
        if &sig != FILE_IDENTIFIER_SIGNATURE {
            return Err(Error::NotVhdx);
        }

        // 2. Header — try both slots, pick the one with the higher
        //    sequence_number that passes CRC.
        let header = pick_header(&mut file, file_len)?;
        let _ = header; // log_offset / log_length parsed but unused (no replay).

        // 3. Region table — try both, pick a valid one.
        let region_table = pick_region_table(&mut file, file_len)?;

        // 4. Metadata.
        let metadata_entry = region_table
            .find(&guids::METADATA)
            .ok_or(Error::Corrupt("metadata region missing"))?;
        let mut metadata_bytes = vec![0u8; metadata_entry.length as usize];
        file.seek(SeekFrom::Start(metadata_entry.file_offset))?;
        file.read_exact(&mut metadata_bytes)?;
        let metadata = MetadataTable::parse(metadata_bytes)?;

        let file_params_bytes = metadata
            .item_data(&item_ids::FILE_PARAMETERS)
            .ok_or(Error::BadMetadata("FileParameters item missing"))?;
        let file_params = FileParameters::parse(file_params_bytes)?;

        let virtual_size_bytes = metadata
            .item_data(&item_ids::VIRTUAL_DISK_SIZE)
            .ok_or(Error::BadMetadata("VirtualDiskSize item missing"))?;
        let virtual_size = read_virtual_disk_size(virtual_size_bytes)?;

        let sector_size_bytes = metadata
            .item_data(&item_ids::LOGICAL_SECTOR_SIZE)
            .ok_or(Error::BadMetadata("LogicalSectorSize item missing"))?;
        let sector_size = read_sector_size(sector_size_bytes)?;

        if !file_params.block_size.is_power_of_two() {
            return Err(Error::Corrupt("block_size not a power of two"));
        }
        if file_params.block_size < 1024 * 1024 || file_params.block_size > 256 * 1024 * 1024 {
            return Err(Error::Corrupt("block_size outside [1 MiB, 256 MiB]"));
        }
        if !sector_size.is_power_of_two() {
            return Err(Error::Corrupt("sector_size not a power of two"));
        }

        let chunk_ratio = compute_chunk_ratio(file_params.block_size, sector_size);
        if chunk_ratio == 0 {
            return Err(Error::Corrupt("chunk_ratio = 0"));
        }

        // 5. BAT.
        let bat_region = region_table
            .find(&guids::BAT)
            .ok_or(Error::Corrupt("BAT region missing"))?;
        let bat_entries_total = bat_region.length as u64 / 8;
        let mut bat_bytes = vec![0u8; bat_region.length as usize];
        file.seek(SeekFrom::Start(bat_region.file_offset))?;
        file.read_exact(&mut bat_bytes)?;
        let mut bat = Vec::with_capacity(bat_entries_total as usize);
        for chunk in bat_bytes.chunks_exact(8) {
            let raw = u64::from_le_bytes(chunk.try_into().unwrap());
            bat.push(BatEntry::from_u64(raw));
        }

        Ok(Self {
            file: Mutex::new(file),
            virtual_size,
            block_size: file_params.block_size,
            sector_size,
            chunk_ratio,
            bat,
            has_parent: file_params.has_parent(),
        })
    }

    pub fn virtual_size(&self) -> u64 {
        self.virtual_size
    }

    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    pub fn sector_size(&self) -> u32 {
        self.sector_size
    }

    pub fn has_parent(&self) -> bool {
        self.has_parent
    }

    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let len = buf.len() as u64;
        if len == 0 {
            return Ok(());
        }
        let end = offset
            .checked_add(len)
            .ok_or(Error::Corrupt("offset+len overflow"))?;
        if end > self.virtual_size {
            return Err(Error::OutOfBounds {
                offset,
                len,
                size: self.virtual_size,
            });
        }
        if self.has_parent {
            return Err(Error::Unsupported(
                "VHDX with parent (differencing) — chain walking not implemented",
            ));
        }

        let block_size = self.block_size as u64;
        let block_mask = block_size - 1;
        let mut cursor = offset;
        let mut written = 0usize;

        while cursor < end {
            let in_block = cursor & block_mask;
            let virt_block_idx = cursor / block_size;
            let chunk_len = std::cmp::min(block_size - in_block, end - cursor) as usize;
            let dst = &mut buf[written..written + chunk_len];

            let bat_idx = data_bat_index(virt_block_idx, self.chunk_ratio) as usize;
            let entry = self
                .bat
                .get(bat_idx)
                .copied()
                .ok_or(Error::Corrupt("BAT index out of range"))?;

            match entry.state {
                PayloadState::FullyPresent => {
                    let host_off = entry.file_offset + in_block;
                    let mut f = self.file.lock().unwrap();
                    f.seek(SeekFrom::Start(host_off))?;
                    f.read_exact(dst)?;
                }
                s if s.zero_fill() => {
                    dst.fill(0);
                }
                PayloadState::PartiallyPresent => {
                    return Err(Error::Unsupported(
                        "PartiallyPresent block (sector-bitmap walking not implemented)",
                    ));
                }
                PayloadState::Reserved(v) => {
                    return Err(Error::Unsupported(Box::leak(
                        format!("BAT entry reserved state {v}").into_boxed_str(),
                    )));
                }
                _ => unreachable!(),
            }

            cursor += chunk_len as u64;
            written += chunk_len;
        }
        Ok(())
    }
}

fn pick_header(file: &mut File, file_len: u64) -> Result<Header> {
    let mut h1 = None;
    let mut h2 = None;
    if file_len >= HEADER1_OFFSET + HEADER_SIZE as u64 {
        let mut buf = vec![0u8; HEADER_SIZE];
        file.seek(SeekFrom::Start(HEADER1_OFFSET))?;
        file.read_exact(&mut buf)?;
        if let Ok(h) = Header::parse(&buf) {
            h1 = Some(h);
        }
    }
    if file_len >= HEADER2_OFFSET + HEADER_SIZE as u64 {
        let mut buf = vec![0u8; HEADER_SIZE];
        file.seek(SeekFrom::Start(HEADER2_OFFSET))?;
        file.read_exact(&mut buf)?;
        if let Ok(h) = Header::parse(&buf) {
            h2 = Some(h);
        }
    }
    match (h1, h2) {
        (Some(a), Some(b)) => {
            if a.sequence_number >= b.sequence_number {
                Ok(a)
            } else {
                Ok(b)
            }
        }
        (Some(a), None) | (None, Some(a)) => Ok(a),
        (None, None) => Err(Error::NoValidHeader),
    }
}

fn pick_region_table(file: &mut File, file_len: u64) -> Result<RegionTable> {
    let mut t1 = None;
    if file_len >= REGION_TABLE1_OFFSET + REGION_TABLE_SIZE as u64 {
        let mut buf = vec![0u8; REGION_TABLE_SIZE];
        file.seek(SeekFrom::Start(REGION_TABLE1_OFFSET))?;
        file.read_exact(&mut buf)?;
        if let Ok(t) = RegionTable::parse(&buf) {
            t1 = Some(t);
        }
    }
    if let Some(t) = t1 {
        return Ok(t);
    }
    if file_len >= REGION_TABLE2_OFFSET + REGION_TABLE_SIZE as u64 {
        let mut buf = vec![0u8; REGION_TABLE_SIZE];
        file.seek(SeekFrom::Start(REGION_TABLE2_OFFSET))?;
        file.read_exact(&mut buf)?;
        if let Ok(t) = RegionTable::parse(&buf) {
            return Ok(t);
        }
    }
    Err(Error::NoValidRegionTable)
}

// ---------------------------------------------------------------------------
// fs_core::BlockRead / BlockDevice bridge
// ---------------------------------------------------------------------------

impl fs_core::BlockRead for VhdxReader {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> fs_core::Result<()> {
        VhdxReader::read_at(self, offset, buf).map_err(vhdx_to_fs_core_error)
    }
    fn size_bytes(&self) -> u64 {
        self.virtual_size()
    }
}

impl fs_core::BlockDevice for VhdxReader {}

fn vhdx_to_fs_core_error(e: Error) -> fs_core::Error {
    match e {
        Error::Io(io) => fs_core::Error::Io(io),
        Error::OutOfBounds { offset, len, size } => {
            fs_core::Error::OutOfBounds { offset, len, size }
        }
        other => fs_core::Error::Custom(other.to_string()),
    }
}
