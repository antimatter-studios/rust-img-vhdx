//! C ABI for the VHDX reader. Returns a generic [`FsCoreDevice`]
//! handle.
//!
//! Four entry points so callers can pick how the backing storage is
//! supplied and whether they want write access:
//!
//! - [`vhdx_open`] — open `path` read-only.
//! - [`vhdx_open_rw`] — open `path` read-write.
//! - [`vhdx_open_on_device`] — wrap an existing `FsCoreDevice`
//!   (e.g. an FSKit-supplied block resource) read-only.
//! - [`vhdx_open_rw_on_device`] — wrap an existing `FsCoreDevice` RW.
//!
//! The on-device variants take ownership of the input handle on
//! success — the caller must NOT call `fs_core_device_close` on it
//! afterwards. On failure the input is freed automatically and the
//! function returns NULL.

#![allow(clippy::missing_safety_doc)]

use crate::VhdxReader;
use fs_core::ffi::{set_last_error, FsCoreDevice};
use std::ffi::CStr;
use std::os::raw::c_char;
use std::panic::AssertUnwindSafe;
use std::ptr;
use std::sync::Arc;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn vhdx_open(path: *const c_char) -> *mut FsCoreDevice {
    open_path(path, false)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn vhdx_open_rw(path: *const c_char) -> *mut FsCoreDevice {
    open_path(path, true)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn vhdx_open_on_device(inner: *mut FsCoreDevice) -> *mut FsCoreDevice {
    unsafe { open_on_device(inner, false) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn vhdx_open_rw_on_device(inner: *mut FsCoreDevice) -> *mut FsCoreDevice {
    unsafe { open_on_device(inner, true) }
}

fn open_path(path: *const c_char, writable: bool) -> *mut FsCoreDevice {
    if path.is_null() {
        set_last_error("path is null");
        return ptr::null_mut();
    }
    let res = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let cstr = unsafe { CStr::from_ptr(path) };
        let s = match cstr.to_str() {
            Ok(s) => s,
            Err(_) => {
                set_last_error("path is not valid UTF-8");
                return ptr::null_mut();
            }
        };
        let reader = if writable {
            VhdxReader::open_rw(s)
        } else {
            VhdxReader::open(s)
        };
        match reader {
            Ok(r) => FsCoreDevice::into_handle(Arc::new(r)),
            Err(e) => {
                set_last_error(e.to_string());
                ptr::null_mut()
            }
        }
    }));
    match res {
        Ok(p) => p,
        Err(_) => {
            set_last_error("panic in vhdx_open");
            ptr::null_mut()
        }
    }
}

unsafe fn open_on_device(inner: *mut FsCoreDevice, writable: bool) -> *mut FsCoreDevice {
    if inner.is_null() {
        set_last_error("inner device handle is null");
        return ptr::null_mut();
    }
    let res = std::panic::catch_unwind(AssertUnwindSafe(|| {
        // Reclaim ownership of the boxed handle; clone the inner Arc so
        // we can stack it under the VHDX reader, then drop the original
        // wrapper so the caller's pointer is consumed.
        let boxed = unsafe { Box::from_raw(inner) };
        let dev_arc = boxed.inner().clone();
        drop(boxed);

        let reader = if writable {
            VhdxReader::open_rw_on_device(dev_arc)
        } else {
            VhdxReader::open_on_device(dev_arc)
        };
        match reader {
            Ok(r) => FsCoreDevice::into_handle(Arc::new(r)),
            Err(e) => {
                set_last_error(e.to_string());
                ptr::null_mut()
            }
        }
    }));
    match res {
        Ok(p) => p,
        Err(_) => {
            set_last_error("panic in vhdx_open_on_device");
            ptr::null_mut()
        }
    }
}
