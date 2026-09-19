#![no_main]
//! The region table: entries carrying a file offset and a length for
//! each region, plus a "required" flag that decides whether an unknown
//! region is fatal or ignorable.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(table) = vhdx::region_table::RegionTable::parse(data) {
        let _ = table.unknown_required();
        let _ = table.find(&[0u8; 16]);
    }
    let _ = vhdx::region_table::compute_crc(data);
});
