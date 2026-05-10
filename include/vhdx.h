/*
 * am-img-vhdx C ABI — opens a VHDX (Microsoft VHD-successor) image
 * and returns a generic FsCoreDevice handle.
 *
 * Link with libam_img_vhdx.a alongside fs_core.h.
 *
 * MIT license. (c) 2026 Antimatter Studios.
 */

#ifndef AM_IMG_VHDX_H
#define AM_IMG_VHDX_H

#include "fs_core.h"

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Open `path` (NUL-terminated UTF-8) as a VHDX image. Returns a generic
 * device handle; free via `fs_core_device_close`.
 *
 * On failure returns NULL and `fs_core_last_error_message()` has detail.
 *
 * Currently supported:
 *   - File identifier verification + dual-header CRC-validated picking
 *   - Region table (BAT + Metadata)
 *   - Metadata: FileParameters, VirtualDiskSize, LogicalSectorSize
 *   - BAT walk for FullyPresent and zero-state blocks
 *   - Log replay against dirty images (data + zero descriptors)
 *   - Read-write writes that allocate fresh blocks at the device tail
 *     and journal the BAT mutation through the log for crash safety
 *
 * Not yet supported (returns FS_CORE_CUSTOM with detail):
 *   - PartiallyPresent blocks on read (sector bitmap walking) — writes
 *     into such entries promote the block to FullyPresent
 *   - Differencing chains (parent VHDX)
 *
 * `vhdx_open` opens read-only — `fs_core_device_write_at` returns
 * FS_CORE_READ_ONLY.
 *
 * `vhdx_open_rw` opens read-write. Writes against unallocated blocks
 * extend the image at the tail, BAT mutations are journalled through
 * the log first so a crash mid-write is recoverable on next open.
 */
FsCoreDevice *vhdx_open(const char *path);
FsCoreDevice *vhdx_open_rw(const char *path);

/*
 * Open a VHDX image whose backing storage is an existing FsCoreDevice
 * (e.g. an FSKit FSBlockDeviceResource lifted via
 * `fs_core_device_from_callbacks`, a slice reader, or any other
 * device the caller already holds). Use this when the VHDX layer
 * needs to sit on top of host-managed storage that isn't a path.
 *
 * Ownership: the returned handle takes over the input device. Do NOT
 * call `fs_core_device_close` on `inner` afterwards. On failure the
 * input is freed automatically and the function returns NULL.
 */
FsCoreDevice *vhdx_open_on_device(FsCoreDevice *inner);
FsCoreDevice *vhdx_open_rw_on_device(FsCoreDevice *inner);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* AM_IMG_VHDX_H */
