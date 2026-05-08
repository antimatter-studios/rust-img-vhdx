# vhdx

Pure-Rust reader for the VHDX virtual-disk format — VHD's modern
successor, used by Hyper-V and WSL2 on Windows. Spec implemented from
Microsoft's published format documentation; no GPL code is copied or
linked.

## Status

- [x] File identifier verification ("vhdxfile")
- [x] Header (4 KiB) with CRC-32C validation; picks the higher
      `sequence_number`.
- [x] Region table (lookup of well-known BAT and Metadata regions).
- [x] Metadata (file parameters, virtual disk size, logical sector size).
- [x] BAT walking with chunk-ratio aware decoding (data + sector-bitmap
      entry interleave).
- [x] `BlockRead + BlockDevice` impls via `am-fs-core`.
- [x] C ABI: `vhdx_open(path) -> *mut FsCoreDevice`.
- [ ] Log replay (currently assumes clean shutdown).
- [ ] Differencing chains (parent-locator metadata).
- [ ] Write support.

## Spec

Microsoft's *VHDX Format Specification* (MS-VHDX). The format is more
involved than VHD because of the log structure, region table, and
chunked BAT — but the read path is approachable when broken into the
file identifier → header → region → metadata → BAT pipeline.

## License

MIT.
