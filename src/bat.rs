//! VHDX BAT (Block Allocation Table) decoding.
//!
//! The BAT is an array of u64 entries split between **payload blocks**
//! and **sector bitmap blocks**, interleaved per chunk:
//!
//! ```text
//!   [chunk 0: chunk_ratio data entries, then 1 sector-bitmap entry]
//!   [chunk 1: chunk_ratio data entries, then 1 sector-bitmap entry]
//!   ...
//! ```
//!
//! `chunk_ratio = (2^23 * logical_sector_size) / block_size`. Each
//! entry layout:
//!
//! ```text
//!   bits  0..3  state
//!   bits  3..20 reserved (zero)
//!   bits 20..64 file_offset_in_mb
//! ```
//!
//! State values for *payload* entries (data blocks):
//!
//! - 0 PAYLOAD_BLOCK_NOT_PRESENT — no block on disk; reads as zero
//!   for non-differencing, defer to parent for differencing.
//! - 1 PAYLOAD_BLOCK_UNDEFINED — same as NOT_PRESENT in practice.
//! - 2 PAYLOAD_BLOCK_ZERO — explicitly zero.
//! - 3 PAYLOAD_BLOCK_UNMAPPED — TRIM-ed; zero.
//! - 6 PAYLOAD_BLOCK_FULLY_PRESENT — block on disk at file_offset.
//! - 7 PAYLOAD_BLOCK_PARTIALLY_PRESENT — block on disk, but only some
//!   sectors valid (consult sector bitmap entry).
//!
//! State values for *sector-bitmap* entries:
//!
//! - 0 SB_BLOCK_NOT_PRESENT
//! - 6 SB_BLOCK_PRESENT — bitmap on disk at file_offset.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadState {
    /// state = 0 — no block on disk.
    NotPresent,
    /// state = 1 — same effective behaviour as NotPresent.
    Undefined,
    /// state = 2 — block reads as zero without disk access.
    Zero,
    /// state = 3 — TRIM-ed; reads as zero.
    Unmapped,
    /// state = 6 — block on disk at file_offset; full payload valid.
    FullyPresent,
    /// state = 7 — block on disk; consult sector bitmap entry.
    PartiallyPresent,
    /// Reserved / unknown state. Treat as NotPresent for safety.
    Reserved(u8),
}

impl PayloadState {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => PayloadState::NotPresent,
            1 => PayloadState::Undefined,
            2 => PayloadState::Zero,
            3 => PayloadState::Unmapped,
            6 => PayloadState::FullyPresent,
            7 => PayloadState::PartiallyPresent,
            other => PayloadState::Reserved(other),
        }
    }

    /// True when the entry's `file_offset` points at a block that
    /// readers should consult.
    pub fn block_on_disk(&self) -> bool {
        matches!(
            self,
            PayloadState::FullyPresent | PayloadState::PartiallyPresent
        )
    }

    /// True when the read should produce zeros without touching disk.
    pub fn zero_fill(&self) -> bool {
        matches!(
            self,
            PayloadState::NotPresent
                | PayloadState::Undefined
                | PayloadState::Zero
                | PayloadState::Unmapped
        )
    }
}

/// Decoded BAT entry. `file_offset` is byte-granular but in practice
/// always 1 MiB-aligned (the encoding only stores the MB index).
#[derive(Debug, Clone, Copy)]
pub struct BatEntry {
    pub state: PayloadState,
    pub file_offset: u64,
}

impl BatEntry {
    pub fn from_u64(raw: u64) -> Self {
        let state = PayloadState::from_u8((raw & 0x7) as u8);
        let file_offset_mb = raw >> 20;
        Self {
            state,
            file_offset: file_offset_mb << 20,
        }
    }
}

/// Compute the chunk_ratio for a given block size and sector size.
/// `(2^23 * sector_size) / block_size`.
pub fn chunk_ratio(block_size: u32, sector_size: u32) -> u64 {
    ((1u64 << 23) * sector_size as u64) / block_size as u64
}

/// Index of the BAT entry covering virtual block `virt_block_idx`,
/// accounting for the interleaved sector-bitmap entries.
///
/// Each chunk holds `chunk_ratio` data entries followed by a single
/// sector-bitmap entry, so a virtual block at offset `block` within
/// the disk maps to BAT index:
///
/// ```text
///   chunk_idx     = block / chunk_ratio
///   block_in_chunk = block % chunk_ratio
///   bat_idx        = chunk_idx * (chunk_ratio + 1) + block_in_chunk
/// ```
pub fn data_bat_index(virt_block_idx: u64, chunk_ratio: u64) -> u64 {
    let chunk_idx = virt_block_idx / chunk_ratio;
    let block_in_chunk = virt_block_idx % chunk_ratio;
    chunk_idx * (chunk_ratio + 1) + block_in_chunk
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_state_decodes_every_documented_value() {
        assert_eq!(PayloadState::from_u8(0), PayloadState::NotPresent);
        assert_eq!(PayloadState::from_u8(1), PayloadState::Undefined);
        assert_eq!(PayloadState::from_u8(2), PayloadState::Zero);
        assert_eq!(PayloadState::from_u8(3), PayloadState::Unmapped);
        assert_eq!(PayloadState::from_u8(6), PayloadState::FullyPresent);
        assert_eq!(PayloadState::from_u8(7), PayloadState::PartiallyPresent);
    }

    #[test]
    fn payload_state_maps_unknown_values_to_reserved() {
        // 4, 5, and anything >7 are not assigned by the spec; the reader
        // must funnel them to Reserved (treated as NotPresent for safety)
        // rather than silently aliasing a real state.
        assert_eq!(PayloadState::from_u8(4), PayloadState::Reserved(4));
        assert_eq!(PayloadState::from_u8(5), PayloadState::Reserved(5));
        assert_eq!(PayloadState::from_u8(255), PayloadState::Reserved(255));
    }

    #[test]
    fn block_on_disk_only_for_present_states() {
        assert!(PayloadState::FullyPresent.block_on_disk());
        assert!(PayloadState::PartiallyPresent.block_on_disk());
        for s in [
            PayloadState::NotPresent,
            PayloadState::Undefined,
            PayloadState::Zero,
            PayloadState::Unmapped,
            PayloadState::Reserved(4),
        ] {
            assert!(!s.block_on_disk(), "{s:?} must not claim a disk block");
        }
    }

    #[test]
    fn zero_fill_for_the_four_absent_states_only() {
        for s in [
            PayloadState::NotPresent,
            PayloadState::Undefined,
            PayloadState::Zero,
            PayloadState::Unmapped,
        ] {
            assert!(s.zero_fill(), "{s:?} should zero-fill");
        }
        assert!(!PayloadState::FullyPresent.zero_fill());
        assert!(!PayloadState::PartiallyPresent.zero_fill());
        // Reserved is handled conservatively by the reader elsewhere; it
        // is not itself a zero_fill state. Pin the current contract.
        assert!(!PayloadState::Reserved(4).zero_fill());
    }

    #[test]
    fn bat_entry_splits_state_and_megabyte_offset() {
        // file_offset_mb = 3, state = 6 (FullyPresent).
        let e = BatEntry::from_u64((3u64 << 20) | 6);
        assert_eq!(e.state, PayloadState::FullyPresent);
        assert_eq!(e.file_offset, 3 << 20);
    }

    #[test]
    fn bat_entry_ignores_reserved_bits_between_state_and_offset() {
        // Set every reserved bit (3..20) plus state=2 and offset_mb=10.
        let reserved = 0xF_FFF8u64; // bits 3..20 all set
        let raw = (10u64 << 20) | reserved | 2;
        let e = BatEntry::from_u64(raw);
        assert_eq!(e.state, PayloadState::Zero);
        assert_eq!(e.file_offset, 10 << 20);
    }

    #[test]
    fn bat_entry_unallocated_is_zero_raw() {
        let e = BatEntry::from_u64(0);
        assert_eq!(e.state, PayloadState::NotPresent);
        assert_eq!(e.file_offset, 0);
    }

    #[test]
    fn chunk_ratio_matches_spec_formula() {
        // (2^23 * sector_size) / block_size.
        // 1 MiB block, 512 sector -> 2^32 / 2^20 = 4096.
        assert_eq!(chunk_ratio(1 << 20, 512), 4096);
        // 2 MiB block, 512 sector -> 2^32 / 2^21 = 2048.
        assert_eq!(chunk_ratio(2 << 20, 512), 2048);
        // 1 MiB block, 4096 sector -> 2^23 * 2^12 / 2^20 = 2^15 = 32768.
        assert_eq!(chunk_ratio(1 << 20, 4096), 32768);
    }

    #[test]
    fn data_bat_index_accounts_for_interleaved_sector_bitmap_entries() {
        let cr = 4096;
        // First chunk maps 1:1.
        assert_eq!(data_bat_index(0, cr), 0);
        assert_eq!(data_bat_index(cr - 1, cr), cr - 1);
        // Crossing into chunk 1 skips the chunk-0 sector-bitmap entry.
        assert_eq!(data_bat_index(cr, cr), cr + 1);
        assert_eq!(data_bat_index(cr + 1, cr), cr + 2);
        // Chunk 2 skips two sector-bitmap entries.
        assert_eq!(data_bat_index(2 * cr, cr), 2 * cr + 2);
    }
}
