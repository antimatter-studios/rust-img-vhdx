# Changelog

Notable changes to `am-img-vhdx`, newest first. This is a `0.x` crate, so the
**minor** is the compatibility boundary: a minor bump may break API, a patch
never does.


## [Unreleased]

### Fixed

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

### Added

- `Header::log_version` and the `header::LOG_VERSION_V0` constant.
- `Error::LogNeedsReplay`, for a log that genuinely holds unapplied
  entries on a device that cannot take them. `Error::ReadOnly` describes
  the opener; this describes the file, and names the move the caller has
  — reopen it writable. Both map to `fs_core::Error::ReadOnly` across the
  `BlockDevice` bridge, so a consumer of that surface sees no change.

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
