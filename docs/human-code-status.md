# Human-code findings — status

Tracks every **High** and **Medium** finding from
[`human-code-report-2026-08-28.md`](human-code-report-2026-08-28.md).
The report predates the work and records everything as open; this is the current
position. Updated 2026-08-29.

**26 findings** — 6 High, 14 Medium, 6 Low. This covers the 20 High and Medium.

| | High | Medium |
|---|---|---|
| Fixed | 3 | 6 |
| Left for a human decision | 3 | 4 |
| Fixable, not yet done | 0 | 4 |

---

## The one that was a live bug

**M14** was filed as "an unused parameter" — `allocate_block_for(&self, bat_idx,
_old: &BatEntry)`, threaded through and never read. Removing it meant looking at
the call site, which was:

```rust
match entry.state {
    PayloadState::FullyPresent => entry.file_offset,
    _ => self.allocate_block_for(bat_idx, &entry)?,
}
```

That `_` catches **PartiallyPresent**. A partially-present block has payload on
disk whose valid sectors are described by a bitmap this crate does not walk —
which is exactly why `read_at` refuses the state outright. The write path
handed it to the allocator, which published a fresh zeroed block over it,
**discarding every sector the bitmap called valid, and reporting success**.

Now refused, with the same reasoning as the reader: a reader that admits it
cannot interpret the block must not have a writer that overwrites it.
`write_to_a_partially_present_block_is_refused` covers it, and fails against the
old code with `must not be overwritten by a fresh allocation: ()` — the `()`
being the `Ok` it used to return.

The parameter is gone as well, which is what the finding asked for.

---

## High

### H1 — replay's stop condition documented but not implemented — **fixed earlier**

[#24](https://github.com/antimatter-studios/rust-img-vhdx/pull/24) — replay now
stops at the first break in the chain.

### H2 — `parse_log_entry` collapses eleven rejection reasons into `None` — **fixed**

This entry was stale: it was fixed while H1 was being worked on, and never
re-marked. `EntryReject` (`src/log.rs`) now names four outcomes — `Empty`,
`Corrupt`, `ForeignChain`, `Malformed` — and the eleven rejection sites each
pick one. Only two of the twelve uses are `Empty`, which is the point: an empty
slot and a corrupt entry are no longer the same answer, and replay can stop on
one while skipping the other.

### H3 — `journal_sector_write` skips journalling silently — **fixed (the contract)**

Two conditions fall back to writing without a log — a log region under 8 KiB,
and an entry larger than the region — and both `return Ok(())`, which a caller
cannot tell from a journalled write. The doc comment promised crash recovery
unconditionally.

**The comment is now honest**: it states both skips, says the crash guarantee
holds *where this journals*, and notes that the fallback is deliberate because
refusing would make images with a small log unwritable.

**Whether silence is the right report is left to you** — returning a value the
caller can act on is a design change to the write path.

### H4 — `Box::leak` to manufacture a `&'static str` per reserved BAT state — **fixed**

`Error::Unsupported` carries `&'static str`, and the code satisfied that with
`Box::leak(format!(...))` — leaking a small allocation every time, on a path an
attacker reaches by writing a reserved state into a BAT entry.

No allocation is needed. States 0, 1, 2, 3, 6 and 7 are all defined, so
`Reserved` can only hold **4 or 5**; the set is closed and the strings are
static.

### H5 — `open_inner` is a 109-line god function — **needs your decision**

Accurate, and splitting the open path is a restructure of the sequence that
establishes every invariant the rest of the reader assumes.

### H6 — encoder and decoder hardcode the same offsets independently — **needs your decision**

Real and worth fixing; it is also a structural change to how the format is
described, and the right shape (a shared offsets module? a codec type?) is a
design call.

---

## Medium

### M2 — `_ => unreachable!()` hid exhaustiveness — **fixed**

`s if s.zero_fill()` plus a catch-all meant a new `PayloadState` variant became
a **runtime panic on attacker-supplied bytes** instead of a compile error. Every
variant is now named, so the compiler checks it.

### M6 — `data_sector_idx` declared, incremented, discarded — **fixed**

Deleted, along with the `let _ = data_sector_idx;` that existed only to stop
clippy noticing.

### M11 — `2047` unnamed in two modules — **fixed**

`MAX_METADATA_ENTRIES` and `MAX_REGION_ENTRIES`, both spec-fixed.

### M12 — block-size bounds as raw arithmetic — **fixed**

`MIN_BLOCK_SIZE` and `MAX_BLOCK_SIZE`.

### M14 — unused `_old` parameter — **fixed**, see above.

### M1, M9, M13 — duplication in `read_at`/`write_at`, header picking, zero-fill — **fixed**

All three were genuine and mechanical, and all three are now single definitions:

- **M1** — `read_at`/`write_at` shared a fifteen-line prologue and an
  eleven-line block-walk. They are now `transfer_end` (bounds and the
  differencing refusal, whose message is the one real difference and is now an
  argument), `block_chunks` (the virtual→host arithmetic, returning named
  `BlockChunk`s), and `bat_entry_at` (the scoped BAT lookup, with the reason
  the scope matters: `write_at` would deadlock against `allocate_block_for`).
- **M9** — `probe_at` replaces the four probe blocks in `pick_header` and
  `pick_region_table`, and documents why a *parse* failure is deliberately not
  an error there while an *I/O* failure is.
- **M13** — `write_zeros` with a single `ZERO_CHUNK`. It returns
  `fs_core::Error` so each caller still maps to its own variant (`Error::Io`
  against `Error::LogReplay`) — that mapping was the only real difference
  between the two copies.

**Consolidating M1 cost detectability, and that needed a new test.** Breaking
the in-block offset in either old copy failed tests; breaking the one shared
definition failed *fewer*, because every write test read back through the same
reader and the identical error on both sides cancelled out. `synthetic.rs`'s
`a_write_lands_at_its_in_block_offset_in_the_host_file` asserts against the raw
file instead — an oracle the reader has no hand in — which takes the mutation
from 3 failing tests back to 4.

### M4 — `read_uNN_le` copy-pasted nine times across four modules — **fixed**

Nine byte-identical definitions of `read_u16_le` / `read_u32_le` / `read_u64_le`,
private to `header`, `metadata`, `region_table` and `log`.

Nothing was wrong with any of them, and that is worth recording: a helper this
small is not duplicated because somebody misunderstood it, but because declaring
it again is cheaper in the moment than importing it. The cost lands on a reader
who has to check that the ninth copy still says `from_le_bytes` rather than
`from_be_bytes` — a difference that changes every field the module parses and is
one character wide.

`src/endian.rs` holds one of each, and says why there is deliberately **no**
big-endian half: VHDX is little-endian throughout, so a big-endian read here
would be a bug, and a helper for it would make the bug spellable.

**Coverage was already there and the probe proves it**, which is the reason this
is safe rather than merely tidy. Flipping each copy to `from_be_bytes` in turn,
before the change: `header` 2 tests, `metadata` 3, `region_table` 4, `log` 11.
After, flipping the single helper fails **18**.

Three tests on the module itself assert the byte order against literal bytes
rather than against `from_le_bytes`, which would only restate the
implementation.

### M3 — the BAT entry encoder is hand-rolled outside `bat.rs` — **needs your decision**

`bat.rs` owns `BatEntry::from_u64` but has no encoder, so the writer builds the
inverse by hand in another module, and asserts a tautology about it. Adding the
encoder is right; where it lives and whether `BatEntry` grows a constructor is a
design call.

### M5 — a self-contradicting comment about data-sector placement — **needs your decision**

Twelve lines that say the spec promises ordering, then that in practice it
rounds, then that for entries this crate writes it is just `LOG_SECTOR_SIZE`. A
reader cannot tell whether the code is spec-correct in general. **Resolving it
requires knowing which is true**, which means reading the spec rather than the
comment — real work, not a rewording.

### M7 — `tail`, `flushed_file_offset`, `last_file_offset` decoded and unused — **needs your decision**

These are the fields that answer "where does the active chain start" and "how
far has it been flushed". H1 is fixed, so the immediate consequence is gone;
whether to use them or drop them depends on how far replay should go.

### M8 — an undocumented cross-module invariant — **fixed**

Re-checked against what replay does after H1, and it holds: `journal_sector_write`
splices an entry wherever it finds room and records nowhere that it did, which is
only safe because `collect_replay_chain` probes every 4 KiB slot and never stops
early. Both halves now say so, and the writer's half — the one that would not
notice the invariant breaking — names the plausible change that would break it
(an early exit at the first empty slot, which looks like an optimisation for a
circular buffer).

### M10 — the GUID-derivation idiom appears three times, unexplained — **fixed**

One `stir_sequence_into_guid`, with the reasoning recovered and written down:
it is **not** any UUID algorithm, and does not need to be — VHDX asks only that
the value *change* when the file is opened for writing, so a reader holding a
cached copy can tell it is stale. Byte 8 because the sequence number lives at
header offset 8..16, so the two halves line up in a hex dump. XOR because it is
reversible and so cannot collapse two distinct sequence numbers onto one GUID.
The all-zero guard because all-zero is the spec's "no write in progress"
sentinel — a derived GUID must never land on it. Three tests pin exactly those
three properties.

---

## Verification

`cargo test` — 50 unit (up from 43), 7 doc, 13 synthetic and 11 corruption
tests pass. Each new test fails against the previous behaviour, which is what
makes it worth having.
