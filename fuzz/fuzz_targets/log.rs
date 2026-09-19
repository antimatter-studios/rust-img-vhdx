#![no_main]
//! The log replay chain.
//!
//! This is the worst of the lot, and the most interesting. The log is a
//! structure the format expects to be PARTIALLY WRITTEN -- that is its
//! whole purpose -- so it is parsed with a corruption tolerance the
//! other structures do not have, and a fuzzer is built to abuse exactly
//! that. Entries are chained by sequence number and each declares its
//! own length.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // NOT AN ALL-ZERO GUID. `collect_replay_chain_checked` returns an
    // empty chain immediately when the expected GUID is all zeros --
    // that is how a cleanly-closed image says "nothing to replay" -- so
    // a target passing zeros would exercise one `if` and stop.
    //
    // The GUID comes from a header the same image supplied, so letting
    // the fuzzer choose it is realistic as well as necessary: matching
    // it against the entries' own GUIDs is part of what is being tested.
    let mut guid = [1u8; 16];
    let take = data.len().min(16);
    guid[..take].copy_from_slice(&data[..take]);
    if guid.iter().all(|b| *b == 0) {
        guid[0] = 1;
    }

    let _ = vhdx::log::collect_replay_chain(data, &guid);
    let _ = vhdx::log::collect_replay_chain_checked(data, &guid);
});
