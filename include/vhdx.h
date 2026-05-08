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
 *
 * Not yet supported (returns FS_CORE_CUSTOM with detail):
 *   - PartiallyPresent blocks (sector bitmap walking)
 *   - Differencing chains (parent VHDX)
 *   - Log replay (assumes clean shutdown)
 *
 * Read-only — `fs_core_device_write_at` returns FS_CORE_READ_ONLY.
 */
FsCoreDevice *vhdx_open(const char *path);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* AM_IMG_VHDX_H */
