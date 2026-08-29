# Human-Code Report — am-img-vhdx

**Date:** 2026-08-28
**Scope:** full crate (`src/*.rs`, 2,157 lines; `tests/*.rs`, 1,222 lines)

> **This is analysis only. No code was changed.**
> Phases 0 (Understand), 1 (Scan and Triage) and 3 (Report) were run.
> Phase 2 (dev-loop implementation) was deliberately **not** started —
> the working tree is untouched apart from this file. Nothing here has
> been fixed; every item below is a proposal awaiting your confirmation.

**Counts:** 26 findings — 6 High, 14 Medium, 6 Low. 0 fixed. 26 open.

---

## Baseline (measured, not changed)

| | Value |
|---|---|
| Tests passing | 51 (33 unit + 7 `corruption` + 11 `synthetic`) |
| Tests failing | 0 |
| Feature-gated | 8 more in `tests/qemu_validation.rs` (`--features qemu-validation`, not run) |
| `cargo clippy --all-targets` | clean — 0 warnings |
| Test framework | built-in `cargo test`; integration fixtures hand-write VHDX bytes in `tests/common/mod.rs` |

The crate is in good shape by the usual measures: clippy-clean, every
module has unit tests, the doc comments are unusually thorough. The
findings below are almost entirely about *the log*, where the density of
implicit decisions is much higher than anywhere else in the crate.

---

## The headline: the log's replay decisions are implicit

Replaying a log means answering three questions — **what is valid**, **in
what order**, and **when to stop**. In this crate:

- **What is valid** is answered by an `Option` with eleven `return None`
  sites, so eleven different reasons collapse into one signal (H2).
- **In what order** is answered by a sort, not by the chain links the
  format provides (H1).
- **When to stop** is *not answered at all* — the scan never stops early;
  it reads the whole region and keeps whatever parsed (H1). The three
  header fields that exist to answer it — `tail`, `flushed_file_offset`,
  `last_file_offset` — are parsed, stored, and never read (M7).

The module doc claims otherwise, which is why this is the top finding.

---

## Findings

### High

#### H1 — Replay's stop condition is documented but not implemented
**Files:** `src/log.rs:50-54` (module doc), `src/log.rs:264-294`
(`collect_replay_chain`)
**Category:** comment that lies / implicit decision
**Coverage:** none for the behaviour in question

Both the module doc and the function doc promise a contiguous chain that
terminates at the first break:

```
//! Replay walks entries in increasing-sequence order starting from
//! `header.log_guid`'s active chain. We keep it conservative: we accept
//! a chain of contiguous entries with valid CRCs, matching log_guid,
//! and strictly increasing sequence numbers. Any break ends the chain.
```

```rust
/// Walk the log region, find the longest chain of contiguous valid
/// entries with matching log_guid + strictly increasing sequence
/// numbers, and return them in apply order.
```

The implementation does something materially different:

```rust
while pos + LOG_SECTOR_SIZE <= log_bytes.len() {
    if let Some(e) = parse_log_entry(log_bytes, pos, expected_log_guid) {
        let len = e.header.entry_length as usize;
        entries.push(e);
        pos += len;
    } else {
        pos += LOG_SECTOR_SIZE;   // <-- skip and keep going
    }
}
...
entries.sort_by_key(|e| e.header.sequence_number);
entries.dedup_by_key(|e| e.header.sequence_number);
entries
```

There is no contiguity test and nothing ends the chain. An entry that
fails CRC in the middle of the region is skipped, and every later entry
is still collected and applied. "Contiguous" is only true of the
*sequence numbers of what happened to survive*, not of the chain: a log
holding sequences 5, 6, 8 replays all three, with 7's writes missing.
The whole point of a journal is that a partially-written batch is either
fully applied or not applied at all; skipping the torn entry and applying
its successors is the one outcome the format is designed to prevent.

Two smaller decisions ride along, both silent:

- `dedup_by_key` removes *consecutive* duplicates only. After the sort
  that is sufficient, but which of two same-sequence entries survives is
  decided by `sort_by_key`'s stability (the lower log offset wins) — a
  tie-break nobody wrote down.
- Stale entries left in the region from an earlier chain that shares the
  same `log_guid` are indistinguishable from live ones and get replayed.

**Proposal:** anchor the walk on `tail`, walk forward entry-by-entry,
stop at the first slot that fails to parse or whose sequence number is
not exactly `previous + 1`, and return the prefix. Whatever policy you
choose, the doc and the code should agree. Needs new tests first (see
"What to fix first").

---

#### H2 — `parse_log_entry` collapses eleven rejection reasons into `None`
**Files:** `src/log.rs:132-258`
**Category:** god function + implicit decision
**Coverage:** happy path only (`encode_then_parse_roundtrip`)

128 lines, one `Option` return, and eleven distinct `return None` sites:

| line | reason |
|---|---|
| 134 | slot runs past the end of the region |
| 138 | no `"loge"` signature — probably just an empty slot |
| 145 | `entry_length` unusable (too small / unaligned / overruns) |
| 150 | CRC mismatch — a torn or corrupt entry |
| 162 | `log_guid` belongs to a different chain |
| 168 | descriptor table overruns the entry |
| 190 | descriptor sequence disagrees with the entry |
| 206 | data sector runs past the entry |
| 211 | data sector lacks the `"data"` signature |
| 217 | data-sector sequence halves disagree |
| 240 | unrecognised descriptor signature |

These are not the same event. Line 138 means "nothing here, keep
scanning." Line 150 means "something was here and it is damaged."
Line 162 means "this belongs to someone else." The caller has exactly
one branch for all of them (`else { pos += LOG_SECTOR_SIZE; }`), and
that branch is only correct for the first. A reader tracing a replay bug
cannot tell which of the eleven fired, and neither can the caller.

The function also mixes three abstraction levels in one body: buffer
bounds, header field decoding, and per-descriptor sector reconstruction.

**Proposal:** return `Result<LogEntry, EntryReject>` with a small enum
(`Empty`, `Corrupt`, `ForeignChain`, `Malformed`), and split out
`parse_entry_header`, `parse_descriptor`, and `reconstruct_sector`.
The caller then makes the stop/skip decision explicitly — which is
exactly what H1 needs.

---

#### H3 — `journal_sector_write` skips journaling silently, three lines under a comment promising it never does
**Files:** `src/reader.rs:496-513`, `src/reader.rs:543-546`
**Category:** comment that lies + magic number
**Coverage:** none — no fixture has an undersized log

```rust
/// Write a 4 KiB sector image through the log: ...
/// After this returns the device is committed
/// to applying the sector — a crash before the in-place write
/// completes will be recovered by replay on next open.
fn journal_sector_write(&self, file_offset: u64, sector: &[u8]) -> Result<()> {
    ...
    if header.log_length == 0 || header.log_offset == 0 || header.log_length < 8192 {
        return Ok(());
    }
```

and again further down:

```rust
    if entry.len() as u64 > header.log_length as u64 {
        // Entry doesn't fit — skip journaling.
        return Ok(());
    }
```

Four conditions return `Ok(())` without writing anything to the log, and
the caller (`allocate_block_for`, line 481) then publishes the BAT entry
in place regardless. In those cases the promise in the doc comment is
false: a crash between the two writes leaves a BAT entry pointing at a
block that was never zero-initialised, with nothing in the log to repair
it. The in-line comment at 508-510 explains the *motive* ("better than
refusing the write") but the doc comment three lines above still states
the unconditional guarantee, and `Ok(())` gives the caller no way to know
which happened.

`8192` is unexplained. It is the size of a minimal one-descriptor entry
(one 4 KiB header/descriptor sector + one 4 KiB data sector), i.e. the
same quantity the second check re-derives at line 543. Two overlapping
bounds checks, one magic number, no name.

**Proposal:** name the constant (`MIN_LOGGABLE_ENTRY_BYTES`), collapse
the two checks into one, return a value the caller can branch on
(`Journaled` / `SkippedLogTooSmall`), and make the doc comment state the
fallback.

---

#### H4 — `Box::leak` to manufacture a `&'static str` for every reserved BAT state
**Files:** `src/reader.rs:341-345`
**Category:** dense/opaque + unbounded resource use

```rust
PayloadState::Reserved(v) => {
    return Err(Error::Unsupported(Box::leak(
        format!("BAT entry reserved state {v}").into_boxed_str(),
    )));
}
```

`Error::Unsupported` holds a `&'static str`, so the only way to include
the runtime value is to leak an allocation. Every read that lands on a
reserved BAT entry leaks ~30 bytes, permanently. An image crafted with
many reserved entries leaks once per read attempt with no bound. The
leak is also invisible at the call site — `Box::leak` reads as a cast.

**Proposal:** widen the variant to `Unsupported(Cow<'static, str>)` (or
add `UnsupportedBatState(u8)`), which removes the leak and makes the
value structured rather than stringly-typed.

---

#### H5 — `open_inner` is a 109-line god function whose comments enumerate its steps
**Files:** `src/reader.rs:147-254`
**Category:** god function
**Coverage:** excellent — every integration test passes through it

The body is six numbered sections in comments: `// 1. File identifier.`
`// 2. Header` `// 3. Log replay` `// 4. Region table.` `// 5. Metadata.`
`// 6. BAT.` When a function needs step numbers to be followed, the steps
are the functions. Section 3 (log replay) is the subtle one and is
buried in the middle of five pieces of straight-line parsing; section 5
mixes metadata lookup with block-size/sector-size validation.

This is the *safest* large refactor in the crate — the coverage is
already there — so it is a good warm-up, but it is lower value than
H1-H4 and should follow them.

---

#### H6 — Encoder and decoder hardcode the same byte offsets independently
**Files:** `src/log.rs:140,148,153-159,186-203,213-214` (decode) vs
`src/log.rs:379-418` (encode); `src/header.rs:46-66` (decode) vs
`src/reader.rs:676-690` (encode)
**Category:** duplication + magic numbers
**Coverage:** one roundtrip test for the log entry; none for the header

The log entry's field offsets (`4`, `8`, `12`, `16`, `24`, `32`, `48`,
`56`) and the descriptor's (`+4`, `+8`, `+16`, `+24`) appear as bare
literals in both `parse_log_entry` and `encode_entry`. The two agree
today only because a single roundtrip test happens to exercise the one
descriptor shape. Nothing structurally prevents drift: a change to one
side with a stale mental model of the other produces an entry that
encodes cleanly, parses cleanly, and replays the wrong bytes.

The header has the same split (`header.rs::parse` decodes, but
`reader.rs::encode_header` encodes — in a different module), plus a
gratuitous inconsistency:

```rust
buf[0..4].copy_from_slice(b"head");        // reader.rs:678
```

when `HEADER_SIGNATURE` is a public constant in the module it is
encoding for.

**Proposal:** one `const` block per structure naming each field offset,
used by both directions; move `encode_header` next to `Header::parse` in
`header.rs`; add an encode-then-decode roundtrip test for the header.

---

### Medium

| ID | File:line | Smell | Note |
|---|---|---|---|
| M1 | `src/reader.rs:288-353` vs `375-432` | duplication | `read_at` and `write_at` repeat the zero-length check, `checked_add` overflow guard, `OutOfBounds` construction, `has_parent` rejection, and the entire block-walk (`in_block` / `virt_block_idx` / `chunk_len` / `bat_idx` / BAT lock+`get`) — ~30 lines duplicated verbatim, differing only in the per-chunk action. |
| M2 | `src/reader.rs:346` | speculative code | `_ => unreachable!()`. The `s if s.zero_fill()` guard hides exhaustiveness from the compiler, so a new `PayloadState` variant becomes a runtime panic on attacker-supplied bytes instead of a compile error. Match on the variants and let the compiler check it. |
| M3 | `src/reader.rs:461-465` | magic number + asymmetry | `let new_raw = (new_block_off & !((1u64 << 20) - 1)) | 6u64;` — `6` is `FullyPresent`, `1<<20` is the MB shift. `bat.rs` owns the decoder (`BatEntry::from_u64`) but has no encoder, so the writer hand-rolls the inverse in another module. `debug_assert_eq!(new_raw & 0x7, 6)` asserts a tautology. |
| M4 | `header.rs:88,92,96`; `log.rs:113,116`; `metadata.rs:143,147`; `region_table.rs:113,117` | duplication | `read_u16_le` / `read_u32_le` / `read_u64_le` copy-pasted nine times across four modules, byte-for-byte identical. Well past the three-instance threshold. |
| M5 | `src/log.rs:171-182` | comment that hedges | Twelve lines of comment about where data sectors start that contradicts itself: "the spec promises they appear in order" → "In practice the spec rounds descriptors+header up to a sector boundary" → "For the encoder we use (and for entries up to a few descriptors), that's just LOG_SECTOR_SIZE." A reader cannot tell whether the code is spec-correct in general or only correct for entries this crate writes. |
| M6 | `src/log.rs:177,238,243` | dead code | `data_sector_idx` is declared, incremented once per data descriptor, then discarded by `let _ = data_sector_idx;`. The discard is what stops clippy flagging it. |
| M7 | `src/log.rs:71,75,76,153,158,159` | unused state | `tail`, `flushed_file_offset`, `last_file_offset` are decoded, stored on `LogEntryHeader`, and consulted by nothing. These are precisely the spec fields that answer "where does the active chain start" and "how far has it been flushed" — their absence is the mechanism behind H1. |
| M8 | `src/reader.rs:547-554` | undocumented cross-module invariant | "Always splice at the start of the log region — replay walks every sector slot anyway, so position only matters for write amplification, not correctness." The writer's correctness therefore depends on `collect_replay_chain` scanning exhaustively — an invariant `log.rs` never states and whose own doc contradicts (H1). Fixing H1 without reading this comment would break the writer. |
| M9 | `src/reader.rs:579-630` | duplication | `pick_header` and `pick_region_table` contain four near-identical "if `dev_size` covers offset+len, read into a buffer, try to parse, keep on success" blocks. |
| M10 | `src/reader.rs:518-528`, `563-567` | dense expression + magic number | The "stir the sequence number into bytes 8..16 by XOR" GUID-derivation idiom appears three times across two loops. Why byte 8, why XOR, and why the result is an acceptable GUID (it is not any UUID algorithm) are all unexplained; only the all-zero guard hints at the constraint. |
| M11 | `metadata.rs:80`, `region_table.rs:78` | magic number | `2047` as the entry-count cap, unnamed in both places, with the value repeated inside the error string. |
| M12 | `src/reader.rs:214` | magic number | `if file_params.block_size < 1024 * 1024 \|\| file_params.block_size > 256 * 1024 * 1024` — spec bounds as raw arithmetic. |
| M13 | `log.rs:315-327` vs `reader.rs:636-650` | duplication | The chunked zero-fill loop is written twice, each with its own `let chunk = 1024 * 1024usize;`. Both also re-allocate the zero buffer on entry. |
| M14 | `src/reader.rs:444` | unused parameter | `fn allocate_block_for(&self, bat_idx: usize, _old: &BatEntry)` — the old entry is threaded through and never used. Either the promotion path should consult it (`PartiallyPresent` is documented as behaving differently) or it should go. |

### Low

| ID | File:line | Smell | Note |
|---|---|---|---|
| L1 | `src/reader.rs:76-77` | dead field | `bat_region_len` is `#[allow(dead_code)]`. Relatedly, nothing checks at open time that the BAT region is large enough to cover `virtual_size / block_size` blocks, so an undersized BAT surfaces as `Corrupt("BAT index out of range")` from deep inside `read_at` rather than as a rejected open. |
| L2 | `header.rs:63`, `reader.rs:683` | silent field loss | `let _log_version = read_u16_le(bytes, 64);` discards the field; `encode_header` writes back a hardcoded `0u16`. Reading and rewriting a header silently rewrites `log_version`. The spec requires 0, but neither site says so. |
| L3 | `capi.rs:48-82`, `84-117` | duplication | `open_path` and `open_on_device` share the same `catch_unwind` + `match res` scaffolding; only the panic message differs. |
| L4 | `src/reader.rs:600` | implicit decision | `if a.sequence_number >= b.sequence_number` — on a tie, slot 1 wins. Deliberate, probably right, unwritten. |
| L5 | `src/log.rs:107-111` | efficiency in a scan loop | `entry_crc` does `bytes.to_vec()` — a full copy of the entry — to zero four bytes. It runs on every candidate slot while scanning a 1 MiB log region. CRC the three spans instead. |
| L6 | `reader.rs:580,448`; `log.rs:305,393`; all `read_uNN_le` | opaque names | `h1`/`h2`, `sz`, `d`, `w`, `b`. Individually trivial; collectively they make the byte-twiddling harder to read than it needs to be. |

---

## Test coverage assessment

Coverage is good structurally and thin exactly where the risk is.

**Well covered:** `bat.rs` (9 tests, every state value and both index
formulas), `metadata.rs` (11), `region_table.rs` (6), `header.rs` (5) —
each with signature, CRC, bounds, and truncation cases. The reader's
open/read/write paths get 18 integration tests including dual-slot
fallback, allocation of unallocated blocks, and multi-block spanning
writes.

**Thin:** the log. `src/log.rs` has **2 tests** for 476 lines:

- `encode_then_parse_roundtrip` — one entry, one descriptor, no `zero`
  descriptor.
- `empty_log_guid_skips_replay` — the sentinel path.

Plus one integration test, `ro_open_against_writable_file_replays_dirty_log`
(`tests/synthetic.rs:175`) — again a single entry, single descriptor.

So there is **no test anywhere with more than one log entry**, and none
with a corrupt, truncated, foreign-GUID, or out-of-sequence entry. Every
decision H1 and H2 are about is untested. The `zero` descriptor variant
(`log.rs:307-328`) has no test at all. `journal_sector_write`'s fallback
paths (H3) have no fixture that reaches them.

---

## What to fix first

Ordered by risk-reduced-per-change. Items 1-3 need tests written *before*
the change, which is why they lead.

1. **Write the missing log tests.** Multi-entry chain; a sequence gap; a
   CRC-corrupt entry mid-region; a foreign `log_guid` mixed in with a
   live chain; a `zero` descriptor. These are cheap — `encode_entry` is
   already public and `tests/synthetic.rs:186` shows the pattern. They
   will pin down what replay *currently* does, which you need before
   changing it, and they will probably surface H1 as a live behaviour
   difference rather than a doc mismatch.
2. **H1 — decide and state the stop condition.** With tests in place,
   either implement the documented contiguous-chain-with-early-stop
   (using `tail` and the sequence links, M7) or change the doc to match
   the scan-everything behaviour and justify it. Do not leave them
   disagreeing. Read M8 first — the writer depends on the current
   behaviour.
3. **H3 — make the journaling fallback visible.** Name the `8192`
   constant, merge the two size checks, and return something the caller
   can branch on. Then fix the doc comment.
4. **H2 — typed rejection reasons.** Falls out naturally once (2) needs
   to distinguish "empty slot" from "corrupt entry"; splitting the
   128-line function is the same change.
5. **H4 — remove the `Box::leak`.** Self-contained, ~10 lines, no
   behaviour change beyond not leaking.
6. **H6 + M3 + M4 — centralise offsets and codecs.** One `const` block
   per on-disk structure; give `bat.rs` a `to_u64`; hoist the nine
   `read_uNN_le` copies into one place. Mechanical, high readability
   return, and it removes a whole class of encoder/decoder drift.
7. **H5 + M1 — split `open_inner`; unify the block walk.** Best-covered
   code in the crate, so lowest risk; save it for last precisely because
   it is the least urgent.
8. **The Medium/Low tail** — M2, M5, M6, M7, M9-M14, L1-L6 — as cleanup
   passes once the log work has settled.

A reasonable stopping point after item 6 would leave the crate with the
log's decisions explicit and tested, which is the part that matters.

---

## Items skipped

| Item | Reason |
|---|---|
| `tests/common/mod.rs` duplicating `header.rs` / `region_table.rs` constants and GUIDs | *Acceptable pattern* — the module doc states this is deliberate so fixtures stay independent of the code under test. Sharing them would let a wrong constant pass its own test. |
| Hand-rolled CRC in `tests/common/mod.rs:84-88` and `tests/synthetic.rs:217-221` | *Below threshold* — 2 instances, and same independence rationale as above. |
| `HeaderSlot::other()` / two-slot rotation | *False positive* — flagged as possible over-engineering; it is required by the VHDX spec and correctly used. |
| `PayloadState::Reserved(u8)` catch-all | *Acceptable pattern* — the conservative fallback is right, and it is tested (`payload_state_maps_unknown_values_to_reserved`). Only the reader-side handling of it (H4) is a problem. |
| `#![allow(clippy::missing_safety_doc)]` in `capi.rs` | *Acceptable pattern* — the module doc covers the ownership contract for all four entry points in one place. |
