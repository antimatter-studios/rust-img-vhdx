#![no_main]
//! A whole image, opened and read.
//!
//! VHDX has more attacker-controlled indirection than any other format
//! in this family: the region table points at the metadata table, which
//! declares the block size and virtual disk size, which the BAT is then
//! indexed with. Each hop is an offset and a length read out of the
//! image, and only opening one reaches all of them.
use libfuzzer_sys::fuzz_target;
use vhdx_fuzz::walk;

fuzz_target!(|data: &[u8]| {
    walk(data);
});
