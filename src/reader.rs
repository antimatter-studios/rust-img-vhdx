//! VHDX read/write path.
//!
//! Walks the file identifier → header → region table → metadata → BAT
//! pipeline once at open time, then resolves `read_at` / `write_at` by
//! indexing the cached BAT.
//!
//! ## Backing storage
//!
//! The reader is generic over [`fs_core::BlockDevice`]. Open from a
//! path via [`VhdxReader::open`] / [`VhdxReader::open_rw`] (the file is
//! wrapped in a [`fs_core::FileDevice`] internally), or hand in any
//! other `BlockDevice` via [`VhdxReader::open_on_device`] /
//! [`VhdxReader::open_rw_on_device`]. The on-device variants are how
//! the VHDX layer stacks on top of an FSKit-supplied block resource,
//! a slice reader, or any other host-managed device.
//!
//! ## Log replay
//!
//! Both RO and RW opens replay any pending log entries before exposing
//! the BAT. A read-only open against a writable device will replay in
//! place (so subsequent reads see post-replay state). On a non-writable
//! device a non-empty log is reported as `Error::ReadOnly` because we
//! can't safely interpret the stale data zones without applying the
//! pending writes first.
//!
//! ## Limitations
//!
//! - No differencing-chain resolution (parent VHDX). Surfaced as
//!   `Error::Unsupported`.
//! - No sector-bitmap (PARTIALLY_PRESENT) support on read; treated as
//!   `Unsupported`. Writes into a sector-bitmap-only entry are
//!   converted into a fully-present block (allocate + write) per spec.

use crate::bat::{chunk_ratio as compute_chunk_ratio, data_bat_index, BatEntry, PayloadState};
use crate::error::{Error, Result};
use crate::header::{Header, HEADER1_OFFSET, HEADER2_OFFSET, HEADER_SIZE};
use crate::log::{apply_chain, collect_replay_chain, encode_entry, PendingWrite, LOG_SECTOR_SIZE};
use crate::metadata::{
    item_ids, read_sector_size, read_virtual_disk_size, FileParameters, MetadataTable,
};
use crate::region_table::{
    guids, RegionTable, REGION_TABLE1_OFFSET, REGION_TABLE2_OFFSET, REGION_TABLE_SIZE,
};
use fs_core::{BlockDevice, FileDevice};
use std::path::Path;
use std::sync::{Arc, Mutex};

const FILE_IDENTIFIER_OFFSET: u64 = 0;
const FILE_IDENTIFIER_SIGNATURE: &[u8; 8] = b"vhdxfile";

/// Encoded BAT entry width (bytes).
const BAT_ENTRY_BYTES: u64 = 8;

pub struct VhdxReader {
    /// Backing block device. All host-offset reads/writes go through
    /// here. `Arc<dyn BlockDevice>` because the trait is `Send + Sync`
    /// and the reader may live behind an `Arc` itself (FFI handles).
    dev: Arc<dyn BlockDevice>,
    /// Current device size — kept in sync with allocations because
    /// allocations grow the file at the tail.
    dev_size: Mutex<u64>,
    /// Decoded header (the slot with the higher sequence_number).
    header: Mutex<Header>,
    /// Header slot the active header was read from. The next header
    /// rewrite goes into the *other* slot per the spec's two-slot
    /// rotation.
    active_header_slot: Mutex<HeaderSlot>,

    virtual_size: u64,
    block_size: u32,
    sector_size: u32,
    chunk_ratio: u64,

    /// Where the BAT region lives on disk.
    bat_region_off: u64,
    #[allow(dead_code)]
    bat_region_len: u32,

    /// In-memory BAT — cached at open. Mutex-wrapped so writers can
    /// publish allocations atomically.
    bat: Mutex<Vec<BatEntry>>,
    has_parent: bool,
    writable: bool,
}

#[derive(Debug, Clone, Copy)]
enum HeaderSlot {
    One,
    Two,
}

impl HeaderSlot {
    fn offset(self) -> u64 {
        match self {
            HeaderSlot::One => HEADER1_OFFSET,
            HeaderSlot::Two => HEADER2_OFFSET,
        }
    }
    fn other(self) -> Self {
        match self {
            HeaderSlot::One => HeaderSlot::Two,
            HeaderSlot::Two => HeaderSlot::One,
        }
    }
}

/// Smallest payload block size the VHDX specification permits.
const MIN_BLOCK_SIZE: u32 = 1024 * 1024;

/// Largest payload block size the VHDX specification permits.
const MAX_BLOCK_SIZE: u32 = 256 * 1024 * 1024;

/// The message for a BAT entry in a reserved payload state.
///
/// `Error::Unsupported` carries `&'static str`, and this used to be
/// satisfied with `Box::leak(format!(...))` — which leaks a small
/// allocation every time, on a path an attacker reaches by writing a
/// reserved state into a BAT entry.
///
/// No allocation is needed. States 0, 1, 2, 3, 6 and 7 are all defined,
/// so `Reserved` can only ever hold 4 or 5; the set is closed and the
/// strings can be static. The fallback exists because `Reserved` is a
/// `u8` and nothing in the type stops a future decoder change widening
/// what reaches it.
fn reserved_state_message(v: u8) -> &'static str {
    match v {
        4 => "BAT entry reserved state 4",
        5 => "BAT entry reserved state 5",
        _ => "BAT entry in a reserved payload state",
    }
}

impl VhdxReader {
    /// Open `path` read-only. The underlying file is wrapped in a
    /// best-effort `FileDevice` — the host file is opened RW when
    /// possible so a dirty image can be log-replayed in place; if the
    /// host file is locked or read-only the open succeeds only when
    /// the log is empty.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let p = path.as_ref();
        let dev = FileDevice::open_best_effort(p).map_err(fs_core_to_vhdx_error)?;
        let writable = dev.is_writable();
        Self::open_inner(Arc::new(dev), false, writable)
    }

    /// Open `path` read-write.
    pub fn open_rw<P: AsRef<Path>>(path: P) -> Result<Self> {
        let p = path.as_ref();
        let dev = FileDevice::open_rw(p).map_err(fs_core_to_vhdx_error)?;
        Self::open_inner(Arc::new(dev), true, true)
    }

    /// Open read-only on top of an arbitrary `BlockDevice`.
    pub fn open_on_device(dev: Arc<dyn BlockDevice>) -> Result<Self> {
        let writable = dev.is_writable();
        Self::open_inner(dev, false, writable)
    }

    /// Open read-write on top of an arbitrary `BlockDevice`. The device
    /// must report `is_writable()`; otherwise the call returns
    /// `Error::ReadOnly`.
    pub fn open_rw_on_device(dev: Arc<dyn BlockDevice>) -> Result<Self> {
        if !dev.is_writable() {
            return Err(Error::ReadOnly);
        }
        Self::open_inner(dev, true, true)
    }

    /// Internal constructor. `writable` controls whether `write_at`
    /// will be allowed; `replay_capable` says whether the underlying
    /// device can absorb log replay (true when the device is RW even
    /// if the reader was opened RO).
    fn open_inner(dev: Arc<dyn BlockDevice>, writable: bool, replay_capable: bool) -> Result<Self> {
        let dev_size = dev.size_bytes();

        // 1. File identifier.
        let mut sig = [0u8; 8];
        dev.read_at(FILE_IDENTIFIER_OFFSET, &mut sig)
            .map_err(fs_core_to_vhdx_error)?;
        if &sig != FILE_IDENTIFIER_SIGNATURE {
            return Err(Error::NotVhdx);
        }

        // 2. Header — try both slots, pick the one with the higher
        //    sequence_number that passes CRC.
        let (header, active_slot) = pick_header(&dev, dev_size)?;

        // 3. Log replay (before we read region/metadata/BAT — those
        //    bytes might be stale). Only attempted when log_offset and
        //    log_length are non-zero AND the log_guid is non-zero.
        if !is_zero_guid(&header.log_guid) && header.log_length > 0 && header.log_offset > 0 {
            if !replay_capable {
                return Err(Error::ReadOnly);
            }
            let mut log_bytes = vec![0u8; header.log_length as usize];
            dev.read_at(header.log_offset, &mut log_bytes)
                .map_err(fs_core_to_vhdx_error)?;
            let chain = collect_replay_chain(&log_bytes, &header.log_guid);
            if !chain.is_empty() {
                apply_chain(&dev, &chain)?;
                // Mark the log as replayed by zeroing the active log
                // chain region and bumping the header.log_guid to a
                // fresh value. Per spec we move the active header to
                // the other slot with a new sequence_number.
                zero_log_region(&dev, header.log_offset, header.log_length)?;
                rewrite_header_clear_log(&dev, &header, active_slot)?;
            }
        }

        // 4. Region table.
        let region_table = pick_region_table(&dev, dev_size)?;

        // 5. Metadata.
        let metadata_entry = region_table
            .find(&guids::METADATA)
            .ok_or(Error::Corrupt("metadata region missing"))?;
        let mut metadata_bytes = vec![0u8; metadata_entry.length as usize];
        dev.read_at(metadata_entry.file_offset, &mut metadata_bytes)
            .map_err(fs_core_to_vhdx_error)?;
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
        if file_params.block_size < MIN_BLOCK_SIZE || file_params.block_size > MAX_BLOCK_SIZE {
            return Err(Error::Corrupt("block_size outside [1 MiB, 256 MiB]"));
        }
        if !sector_size.is_power_of_two() {
            return Err(Error::Corrupt("sector_size not a power of two"));
        }

        let chunk_ratio = compute_chunk_ratio(file_params.block_size, sector_size);
        if chunk_ratio == 0 {
            return Err(Error::Corrupt("chunk_ratio = 0"));
        }

        // 6. BAT.
        let bat_region = region_table
            .find(&guids::BAT)
            .ok_or(Error::Corrupt("BAT region missing"))?;
        let mut bat_bytes = vec![0u8; bat_region.length as usize];
        dev.read_at(bat_region.file_offset, &mut bat_bytes)
            .map_err(fs_core_to_vhdx_error)?;
        let mut bat = Vec::with_capacity(bat_bytes.len() / 8);
        for chunk in bat_bytes.chunks_exact(8) {
            let raw = u64::from_le_bytes(chunk.try_into().unwrap());
            bat.push(BatEntry::from_u64(raw));
        }

        Ok(Self {
            dev,
            dev_size: Mutex::new(dev_size),
            header: Mutex::new(header),
            active_header_slot: Mutex::new(active_slot),
            virtual_size,
            block_size: file_params.block_size,
            sector_size,
            chunk_ratio,
            bat_region_off: bat_region.file_offset,
            bat_region_len: bat_region.length,
            bat: Mutex::new(bat),
            has_parent: file_params.has_parent(),
            writable,
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

    pub fn is_writable(&self) -> bool {
        self.writable
    }

    fn dev_read(&self, off: u64, buf: &mut [u8]) -> Result<()> {
        self.dev.read_at(off, buf).map_err(fs_core_to_vhdx_error)
    }

    fn dev_write(&self, off: u64, buf: &[u8]) -> Result<()> {
        self.dev.write_at(off, buf).map_err(fs_core_to_vhdx_error)
    }

    fn dev_flush(&self) -> Result<()> {
        self.dev.flush().map_err(fs_core_to_vhdx_error)
    }

    /// The end offset of a transfer, or an error saying why it cannot
    /// happen. `None` means "nothing to do" — a zero-length transfer.
    ///
    /// `read_at` and `write_at` opened with the same fifteen lines, and
    /// the only real difference was the sentence in the differencing
    /// error. That sentence is the argument; everything else is one
    /// definition now, so the two entry points cannot drift on which
    /// offsets they consider in range.
    fn transfer_end(
        &self,
        offset: u64,
        len: u64,
        no_parent_support: &'static str,
    ) -> Result<Option<u64>> {
        if len == 0 {
            return Ok(None);
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
            return Err(Error::Unsupported(no_parent_support));
        }
        Ok(Some(end))
    }

    /// The BAT entry for a block index, or `Corrupt` if the index is
    /// past the table.
    ///
    /// Scoped so the lock is released before the caller touches the
    /// device: holding the BAT mutex across an I/O would serialise
    /// every reader on the slowest one, and `write_at` would deadlock
    /// against `allocate_block_for`, which takes it again.
    fn bat_entry_at(&self, bat_idx: usize) -> Result<BatEntry> {
        let bat = self.bat.lock().unwrap();
        bat.get(bat_idx)
            .copied()
            .ok_or(Error::Corrupt("BAT index out of range"))
    }

    /// Split `offset..end` into the pieces that fall in one block each.
    ///
    /// Returns, per piece: the byte offset within the block, the BAT
    /// index that block lives at, and the piece's position and length
    /// within the caller's buffer. The first piece may start mid-block
    /// and the last may end mid-block; the ones between are whole
    /// blocks.
    ///
    /// This is the arithmetic both `read_at` and `write_at` were doing
    /// inline. Getting it wrong silently mis-addresses data rather than
    /// failing, which is the kind of bug two copies can disagree on for
    /// a long time without anyone noticing.
    fn block_chunks(&self, offset: u64, end: u64) -> Result<Vec<BlockChunk>> {
        let block_size = self.block_size as u64;
        let block_mask = block_size - 1;
        let mut chunks = Vec::new();
        let mut cursor = offset;
        let mut buf_pos = 0usize;
        while cursor < end {
            let in_block = cursor & block_mask;
            let virt_block_idx = cursor / block_size;
            let chunk_len = std::cmp::min(block_size - in_block, end - cursor) as usize;
            chunks.push(BlockChunk {
                in_block,
                bat_idx: data_bat_index(virt_block_idx, self.chunk_ratio) as usize,
                buf_pos,
                chunk_len,
            });
            cursor += chunk_len as u64;
            buf_pos += chunk_len;
        }
        Ok(chunks)
    }

    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let Some(end) = self.transfer_end(
            offset,
            buf.len() as u64,
            "VHDX with parent (differencing) — chain walking not implemented",
        )?
        else {
            return Ok(());
        };

        for chunk in self.block_chunks(offset, end)? {
            let BlockChunk {
                in_block,
                bat_idx,
                buf_pos,
                chunk_len,
            } = chunk;
            let dst = &mut buf[buf_pos..buf_pos + chunk_len];
            let entry = self.bat_entry_at(bat_idx)?;

            // Every variant is named. The previous `s if s.zero_fill()`
            // arm plus `_ => unreachable!()` hid exhaustiveness from the
            // compiler, so adding a PayloadState variant would have
            // become a runtime panic on attacker-supplied bytes rather
            // than a build failure.
            match entry.state {
                PayloadState::FullyPresent => {
                    let host_off = entry.file_offset + in_block;
                    self.dev_read(host_off, dst)?;
                }
                PayloadState::NotPresent
                | PayloadState::Undefined
                | PayloadState::Zero
                | PayloadState::Unmapped => {
                    dst.fill(0);
                }
                PayloadState::PartiallyPresent => {
                    return Err(Error::Unsupported(
                        "PartiallyPresent block (sector-bitmap walking not implemented)",
                    ));
                }
                PayloadState::Reserved(v) => {
                    return Err(Error::Unsupported(reserved_state_message(v)));
                }
            }
        }
        Ok(())
    }

    /// Write `buf` at virtual `offset`. Behaviour by BAT state:
    ///
    /// - `FullyPresent`: direct write through to the existing host
    ///   block.
    /// - `NotPresent` / `Undefined` / `Zero` / `Unmapped`: allocate a
    ///   fresh host block at the device tail, zero-init it, write the
    ///   user payload at the in-block offset, journal the BAT mutation
    ///   through the log, then publish the BAT entry on disk.
    /// - `PartiallyPresent`: same allocate-and-write path as
    ///   unallocated — we promote to FullyPresent rather than honour
    ///   the sector bitmap. The original sector-bitmap entry is left
    ///   alone; subsequent reads come from the new fully-present block.
    ///
    /// Crash-safety: every BAT mutation is committed to the log first
    /// (with `dev.flush()` after the log write), then the BAT entry is
    /// rewritten in place, then the header is bumped to a fresh
    /// sequence_number with `file_write_guid` invalidated. A crash
    /// after the log commit but before the header bump is recovered on
    /// the next open by replaying the log; a crash before the log
    /// commit loses only the in-flight write.
    pub fn write_at(&self, offset: u64, buf: &[u8]) -> Result<()> {
        if !self.writable {
            return Err(Error::ReadOnly);
        }
        let Some(end) = self.transfer_end(
            offset,
            buf.len() as u64,
            "VHDX with parent (differencing) — write not implemented",
        )?
        else {
            return Ok(());
        };

        for chunk in self.block_chunks(offset, end)? {
            let BlockChunk {
                in_block,
                bat_idx,
                buf_pos,
                chunk_len,
            } = chunk;
            let src = &buf[buf_pos..buf_pos + chunk_len];
            let entry = self.bat_entry_at(bat_idx)?;

            let host_block_off = match entry.state {
                PayloadState::FullyPresent => entry.file_offset,

                // Nothing on disk yet, or defined to read as zero: a
                // fresh zero-initialised block is exactly right.
                PayloadState::NotPresent
                | PayloadState::Undefined
                | PayloadState::Zero
                | PayloadState::Unmapped => self.allocate_block_for(bat_idx)?,

                // NOT allocatable. A partially-present block has payload
                // on disk whose valid sectors are described by a bitmap
                // this crate does not walk — `read_at` refuses it for
                // that reason. Allocating a fresh block here would
                // publish a zeroed one over it and DISCARD those
                // sectors, which is worse than refusing: the reader
                // admits it cannot interpret the block, so the writer
                // must not overwrite it either.
                PayloadState::PartiallyPresent => {
                    return Err(Error::Unsupported(
                        "write to a PartiallyPresent block (sector-bitmap walking not implemented)",
                    ));
                }

                PayloadState::Reserved(v) => {
                    return Err(Error::Unsupported(reserved_state_message(v)));
                }
            };

            // Direct payload write into the (possibly newly-allocated,
            // zero-initialised) host block.
            self.dev_write(host_block_off + in_block, src)?;
        }

        self.dev_flush()?;
        Ok(())
    }

    pub fn flush(&self) -> Result<()> {
        if !self.writable {
            return Ok(());
        }
        self.dev_flush()
    }

    /// Allocate a fresh host block for `bat_idx` at the device tail,
    /// zero-init it, journal the BAT mutation through the log, then
    /// publish the BAT entry. Returns the host offset of the new block.
    fn allocate_block_for(&self, bat_idx: usize) -> Result<u64> {
        let block_size = self.block_size as u64;

        // Pick a block-aligned offset at the device tail.
        let mut sz = self.dev_size.lock().unwrap();
        let aligned = (*sz + (block_size - 1)) & !(block_size - 1);
        let new_block_off = aligned;
        let new_dev_size = new_block_off + block_size;
        *sz = new_dev_size;
        drop(sz);

        // Zero-init the block on disk so reads from sectors the caller
        // didn't touch return zeros (matches FullyPresent semantics).
        let zeros = vec![0u8; block_size as usize];
        self.dev_write(new_block_off, &zeros)?;
        self.dev_flush()?;

        // Build the new BAT entry (FullyPresent state = 6 at
        // new_block_off). The on-disk encoding stores file_offset_in_mb
        // in the top 44 bits and the state in the bottom 3.
        let new_raw = (new_block_off & !((1u64 << 20) - 1)) | 6u64;
        debug_assert_eq!(new_raw & 0x7, 6);

        // Compute on-disk position of this BAT entry and the
        // surrounding 4 KiB sector that contains it.
        let entry_off_in_region = (bat_idx as u64) * BAT_ENTRY_BYTES;
        let entry_off_on_disk = self.bat_region_off + entry_off_in_region;
        let sector_off = entry_off_on_disk & !((LOG_SECTOR_SIZE as u64) - 1);
        let entry_off_in_sector = (entry_off_on_disk - sector_off) as usize;

        // Read the current sector, splice the new BAT entry, write
        // through the log first, then publish in place.
        let mut sector = vec![0u8; LOG_SECTOR_SIZE];
        self.dev_read(sector_off, &mut sector)?;
        sector[entry_off_in_sector..entry_off_in_sector + 8]
            .copy_from_slice(&new_raw.to_le_bytes());

        self.journal_sector_write(sector_off, &sector)?;

        // Now publish the BAT entry on disk.
        self.dev_write(sector_off, &sector)?;
        self.dev_flush()?;

        // Update in-memory BAT.
        {
            let mut bat = self.bat.lock().unwrap();
            bat[bat_idx] = BatEntry::from_u64(new_raw);
        }

        Ok(new_block_off)
    }

    /// Write a 4 KiB sector image through the log: bump the header's
    /// sequence_number into the *other* header slot with a fresh
    /// log_guid, encode a one-descriptor log entry, splice it into the
    /// log region, flush.
    ///
    /// # It does not always journal, and returns `Ok` when it does not
    ///
    /// Two conditions make it fall back to writing without a log: a log
    /// region too small to hold one entry (absent, or under 8 KiB), and
    /// an entry larger than the whole region. Both return `Ok(())`,
    /// which a caller cannot distinguish from a journalled write.
    ///
    /// So the crash guarantee is conditional: WHERE THIS JOURNALS, a
    /// crash before the in-place write completes is recovered by replay
    /// on next open. Where it skips, the write is not crash-safe and
    /// nothing says so at the call site.
    ///
    /// The fallback is deliberate — refusing the write outright would
    /// make images with a small log unwritable, and most have at least
    /// 1 MiB. Whether silence is the right way to report it is a design
    /// question this comment does not settle; it only stops the
    /// contract claiming more than the code delivers.
    ///
    /// # What this depends on, in another module
    ///
    /// This splices an entry **wherever it finds room** in the log
    /// region, and records nowhere that it did so. That is only safe
    /// because [`crate::log::collect_replay_chain`] probes *every* slot
    /// and never stops early — it says so, and this depends on it.
    ///
    /// If replay ever gained an early exit (stopping at the first empty
    /// slot, say, which is a natural-looking optimisation for a
    /// circular buffer), entries this writer placed after a gap would
    /// stop being replayed, and the crash guarantee above would quietly
    /// become false. The invariant lives in two modules; this half is
    /// the one that would not notice it breaking.
    fn journal_sector_write(&self, file_offset: u64, sector: &[u8]) -> Result<()> {
        debug_assert_eq!(sector.len(), LOG_SECTOR_SIZE);

        let mut header = self.header.lock().unwrap();
        let mut active_slot = self.active_header_slot.lock().unwrap();

        // If the log region is too small to hold one entry, fall back
        // to writing without a log. Better than refusing the write —
        // and most VHDX images have at least 1 MiB of log.
        if header.log_length == 0 || header.log_offset == 0 || header.log_length < 8192 {
            return Ok(());
        }

        // Pick a fresh log_guid + sequence_number for this entry. We
        // bump sequence_number monotonically and derive the new
        // log_guid from it so both slots stay distinguishable.
        let new_seq = header.sequence_number.wrapping_add(1);
        let mut new_log_guid = header.log_guid;
        stir_sequence_into_guid(&mut new_log_guid, new_seq);

        // Encode a single-descriptor entry covering this sector.
        let dev_size = *self.dev_size.lock().unwrap();
        let entry = encode_entry(
            new_seq,
            0,
            &new_log_guid,
            dev_size,
            dev_size,
            &[PendingWrite {
                file_offset,
                sector: sector.to_vec(),
            }],
        );
        if entry.len() as u64 > header.log_length as u64 {
            // Entry doesn't fit — skip journaling.
            return Ok(());
        }
        // Clear the whole log region first, then splice at its start.
        //
        // The clear is what makes this correct: it guarantees the entry
        // written below is the only one in the region, so it is the
        // first entry of its own chain — which is exactly what the
        // `tail: 0` passed to `encode_entry` above claims, and what
        // `collect_replay_chain` anchors its walk on. Position itself
        // is a write-amplification question; the invariant replay
        // depends on is that a chain's first entry really is where its
        // `tail` says it is.
        zero_log_region(&self.dev, header.log_offset, header.log_length)?;
        self.dev_write(header.log_offset, &entry)?;
        self.dev_flush()?;

        // Bump the header into the *other* slot. Update sequence_number,
        // log_guid, file_write_guid (per spec, invalidates other readers'
        // caches), data_write_guid.
        let next_slot = active_slot.other();
        let mut new_header = header.clone();
        new_header.sequence_number = new_seq;
        new_header.log_guid = new_log_guid;
        stir_sequence_into_guid(&mut new_header.file_write_guid, new_seq);
        stir_sequence_into_guid(&mut new_header.data_write_guid, new_seq);

        let header_bytes = encode_header(&new_header);
        self.dev_write(next_slot.offset(), &header_bytes)?;
        self.dev_flush()?;

        *header = new_header;
        *active_slot = next_slot;
        Ok(())
    }
}

/// Derive a fresh GUID by stirring `seq` into an existing one.
///
/// # What this is, and what it is not
///
/// It is **not** any UUID algorithm. VHDX asks only that
/// `file_write_guid`, `data_write_guid` and `log_guid` **change** when
/// the file is opened for writing, so that a reader which saw an older
/// value knows its cached state is stale. It does not ask for
/// randomness, uniqueness across machines, or an RFC 4122 shape — a
/// value that differs from the last one is the whole requirement.
///
/// So: XOR the little-endian sequence number over bytes 8..16.
///
/// - **Byte 8** because the sequence number already lives at offset
///   8..16 of the header, so the two halves stay aligned in a hex dump
///   — the same eight bytes in both places.
/// - **XOR** because it is reversible and cannot collapse: a distinct
///   `seq` always yields a distinct result from the same input, so two
///   consecutive writes cannot produce the same GUID.
/// - **Not the top eight bytes**, which carry whatever the creating
///   tool put there; leaving them alone keeps the value recognisable as
///   descended from the original.
///
/// # The all-zero guard
///
/// All-zero is the spec's sentinel for *"no write in progress"*, so a
/// derived GUID must never land on it — a `log_guid` of zero would tell
/// the next opener there is no log to replay. It can only happen if the
/// input was itself the XOR of `seq`, which is vanishingly unlikely and
/// still worth refusing rather than hoping.
/// One piece of a transfer that falls entirely inside a single block.
///
/// A transfer of any size crosses block boundaries; each crossing needs
/// its own BAT lookup, because the blocks either side may be in
/// different states and at unrelated host offsets. This names the four
/// numbers that describe a piece so the walk reads as a walk instead of
/// as arithmetic.
struct BlockChunk {
    /// Byte offset of this piece within its block.
    in_block: u64,
    /// Index of the BAT entry describing that block.
    bat_idx: usize,
    /// Where this piece starts in the caller's buffer.
    buf_pos: usize,
    /// How many bytes this piece covers.
    chunk_len: usize,
}

fn stir_sequence_into_guid(guid: &mut [u8; 16], seq: u64) {
    for (i, b) in seq.to_le_bytes().iter().enumerate() {
        guid[8 + i] ^= *b;
    }
    if guid.iter().all(|b| *b == 0) {
        guid[0] = 1;
    }
}

/// Read `len` bytes at `off` and parse them, or `None`.
///
/// The four probe blocks in `pick_header` and `pick_region_table` were
/// the same six lines each: bounds-check the offset against the device,
/// read, parse, keep it only if it parsed.
///
/// A parse failure is deliberately **not** an error. Both structures
/// are written in two copies precisely so one can be unreadable — a
/// crash between the two writes leaves exactly that — and the caller's
/// job is to prefer whichever survived. An I/O failure *is* an error,
/// because it says nothing about the copy's contents.
fn probe_at<T>(
    dev: &Arc<dyn BlockDevice>,
    dev_size: u64,
    off: u64,
    len: usize,
    parse: impl Fn(&[u8]) -> Result<T>,
) -> Result<Option<T>> {
    if dev_size < off + len as u64 {
        return Ok(None);
    }
    let mut buf = vec![0u8; len];
    dev.read_at(off, &mut buf).map_err(fs_core_to_vhdx_error)?;
    Ok(parse(&buf).ok())
}

fn pick_header(dev: &Arc<dyn BlockDevice>, dev_size: u64) -> Result<(Header, HeaderSlot)> {
    let h1 = probe_at(dev, dev_size, HEADER1_OFFSET, HEADER_SIZE, Header::parse)?;
    let h2 = probe_at(dev, dev_size, HEADER2_OFFSET, HEADER_SIZE, Header::parse)?;
    match (h1, h2) {
        (Some(a), Some(b)) => {
            // On a tie, slot 1 wins. The two are written alternately
            // and the sequence number is bumped on every write, so a
            // tie means both copies describe the same state — either is
            // correct, and picking one deterministically keeps the
            // choice reproducible.
            if a.sequence_number >= b.sequence_number {
                Ok((a, HeaderSlot::One))
            } else {
                Ok((b, HeaderSlot::Two))
            }
        }
        (Some(a), None) => Ok((a, HeaderSlot::One)),
        (None, Some(b)) => Ok((b, HeaderSlot::Two)),
        (None, None) => Err(Error::NoValidHeader),
    }
}

fn pick_region_table(dev: &Arc<dyn BlockDevice>, dev_size: u64) -> Result<RegionTable> {
    for off in [REGION_TABLE1_OFFSET, REGION_TABLE2_OFFSET] {
        if let Some(t) = probe_at(dev, dev_size, off, REGION_TABLE_SIZE, RegionTable::parse)? {
            return Ok(t);
        }
    }
    Err(Error::NoValidRegionTable)
}

fn is_zero_guid(g: &[u8; 16]) -> bool {
    g.iter().all(|b| *b == 0)
}

/// Largest single write used to zero a range.
///
/// A zero descriptor names a length taken straight off disk, and a log
/// region can be megabytes; writing it in one call would allocate a
/// buffer of whatever the file asked for. One megabyte is small enough
/// that a pathological length costs time rather than memory, and large
/// enough that the loop is not the cost.
pub(crate) const ZERO_CHUNK: usize = 1024 * 1024;

/// Write `len` zero bytes at `off`, in [`ZERO_CHUNK`]-sized writes.
///
/// The buffer is allocated once and re-sliced, rather than per chunk.
///
/// Errors come back as `fs_core`'s, because the two callers map them
/// differently — the reader to `Error::Io`, the log replayer to
/// `Error::LogReplay` with its own context. That difference is the only
/// thing the two copies of this loop actually differed in, and it
/// belongs to the caller.
pub(crate) fn write_zeros(
    dev: &Arc<dyn BlockDevice>,
    off: u64,
    len: u64,
) -> std::result::Result<(), fs_core::Error> {
    let zeros = vec![0u8; ZERO_CHUNK];
    let mut remaining = len;
    let mut cur = off;
    while remaining > 0 {
        let n = std::cmp::min(remaining, ZERO_CHUNK as u64) as usize;
        dev.write_at(cur, &zeros[..n])?;
        cur += n as u64;
        remaining -= n as u64;
    }
    Ok(())
}

fn zero_log_region(dev: &Arc<dyn BlockDevice>, off: u64, len: u32) -> Result<()> {
    write_zeros(dev, off, len as u64).map_err(fs_core_to_vhdx_error)?;
    dev.flush().map_err(fs_core_to_vhdx_error)?;
    Ok(())
}

/// Rewrite the active header slot with `log_guid` zeroed (sentinel for
/// "log is empty") and a bumped sequence_number. Used after a successful
/// log replay to mark the chain as consumed.
fn rewrite_header_clear_log(
    dev: &Arc<dyn BlockDevice>,
    header: &Header,
    active_slot: HeaderSlot,
) -> Result<()> {
    let mut new_header = header.clone();
    new_header.sequence_number = header.sequence_number.wrapping_add(1);
    new_header.log_guid = [0u8; 16];

    let bytes = encode_header(&new_header);
    // Write into the *other* slot so a crash mid-write doesn't lose
    // the previous good header.
    let other = active_slot.other();
    dev.write_at(other.offset(), &bytes)
        .map_err(fs_core_to_vhdx_error)?;
    dev.flush().map_err(fs_core_to_vhdx_error)?;
    Ok(())
}

/// Encode a `Header` into the 4 KiB on-disk representation with a
/// fresh CRC-32C in the checksum field.
pub fn encode_header(h: &Header) -> Vec<u8> {
    let mut buf = vec![0u8; HEADER_SIZE];
    buf[0..4].copy_from_slice(b"head");
    buf[8..16].copy_from_slice(&h.sequence_number.to_le_bytes());
    buf[16..32].copy_from_slice(&h.file_write_guid);
    buf[32..48].copy_from_slice(&h.data_write_guid);
    buf[48..64].copy_from_slice(&h.log_guid);
    buf[64..66].copy_from_slice(&0u16.to_le_bytes()); // log_version
    buf[66..68].copy_from_slice(&h.version.to_le_bytes());
    buf[68..72].copy_from_slice(&h.log_length.to_le_bytes());
    buf[72..80].copy_from_slice(&h.log_offset.to_le_bytes());
    let crc = crate::header::compute_crc(&buf);
    buf[4..8].copy_from_slice(&crc.to_le_bytes());
    buf
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

impl fs_core::BlockDevice for VhdxReader {
    fn write_at(&self, offset: u64, buf: &[u8]) -> fs_core::Result<()> {
        VhdxReader::write_at(self, offset, buf).map_err(vhdx_to_fs_core_error)
    }
    fn flush(&self) -> fs_core::Result<()> {
        VhdxReader::flush(self).map_err(vhdx_to_fs_core_error)
    }
    fn is_writable(&self) -> bool {
        VhdxReader::is_writable(self)
    }
}

fn vhdx_to_fs_core_error(e: Error) -> fs_core::Error {
    match e {
        Error::Io(io) => fs_core::Error::Io(io),
        Error::OutOfBounds { offset, len, size } => {
            fs_core::Error::OutOfBounds { offset, len, size }
        }
        Error::ReadOnly => fs_core::Error::ReadOnly,
        other => fs_core::Error::Custom(other.to_string()),
    }
}

fn fs_core_to_vhdx_error(e: fs_core::Error) -> Error {
    match e {
        fs_core::Error::Io(io) => Error::Io(io),
        fs_core::Error::ShortRead { offset, want, got } => Error::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!("short read at {offset}: wanted {want} got {got}"),
        )),
        fs_core::Error::ReadOnly => Error::ReadOnly,
        fs_core::Error::OutOfBounds { offset, len, size } => {
            Error::OutOfBounds { offset, len, size }
        }
        fs_core::Error::Custom(s) => Error::LogReplay(s),
    }
}

#[cfg(test)]
mod shared_helper_tests {
    use super::*;

    /// The sequence number lands on bytes 8..16, and nowhere else.
    ///
    /// Not any UUID algorithm — VHDX asks only that the value *change*
    /// when the file is opened for writing, so a reader with a cached
    /// copy knows it is stale. These pin which bytes move and which do
    /// not, because "the top eight stay put" is what keeps the value
    /// recognisable as descended from the original.
    #[test]
    fn stirring_touches_only_the_bottom_eight_bytes() {
        let mut g = [0xAAu8; 16];
        stir_sequence_into_guid(&mut g, 0x0102_0304_0506_0708);
        assert_eq!(&g[..8], &[0xAA; 8], "the top eight are untouched");
        assert_eq!(
            &g[8..],
            &[0xA2, 0xAD, 0xAC, 0xAF, 0xAE, 0xA9, 0xA8, 0xAB],
            "little-endian sequence, XORed over the bottom eight"
        );
    }

    /// A distinct sequence always gives a distinct GUID.
    ///
    /// This is why XOR: it is reversible, so two consecutive writes
    /// cannot land on the same value from the same input — which is the
    /// entire requirement the format places on these fields.
    #[test]
    fn consecutive_sequences_cannot_collide() {
        let base = [0x11u8; 16];
        let mut seen = std::collections::HashSet::new();
        for seq in 0..64u64 {
            let mut g = base;
            stir_sequence_into_guid(&mut g, seq);
            assert!(seen.insert(g), "sequence {seq} repeated an earlier GUID");
        }
    }

    /// All-zero is the spec's "no write in progress" sentinel, so a
    /// derived GUID must never land on it — a zero `log_guid` tells the
    /// next opener there is no log to replay.
    #[test]
    fn the_result_is_never_the_all_zero_sentinel() {
        let seq = 0x0102_0304_0506_0708u64;
        // The one input that would XOR to zero.
        let mut g = [0u8; 16];
        g[8..].copy_from_slice(&seq.to_le_bytes());
        stir_sequence_into_guid(&mut g, seq);
        assert!(g.iter().any(|b| *b != 0), "must not be the zero sentinel");
    }

    /// `write_zeros` writes the whole range, in bounded pieces.
    #[test]
    fn write_zeros_covers_the_range_it_is_given() {
        struct Mem(std::sync::Mutex<Vec<u8>>);
        impl fs_core::BlockRead for Mem {
            fn read_at(&self, off: u64, buf: &mut [u8]) -> fs_core::Result<()> {
                let b = self.0.lock().unwrap();
                let s = off as usize;
                buf.copy_from_slice(&b[s..s + buf.len()]);
                Ok(())
            }
            fn size_bytes(&self) -> u64 {
                self.0.lock().unwrap().len() as u64
            }
        }
        impl BlockDevice for Mem {
            fn write_at(&self, off: u64, buf: &[u8]) -> fs_core::Result<()> {
                let mut b = self.0.lock().unwrap();
                let s = off as usize;
                b[s..s + buf.len()].copy_from_slice(buf);
                Ok(())
            }
            fn is_writable(&self) -> bool {
                true
            }
        }
        let dev: Arc<dyn BlockDevice> = Arc::new(Mem(std::sync::Mutex::new(vec![
            0xAAu8;
            ZERO_CHUNK * 2
                + 64
        ])));
        // Dirty the range first so zeroing is observable.
        dev.write_at(0, &[0xFFu8; 128]).unwrap();
        write_zeros(&dev, 0, (ZERO_CHUNK + 32) as u64).expect("zero");
        let mut buf = vec![0xAAu8; 128];
        dev.read_at(0, &mut buf).unwrap();
        assert!(buf.iter().all(|b| *b == 0), "the head was zeroed");
        let mut tail = vec![0xAAu8; 8];
        dev.read_at((ZERO_CHUNK + 24) as u64, &mut tail).unwrap();
        assert!(
            tail.iter().all(|b| *b == 0),
            "past one chunk was zeroed too"
        );
    }
}
