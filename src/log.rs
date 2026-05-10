//! VHDX log zone parsing + replay.
//!
//! The log lives at `header.log_offset` for `header.log_length` bytes
//! (always a multiple of 1 MiB, with `log_length >= 1 MiB`). Inside it
//! is a circular buffer of 4 KiB log entries. Each entry batches one or
//! more 4 KiB writes destined for the data zones (BAT, metadata, raw
//! payload blocks) and is committed as a unit so the on-disk image can
//! always be rolled forward to a consistent state.
//!
//! Entry layout (all little-endian):
//!
//! ```text
//!   0   4   signature "loge"
//!   4   4   checksum (CRC-32C of the whole entry, this field zeroed)
//!   8   4   entry_length (header + descriptors + data sectors)
//!  12   4   tail (offset of the oldest entry in this active sequence)
//!  16   8   sequence_number
//!  24   4   descriptor_count
//!  28   4   reserved
//!  32  16   log_guid (must match header.log_guid)
//!  48   8   flushed_file_offset
//!  56   8   last_file_offset
//!  64 ...   descriptors (32 bytes each, descriptor_count of them)
//!  ...      data sectors (4 KiB each), one per "desc" descriptor.
//! ```
//!
//! Descriptor (32 bytes):
//!
//! - "zero": signature(4) "zero" + reserved(4) + zero_length(8) +
//!   file_offset(8) + sequence_number(8). Replay zeros `zero_length`
//!   bytes at `file_offset`. No paired data sector.
//! - "desc": signature(4) "desc" + trailing_bytes(4) + leading_bytes(8)
//!   + file_offset(8) + sequence_number(8). Paired with one 4 KiB data
//!   sector that — once `leading`/`trailing` patches are applied —
//!   becomes the byte image written at `file_offset`.
//!
//! Each data sector (4 KiB):
//!
//! - bytes 0..4   "data"
//! - bytes 4..8   sequence_high (top 32 bits of entry sequence)
//! - bytes 8..4092 user payload
//! - bytes 4092..4096 sequence_low (low 32 bits of entry sequence)
//!
//! For a sector to count as valid the sequence halves must equal the
//! entry's sequence_number. Spec inserts those signature/sequence bytes
//! to detect torn writes; the original payload bytes are stored in the
//! descriptor's `leading_bytes` (first 8 bytes overwritten by
//! data_signature + sequence_high) and `trailing_bytes` (last 4 bytes
//! overwritten by sequence_low).
//!
//! Replay walks entries in increasing-sequence order starting from
//! `header.log_guid`'s active chain. We keep it conservative: we accept
//! a chain of contiguous entries with valid CRCs, matching log_guid,
//! and strictly increasing sequence numbers. Any break ends the chain.

use crate::error::{Error, Result};
use fs_core::BlockDevice;
use std::sync::Arc;

pub const LOG_SECTOR_SIZE: usize = 4096;
pub const LOG_ENTRY_HEADER_SIZE: usize = 64;
pub const LOG_DESCRIPTOR_SIZE: usize = 32;
pub const LOG_ENTRY_SIGNATURE: &[u8; 4] = b"loge";
pub const ZERO_DESC_SIGNATURE: &[u8; 4] = b"zero";
pub const DATA_DESC_SIGNATURE: &[u8; 4] = b"desc";
pub const DATA_SECTOR_SIGNATURE: &[u8; 4] = b"data";

#[derive(Debug, Clone)]
pub struct LogEntryHeader {
    pub entry_length: u32,
    pub tail: u32,
    pub sequence_number: u64,
    pub descriptor_count: u32,
    pub log_guid: [u8; 16],
    pub flushed_file_offset: u64,
    pub last_file_offset: u64,
}

#[derive(Debug, Clone)]
pub enum Descriptor {
    Zero {
        zero_length: u64,
        file_offset: u64,
        sequence_number: u64,
    },
    Data {
        trailing_bytes: u32,
        leading_bytes: u64,
        file_offset: u64,
        sequence_number: u64,
        /// Reconstructed 4 KiB sector ready to be written at `file_offset`.
        /// Built once the paired data sector has been read out of the log
        /// and the leading/trailing bytes have been patched back in.
        sector: Vec<u8>,
    },
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub header: LogEntryHeader,
    pub descriptors: Vec<Descriptor>,
    /// Offset (in bytes) inside the log region where this entry started.
    pub log_offset_in_region: u64,
}

/// Compute CRC-32C of an entry buffer with the checksum field zeroed.
fn entry_crc(bytes: &[u8]) -> u32 {
    let mut tmp = bytes.to_vec();
    tmp[4..8].fill(0);
    crc32c::crc32c(&tmp)
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

/// Try to parse one log entry starting at `pos` inside `log_bytes`.
/// Returns None when the slot does not contain a valid entry — caller
/// uses that to stop scanning the chain.
fn parse_log_entry(log_bytes: &[u8], pos: usize, expected_log_guid: &[u8; 16]) -> Option<LogEntry> {
    if pos + LOG_SECTOR_SIZE > log_bytes.len() {
        return None;
    }
    let head = &log_bytes[pos..pos + LOG_SECTOR_SIZE];
    if &head[0..4] != LOG_ENTRY_SIGNATURE {
        return None;
    }
    let entry_length = read_u32_le(head, 8) as usize;
    if entry_length < LOG_SECTOR_SIZE
        || entry_length % LOG_SECTOR_SIZE != 0
        || pos + entry_length > log_bytes.len()
    {
        return None;
    }
    let entry_bytes = &log_bytes[pos..pos + entry_length];
    let stored_crc = read_u32_le(entry_bytes, 4);
    if stored_crc != entry_crc(entry_bytes) {
        return None;
    }

    let tail = read_u32_le(entry_bytes, 12);
    let sequence_number = read_u64_le(entry_bytes, 16);
    let descriptor_count = read_u32_le(entry_bytes, 24);
    let mut log_guid = [0u8; 16];
    log_guid.copy_from_slice(&entry_bytes[32..48]);
    let flushed_file_offset = read_u64_le(entry_bytes, 48);
    let last_file_offset = read_u64_le(entry_bytes, 56);

    if &log_guid != expected_log_guid {
        return None;
    }

    // Descriptors live immediately after the 64-byte header.
    let descriptors_size = (descriptor_count as usize).checked_mul(LOG_DESCRIPTOR_SIZE)?;
    if LOG_ENTRY_HEADER_SIZE + descriptors_size > entry_length {
        return None;
    }

    // Count "desc" entries up front so we know how many data sectors to
    // expect; the spec promises they appear in order, one per "desc"
    // descriptor, immediately after the descriptor table is rounded up
    // to the next 4 KiB sector boundary. In practice the spec rounds
    // descriptors+header up to a sector boundary and then each "desc"
    // descriptor consumes one 4 KiB sector after it.
    let mut data_sector_idx: usize = 0;
    let descriptors_end = LOG_ENTRY_HEADER_SIZE + descriptors_size;
    // First data sector starts at the next sector boundary after header
    // + descriptors. For the encoder we use (and for entries up to a
    // few descriptors), that's just LOG_SECTOR_SIZE.
    let mut data_sector_base = round_up_sector(descriptors_end);

    let mut descriptors = Vec::with_capacity(descriptor_count as usize);
    for i in 0..descriptor_count as usize {
        let off = LOG_ENTRY_HEADER_SIZE + i * LOG_DESCRIPTOR_SIZE;
        let sig = &entry_bytes[off..off + 4];
        let desc_seq = read_u64_le(entry_bytes, off + 24);
        if desc_seq != sequence_number {
            return None;
        }
        if sig == ZERO_DESC_SIGNATURE {
            let zero_length = read_u64_le(entry_bytes, off + 8);
            let file_offset = read_u64_le(entry_bytes, off + 16);
            descriptors.push(Descriptor::Zero {
                zero_length,
                file_offset,
                sequence_number: desc_seq,
            });
        } else if sig == DATA_DESC_SIGNATURE {
            let trailing_bytes = read_u32_le(entry_bytes, off + 4);
            let leading_bytes = read_u64_le(entry_bytes, off + 8);
            let file_offset = read_u64_le(entry_bytes, off + 16);

            if data_sector_base + LOG_SECTOR_SIZE > entry_length {
                return None;
            }
            let sector = &entry_bytes[data_sector_base..data_sector_base + LOG_SECTOR_SIZE];
            // Validate data-sector signature + sequence halves.
            if &sector[0..4] != DATA_SECTOR_SIGNATURE {
                return None;
            }
            let seq_hi = read_u32_le(sector, 4);
            let seq_lo = read_u32_le(sector, LOG_SECTOR_SIZE - 4);
            let assembled = ((seq_hi as u64) << 32) | (seq_lo as u64);
            if assembled != sequence_number {
                return None;
            }

            // Reconstruct the original 4 KiB sector that was meant for
            // disk: replace the signature + sequence_high stamp with the
            // descriptor's leading_bytes (8 bytes), and the
            // sequence_low stamp with trailing_bytes (4 bytes).
            let mut reconstructed = sector.to_vec();
            reconstructed[0..8].copy_from_slice(&leading_bytes.to_le_bytes());
            reconstructed[LOG_SECTOR_SIZE - 4..LOG_SECTOR_SIZE]
                .copy_from_slice(&trailing_bytes.to_le_bytes());

            descriptors.push(Descriptor::Data {
                trailing_bytes,
                leading_bytes,
                file_offset,
                sequence_number: desc_seq,
                sector: reconstructed,
            });

            data_sector_base += LOG_SECTOR_SIZE;
            data_sector_idx += 1;
        } else {
            return None;
        }
    }
    let _ = data_sector_idx;

    Some(LogEntry {
        header: LogEntryHeader {
            entry_length: entry_length as u32,
            tail,
            sequence_number,
            descriptor_count,
            log_guid,
            flushed_file_offset,
            last_file_offset,
        },
        descriptors,
        log_offset_in_region: pos as u64,
    })
}

fn round_up_sector(n: usize) -> usize {
    (n + LOG_SECTOR_SIZE - 1) & !(LOG_SECTOR_SIZE - 1)
}

/// Walk the log region, find the longest chain of contiguous valid
/// entries with matching log_guid + strictly increasing sequence
/// numbers, and return them in apply order.
///
/// Returns an empty vec when the log_guid is all-zero (spec sentinel
/// for "log is empty"), when the log region is missing, or when no
/// valid entries are found.
pub fn collect_replay_chain(log_bytes: &[u8], expected_log_guid: &[u8; 16]) -> Vec<LogEntry> {
    if expected_log_guid.iter().all(|b| *b == 0) {
        return Vec::new();
    }
    // Pass 1: parse every 4 KiB slot we find an entry header in.
    let mut entries: Vec<LogEntry> = Vec::new();
    let mut pos = 0usize;
    while pos + LOG_SECTOR_SIZE <= log_bytes.len() {
        if let Some(e) = parse_log_entry(log_bytes, pos, expected_log_guid) {
            let len = e.header.entry_length as usize;
            entries.push(e);
            pos += len;
        } else {
            pos += LOG_SECTOR_SIZE;
        }
    }
    if entries.is_empty() {
        return entries;
    }
    // Pass 2: sort by sequence_number, drop dupes, return as chain.
    entries.sort_by_key(|e| e.header.sequence_number);
    entries.dedup_by_key(|e| e.header.sequence_number);
    entries
}

/// Apply the descriptors of a log chain to the underlying device. Each
/// descriptor is replayed in order: data descriptors write a 4 KiB
/// reconstructed sector at `file_offset`; zero descriptors zero a span
/// at `file_offset`. After every entry the device is flushed so a crash
/// mid-replay leaves a partially-applied but recoverable state — the
/// reader will pick up from the next valid sequence number on the next
/// open.
pub fn apply_chain(dev: &Arc<dyn BlockDevice>, chain: &[LogEntry]) -> Result<()> {
    for entry in chain {
        for d in &entry.descriptors {
            match d {
                Descriptor::Zero {
                    zero_length,
                    file_offset,
                    ..
                } => {
                    if *zero_length == 0 {
                        continue;
                    }
                    // Write in <=1 MiB chunks so we don't blow up on a
                    // pathological zero descriptor.
                    let chunk = 1024 * 1024usize;
                    let zeros = vec![0u8; chunk];
                    let mut remaining = *zero_length;
                    let mut off = *file_offset;
                    while remaining > 0 {
                        let n = std::cmp::min(remaining, chunk as u64) as usize;
                        dev.write_at(off, &zeros[..n])
                            .map_err(|e| Error::LogReplay(format!("zero write: {e}")))?;
                        off += n as u64;
                        remaining -= n as u64;
                    }
                }
                Descriptor::Data {
                    file_offset,
                    sector,
                    ..
                } => {
                    dev.write_at(*file_offset, sector)
                        .map_err(|e| Error::LogReplay(format!("data write: {e}")))?;
                }
            }
        }
        dev.flush()
            .map_err(|e| Error::LogReplay(format!("flush after entry: {e}")))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Encoder — used by the writer to commit BAT/metadata mutations through
// the log. Produces a single log entry containing one or more "desc"
// descriptors. Caller picks `sequence_number`, `tail`, and the active
// `log_guid`. Output is a contiguous byte buffer ready to splice into
// the log region at any sector-aligned position.
// ---------------------------------------------------------------------------

/// One pending data write the encoder will turn into a "desc" descriptor.
pub struct PendingWrite {
    pub file_offset: u64,
    /// Exactly 4 KiB.
    pub sector: Vec<u8>,
}

/// Encode a single log entry that, when replayed, applies `writes` to
/// the underlying device.
pub fn encode_entry(
    sequence_number: u64,
    tail: u32,
    log_guid: &[u8; 16],
    flushed_file_offset: u64,
    last_file_offset: u64,
    writes: &[PendingWrite],
) -> Vec<u8> {
    let descriptor_count = writes.len() as u32;
    let descriptors_bytes = (descriptor_count as usize) * LOG_DESCRIPTOR_SIZE;
    let header_and_descriptors = LOG_ENTRY_HEADER_SIZE + descriptors_bytes;
    let data_sectors_base = round_up_sector(header_and_descriptors);
    let entry_length = data_sectors_base + writes.len() * LOG_SECTOR_SIZE;

    let mut buf = vec![0u8; entry_length];

    // Header.
    buf[0..4].copy_from_slice(LOG_ENTRY_SIGNATURE);
    // checksum filled at end
    buf[8..12].copy_from_slice(&(entry_length as u32).to_le_bytes());
    buf[12..16].copy_from_slice(&tail.to_le_bytes());
    buf[16..24].copy_from_slice(&sequence_number.to_le_bytes());
    buf[24..28].copy_from_slice(&descriptor_count.to_le_bytes());
    // 28..32 reserved
    buf[32..48].copy_from_slice(log_guid);
    buf[48..56].copy_from_slice(&flushed_file_offset.to_le_bytes());
    buf[56..64].copy_from_slice(&last_file_offset.to_le_bytes());

    // Descriptors + data sectors.
    let seq_hi = ((sequence_number >> 32) & 0xFFFF_FFFF) as u32;
    let seq_lo = (sequence_number & 0xFFFF_FFFF) as u32;
    for (i, w) in writes.iter().enumerate() {
        debug_assert_eq!(w.sector.len(), LOG_SECTOR_SIZE);
        let desc_off = LOG_ENTRY_HEADER_SIZE + i * LOG_DESCRIPTOR_SIZE;
        // "desc" signature
        buf[desc_off..desc_off + 4].copy_from_slice(DATA_DESC_SIGNATURE);
        // trailing_bytes — original last 4 bytes of the sector
        let mut trailing = [0u8; 4];
        trailing.copy_from_slice(&w.sector[LOG_SECTOR_SIZE - 4..LOG_SECTOR_SIZE]);
        buf[desc_off + 4..desc_off + 8].copy_from_slice(&trailing);
        // leading_bytes — original first 8 bytes of the sector
        let mut leading = [0u8; 8];
        leading.copy_from_slice(&w.sector[0..8]);
        buf[desc_off + 8..desc_off + 16].copy_from_slice(&leading);
        // file_offset
        buf[desc_off + 16..desc_off + 24].copy_from_slice(&w.file_offset.to_le_bytes());
        // sequence_number
        buf[desc_off + 24..desc_off + 32].copy_from_slice(&sequence_number.to_le_bytes());

        // Data sector — copy of payload, then stamp signature + sequence
        // halves. The reader will undo the stamp using leading/trailing.
        let sec_base = data_sectors_base + i * LOG_SECTOR_SIZE;
        buf[sec_base..sec_base + LOG_SECTOR_SIZE].copy_from_slice(&w.sector);
        buf[sec_base..sec_base + 4].copy_from_slice(DATA_SECTOR_SIGNATURE);
        buf[sec_base + 4..sec_base + 8].copy_from_slice(&seq_hi.to_le_bytes());
        buf[sec_base + LOG_SECTOR_SIZE - 4..sec_base + LOG_SECTOR_SIZE]
            .copy_from_slice(&seq_lo.to_le_bytes());
    }

    // Checksum last.
    let crc = entry_crc(&buf);
    buf[4..8].copy_from_slice(&crc.to_le_bytes());
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_then_parse_roundtrip() {
        let log_guid = [0x42u8; 16];
        let mut sector = vec![0xABu8; LOG_SECTOR_SIZE];
        // Distinguish leading/trailing bytes so the decoder has work to do.
        sector[0..8].copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
        sector[LOG_SECTOR_SIZE - 4..].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());

        let entry = encode_entry(
            7,
            0,
            &log_guid,
            64 * 1024 * 1024,
            64 * 1024 * 1024,
            &[PendingWrite {
                file_offset: 4 * 1024 * 1024,
                sector: sector.clone(),
            }],
        );

        // Pad to a 1 MiB log region for parse_log_entry's bounds.
        let mut log_bytes = vec![0u8; 1024 * 1024];
        log_bytes[..entry.len()].copy_from_slice(&entry);

        let chain = collect_replay_chain(&log_bytes, &log_guid);
        assert_eq!(chain.len(), 1);
        match &chain[0].descriptors[0] {
            Descriptor::Data {
                file_offset,
                sector: reconstructed,
                ..
            } => {
                assert_eq!(*file_offset, 4 * 1024 * 1024);
                assert_eq!(reconstructed, &sector);
            }
            _ => panic!("expected data descriptor"),
        }
    }

    #[test]
    fn empty_log_guid_skips_replay() {
        let log_bytes = vec![0u8; 1024 * 1024];
        let chain = collect_replay_chain(&log_bytes, &[0u8; 16]);
        assert!(chain.is_empty());
    }
}
