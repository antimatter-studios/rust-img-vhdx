//! VHDX region table (64 KiB at file offsets 192 KiB and 256 KiB).
//!
//! Lookup table for well-known regions: BAT, Metadata. The format
//! allows other (vendor-defined) regions but a v1 reader only needs
//! the two canonical ones.
//!
//! Layout:
//!
//! ```text
//!   0   4   signature "regi"
//!   4   4   checksum (CRC-32C with field zeroed)
//!   8   4   entry_count
//!  12   4   reserved
//!  16  ... entries (32 bytes each, up to entry_count = 2047)
//! ```
//!
//! Entry layout (32 bytes):
//!
//! ```text
//!   0  16  guid
//!  16   8  file_offset
//!  24   4  length
//!  28   4  required (bit 0 = required-by-this-impl)
//! ```

use crate::error::{Error, Result};

/// Largest `entry_count` the region table header may declare.
/// Fixed by the VHDX specification.
const MAX_REGION_ENTRIES: usize = 2047;

pub const REGION_TABLE_SIZE: usize = 64 * 1024;
pub const REGION_TABLE1_OFFSET: u64 = 192 * 1024;
pub const REGION_TABLE2_OFFSET: u64 = 256 * 1024;
pub const REGION_TABLE_SIGNATURE: &[u8; 4] = b"regi";

/// Well-known region GUIDs in their on-disk (mixed-endian) byte form.
pub mod guids {
    /// BAT region:    2DC27766-F623-4200-9D64-115E9BFD4A08
    pub const BAT: [u8; 16] = [
        0x66, 0x77, 0xC2, 0x2D, 0x23, 0xF6, 0x00, 0x42, 0x9D, 0x64, 0x11, 0x5E, 0x9B, 0xFD, 0x4A,
        0x08,
    ];
    /// Metadata region: 8B7CA206-4790-4B9A-B8FE-575F050F886E
    pub const METADATA: [u8; 16] = [
        0x06, 0xA2, 0x7C, 0x8B, 0x90, 0x47, 0x9A, 0x4B, 0xB8, 0xFE, 0x57, 0x5F, 0x05, 0x0F, 0x88,
        0x6E,
    ];
}

#[derive(Debug, Clone, Copy)]
pub struct RegionEntry {
    pub guid: [u8; 16],
    pub file_offset: u64,
    pub length: u32,
    pub required: bool,
}

#[derive(Debug, Clone)]
pub struct RegionTable {
    pub entries: Vec<RegionEntry>,
}

impl RegionTable {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < REGION_TABLE_SIZE {
            return Err(Error::Corrupt("region table shorter than 64 KiB"));
        }
        if &bytes[0..4] != REGION_TABLE_SIGNATURE {
            return Err(Error::Corrupt("region-table signature mismatch"));
        }
        let stored_crc = read_u32_le(bytes, 4);
        let computed = compute_crc(bytes);
        if stored_crc != computed {
            return Err(Error::BadChecksum {
                expected: stored_crc,
                found: computed,
                what: "region-table",
            });
        }
        let entry_count = read_u32_le(bytes, 8) as usize;
        if entry_count > MAX_REGION_ENTRIES {
            return Err(Error::Corrupt("region-table entry_count > 2047"));
        }

        let mut entries = Vec::with_capacity(entry_count);
        for i in 0..entry_count {
            let off = 16 + i * 32;
            let mut guid = [0u8; 16];
            guid.copy_from_slice(&bytes[off..off + 16]);
            let file_offset = read_u64_le(bytes, off + 16);
            let length = read_u32_le(bytes, off + 24);
            let required = read_u32_le(bytes, off + 28) & 1 != 0;
            entries.push(RegionEntry {
                guid,
                file_offset,
                length,
                required,
            });
        }
        Ok(Self { entries })
    }

    pub fn find(&self, target: &[u8; 16]) -> Option<&RegionEntry> {
        self.entries.iter().find(|e| &e.guid == target)
    }
}

/// CRC-32C of the region-table header with the checksum field zeroed.
pub fn compute_crc(bytes: &[u8]) -> u32 {
    let mut buf = vec![0u8; REGION_TABLE_SIZE];
    buf.copy_from_slice(&bytes[..REGION_TABLE_SIZE]);
    buf[4..8].fill(0);
    crc32c::crc32c(&buf)
}

fn read_u32_le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn read_u64_le(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        b[off],
        b[off + 1],
        b[off + 2],
        b[off + 3],
        b[off + 4],
        b[off + 5],
        b[off + 6],
        b[off + 7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a valid region table with a BAT entry (offset 2 MiB) and a
    /// metadata entry (offset 1 MiB) and a correct CRC-32C.
    fn valid_region_table() -> Vec<u8> {
        let mut rt = vec![0u8; REGION_TABLE_SIZE];
        rt[0..4].copy_from_slice(REGION_TABLE_SIGNATURE);
        rt[8..12].copy_from_slice(&2u32.to_le_bytes()); // entry_count = 2

        let off = 16;
        rt[off..off + 16].copy_from_slice(&guids::BAT);
        rt[off + 16..off + 24].copy_from_slice(&(2u64 << 20).to_le_bytes());
        rt[off + 24..off + 28].copy_from_slice(&8u32.to_le_bytes());
        rt[off + 28..off + 32].copy_from_slice(&1u32.to_le_bytes()); // required

        let off = 48;
        rt[off..off + 16].copy_from_slice(&guids::METADATA);
        rt[off + 16..off + 24].copy_from_slice(&(1u64 << 20).to_le_bytes());
        rt[off + 24..off + 28].copy_from_slice(&(64u32 * 1024).to_le_bytes());
        rt[off + 28..off + 32].copy_from_slice(&0u32.to_le_bytes()); // not required

        let crc = compute_crc(&rt);
        rt[4..8].copy_from_slice(&crc.to_le_bytes());
        rt
    }

    #[test]
    fn parses_entries_and_finds_known_guids() {
        let rt = RegionTable::parse(&valid_region_table()).unwrap();
        assert_eq!(rt.entries.len(), 2);

        let bat = rt.find(&guids::BAT).expect("BAT entry present");
        assert_eq!(bat.file_offset, 2 << 20);
        assert_eq!(bat.length, 8);
        assert!(bat.required);

        let meta = rt.find(&guids::METADATA).expect("metadata entry present");
        assert_eq!(meta.file_offset, 1 << 20);
        assert!(!meta.required);
    }

    #[test]
    fn find_returns_none_for_unknown_guid() {
        let rt = RegionTable::parse(&valid_region_table()).unwrap();
        assert!(rt.find(&[0xFF; 16]).is_none());
    }

    #[test]
    fn rejects_buffer_shorter_than_64_kib() {
        let err = RegionTable::parse(&[0u8; 1000]).unwrap_err();
        assert!(matches!(err, Error::Corrupt(_)), "got {err:?}");
    }

    #[test]
    fn rejects_bad_signature() {
        let mut rt = valid_region_table();
        rt[0..4].copy_from_slice(b"junk");
        let crc = compute_crc(&rt);
        rt[4..8].copy_from_slice(&crc.to_le_bytes());
        let err = RegionTable::parse(&rt).unwrap_err();
        assert!(matches!(err, Error::Corrupt(_)), "got {err:?}");
    }

    #[test]
    fn rejects_crc_mismatch() {
        let mut rt = valid_region_table();
        rt[16] ^= 0xFF; // perturb the first entry's GUID
        let err = RegionTable::parse(&rt).unwrap_err();
        match err {
            Error::BadChecksum { what, .. } => assert_eq!(what, "region-table"),
            other => panic!("expected BadChecksum, got {other:?}"),
        }
    }

    #[test]
    fn rejects_entry_count_above_max() {
        let mut rt = valid_region_table();
        rt[8..12].copy_from_slice(&2048u32.to_le_bytes());
        let crc = compute_crc(&rt);
        rt[4..8].copy_from_slice(&crc.to_le_bytes());
        let err = RegionTable::parse(&rt).unwrap_err();
        assert!(matches!(err, Error::Corrupt(_)), "got {err:?}");
    }
}
