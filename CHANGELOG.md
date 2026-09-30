# Changelog

Notable changes to `am-img-vhdx`, newest first. This is a `0.x` crate, so the
**minor** is the compatibility boundary: a minor bump may break API, a patch
never does.


## [Unreleased]

### Added

- **`img.vhdx`, the command-line tool**, one multi-call binary named
  `rust-img-vhdx` behind a new `cli` feature (clap, MIT/Apache-2.0), so the
  static library gains no dependency. `img.vhdx <image> info`/`get [key]`
  reports the image as JSON (`--text` for people), and `read [--offset N]
  [--length N] [-o FILE]` streams the guest's raw bytes, the whole virtual
  disk when no range is given. Neither writes the image: a log left to
  replay is replayed in memory and reported as `dirty`. `create` (no creator
  in the library), `resize` and `set` answer `not implemented` (exit 3), and
  `write` until its verb lands. `rust-img-vhdx doctor` checks that the
  `img.vhdx` on `PATH` is this one. `chore test:cli` tests the installed tool
  against `qemu-img`, and CI runs it on every pull request.

- `VhdxReader::physical_sector_size()`: the PhysicalSectorSize metadata item,
  when the file carries one. Reported, never enforced.

- Releases carry a build-provenance attestation: the published `.crate` is
  attached to the GitHub release for its tag, checked first against the
  crates.io checksum, and verifiable with `gh attestation verify` (see the
  README, "Verifying a release").

## [0.4.0] — 2026-09-27

### Added

- **`tests/changelog.rs`: the release's own shape, checked rather than
  remembered.** *(#63)* Five assertions, each of which was confirmed to fail
  on a mutation rather than assumed to work:

  - `[package].version` equals the newest `## [x.y.z]` section. A release is a
    tag, a manifest version and a changelog section saying one thing; two of
    those three are in this repository and can be compared.
  - **a released section carrying `BREAKING` bumped the minor.** This is #63 as
    an assertion. The judgement — is this breaking? — stays with whoever writes
    the entry; the test insists an entry that already says so is not shipped as
    a patch.
  - each `## [...]` section uses a `### Heading` at most once.
  - every released section has a `[x.y.z]: <url>` definition.
  - the version parser reads a heading or skips it, never guesses. A heading it
    misread would let a real release escape the checks above while they passed.

  Two of those found existing defects on their first run: `[0.2.0]` carried two
  `### Added` blocks, and the link definitions still compared `v0.3.4...HEAD`
  with 0.3.5 released and no `[0.3.5]` definition at all — so that heading
  rendered as literal brackets. Both fixed here.

  #63 asks for a release-checklist item. This is the same idea written as
  something that runs: it notes the identical mistake was found in
  `rust-partitions` and `rust-fs-ext4` in the same month, and that a review bot
  reported the duplicate-heading case in a sibling five times before anyone
  acted. A convention five accurate reports did not enforce will not enforce
  itself.

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
  with a non-zero GUID, because `collect_replay_chain` returns an
  empty chain immediately for an all-zero one — a target passing zeros
  would exercise one `if` and stop.

  Each target carries its own case budget. A VHDX is 8 MB before it holds
  anything, which is the format's floor rather than a choice, so one
  whole image is committed for the target that must open one and the rest
  are 64 KiB sections cut from three (#108).

- `Error::LogUnassembled`, for a log that holds entries whose active chain
  could not be worked out. *(#41 — BREAKING: a new `Error` variant, and
  `log::collect_replay_chain` returns `Result<Vec<LogEntry>>`.)*

  `open` applies the log before it reads the region table, the metadata and
  the BAT, because those live in bytes the log may be part-way through
  changing — `src/reader.rs`'s module doc says the order is not negotiable.
  That argument was applied to the read-only branch, where a pending log on an
  unwritable device is `Error::LogNeedsReplay`, and **not** to a chain that
  could not be assembled: `select_chain` returned an empty `Vec` for it, which
  is what a log with nothing pending returns, so `open` fell through and read
  the stale bytes. The caller got a reader serving pre-crash data with no error
  and no signal — a file that looks like its last writes were never made rather
  than one that is damaged. Write to it and you write on top of a state the log
  was mid-way through changing.

  The condition is a head entry whose `tail` names no entry discovery found.
  Declining to guess was always right — the entry lowest in the region may
  belong to a run the head has disowned, and applying it would write stale
  bytes and then erase the log holding the live chain. Reporting it is what was
  missing.

  Distinct from `Error::LogReplay` on purpose: **nothing has been written** when
  this is returned, so the image is exactly as it was found. That is the
  difference between "reopen it elsewhere, or with a recovery tool" and "this
  file is now half-changed", and it is asserted — the test compares the whole
  file byte for byte after the refused open.

  qemu behaves as this crate used to, so this is a hardening item rather than a
  divergence from the reference implementation. Refusing rather than reporting
  via a flag, because this crate is a library other readers stack on and ships
  no CLI of its own: a refusal can be relaxed into a report later without
  anyone having lost data in the meantime, and the reverse is not true.

- `Header::log_version` and the `header::LOG_VERSION_V0` constant.
- `Error::LogNeedsReplay`, for a log that genuinely holds unapplied
  entries on a device that cannot take them. `Error::ReadOnly` describes
  the opener; this describes the file, and names the move the caller has
  — reopen it writable. Both map to `fs_core::Error::ReadOnly` across the
  `BlockDevice` bridge, so a consumer of that surface sees no change.

### Fixed

- **A panicking test removes its fixture, including the 64 MiB sparse
  images.** *(#107)* `tests/corruption.rs` ended each test with a bare
  `let _ = std::fs::remove_file(&path);` — 38 of them — and a test that panics
  never reaches that line. A failing test is exactly when the fixture leaks,
  and a failing test is what a developer then reruns in a loop; three of these
  build 64 MiB sparse images.

  Measured, by injecting a panic into
  `a_bat_region_longer_than_the_disk_needs_is_not_read_past_the_disk`:

  | | left in `TMPDIR` |
  |---|---|
  | before | **1 file, 64 MiB**, per failing run |
  | after | **0** |

  `tmp_path` returns a `TempPath` whose `Drop` removes the file, and `Drop`
  runs during unwind. It derefs to `Path` and implements `AsRef<Path>`, so the
  call sites are unchanged; the 38 manual removals are gone, as are 24 more in
  `tests/synthetic.rs` and the local `RemoveOnDrop` that one loop there had
  already reached for.

  `tests/qemu_validation.rs` carried a **second copy** of the same type,
  written out again because it does not use the fixture builders in
  `tests/common/mod.rs` and importing that module for one RAII wrapper would
  drag them in. `TempPath` lives in `tests/common/temp_path.rs` and both
  targets reach it with `#[path]`, which pulls in that file and nothing else —
  so the copy that would have drifted is gone rather than doubled.

  The guard is `a_panicking_test_still_removes_its_fixture`, which plants a
  probe inside a closure that panics and fails if it survives. Without it, an
  emptied `Drop` body leaves the whole suite green — every test cleans up on
  its way out whether the removal works or not, so nothing else depends on it.
  It compares **that exact path**, not a prefix scan of the temp directory:
  several test binaries share one `TMPDIR`, pids are reused and the
  per-process counter restarts at zero, so a stale probe can carry the same
  name. `probes_left_by_other_runs_do_not_fail_the_drop_test` plants two such
  corpses and requires the guard to ignore them.

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

- **`a_released_section_that_breaks_api_bumped_the_minor` no longer demands
  that the changelog contain a break.** It carried a control asserting one
  exists, so that a scan matching nothing could not pass vacuously —
  reasonable, and wrong: a crate whose released history has broken nothing
  would have to invent a break to satisfy it.

  Porting this file to `rust-img-qcow2`, which has only added public methods
  since v0.4.5, failed on that **control** rather than on the rule. That is the
  same "a check that cannot fail" defect the control was written to prevent,
  arrived at from the other side — the check could not *pass* on an honest
  changelog.

  Proving the scan works belongs in a test of the scan, which
  `a_break_is_recognised_however_it_is_spelled` already does against bodies it
  is handed. The remaining control — at least two released sections, so there
  is a pair to compare — stays.

- **`a_released_section_that_breaks_api_bumped_the_minor` matches the marker
  case-insensitively.** It was `body.contains("BREAKING")`, matched against the
  uppercase spelling this repository happens to use. The sibling
  `rust-img-vhd` writes it lowercase — ``**`Error::ReadOnly` carries its
  cause** (breaking: match `ReadOnly(_)`)`` — and an `Error` variant that
  gained a payload is as breaking as anything here, so the guard would have
  passed that changelog and let the release ship as a patch.

  A check that misses the very case it was written for reports protection it is
  not providing, which is the defect this whole file exists to catch, found
  inside the file itself. `a_break_is_recognised_however_it_is_spelled` covers
  the spellings the family actually uses and the near-misses it must not match.

  Deliberately loose — the word, in any case, anywhere in the section. A false
  positive costs a minor bump nobody needed; a false negative costs a consumer
  a build that stopped compiling on a patch.

- **`fuzz/Cargo.toml` follows this crate's `am-fs-core` pin, and a test says
  so.** *(rust-img-qcow2#118)* The fuzz crate is a separate package with its own
  manifest and lockfile, so nothing about bumping the parent's dependency
  pointed at the child's: this one required `0.2.10` while the crate required
  `0.2.13`, and `fuzz.yml` already checked core out at `v0.2.13`.

  It was green throughout, which is the problem. `version = "0.2.10"` is a caret
  requirement that `0.2.13` satisfies, the `path` source is what cargo actually
  uses, and `cargo fuzz run` is not passed `--locked`, so the stale
  `fuzz/Cargo.lock` was rewritten in place on every run. The day core reaches
  `0.3.0` the parent resolves and the fuzz crate does not — and that surfaces
  in a nightly cron, naming a version requirement rather than the bump behind
  it.

  `the_fuzz_crate_requires_the_same_core_as_this_one` in
  `tests/fuzz_decoders.rs` compares the two manifests' `version` fields. It
  refuses a bare `path` dependency too, since one passes every other check in
  that file while saying nothing about which core it is for. Both failure modes
  were confirmed to fail before the fix went in.

  All four image crates had drifted, in three different ways.

- **`Error` is `#[non_exhaustive]`.** *(#63 — BREAKING: a caller matching on
  it needs a wildcard arm.)* Three variants were added during the 0.3 line,
  most recently `LogUnassembled` (#41), and each was a break that the changelog
  had to be corrected for. With the attribute, the next one is not. Adding it
  is itself breaking, which is why it lands in this bump rather than later as a
  patch — doing it later would repeat the problem it removes.

  **`Header` deliberately does not carry it.** `pub fn encode_header(h:
  &Header)` means a downstream has no way to build one except by struct
  literal, which `#[non_exhaustive]` forbids outright — it would make a public
  function uncallable from outside the crate. A format structure's fields *are*
  the format, so a new one there stays a minor bump, and
  `a_released_section_that_breaks_api_bumped_the_minor` is what catches it
  being released as a patch.

- **CI builds the public docs, and a broken intra-doc link is an error.**
  *(#124)* `cargo build`, `cargo test` and `cargo clippy` all ignore intra-doc
  links, so a link to a private item or to something renamed away was
  invisible to every gate here. Measured on a tree with one deliberate
  unresolved link: `cargo clippy --locked --all-targets -- -D warnings`
  reported **0**, and `cargo doc` reported an error.

  `RUSTDOCFLAGS: -D warnings`, because rustdoc's default is to warn and carry
  on — which is how a page ships with its links dead and no failure anywhere.
  The errors also mask each other, since rustdoc stops at the first failing
  pass: the sibling `rust-img-qcow2` reached eight (qcow2#105) and fixing
  those surfaced seven more. That is the argument for gating before there is a
  backlog.

  The step lives in the `fmt` job, which now clones `../rust-fs-core` for the
  first time: `cargo doc` resolves the path dependency even though `cargo fmt`
  does not. Nothing needed fixing — #123 cleared the one error that existed.

- **`log::collect_replay_chain` returns `Result<Vec<LogEntry>>`, and the
  `Vec`-returning wrapper is gone rather than deprecated.** *(#41 —
  BREAKING.)* There were two functions: `collect_replay_chain_checked`, which
  `open` used, and `collect_replay_chain`, which was the same call with
  `unwrap_or_default()` inside it. The second turned both refusals — a region
  discovery could not finish examining (#73) and a chain that could not be
  assembled (#41) — into the same empty vector a healthy log returns, from a
  `pub` function. One function, one name, and a caller has to look at the
  `Result`. `Ok(empty)` now means one thing only: the log has nothing to do.

- **The qemu cross-validation target is gated by its feature, and linted.**
  *(#110)* `tests/qemu_validation.rs` opens with
  `#![cfg(feature = "qemu-validation")]` and `Cargo.toml` had no `[[test]]`
  entry for it, so `cargo test --locked --all-targets` — the debug matrix and
  the release job — built an empty binary, ran it, and printed

  ```text
       Running tests/qemu_validation.rs
  test result: ok. 0 passed; 0 failed; 0 ignored; ...
  ```

  a passing line under the cross-validation suite's own name for a run that
  validated nothing. `0 passed` and `15 passed` read the same to anyone
  scanning, and the executed-test floor cannot tell them apart either: it sums
  `passed` counts and a zero adds nothing.

  `required-features = ["qemu-validation"]` takes the target out of
  `--all-targets`, so the line is **absent** rather than green — measured, the
  debug tier's log no longer mentions `qemu_validation` at all.

  That leaves the `qemu-validation` job as the only place the file is compiled
  with its bodies present, so that job now lints it:
  `cargo clippy --locked --features qemu-validation --test qemu_validation --
  -D warnings`. `-D warnings` is enforced on every other target here and was
  not enforced on the crate's only independent oracle. Demonstrated: a
  deliberate `let _x = vec![1].len() == 0` in that file is **2 errors** under
  the new step and **0** under `--all-targets`.

  Two guards, so neither half can quietly go away again:
  `tests/feature_gated_targets.rs` parses `Cargo.toml` and holds every
  test target with a crate-level `#![cfg(feature = "...")]` to having a
  matching `required-features`; `the_pr_gate_still_cross_validates_against_qemu_img`
  in `tests/ci_profile.rs` gained a third assertion for the clippy step, with
  six cases of its own covering the spellings that do and do not lint it.

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
- Release-on-tag pipeline using trusted publishing, and CI (test, fmt, clippy).

### Changed

- `am-fs-core` dependency moves to 0.2.

[Unreleased]: https://github.com/antimatter-studios/rust-img-vhdx/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/antimatter-studios/rust-img-vhdx/compare/v0.3.5...v0.4.0
[0.3.5]: https://github.com/antimatter-studios/rust-img-vhdx/compare/v0.3.4...v0.3.5
[0.3.4]: https://github.com/antimatter-studios/rust-img-vhdx/compare/v0.3.3...v0.3.4
[0.3.3]: https://github.com/antimatter-studios/rust-img-vhdx/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/antimatter-studios/rust-img-vhdx/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/antimatter-studios/rust-img-vhdx/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/antimatter-studios/rust-img-vhdx/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/antimatter-studios/rust-img-vhdx/releases/tag/v0.2.0
