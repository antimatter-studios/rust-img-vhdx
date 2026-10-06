#![no_main]
//! The metadata table and the items behind it. This is where the block
//! size and the virtual disk size come from, so every number the BAT
//! walk uses originates here.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(table) = img_vhdx::metadata::MetadataTable::parse(data.to_vec()) {
        let _ = table.unknown_required();
        if let Some(item) = table.item_data(&[0u8; 16]) {
            let _ = img_vhdx::metadata::FileParameters::parse(item);
        }
    }
    let _ = img_vhdx::metadata::FileParameters::parse(data);
});
