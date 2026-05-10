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
      unallocated / zero / unmapped / partially-present BAT entries,
      writes through to allocated blocks otherwise. BAT mutations are
      journalled through the log first (one-descriptor entry per
      sector) so a crash mid-write is recoverable on next open. After
      the BAT is published the active header is rotated to the other
      slot with a fresh `file_write_guid` per the spec.
- [ ] PartiallyPresent blocks on read (sector bitmap walking) — writes
      promote to FullyPresent.
- [ ] Differencing chains (parent locator metadata + chain walk).

## Spec

Microsoft's *VHDX Format Specification* (MS-VHDX). The format is more
involved than VHD because of the log structure, region table, and
chunked BAT — but the read path is approachable when broken into the
file identifier → header → region → metadata → BAT pipeline, and the
write path layers on top once the log is understood.

## License

MIT.
