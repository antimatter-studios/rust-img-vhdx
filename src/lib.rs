//! Pure-Rust VHDX (Microsoft VHD's modern successor) reader. Used by
//! Hyper-V and WSL2.
//!
//! Implements [`fs_core::BlockRead`] and [`fs_core::BlockDevice`] so a
//! `VhdxReader` plugs into the partition probe + filesystem driver
//! stack — and can be exposed as a generic
//! [`fs_core::ffi::FsCoreDevice`] handle through the C ABI.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod bat;
pub mod capi;
/// Little-endian field reads, shared by every parser here.
mod endian;
pub mod error;
pub mod header;
pub mod log;
pub mod metadata;
pub mod reader;
pub mod region_table;

pub use error::{Error, Result};
pub use reader::VhdxReader;
