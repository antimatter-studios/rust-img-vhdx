//! `img.vhdx info` and `read` on an image whose log still holds an entry to
//! replay: they report and read the image as the log says it is, and the
//! file is byte-for-byte what it was.
//!
//! Opening such an image replays the log, and the library does it in place
//! when the file can be written. The read-only verbs must not: the replay
//! lands in memory. qemu-img cannot make an image in this state, so the
//! crate's own replayable fixture is used, the one tests/synthetic.rs
//! checks the in-place replay with.

mod common;

use std::path::Path;
use std::process::{Command, Output};

use common::*;

const TOOL: &str = env!("CARGO_BIN_EXE_rust-img-vhdx");

fn img(image: &Path, args: &[&str]) -> Output {
    Command::new(TOOL)
        .arg("img")
        .arg(image)
        .args(args)
        .output()
        .unwrap()
}

/// The fixture with a single-entry log that overwrites the 4 KiB sector at
/// 8 KiB into block 0 with 0xEE.
fn dirty_image(name: &str) -> TempPath {
    let path = tmp_path(name);
    build_big_vhdx(&path, &pattern_block(9));
    inject_dirty_log(&path, [0x5au8; 16]);
    path
}

#[test]
fn info_reports_a_log_to_replay_as_dirty_and_leaves_the_file_alone() {
    let path = dirty_image("cli-dirty-info");
    let before = std::fs::read(&path).unwrap();

    let out = img(&path, &["get", "dirty", "--text"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "true");
    assert!(
        std::fs::read(&path).unwrap() == before,
        "info wrote to the image it was asked to look at"
    );

    // The header reported is the file's own, not the one a replay would
    // write: the fixture's log names sequence 5.
    let out = img(&path, &["get", "vhdx.header_sequence", "--text"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "5");
}

#[test]
fn read_serves_the_replayed_bytes_and_leaves_the_file_alone() {
    let path = dirty_image("cli-dirty-read");
    let before = std::fs::read(&path).unwrap();
    let block0 = pattern_block(9);

    let out = img(&path, &["read", "--offset", "4096", "--length", "12288"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout.len(), 12288);
    assert_eq!(&out.stdout[..4096], &block0[4096..8192], "before the entry");
    assert!(
        out.stdout[4096..8192].iter().all(|b| *b == 0xEE),
        "the log's sector was not replayed"
    );
    assert_eq!(
        &out.stdout[8192..],
        &block0[12288..16384],
        "after the entry"
    );
    assert!(
        std::fs::read(&path).unwrap() == before,
        "read wrote to the image it was asked to read"
    );
}

#[cfg(unix)]
#[test]
fn an_image_file_nobody_may_write_still_replays_in_memory() {
    use std::os::unix::fs::PermissionsExt;

    let path = dirty_image("cli-dirty-locked");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    let out = img(&path, &["read", "--offset", "8192", "--length", "4096"]);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        out.status.success(),
        "a read-only file with a log to replay was refused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.iter().all(|b| *b == 0xEE));
}

#[test]
fn a_clean_image_is_not_dirty() {
    let path = tmp_path("cli-clean");
    build_big_vhdx(&path, &pattern_block(3));
    let out = img(&path, &["get", "dirty", "--text"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "false");
}
