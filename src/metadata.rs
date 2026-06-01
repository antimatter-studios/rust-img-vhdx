//! VHDX metadata region: a header (32 bytes) followed by entries
//! (32 bytes each), each pointing at an item's data inside the same
//! region.
//!
//! Layout (offsets within the metadata region):
//!
//! ```text
//!   0   8  signature "metadata"
//!   8   2  reserved
//!  10   2  entry_count
//!  12  20  reserved
//!  32 ... entries (32 bytes each)
//! ```
//!
//! Entry layout (32 bytes):
//!
//! ```text
//!   0  16  item_id (GUID)
//!  16   4  offset  (relative to metadata region)
//!  20   4  length
//!  24   4  flags   (bit 0 = user, bit 1 = virtual_disk, bit 2 = required)
//!  28   4  reserved
//! ```

use crate::error::{Error, Result};

pub const METADATA_HEADER_SIZE: usize = 32;
pub const METADATA_ENTRY_SIZE: usize = 32;
pub const METADATA_SIGNATURE: &[u8; 8] = b"metadata";

/// Well-known metadata item GUIDs in on-disk (mixed-endian) byte form.
pub mod item_ids {
    /// File Parameters: CAA16737-FA36-4D43-B3B6-33F0AA44E76B
    pub const FILE_PARAMETERS: [u8; 16] = [
        0x37, 0x67, 0xA1, 0xCA, 0x36, 0xFA, 0x43, 0x4D, 0xB3, 0xB6, 0x33, 0xF0, 0xAA, 0x44, 0xE7,
        0x6B,
    ];
    /// Virtual Disk Size: 2FA54224-CD1B-4876-B211-5DBED83BF4B8
    pub const VIRTUAL_DISK_SIZE: [u8; 16] = [
        0x24, 0x42, 0xA5, 0x2F, 0x1B, 0xCD, 0x76, 0x48, 0xB2, 0x11, 0x5D, 0xBE, 0xD8, 0x3B, 0xF4,
        0xB8,
    ];
    /// Logical Sector Size: 8141BF1D-A96F-4709-BA47-F233A8FAAB5F
    pub const LOGICAL_SECTOR_SIZE: [u8; 16] = [
        0x1D, 0xBF, 0x41, 0x81, 0x6F, 0xA9, 0x09, 0x47, 0xBA, 0x47, 0xF2, 0x33, 0xA8, 0xFA, 0xAB,
        0x5F,
    ];
    /// Physical Sector Size: CDA348C7-445D-4471-9CC9-E9885251C556
    pub const PHYSICAL_SECTOR_SIZE: [u8; 16] = [
        0xC7, 0x48, 0xA3, 0xCD, 0x5D, 0x44, 0x71, 0x44, 0x9C, 0xC9, 0xE9, 0x88, 0x52, 0x51, 0xC5,
        0x56,
    ];
}

#[derive(Debug, Clone, Copy)]
pub struct MetadataEntry {
    pub item_id: [u8; 16],
    pub offset: u32,
    pub length: u32,
    pub flags: u32,
}

#[derive(Debug, Clone)]
pub struct MetadataTable {
    pub entries: Vec<MetadataEntry>,
    /// Raw bytes of the entire metadata region — entry data is read
    /// out of this slice using `entry.offset` / `entry.length`.
    pub region_bytes: Vec<u8>,
}

impl MetadataTable {
    pub fn parse(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() < METADATA_HEADER_SIZE {
            return Err(Error::BadMetadata("region shorter than 32 bytes"));
        }
        if &bytes[0..8] != METADATA_SIGNATURE {
            return Err(Error::BadMetadata("signature mismatch"));
        }
        let entry_count = read_u16_le(&bytes, 10) as usize;
        if entry_count > 2047 {
            return Err(Error::BadMetadata("entry_count > 2047"));
        }
        let total_entries_bytes = METADATA_HEADER_SIZE + entry_count * METADATA_ENTRY_SIZE;
        if bytes.len() < total_entries_bytes {
            return Err(Error::BadMetadata("region truncated mid-entry"));
        }

        let mut entries = Vec::with_capacity(entry_count);
        for i in 0..entry_count {
            let off = METADATA_HEADER_SIZE + i * METADATA_ENTRY_SIZE;
            let mut item_id = [0u8; 16];
            item_id.copy_from_slice(&bytes[off..off + 16]);
            let item_offset = read_u32_le(&bytes, off + 16);
            let length = read_u32_le(&bytes, off + 20);
            let flags = read_u32_le(&bytes, off + 24);
            entries.push(MetadataEntry {
                item_id,
                offset: item_offset,
                length,
                flags,
            });
        }
        Ok(Self {
            entries,
            region_bytes: bytes,
        })
    }

    pub fn item_data(&self, item_id: &[u8; 16]) -> Option<&[u8]> {
        let entry = self.entries.iter().find(|e| &e.item_id == item_id)?;
        let start = entry.offset as usize;
        let end = start + entry.length as usize;
        if end > self.region_bytes.len() {
            return None;
        }
        Some(&self.region_bytes[start..end])
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FileParameters {
    pub block_size: u32,
    /// bit 0 = leave_blocks_allocated, bit 1 = has_parent
    pub flags: u32,
}

impl FileParameters {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 8 {
            return Err(Error::BadMetadata("FileParameters shorter than 8 bytes"));
        }
        Ok(Self {
            block_size: read_u32_le(bytes, 0),
            flags: read_u32_le(bytes, 4),
        })
    }

    pub fn has_parent(&self) -> bool {
        self.flags & 0x2 != 0
    }
}

fn read_u16_le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn read_u32_le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

pub fn read_virtual_disk_size(bytes: &[u8]) -> Result<u64> {
    if bytes.len() < 8 {
        return Err(Error::BadMetadata("VirtualDiskSize shorter than 8 bytes"));
    }
    Ok(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

pub fn read_sector_size(bytes: &[u8]) -> Result<u32> {
    if bytes.len() < 4 {
        return Err(Error::BadMetadata("sector size shorter than 4 bytes"));
    }
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a metadata region with `entry_count` declared in the header
    /// and one File Parameters entry whose item data sits right after the
    /// entry table.
    fn region_with_one_file_params(entry_count: u16, block_size: u32, flags: u32) -> Vec<u8> {
        let mut m = vec![0u8; 64 * 1024];
        m[0..8].copy_from_slice(METADATA_SIGNATURE);
        m[10..12].copy_from_slice(&entry_count.to_le_bytes());

        let item_off = (METADATA_HEADER_SIZE + METADATA_ENTRY_SIZE) as u32;
        let entry = METADATA_HEADER_SIZE;
        m[entry..entry + 16].copy_from_slice(&item_ids::FILE_PARAMETERS);
        m[entry + 16..entry + 20].copy_from_slice(&item_off.to_le_bytes());
        m[entry + 20..entry + 24].copy_from_slice(&8u32.to_le_bytes());
        m[entry + 24..entry + 28].copy_from_slice(&0x6u32.to_le_bytes());

        let i = item_off as usize;
        m[i..i + 4].copy_from_slice(&block_size.to_le_bytes());
        m[i + 4..i + 8].copy_from_slice(&flags.to_le_bytes());
        m
    }

    #[test]
    fn parses_entries_and_reads_item_data() {
        let m = region_with_one_file_params(1, 1 << 20, 0);
        let table = MetadataTable::parse(m).unwrap();
        assert_eq!(table.entries.len(), 1);
        let data = table
            .item_data(&item_ids::FILE_PARAMETERS)
            .expect("file params item present");
        let fp = FileParameters::parse(data).unwrap();
        assert_eq!(fp.block_size, 1 << 20);
        assert!(!fp.has_parent());
    }

    #[test]
    fn item_data_returns_none_for_absent_item() {
        let m = region_with_one_file_params(1, 1 << 20, 0);
        let table = MetadataTable::parse(m).unwrap();
        assert!(table.item_data(&item_ids::VIRTUAL_DISK_SIZE).is_none());
    }

    #[test]
    fn item_data_returns_none_when_entry_points_out_of_bounds() {
        let mut m = region_with_one_file_params(1, 1 << 20, 0);
        // Make the entry's length run past the end of the region.
        let entry = METADATA_HEADER_SIZE;
        m[entry + 20..entry + 24].copy_from_slice(&u32::MAX.to_le_bytes());
        let table = MetadataTable::parse(m).unwrap();
        assert!(table.item_data(&item_ids::FILE_PARAMETERS).is_none());
    }

    #[test]
    fn rejects_region_shorter_than_header() {
        let err = MetadataTable::parse(vec![0u8; 8]).unwrap_err();
        assert!(matches!(err, Error::BadMetadata(_)), "got {err:?}");
    }

    #[test]
    fn rejects_bad_signature() {
        let mut m = region_with_one_file_params(1, 1 << 20, 0);
        m[0..8].copy_from_slice(b"NOTmeta!");
        let err = MetadataTable::parse(m).unwrap_err();
        assert!(matches!(err, Error::BadMetadata(_)), "got {err:?}");
    }

    #[test]
    fn rejects_entry_count_above_max() {
        let mut m = region_with_one_file_params(1, 1 << 20, 0);
        m[10..12].copy_from_slice(&2048u16.to_le_bytes());
        let err = MetadataTable::parse(m).unwrap_err();
        assert!(matches!(err, Error::BadMetadata(_)), "got {err:?}");
    }

    #[test]
    fn rejects_region_truncated_mid_entry() {
        // Declare 3 entries but provide a buffer that only spans the
        // header + first entry.
        let mut m = vec![0u8; METADATA_HEADER_SIZE + METADATA_ENTRY_SIZE];
        m[0..8].copy_from_slice(METADATA_SIGNATURE);
        m[10..12].copy_from_slice(&3u16.to_le_bytes());
        let err = MetadataTable::parse(m).unwrap_err();
        assert!(matches!(err, Error::BadMetadata(_)), "got {err:?}");
    }

    #[test]
    fn file_parameters_has_parent_reads_flag_bit_1() {
        let with_parent = FileParameters::parse(&[0, 0, 16, 0, 0x02, 0, 0, 0]).unwrap();
        assert!(with_parent.has_parent());
        let leave_allocated = FileParameters::parse(&[0, 0, 16, 0, 0x01, 0, 0, 0]).unwrap();
        assert!(!leave_allocated.has_parent());
    }

    #[test]
    fn file_parameters_rejects_short_buffer() {
        let err = FileParameters::parse(&[0u8; 4]).unwrap_err();
        assert!(matches!(err, Error::BadMetadata(_)), "got {err:?}");
    }

    #[test]
    fn reads_virtual_disk_size_little_endian() {
        let v = read_virtual_disk_size(&(0x1234_5678_9ABCu64).to_le_bytes()).unwrap();
        assert_eq!(v, 0x1234_5678_9ABC);
        assert!(read_virtual_disk_size(&[0u8; 4]).is_err());
    }

    #[test]
    fn reads_sector_size_little_endian() {
        let s = read_sector_size(&512u32.to_le_bytes()).unwrap();
        assert_eq!(s, 512);
        assert!(read_sector_size(&[0u8; 2]).is_err());
    }
}
