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
//! - "desc": signature(4) "desc" + trailing_bytes(4) + leading_bytes(8) +
//!   file_offset(8) + sequence_number(8). Paired with one 4 KiB data
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
//! Replay walks the active chain forward from its start and stops at
//! the first break. A slot joins the chain only if it holds a CRC-valid
//! entry stamped with `header.log_guid`, sitting immediately after its
//! predecessor, and carrying exactly the next sequence number. The
//! chain starts where the highest-sequence entry's `tail` says its
//! sequence began.
//!
//! Everything after a break — a torn entry, a gap in the sequence, an
//! entry belonging to another chain — is left unapplied. A journal's
//! whole promise is that a batch lands whole or not at all, so applying
//! the successors of an entry that did not survive would produce an
//! image no writer ever committed. Stopping instead leaves the image
//! exactly as it stood when the last surviving entry committed, which
//! is a state that did exist.
//!
//! The region is circular, so a chain whose entries run off the end of
//! it is followed round to offset 0 rather than cut short there.

use crate::endian::{read_u32_le, read_u64_le};
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

/// Why a 4 KiB slot did not yield a log entry belonging to the chain
/// being replayed.
///
/// The variants are not interchangeable, which is the reason they are
/// named at all: `Empty` says nothing was ever written here, and the
/// other three say something was written here and cannot be used. A
/// replayer that cannot tell those apart cannot tell "the log ends
/// here" from "the log is damaged here", and will happily step over the
/// damage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryReject {
    /// No entry signature. An unused slot, or the interior of an entry
    /// whose header sits earlier in the region. Nothing was lost.
    Empty,
    /// An entry header is present but the bytes under it do not check
    /// out — a failed CRC, or a length the header contradicts. This is
    /// what a crash partway through writing an entry leaves behind.
    Corrupt,
    /// A structurally sound entry stamped with a different `log_guid`.
    /// It belongs to another chain and is not ours to replay.
    ForeignChain,
    /// CRC-clean but internally inconsistent: a descriptor table that
    /// overruns the entry, an unrecognised descriptor signature, or a
    /// data sector that is missing or stamped with the wrong sequence.
    Malformed,
}

/// Either an entry or the reason there isn't one.
type ParseOutcome = std::result::Result<LogEntry, EntryReject>;

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
///
/// In three pieces rather than over a zeroed copy: discovery checksums
/// every candidate slot, and copying each one first meant an allocation
/// the size of whatever length the slot declared -- up to the whole log
/// region, for a slot that was about to be rejected.
fn entry_crc(bytes: &[u8]) -> u32 {
    let crc = crc32c::crc32c(&bytes[0..4]);
    let crc = crc32c::crc32c_append(crc, &[0u8; 4]);
    crc32c::crc32c_append(crc, &bytes[8..])
}

/// Try to parse one log entry starting at `pos` inside `log_bytes`.
///
/// The error says *why* the slot was refused. `Empty` means keep
/// looking; the other three mean something was written here that cannot
/// be replayed, which the caller must not treat as an empty slot.
fn parse_log_entry(
    log_bytes: &[u8],
    pos: usize,
    expected_log_guid: &[u8; 16],
    checksum_budget: &mut usize,
) -> ParseOutcome {
    if pos + LOG_SECTOR_SIZE > log_bytes.len() {
        return Err(EntryReject::Empty);
    }
    let head = &log_bytes[pos..pos + LOG_SECTOR_SIZE];
    if &head[0..4] != LOG_ENTRY_SIGNATURE {
        return Err(EntryReject::Empty);
    }
    let entry_length = read_u32_le(head, 8) as usize;
    if entry_length < LOG_SECTOR_SIZE
        || !entry_length.is_multiple_of(LOG_SECTOR_SIZE)
        || pos + entry_length > log_bytes.len()
    {
        return Err(EntryReject::Corrupt);
    }
    // The GUID sits inside the first sector, so a slot belonging to
    // some other chain is rejected without checksumming the length it
    // declared. The checksum is the expensive part of a probe and this
    // is the one field that can rule a slot out before paying it.
    let mut log_guid = [0u8; 16];
    log_guid.copy_from_slice(&head[32..48]);
    if &log_guid != expected_log_guid {
        return Err(EntryReject::ForeignChain);
    }

    // A probe checksums as many bytes as the slot claims to be long,
    // and a slot that claims to reach the end of the region costs the
    // whole region. Discovery steps one sector after a refusal, so
    // every sector of a region filled with such slots pays that in
    // turn: the work is quadratic in the region's length. Measured
    // before this budget existed: 4 MiB of log took 143 ms, 32 MiB took
    // 12.6 s, and `log_length` is a u32, so 4 GiB would have taken
    // days -- on the plain read-only open.
    //
    // The budget is what a well-formed region can honestly need. Its
    // entries tile it without overlapping, so discovery checksums each
    // byte about once; four times that leaves room for a region that is
    // partly rewritten without leaving room for the quadratic case.
    if *checksum_budget < entry_length {
        return Err(EntryReject::Corrupt);
    }
    *checksum_budget -= entry_length;

    let entry_bytes = &log_bytes[pos..pos + entry_length];
    let stored_crc = read_u32_le(entry_bytes, 4);
    if stored_crc != entry_crc(entry_bytes) {
        return Err(EntryReject::Corrupt);
    }

    let tail = read_u32_le(entry_bytes, 12);
    let sequence_number = read_u64_le(entry_bytes, 16);
    let descriptor_count = read_u32_le(entry_bytes, 24);
    let flushed_file_offset = read_u64_le(entry_bytes, 48);
    let last_file_offset = read_u64_le(entry_bytes, 56);

    // Descriptors live immediately after the 64-byte header.
    let Some(descriptors_size) = (descriptor_count as usize).checked_mul(LOG_DESCRIPTOR_SIZE)
    else {
        return Err(EntryReject::Malformed);
    };
    if LOG_ENTRY_HEADER_SIZE + descriptors_size > entry_length {
        return Err(EntryReject::Malformed);
    }

    // Count "desc" entries up front so we know how many data sectors to
    // expect; the spec promises they appear in order, one per "desc"
    // descriptor, immediately after the descriptor table is rounded up
    // to the next 4 KiB sector boundary. In practice the spec rounds
    // descriptors+header up to a sector boundary and then each "desc"
    // descriptor consumes one 4 KiB sector after it.
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
            return Err(EntryReject::Malformed);
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
                return Err(EntryReject::Malformed);
            }
            let sector = &entry_bytes[data_sector_base..data_sector_base + LOG_SECTOR_SIZE];
            // Validate data-sector signature + sequence halves.
            if &sector[0..4] != DATA_SECTOR_SIGNATURE {
                return Err(EntryReject::Malformed);
            }
            let seq_hi = read_u32_le(sector, 4);
            let seq_lo = read_u32_le(sector, LOG_SECTOR_SIZE - 4);
            let assembled = ((seq_hi as u64) << 32) | (seq_lo as u64);
            if assembled != sequence_number {
                return Err(EntryReject::Malformed);
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
        } else {
            return Err(EntryReject::Malformed);
        }
    }

    Ok(LogEntry {
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

/// The entry that starts at exactly `offset`, if discovery found one.
fn entry_starting_at(found: &[LogEntry], offset: usize) -> Option<&LogEntry> {
    found
        .iter()
        .find(|e| e.log_offset_in_region == offset as u64)
}

/// Collect the entries that may be replayed, in apply order.
///
/// Discovery scans the whole region, because an entry can be anywhere
/// in it. Selection then walks forward from the chain's start and stops
/// at the first slot that is not a valid continuation, so the result is
/// always a prefix of one chain and never a set of survivors gathered
/// from either side of a break.
///
/// Returns an empty vec when `expected_log_guid` is all-zero (the spec
/// sentinel for "log is empty"), when the log region is missing, when
/// the region holds no entry of this chain, or when the chain's own
/// start cannot be located.
/// How many times over discovery may checksum the log region.
///
/// See the budget note in `parse_log_entry` for what this stops.
pub const DISCOVERY_CHECKSUM_PASSES: usize = 4;

/// Probe every slot in the region and return what is there, along with
/// how many bytes were checksummed doing it.
///
/// Split out from [`collect_replay_chain`] so the budget is testable:
/// the number returned is the work the budget bounds, and a test can
/// assert on it rather than on how long the call took.
fn discover(log_bytes: &[u8], expected_log_guid: &[u8; 16]) -> (Vec<LogEntry>, usize) {
    // Probe every 4 KiB slot and never stop early. An entry can sit
    // anywhere in the region: the log is a circular buffer, and this
    // crate's own writer splices wherever it finds room without
    // recording where. Discovery only establishes what is present; it
    // decides nothing about what gets applied.
    let allowance = log_bytes.len().saturating_mul(DISCOVERY_CHECKSUM_PASSES);
    let mut budget = allowance;
    let mut found: Vec<LogEntry> = Vec::new();
    let mut pos = 0usize;
    while pos + LOG_SECTOR_SIZE <= log_bytes.len() {
        match parse_log_entry(log_bytes, pos, expected_log_guid, &mut budget) {
            Ok(entry) => {
                pos += entry.header.entry_length as usize;
                found.push(entry);
            }
            // A refused slot tells us nothing trustworthy about how far
            // whatever is there extends, so step one sector and probe
            // again rather than believing a length we just rejected.
            Err(_) => pos += LOG_SECTOR_SIZE,
        }
    }
    (found, allowance - budget)
}

pub fn collect_replay_chain(log_bytes: &[u8], expected_log_guid: &[u8; 16]) -> Vec<LogEntry> {
    if expected_log_guid.iter().all(|b| *b == 0) {
        return Vec::new();
    }

    // Pass 1 — discovery.
    let (found, _checksummed) = discover(log_bytes, expected_log_guid);
    if found.is_empty() {
        return found;
    }

    // Pass 2 — the head. The active chain ends at the highest sequence
    // number present. On a tie the entry at the lower log offset wins:
    // two entries claiming one sequence number means one of them is
    // stale, and `found` is in ascending offset order, so the strict
    // `>` below keeps the first one seen. That tie-break is arbitrary,
    // but it is now written down.
    let mut head = &found[0];
    for entry in &found[1..] {
        if entry.header.sequence_number > head.header.sequence_number {
            head = entry;
        }
    }

    // Pass 3 — where the chain starts. `tail` is the head's own
    // statement of where its sequence began, which is the field the
    // format provides so that a replayer does not have to guess. Using
    // it is what keeps an older chain still lying in the region, or a
    // chain that does not begin at offset 0, from being mistaken for
    // part of this one.
    //
    // If it resolves to nothing we found, we do not guess. The caller
    // erases the log region and marks the image clean the moment a
    // chain comes back, so falling back to whatever entry sits lowest
    // in the region would let a stale chain be applied and destroy the
    // live one on its way out. Replaying nothing leaves the log intact
    // for a reader that can make sense of it.
    let Some(first) = entry_starting_at(&found, head.header.tail as usize) else {
        return Vec::new();
    };
    let start = first.log_offset_in_region as usize;

    // Pass 4 — the walk, and the stop. Each entry must sit immediately
    // after its predecessor and carry exactly the next sequence number.
    // The first slot that fails either test ends the chain, whatever
    // the reason: a torn entry, a foreign one, a gap in the sequence,
    // or simply the end of what was written.
    let mut chain: Vec<LogEntry> = Vec::new();
    let mut pos = start;
    let mut expected_sequence: Option<u64> = None;
    while let Some(entry) = entry_starting_at(&found, pos) {
        if let Some(wanted) = expected_sequence {
            if entry.header.sequence_number != wanted {
                break;
            }
        }
        pos += entry.header.entry_length as usize;
        // The region is circular: an entry ending exactly at its end is
        // followed by one at offset 0. Sequence numbers rise by one at
        // every step, so no entry can be reached twice and the walk
        // terminates whether or not it goes round.
        if pos >= log_bytes.len() {
            pos -= log_bytes.len();
        }
        let next = entry.header.sequence_number.checked_add(1);
        chain.push(entry.clone());
        match next {
            Some(n) => expected_sequence = Some(n),
            None => break,
        }
    }
    chain
}

/// Apply the descriptors of a log chain to the underlying device. Each
/// descriptor is replayed in order: data descriptors write a 4 KiB
/// reconstructed sector at `file_offset`; zero descriptors zero a span
/// at `file_offset`. After every entry the device is flushed so a crash
/// mid-replay leaves a partially-applied but recoverable state — the
/// reader will pick up from the next valid sequence number on the next
/// open.
/// The largest VHDX block, and so the most one descriptor's allocation
/// can have grown the file.
const MAX_BLOCK_SIZE: u64 = 256 * 1024 * 1024;

/// How far the file may reach once this chain has been applied.
///
/// The greater of what the file is now and what the chain says it
/// became, with the latter bounded by one block per descriptor. See the
/// note in [`apply_chain`].
fn allowed_extent(current: u64, chain: &[LogEntry]) -> u64 {
    let descriptors: u64 = chain
        .iter()
        .map(|entry| entry.descriptors.len() as u64)
        .sum();
    let ceiling = current.saturating_add(descriptors.saturating_mul(MAX_BLOCK_SIZE));
    let claimed = chain
        .iter()
        .map(|entry| entry.header.last_file_offset)
        .max()
        .unwrap_or(0)
        .min(ceiling);
    current.max(claimed)
}

pub fn apply_chain(dev: &Arc<dyn BlockDevice>, chain: &[LogEntry]) -> Result<()> {
    // WHERE EACH DESCRIPTOR LANDS, BEFORE ANY OF THEM LAND.
    //
    // A descriptor says where on the device to write and how much, and
    // both numbers came out of the image. Replay runs on `open` --
    // including a read-only open, which takes the host file read-write
    // when it can, because a dirty VHDX has to be replayed before its
    // region table, metadata and BAT mean anything. So merely handing
    // this reader a file is enough to ask it to write.
    //
    // Against a file-backed device a write past the end is not an
    // error: the file grows. A "zero" descriptor starting at 0 and
    // running for eight times the file's length erased the image and
    // grew it to eight times its size, and `open` then reported "no
    // valid VHDX region table found" -- so the caller was told the file
    // was not a VHDX, and not that it had just been overwritten.
    //
    // The whole chain is checked before the first write. A span that
    // leaves the device means the log is not describing this device,
    // and a partly-applied chain is precisely the damage: descriptors
    // one and two are as destructive as the one that got caught.
    //
    // BUT A LOG MAY LEGITIMATELY GROW THE FILE. That is what
    // `last_file_offset` in the entry header is for: the highest byte
    // the entry is aware of. After a crash mid-allocation the log
    // describes BAT and metadata sectors past the current end, and the
    // reference implementation applies every descriptor (writes past
    // EOF extend the file) and then truncates up to that offset. A
    // bound of the file's current size alone refuses that chain and
    // with it the whole open -- the ordinary post-crash shape.
    //
    // How far it may grow is bounded by what a log entry can be about.
    // Each descriptor is one sector or one zeroed span within a single
    // allocation, and a VHDX block is at most `MAX_BLOCK_SIZE`, so a
    // chain cannot have made the file grow by more than one block per
    // descriptor. A `last_file_offset` beyond that is not describing
    // this file.
    let dev_size = allowed_extent(dev.size_bytes(), chain);
    for entry in chain {
        for d in &entry.descriptors {
            let (file_offset, length) = match d {
                Descriptor::Zero {
                    zero_length,
                    file_offset,
                    ..
                } => (*file_offset, *zero_length),
                Descriptor::Data {
                    file_offset,
                    sector,
                    ..
                } => (*file_offset, sector.len() as u64),
            };
            let end = file_offset.checked_add(length).ok_or_else(|| {
                Error::LogReplay(format!(
                    "descriptor at {file_offset} spans {length} bytes, which overflows"
                ))
            })?;
            if end > dev_size {
                return Err(Error::LogReplay(format!(
                    "descriptor writes {length} bytes at {file_offset}, ending at {end} \
                     on a device of {dev_size} bytes"
                )));
            }
        }
    }

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
                    crate::reader::write_zeros(dev, *file_offset, *zero_length)
                        .map_err(|e| Error::LogReplay(format!("zero write: {e}")))?;
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

    // -----------------------------------------------------------------
    // Chain-shape fixtures.
    //
    // Every entry below is a one-descriptor entry, so each occupies
    // exactly two 4 KiB sectors: one for the header + descriptor table,
    // one for the paired data sector. Laying them back to back at
    // multiples of that length is the shape the format calls a
    // contiguous chain, and it is what the replay walk has to follow.
    // -----------------------------------------------------------------

    const TEST_REGION_LEN: usize = 1024 * 1024;
    const ONE_WRITE_ENTRY_LEN: usize = 2 * LOG_SECTOR_SIZE;
    const LIVE_GUID: [u8; 16] = [0x42u8; 16];
    const FOREIGN_GUID: [u8; 16] = [0x99u8; 16];

    /// Device offset that entry `seq` in a fixture chain writes to.
    /// Distinct per sequence number so a replayed entry is identifiable
    /// by where it landed.
    fn target_of(seq: u64) -> u64 {
        4 * 1024 * 1024 + seq * LOG_SECTOR_SIZE as u64
    }

    /// A one-descriptor entry that writes a `seq`-derived fill pattern
    /// at `target_of(seq)`.
    fn one_write_entry(seq: u64, tail: u32, guid: &[u8; 16]) -> Vec<u8> {
        encode_entry(
            seq,
            tail,
            guid,
            0,
            0,
            &[PendingWrite {
                file_offset: target_of(seq),
                sector: vec![0x10u8.wrapping_add(seq as u8); LOG_SECTOR_SIZE],
            }],
        )
    }

    fn splice(region: &mut [u8], off: usize, entry: &[u8]) {
        region[off..off + entry.len()].copy_from_slice(entry);
    }

    /// Region holding sequences `1..=count` back to back from offset 0,
    /// all in the live chain, all advertising `tail = 0`.
    fn contiguous_chain(count: u64) -> Vec<u8> {
        let mut region = vec![0u8; TEST_REGION_LEN];
        for seq in 1..=count {
            let off = (seq as usize - 1) * ONE_WRITE_ENTRY_LEN;
            splice(&mut region, off, &one_write_entry(seq, 0, &LIVE_GUID));
        }
        region
    }

    fn sequences(chain: &[LogEntry]) -> Vec<u64> {
        chain.iter().map(|e| e.header.sequence_number).collect()
    }

    fn replayed_offsets(chain: &[LogEntry]) -> Vec<u64> {
        chain
            .iter()
            .flat_map(|e| e.descriptors.iter())
            .map(|d| match d {
                Descriptor::Data { file_offset, .. } => *file_offset,
                Descriptor::Zero { file_offset, .. } => *file_offset,
            })
            .collect()
    }

    /// In-memory device so a chain can be applied and the resulting
    /// bytes inspected — the only way to state "the successors were not
    /// written" as an assertion about the device rather than about the
    /// chain.
    struct MemDevice(std::sync::Mutex<Vec<u8>>);

    impl MemDevice {
        fn filled(len: usize, fill: u8) -> Arc<dyn BlockDevice> {
            Arc::new(MemDevice(std::sync::Mutex::new(vec![fill; len])))
        }
        fn byte_at(dev: &Arc<dyn BlockDevice>, off: u64) -> u8 {
            let mut b = [0u8; 1];
            fs_core::BlockRead::read_at(dev.as_ref(), off, &mut b).unwrap();
            b[0]
        }
    }

    impl fs_core::BlockRead for MemDevice {
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> fs_core::Result<()> {
            let m = self.0.lock().unwrap();
            let start = offset as usize;
            buf.copy_from_slice(&m[start..start + buf.len()]);
            Ok(())
        }
        fn size_bytes(&self) -> u64 {
            self.0.lock().unwrap().len() as u64
        }
    }

    impl BlockDevice for MemDevice {
        fn write_at(&self, offset: u64, buf: &[u8]) -> fs_core::Result<()> {
            let mut m = self.0.lock().unwrap();
            let start = offset as usize;
            m[start..start + buf.len()].copy_from_slice(buf);
            Ok(())
        }
        fn is_writable(&self) -> bool {
            true
        }
    }

    // -----------------------------------------------------------------
    // What replay must do.
    // -----------------------------------------------------------------

    #[test]
    fn contiguous_multi_entry_chain_replays_in_full() {
        let region = contiguous_chain(4);
        let chain = collect_replay_chain(&region, &LIVE_GUID);
        assert_eq!(sequences(&chain), vec![1, 2, 3, 4]);
        assert_eq!(
            replayed_offsets(&chain),
            (1..=4).map(target_of).collect::<Vec<_>>()
        );
    }

    #[test]
    fn torn_entry_ends_the_chain_and_its_successors_are_dropped() {
        let mut region = contiguous_chain(4);
        // Tear entry 3: flip a byte inside its data sector. The entry
        // header still says "loge" and still claims its length, but the
        // CRC no longer covers what is there — exactly what a crash
        // partway through writing an entry leaves behind.
        region[2 * ONE_WRITE_ENTRY_LEN + LOG_SECTOR_SIZE + 100] ^= 0xFF;

        let chain = collect_replay_chain(&region, &LIVE_GUID);
        assert_eq!(
            sequences(&chain),
            vec![1, 2],
            "replay must stop at the torn entry, not step over it"
        );
        assert!(
            !replayed_offsets(&chain).contains(&target_of(4)),
            "entry 4 follows the tear and must not be applied"
        );
    }

    #[test]
    fn successors_of_a_torn_entry_never_reach_the_device() {
        let mut region = contiguous_chain(4);
        region[2 * ONE_WRITE_ENTRY_LEN + LOG_SECTOR_SIZE + 100] ^= 0xFF;

        let dev = MemDevice::filled(8 * 1024 * 1024, 0xCC);
        apply_chain(&dev, &collect_replay_chain(&region, &LIVE_GUID)).unwrap();

        assert_eq!(MemDevice::byte_at(&dev, target_of(1)), 0x11);
        assert_eq!(MemDevice::byte_at(&dev, target_of(2)), 0x12);
        assert_eq!(
            MemDevice::byte_at(&dev, target_of(3)),
            0xCC,
            "the torn entry itself must not be applied"
        );
        assert_eq!(
            MemDevice::byte_at(&dev, target_of(4)),
            0xCC,
            "applying entry 4 without entry 3 is the state the log exists to prevent"
        );
    }

    #[test]
    fn a_sequence_gap_ends_the_chain() {
        let mut region = contiguous_chain(4);
        // Re-stamp the third slot with sequence 9 instead of 3.
        splice(
            &mut region,
            2 * ONE_WRITE_ENTRY_LEN,
            &one_write_entry(9, 0, &LIVE_GUID),
        );
        let chain = collect_replay_chain(&region, &LIVE_GUID);
        assert_eq!(sequences(&chain), vec![1, 2]);
    }

    #[test]
    fn a_foreign_chain_entry_ends_the_chain() {
        let mut region = contiguous_chain(4);
        // Third slot holds a perfectly valid entry belonging to some
        // other log_guid. It is not ours to replay, and it breaks the
        // run, so nothing after it is ours to replay either.
        splice(
            &mut region,
            2 * ONE_WRITE_ENTRY_LEN,
            &one_write_entry(3, 0, &FOREIGN_GUID),
        );
        let chain = collect_replay_chain(&region, &LIVE_GUID);
        assert_eq!(sequences(&chain), vec![1, 2]);
    }

    #[test]
    fn tail_anchors_the_chain_past_an_older_one_in_the_same_region() {
        // Live chain (5, 6) at the start of the region, announcing
        // tail = 0. Sequences 1 and 2 are left over from an earlier
        // chain that used the same log_guid and were never overwritten.
        let mut region = vec![0u8; TEST_REGION_LEN];
        splice(&mut region, 0, &one_write_entry(5, 0, &LIVE_GUID));
        splice(
            &mut region,
            ONE_WRITE_ENTRY_LEN,
            &one_write_entry(6, 0, &LIVE_GUID),
        );
        splice(
            &mut region,
            4 * ONE_WRITE_ENTRY_LEN,
            &one_write_entry(1, 4 * ONE_WRITE_ENTRY_LEN as u32, &LIVE_GUID),
        );
        splice(
            &mut region,
            5 * ONE_WRITE_ENTRY_LEN,
            &one_write_entry(2, 4 * ONE_WRITE_ENTRY_LEN as u32, &LIVE_GUID),
        );

        let chain = collect_replay_chain(&region, &LIVE_GUID);
        assert_eq!(sequences(&chain), vec![5, 6]);
        let applied = replayed_offsets(&chain);
        assert!(!applied.contains(&target_of(1)));
        assert!(!applied.contains(&target_of(2)));
    }

    #[test]
    fn a_chain_starting_away_from_offset_zero_is_found_via_tail() {
        // Nothing at the head of the region; the chain begins three
        // entries in and says so in `tail`.
        let start = 3 * ONE_WRITE_ENTRY_LEN;
        let mut region = vec![0u8; TEST_REGION_LEN];
        splice(
            &mut region,
            start,
            &one_write_entry(1, start as u32, &LIVE_GUID),
        );
        splice(
            &mut region,
            start + ONE_WRITE_ENTRY_LEN,
            &one_write_entry(2, start as u32, &LIVE_GUID),
        );

        let chain = collect_replay_chain(&region, &LIVE_GUID);
        assert_eq!(sequences(&chain), vec![1, 2]);
        assert_eq!(chain[0].log_offset_in_region, start as u64);
    }

    #[test]
    fn a_chain_that_wraps_the_end_of_the_region_is_followed_round() {
        // Sequences 1 and 2 fill the last two entry slots and 3 lands
        // back at offset 0. Cutting the chain at the wrap would apply
        // 1 and 2, then erase the region holding 3 — losing a committed
        // entry rather than declining to replay a doubtful one.
        let start = TEST_REGION_LEN - 2 * ONE_WRITE_ENTRY_LEN;
        let mut region = vec![0u8; TEST_REGION_LEN];
        splice(
            &mut region,
            start,
            &one_write_entry(1, start as u32, &LIVE_GUID),
        );
        splice(
            &mut region,
            TEST_REGION_LEN - ONE_WRITE_ENTRY_LEN,
            &one_write_entry(2, start as u32, &LIVE_GUID),
        );
        splice(
            &mut region,
            0,
            &one_write_entry(3, start as u32, &LIVE_GUID),
        );

        let chain = collect_replay_chain(&region, &LIVE_GUID);
        assert_eq!(sequences(&chain), vec![1, 2, 3]);
    }

    #[test]
    fn an_unlocatable_chain_start_replays_nothing() {
        // The head says its sequence began at the second slot, and that
        // slot is torn. Where this chain starts is now unknowable, and
        // the entry still sitting at offset 0 belongs to a run the head
        // has disowned. Replaying it would apply a stale write and then
        // let the caller erase the log that still holds the real chain.
        let mut region = contiguous_chain(4);
        splice(
            &mut region,
            3 * ONE_WRITE_ENTRY_LEN,
            &one_write_entry(4, ONE_WRITE_ENTRY_LEN as u32, &LIVE_GUID),
        );
        region[ONE_WRITE_ENTRY_LEN + LOG_SECTOR_SIZE + 7] ^= 0xFF;

        assert!(collect_replay_chain(&region, &LIVE_GUID).is_empty());
    }

    /// Hand-built entry carrying a single "zero" descriptor. The
    /// encoder only emits "desc" descriptors, so the only way to cover
    /// the zeroing half of replay is to lay the bytes down here.
    fn zero_descriptor_entry(seq: u64, guid: &[u8; 16], file_offset: u64, len: u64) -> Vec<u8> {
        let mut buf = vec![0u8; LOG_SECTOR_SIZE];
        buf[0..4].copy_from_slice(LOG_ENTRY_SIGNATURE);
        buf[8..12].copy_from_slice(&(LOG_SECTOR_SIZE as u32).to_le_bytes());
        buf[12..16].copy_from_slice(&0u32.to_le_bytes());
        buf[16..24].copy_from_slice(&seq.to_le_bytes());
        buf[24..28].copy_from_slice(&1u32.to_le_bytes());
        buf[32..48].copy_from_slice(guid);
        let d = LOG_ENTRY_HEADER_SIZE;
        buf[d..d + 4].copy_from_slice(ZERO_DESC_SIGNATURE);
        buf[d + 8..d + 16].copy_from_slice(&len.to_le_bytes());
        buf[d + 16..d + 24].copy_from_slice(&file_offset.to_le_bytes());
        buf[d + 24..d + 32].copy_from_slice(&seq.to_le_bytes());
        let crc = entry_crc(&buf);
        buf[4..8].copy_from_slice(&crc.to_le_bytes());
        buf
    }

    #[test]
    fn zero_descriptor_parses_and_zeroes_its_span() {
        let mut region = vec![0u8; TEST_REGION_LEN];
        splice(
            &mut region,
            0,
            &zero_descriptor_entry(1, &LIVE_GUID, 2 * 1024 * 1024, 8192),
        );

        let chain = collect_replay_chain(&region, &LIVE_GUID);
        assert_eq!(chain.len(), 1);
        match &chain[0].descriptors[0] {
            Descriptor::Zero {
                zero_length,
                file_offset,
                ..
            } => {
                assert_eq!(*zero_length, 8192);
                assert_eq!(*file_offset, 2 * 1024 * 1024);
            }
            _ => panic!("expected a zero descriptor"),
        }

        let dev = MemDevice::filled(4 * 1024 * 1024, 0xCC);
        apply_chain(&dev, &chain).unwrap();
        assert_eq!(MemDevice::byte_at(&dev, 2 * 1024 * 1024), 0);
        assert_eq!(MemDevice::byte_at(&dev, 2 * 1024 * 1024 + 8191), 0);
        assert_eq!(MemDevice::byte_at(&dev, 2 * 1024 * 1024 + 8192), 0xCC);
    }

    /// A log may legitimately grow the file: `last_file_offset` is the
    /// highest byte the entry is aware of, and after a crash
    /// mid-allocation the log describes sectors past the current end.
    /// The reference implementation applies every descriptor -- writes
    /// past EOF extend the file -- and then truncates up to that
    /// offset. Bounding by the file's current size alone refused that
    /// chain, and with it the whole open.
    ///
    /// How far it may grow is bounded by what an entry can be about:
    /// one allocation per descriptor, and a VHDX block is at most
    /// 256 MiB.
    #[test]
    fn a_chain_may_grow_the_file_by_what_its_descriptors_could_have_allocated() {
        const MIB: u64 = 1024 * 1024;
        let entry = |last_file_offset: u64, descriptors: usize| LogEntry {
            header: LogEntryHeader {
                entry_length: LOG_SECTOR_SIZE as u32,
                tail: 0,
                sequence_number: 1,
                descriptor_count: descriptors as u32,
                log_guid: LIVE_GUID,
                flushed_file_offset: 0,
                last_file_offset,
            },
            descriptors: (0..descriptors)
                .map(|i| Descriptor::Zero {
                    zero_length: 4096,
                    file_offset: i as u64 * 4096,
                    sequence_number: 1,
                })
                .collect(),
            log_offset_in_region: 0,
        };

        // A chain that says nothing about the file's size does not
        // grow it.
        assert_eq!(allowed_extent(64 * MIB, &[entry(0, 1)]), 64 * MIB);

        // One that says the file became a sector longer is believed:
        // this is the post-crash shape.
        assert_eq!(
            allowed_extent(64 * MIB, &[entry(64 * MIB + 4096, 1)]),
            64 * MIB + 4096
        );

        // One descriptor cannot have allocated more than one block, so
        // a claim past that is not describing this file.
        let outrageous = entry(1 << 62, 1);
        assert_eq!(
            allowed_extent(64 * MIB, &[outrageous]),
            64 * MIB + 256 * MIB
        );

        // Four descriptors, four blocks.
        assert_eq!(
            allowed_extent(64 * MIB, &[entry(1 << 62, 4)]),
            64 * MIB + 4 * 256 * MIB
        );

        // A claim SHORTER than the file does not shrink the bound --
        // the descriptors still have to land inside what is there.
        assert_eq!(allowed_extent(64 * MIB, &[entry(4096, 1)]), 64 * MIB);
    }

    /// A descriptor names where on the device to write, and that
    /// number comes out of the image.
    ///
    /// Replay runs on `open`, including a read-only open -- the reader
    /// takes the host file read-write when it can, because a dirty
    /// VHDX has to be replayed before its BAT and metadata mean
    /// anything. So a descriptor pointing off the end of the device is
    /// a write somebody gets by handing over a file, and against a
    /// file-backed device it is not even an error: the file grows.
    ///
    /// A span that leaves the device means the log is not describing
    /// this device, so nothing in the chain is applied. Refusing the
    /// whole chain rather than the one descriptor is the point -- a
    /// partly-applied chain is the damage.
    /// Discovery probes every slot, and a probe costs a checksum over
    /// whatever length the slot declares. A region whose every sector
    /// declares that it runs to the end of the region therefore costs
    /// the region's length, once per sector -- quadratic work on the
    /// read-only open path, where `log_length` is a u32 and so the
    /// region can be 4 GiB.
    ///
    /// The assertion is on bytes checksummed rather than on elapsed
    /// time, because that is what the budget actually bounds and it
    /// does not depend on how loaded the machine is.
    #[test]
    fn a_region_of_overlapping_claims_is_checksummed_a_bounded_number_of_times() {
        const REGION: usize = 4 * 1024 * 1024;
        let mut region = vec![0u8; REGION];
        // Every sector: a well-formed header claiming to reach the end
        // of the region, carrying the live GUID, with a checksum that
        // will not match.
        for pos in (0..REGION).step_by(LOG_SECTOR_SIZE) {
            let len = (REGION - pos) as u32;
            region[pos..pos + 4].copy_from_slice(LOG_ENTRY_SIGNATURE);
            region[pos + 8..pos + 12].copy_from_slice(&len.to_le_bytes());
            region[pos + 32..pos + 48].copy_from_slice(&LIVE_GUID);
        }

        let (found, checksummed) = discover(&region, &LIVE_GUID);
        assert!(found.is_empty(), "none of those slots is a valid entry");
        assert!(
            checksummed <= REGION * DISCOVERY_CHECKSUM_PASSES,
            "discovery checksummed {checksummed} bytes of a {REGION}-byte region, \
             which is {:.1} times over",
            checksummed as f64 / REGION as f64
        );
    }

    /// The budget must not cost a well-formed region its entries: a log
    /// whose entries tile it is checksummed about once through.
    #[test]
    fn a_well_formed_region_stays_well_inside_the_budget() {
        let region = contiguous_chain(4);
        let (found, checksummed) = discover(&region, &LIVE_GUID);
        assert_eq!(sequences(&found), vec![1, 2, 3, 4]);
        assert!(
            checksummed <= region.len(),
            "a tiling region checksummed {checksummed} bytes of {}",
            region.len()
        );
    }

    #[test]
    fn a_zero_span_running_past_the_device_is_refused_before_anything_is_written() {
        let mut region = vec![0u8; TEST_REGION_LEN];
        // Starts inside the device and runs off the end of it.
        splice(
            &mut region,
            0,
            &zero_descriptor_entry(1, &LIVE_GUID, 2 * 1024 * 1024, u64::MAX),
        );
        let chain = collect_replay_chain(&region, &LIVE_GUID);
        assert_eq!(chain.len(), 1, "the entry itself is well formed");

        let dev = MemDevice::filled(4 * 1024 * 1024, 0xCC);
        assert!(
            apply_chain(&dev, &chain).is_err(),
            "replay accepted a span of u64::MAX bytes on a 4 MiB device"
        );
        assert_eq!(
            MemDevice::byte_at(&dev, 2 * 1024 * 1024),
            0xCC,
            "the refused span was written anyway, up to where the device ended"
        );
    }

    #[test]
    fn a_zero_span_starting_past_the_device_is_refused() {
        let mut region = vec![0u8; TEST_REGION_LEN];
        splice(
            &mut region,
            0,
            &zero_descriptor_entry(1, &LIVE_GUID, 1 << 40, 4096),
        );
        let chain = collect_replay_chain(&region, &LIVE_GUID);
        let dev = MemDevice::filled(4 * 1024 * 1024, 0xCC);
        assert!(apply_chain(&dev, &chain).is_err());
    }

    #[test]
    fn a_data_sector_past_the_device_is_refused() {
        let mut sector = vec![0xABu8; LOG_SECTOR_SIZE];
        sector[0..8].copy_from_slice(&0u64.to_le_bytes());
        sector[LOG_SECTOR_SIZE - 4..].copy_from_slice(&0u32.to_le_bytes());
        let entry = encode_entry(
            1,
            0,
            &LIVE_GUID,
            0,
            0,
            &[PendingWrite {
                file_offset: 1 << 40,
                sector,
            }],
        );
        let mut region = vec![0u8; TEST_REGION_LEN];
        splice(&mut region, 0, &entry);
        let chain = collect_replay_chain(&region, &LIVE_GUID);
        assert_eq!(chain.len(), 1, "the entry itself is well formed");

        let dev = MemDevice::filled(4 * 1024 * 1024, 0xCC);
        assert!(
            apply_chain(&dev, &chain).is_err(),
            "replay wrote a sector at byte 2^40 of a 4 MiB device"
        );
    }

    /// The refusal has to come before the first write, not part way
    /// through: a chain whose first descriptor is fine and whose second
    /// is not must leave the device as it found it.
    #[test]
    fn a_chain_with_one_bad_descriptor_applies_none_of_it() {
        let mut region = vec![0u8; TEST_REGION_LEN];
        splice(
            &mut region,
            0,
            &zero_descriptor_entry(1, &LIVE_GUID, 1024 * 1024, 4096),
        );
        splice(
            &mut region,
            LOG_SECTOR_SIZE,
            &zero_descriptor_entry(2, &LIVE_GUID, 1 << 40, 4096),
        );
        let chain = collect_replay_chain(&region, &LIVE_GUID);
        assert_eq!(chain.len(), 2);

        let dev = MemDevice::filled(4 * 1024 * 1024, 0xCC);
        assert!(apply_chain(&dev, &chain).is_err());
        assert_eq!(
            MemDevice::byte_at(&dev, 1024 * 1024),
            0xCC,
            "the first descriptor of a chain that was going to be refused \
             was applied anyway"
        );
    }
}
