# Changelog

Notable changes to `am-img-vhdx`, newest first. This is a `0.x` crate, so the
**minor** is the compatibility boundary: a minor bump may break API, a patch
never does.


## [Unreleased]

### Added

- **The region, metadata, BAT and log parsers are fuzzed, on two tiers.**
  VHDX has more attacker-controlled indirection than any other format in
  this family: the region table points at the metadata table, which
  declares the block size and virtual disk size, which the BAT is then
  indexed with. `fuzz/` holds `image`, `header`, `region_table`,
  `metadata` and `log`, nightly on a bounded budget;
  `tests/fuzz_decoders.rs` is the gate, 18,504 deterministic cases in
  about a second on the stable toolchain.

  The log target is the one worth having most: the log is a structure the
  format expects to be *partially written*, so it is parsed with a
  corruption tolerance the other structures do not have. It is fuzzed
  with a non-zero GUID, because `collect_replay_chain_checked` returns an
  empty chain immediately for an all-zero one — a target passing zeros
  would exercise one `if` and stop.

  Each target carries its own case budget. A VHDX is 8 MB before it holds
  anything, which is the format's floor rather than a choice, so one
  whole image is committed for the target that must open one and the rest
  are 64 KiB sections cut from three (#108).

- `Header::log_version` and the `header::LOG_VERSION_V0` constant.
- `Error::LogNeedsReplay`, for a log that genuinely holds unapplied
  entries on a device that cannot take them. `Error::ReadOnly` describes
  the opener; this describes the file, and names the move the caller has
  — reopen it writable. Both map to `fs_core::Error::ReadOnly` across the
  `BlockDevice` bridge, so a consumer of that surface sees no change.

### Fixed

- **`compute_crc` refuses a short buffer instead of panicking, and now
  returns a `Result`.** *(#113 — BREAKING: `header::compute_crc` and
  `region_table::compute_crc` return `Result<u32>` rather than `u32`.)*
  Both sliced the structure's fixed size out of the caller's buffer —
  `bytes[..HEADER_SIZE]`, `bytes[..REGION_TABLE_SIZE]` — without
  checking the buffer was that long, so the function whose whole job is
  validating untrusted bytes panicked on anything shorter. A truncated
  or corrupt image reaches it through the read path; the nightly fuzzer
  reached it on an empty buffer within seconds of its first unattended
  run, on both targets at once.

  Returning `Result` rather than padding the short buffer out, because
  the CRC is defined over exactly 4 KiB and exactly 64 KiB: there is no
  honest `u32` to hand back for fewer bytes than that, and any sentinel
  can collide with a real checksum. The refusal is `Error::Corrupt` with
  the same wording the two `parse` functions already use for the same
  condition. Both `parse` paths check the length before they get here,
  so no image that opened before opens differently now.

  Empty and one-byte-short seeds for both structures are committed to
  `fuzz/corpus/header` and `fuzz/corpus/region_table`, and
  `scripts/make-fuzz-corpus.sh` rebuilds them — it deletes the corpus
  before it writes it, so a seed it does not know how to make survives
  only until the next rebuild.

- **A region marked Required whose GUID we do not know is refused.** The
  flag was parsed onto `RegionEntry` and read by nothing outside the
  module's own tests, so an image carrying such a region was read as
  though it were not there: BAT found, metadata found, payload blocks
  returned raw, and no error, because nothing looked. The flag exists so
  a file that transforms its payload — an encryption region, a dedup map
  — is not read raw by an implementation that has never heard of the
  transform. `qemu-img` refuses such a file; the two now agree. An
  unknown region with the flag *clear* is still ignored, as the format
  asks.

- **A log format we cannot parse is refused instead of replayed.** The
  header's `log_version` was read into `_` and discarded, so an image
  declaring any log format at all was handed to the version-0 parser and
  its descriptors applied — a write, on top of real data. `open` now
  refuses a `log_version` other than 0, unconditionally rather than only
  when the log is dirty, since a later write would append to that region
  too. `qemu-img` refuses such a file outright; the two now agree.
- **Rewriting a header preserves the log version it carries.**
  `encode_header` wrote a literal `0`, so every replay — which rewrites
  the header to clear the log GUID — silently reset the format the file
  declared itself to be in.
- **A stale log GUID no longer makes a readable image unopenable.** The
  decision to refuse a read-only opener was taken from the header's
  `log_guid` alone, three statements before the log was even read. A
  non-zero GUID says a writer stamped the file, not that anything is
  waiting to be applied — a clean shutdown that failed to zero it, or a
  log whose entries have all been superseded, leaves it set with an empty
  chain. Such images are read by every other tool and were refused here,
  with a message describing the opener rather than the file. The
  capability test now happens after the chain is assembled.

### Changed

- **Allocation and log replay ask the device for room instead of writing
  past its end, and the `am-fs-core` pin moves to v0.2.13.** *(#111, #117)*
  Appending is the only way VHDX allocates, and it worked because a write
  past the end of a `FileDevice` grew the file underneath it.
  rust-fs-core#75 made that a refusal — rightly, since `size_bytes()` went
  on reporting the length taken at construction while the file grew, so the
  two halves of one device disagreed about where it ended
  (rust-fs-core#70) — and the pin sat six releases behind because of it.
  Measured: 8 failures against core `main`, 0 against v0.2.10, every one a
  write landing at the device's exact end.

  `BlockDevice::set_len` (rust-fs-core#161) is the room asked for out loud,
  and it is called in the two places that grow the file:

  - `VhdxReader::allocate_block_for` extends to the new tail **before** the
    zero-init write and **under the `dev_size` lock**, so the number
    `host_offset` and `journal_sector_write` read as the device's bound
    never names bytes the device does not have. A refused extension now
    leaves that bound describing the device that is really there, where
    before it was raised first and stayed raised.
  - `log::apply_chain` extends **once, up front**, to the bound its
    descriptor check has already enforced. That bound is
    `last_file_offset`, so the file ends where the log says it ends and the
    one-byte write at `wanted - 1` that used to extend it is gone. A device
    that cannot grow is refused there, before any descriptor lands, rather
    than part-way through a chain.

  `VhdxReader`'s own `impl BlockDevice` answers `can_grow() == false`
  explicitly: the guest disk's length lives in the metadata region, so
  growing it is not something a caller can ask for by writing past the end.

  Both trait methods are **defaulted** to a refusal, so a wrapping device
  that omits them turns a growable device into one that cannot allocate.
  The doubles in `tests/synthetic.rs` forward both; `CutAfter` spends its
  crash budget on `set_len` as well, because a lost extension is a crash
  point of its own — the BAT entry reaches the log naming a block the file
  is not long enough to hold, which is the shape replay's extension exists
  for.

  `ci.yml` cloned core twice at two different pins, the dependency's and
  the output-budget wrapper's. Both are v0.2.13 now, so it clones once.

- **One required check, `ci-ok`, stands for every job in `ci.yml`.**
  Branch protection named six job names by hand — `fmt`,
  `qemu-validation`, the three `test / <os>` matrix legs and
  `test (release)` — so adding a matrix leg produced a check that gated
  nothing, and renaming a job left a required name no job reports, which
  GitHub reads as permanently pending rather than failed. `ci-ok` runs
  with `if: always()`, `needs:` every other job, and fails when any of
  them failed, was cancelled or was skipped. `tests/ci_aggregate_gate.rs`
  holds `ci.yml` and `.github-guard` to each other (#105).

## [0.3.5] — 2026-09-06

### Fixed

- Opening a file read-only no longer replays its log and destroys it.
  A VHDX carries a log of metadata writes, and replaying it is part of
  opening the image — but a reader that was handed the file read-only
  was replaying into it anyway, so simply looking at an image changed
  it.
- A log chain may legitimately grow the file, within what its
  descriptors allocated. Refusing every chain that reached past the
  current end of file refused images the reference tools write.

## [0.3.4] — 2026-09-04

### Fixed

- **A bug that duplication had been hiding.** The block walk, the probe read,
  the zero-fill and the GUID stirring each existed in more than one copy, and
  the copies did not agree. Collapsing each to one definition surfaced the
  disagreement as a defect rather than as a style question.

### Changed

- The little-endian field reads move into one module instead of being spelled
  out at each parse site.

## [0.3.3] — 2026-08-29

### Fixed

- **Log replay stops at the first break in the chain.** It had been continuing
  past a discontinuity, which means replaying entries that the log does not
  actually vouch for — writing them onto a live image.

### Added

- `chore` tasks own this crate's build, and the code-review report is recorded
  in the repo.
- The github-guard hook set replaces the hand-rolled pre-commit hooks.

### Changed

- Dependencies are pinned and locked for reproducible builds.

## [0.3.2] — 2026-06-21

### Changed

- The publish job clones its path-dependency siblings, pinned to a tag rather
  than tracking a branch, and publishing is gated on the disk-image validator
  cross-check. A release built from a floating dependency is not reproducible.

## [0.3.1] — 2026-06-09

### Changed

- Pinned toolchain moves from 1.94.1 to 1.95.0, in lockstep with the rest of
  the family. A straggler links two copies of `_rust_eh_personality` into any
  consumer that binds both.

## [0.3.0] — 2026-06-01

### Added

- Cross-validation against an external disk-image validator.
- Unit tests for the VHDX structure parsers, reader corruption and recovery
  tests, and shared synthetic builders.

## [0.2.0] — 2026-05-12

### Added

- Device-backed reader, log replay and the write path.

### Added

- Release-on-tag pipeline using trusted publishing, and CI (test, fmt, clippy).

### Changed

- `am-fs-core` dependency moves to 0.2.

[Unreleased]: https://github.com/antimatter-studios/rust-img-vhdx/compare/v0.3.4...HEAD
[0.3.4]: https://github.com/antimatter-studios/rust-img-vhdx/compare/v0.3.3...v0.3.4
[0.3.3]: https://github.com/antimatter-studios/rust-img-vhdx/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/antimatter-studios/rust-img-vhdx/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/antimatter-studios/rust-img-vhdx/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/antimatter-studios/rust-img-vhdx/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/antimatter-studios/rust-img-vhdx/releases/tag/v0.2.0
