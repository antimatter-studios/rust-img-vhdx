# vhdx

Pure-Rust reader/writer for the VHDX virtual-disk format — VHD's modern
successor, used by Hyper-V and WSL2 on Windows. Spec implemented from
Microsoft's published format documentation; no GPL code is copied or
linked.

## Status

- [x] File identifier verification ("vhdxfile")
- [x] Header (4 KiB) with CRC-32C validation; picks the higher
      `sequence_number`, two-slot rotation on rewrite.
- [x] Region table (lookup of well-known BAT and Metadata regions).
- [x] Metadata (file parameters, virtual disk size, logical sector size).
- [x] BAT walking with chunk-ratio aware decoding (data + sector-bitmap
      entry interleave).
- [x] `BlockRead + BlockDevice` impls via `am-fs-core`.
- [x] Device-backed reader — opens on top of any
      `Arc<dyn fs_core::BlockDevice>` (file, FSKit block resource,
      slice, callback-backed device).
- [x] C ABI: `vhdx_open` / `vhdx_open_rw` / `vhdx_open_on_device` /
      `vhdx_open_rw_on_device`, all returning a generic
      `FsCoreDevice` handle.
- [x] Log replay against dirty images. RO opens replay in place when
      the underlying device is writable; non-writable backing with a
      non-empty log is reported as `ReadOnly` rather than silently
      serving stale data zones.
- [x] Write path. Allocates fresh blocks at the device tail for
      unallocated / zero / unmapped BAT entries, writes through to
      allocated blocks otherwise, and refuses partially-present ones.
      Where the log region can hold the entry, BAT mutations are
      journalled through the log first (one-descriptor entry per
      sector) so a crash mid-write is recoverable on next open; an
      absent or too-small log region publishes the BAT entry
      unjournalled. After
      the BAT is published the active header is rotated to the other
      slot with a fresh `file_write_guid` per the spec.
- [ ] PartiallyPresent blocks (sector bitmap walking) — reads and
      writes touching one are refused as unsupported.
- [ ] Differencing chains (parent locator metadata + chain walk).

## Spec

Microsoft's *VHDX Format Specification* (MS-VHDX). The format is more
involved than VHD because of the log structure, region table, and
chunked BAT — but the read path is approachable when broken into the
file identifier → header → region → metadata → BAT pipeline, and the
write path layers on top once the log is understood.

## Verifying a release

From the next release onward, every version published to crates.io is
also attached to the GitHub release for its tag, with a build-provenance
attestation signed by this repository's release workflow. It proves the
crate was built by `.github/workflows/release.yml` from a commit in this
repository, not uploaded from someone's machine. To check the crates.io
download of version `X.Y.Z`:

```sh
curl -sSfLo am-img-vhdx-X.Y.Z.crate https://static.crates.io/crates/am-img-vhdx/am-img-vhdx-X.Y.Z.crate
gh attestation verify am-img-vhdx-X.Y.Z.crate \
  --repo antimatter-studios/rust-img-vhdx \
  --signer-workflow antimatter-studios/rust-img-vhdx/.github/workflows/release.yml
```

The workflow refuses to attest a `.crate` whose sha256 differs from the
checksum crates.io records for that version, so the file on the release
page and the crates.io download are the same bytes.

## License

MIT.
