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
