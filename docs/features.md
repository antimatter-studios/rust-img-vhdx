# Features

What this crate does today, what it refuses, and what is coming. **Every
pull request that adds, fixes, refuses or removes behaviour updates its row
here, in the same pull request** (AGENTS.md). The reasoning behind each change
is in [CHANGELOG.md](../CHANGELOG.md).

**Since** is the release a row's current state shipped in, with the issue or
pull request the changelog cites for it. Work merged after the last release
is **Unreleased (#N)** until the next one. **Tracking** names the issue for
anything not finished.

States:

- **Supported**: works, and is checked against `qemu-img`.
- **Experimental**: works in every test, but is new.
- **Partial**: works for part of the case, and the row says which part.
- **Refused**: recognised and refused by name, rather than misread.
- **Not supported**: neither read nor refused by name.
- **Upcoming**: an open issue with a plan.

## Reading

| Feature | State | Since | Tracking | Checked by |
|---|---|---|---|---|
| File identifier; both 4 KiB headers with CRC-32C, the higher sequence number used | Supported | 0.2.0 | | `corruption.rs`, `synthetic.rs` |
| Region table: BAT and metadata regions; an unknown optional region ignored | Supported | 0.2.0 | | `corruption.rs`, `qemu_validation.rs` |
| An unknown region or metadata item marked Required | Refused, as the reference tool refuses it | 0.4.0 | | `qemu_validation.rs`, `corruption.rs` |
| Metadata: file parameters, virtual size, 512 and 4096-byte logical sectors | Supported | 0.2.0 | | `qemu_validation.rs`, `corruption.rs` |
| BAT walking, chunk-ratio aware (data and sector-bitmap entries interleaved) | Supported | 0.2.0 | | `synthetic.rs`, `qemu_validation.rs` |
| Log replay of a dirty image: in place on a writable device, in memory for the tools | Supported | 0.2.0 | | `synthetic.rs`, `tests/cli/test-info.sh` |
| A dirty log on a device that cannot be written | Refused (`ReadOnly`), never served stale | 0.2.0 | | `synthetic.rs` |
| A log chain that cannot be assembled, or a log version other than 0 | Refused | 0.4.0 | | `synthetic.rs`, `corruption.rs`, `qemu_validation.rs` |
| A log region overlapping another region, unaligned, or past the end of the file | Refused | 0.4.0 | | `corruption.rs`, `qemu_validation.rs` |
| Partially present blocks (sector bitmaps) | Refused | 0.2.0 | | `synthetic.rs` |
| Differencing images (a parent chain) | Refused at open | 0.4.0 | | `corruption.rs` |
| Fuzzed region, metadata, BAT and log parsers | Supported | 0.4.0 | | `fuzz_decoders.rs` |

## Writing

| Feature | State | Since | Tracking | Checked by |
|---|---|---|---|---|
| Writes to allocated blocks | Supported | 0.2.0 | | `synthetic.rs`, `qemu_validation.rs` |
| Allocating writes at the device's tail, for unallocated, zero and unmapped blocks | Supported | 0.2.0 | | `synthetic.rs`, `qemu_validation.rs` |
| BAT changes journalled through the log first, when the log region can hold them | Supported | 0.2.0 | | `synthetic.rs`, `qemu_validation.rs` |
| Header rotation with a fresh `file_write_guid` after each publish | Supported | 0.2.0 | | `synthetic.rs` |
| A log an earlier writer left, replayed before a write | Supported | 0.2.0 | | `synthetic.rs` |
| Writes to partially present blocks | Refused | 0.2.0 | | `synthetic.rs` |
| Creating or resizing an image | Not supported | | | `tests/cli/test-unsupported.sh` |

## Interfaces

| Feature | State | Since | Tracking | Checked by |
|---|---|---|---|---|
| Rust API, over a path or any `rust-fs-core` device | Supported | 0.2.0 | | `synthetic.rs` |
| C ABI returning `FsCoreDevice` handles | Supported | 0.2.0 | | `header_names_the_built_library.rs` |
| `img.vhdx` `info`/`get`, `read`, `write` (`--features cli`) | Supported | 0.5.0 | | `cli_read.rs`, `cli_write.rs`, `tests/cli/test-info.sh`, `tests/cli/test-read.sh`, `tests/cli/test-write.sh` |
| `img.vhdx` `create`, `resize`, `set` | Not supported (`not implemented`, exit 3) | 0.5.0 | | `tests/cli/test-unsupported.sh` |
| `rust-img-vhdx doctor`, man pages, shell completions | Supported | 0.5.0 | | `tests/cli/test-names.sh`, `tests/cli/test-docs.sh` |
