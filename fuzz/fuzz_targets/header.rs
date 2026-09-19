#![no_main]
//! A header block. There are two copies, and which one is believed when
//! they disagree is decided by a sequence number in the bytes -- so a
//! crafted image chooses which header the reader trusts.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = vhdx::header::Header::parse(data);
    let _ = vhdx::header::compute_crc(data);
});
