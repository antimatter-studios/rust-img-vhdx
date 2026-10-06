//! `img.vhdx write` refuses input that cannot be written before writing
//! any of it, and leaves the image as it was; and a write into an image
//! whose log still holds an entry replays the log first.
//!
//! The tool reads its input from stdin: a regular file redirected there is
//! measured before a byte is read, and a pipe is read no further than one
//! byte past what fits. What the written bytes look like to another
//! implementation is tests/cli/test-write.sh's question, against qemu-img.

mod common;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use common::*;

const TOOL: &str = env!("CARGO_BIN_EXE_rust-img-vhdx");

fn img(args: &[&str]) -> Command {
    let mut cmd = Command::new(TOOL);
    cmd.arg("img").args(args);
    cmd
}

fn write_from_file(image: &Path, offset: &str, input: &Path) -> Output {
    img(&[image.to_str().unwrap(), "write", "--offset", offset])
        .stdin(std::fs::File::open(input).unwrap())
        .output()
        .unwrap()
}

fn one_block_image(name: &str) -> TempPath {
    let path = tmp_path(name);
    build_vhdx(&path, &[0x33u8; BLOCK_SIZE as usize]);
    path
}

#[test]
fn write_refuses_an_input_past_the_virtual_disk_by_its_length() {
    let image = one_block_image("cli-past-end");
    let before = std::fs::read(&image).unwrap();

    let input = tmp_path("cli-past-end-input");
    std::fs::File::create(&input)
        .unwrap()
        .set_len(VIRTUAL_DISK_SIZE + 1)
        .unwrap();
    let wrote = write_from_file(&image, "0", &input);
    let stderr = String::from_utf8_lossy(&wrote.stderr);
    assert_eq!(
        wrote.status.code(),
        Some(1),
        "an oversized input was accepted: {stderr}"
    );
    // Off Unix stdin is always read as a pipe (see `stdin_file`), so the
    // refusal there is the pipe's: the bytes on stdin outnumber the room.
    assert!(
        stderr.contains("run past") || (cfg!(not(unix)) && stderr.contains("more than")),
        "refused, but not by its length: {stderr}"
    );
    assert!(
        std::fs::read(&image).unwrap() == before,
        "the refused write changed the image"
    );

    // And an input that fits, into the present block, still writes.
    std::fs::write(&input, b"fits").unwrap();
    let wrote = write_from_file(&image, "512", &input);
    assert!(
        wrote.status.success(),
        "{}",
        String::from_utf8_lossy(&wrote.stderr)
    );
    let r = img_vhdx::VhdxReader::open(&image).unwrap();
    let mut back = [0u8; 6];
    r.read_at(511, &mut back).unwrap();
    assert_eq!(&back, b"\x33fits\x33");
}

/// Input through a pipe has no length to check first; it is read no
/// further than one byte past what fits, and refused before any is written.
#[test]
fn write_refuses_a_piped_input_past_the_virtual_disk_before_writing() {
    let image = one_block_image("cli-pipe-past-end");
    let before = std::fs::read(&image).unwrap();

    let mut child = img(&[image.to_str().unwrap(), "write", "--offset", "512"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    // More than fits; the tool may stop reading before all of it is sent.
    let _ = stdin.write_all(&vec![0xAB; VIRTUAL_DISK_SIZE as usize * 2]);
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "an oversized pipe was accepted: {stderr}"
    );
    assert!(stderr.contains("more than"), "{stderr}");
    assert!(
        std::fs::read(&image).unwrap() == before,
        "the refused write changed the image"
    );
}

/// An image given as its own input is refused, and left as it was.
#[cfg(unix)]
#[test]
fn write_refuses_the_image_as_its_own_input() {
    let image = one_block_image("cli-self");
    let before = std::fs::read(&image).unwrap();
    let wrote = write_from_file(&image, "0", &image);
    let stderr = String::from_utf8_lossy(&wrote.stderr);
    assert!(!wrote.status.success(), "an image was written into itself");
    assert!(stderr.contains("is the image being written"), "{stderr}");
    assert!(
        std::fs::read(&image).unwrap() == before,
        "the refused write changed the image"
    );
}

/// A write into an image an earlier writer left with a log to replay: the
/// log lands first, then the write, and both are in the file afterwards.
/// qemu-img cannot make an image in this state, so the crate's own
/// replayable fixture is used.
#[test]
fn write_into_an_image_with_a_log_to_replay_keeps_the_logged_sector() {
    let image = tmp_path("cli-dirty-write");
    let block0 = pattern_block(9);
    build_big_vhdx(&image, &block0);
    inject_dirty_log(&image, [0x5au8; 16]);

    let input = tmp_path("cli-dirty-write-input");
    std::fs::write(&input, [0x77u8; 100]).unwrap();
    let wrote = write_from_file(&image, "20000", &input);
    assert!(
        wrote.status.success(),
        "{}",
        String::from_utf8_lossy(&wrote.stderr)
    );

    let out = img(&[image.to_str().unwrap(), "get", "dirty", "--text"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "false",
        "the write left the log to replay"
    );
    let r = img_vhdx::VhdxReader::open(&image).unwrap();
    let mut got = vec![0u8; 20100];
    r.read_at(0, &mut got).unwrap();
    assert_eq!(&got[..8192], &block0[..8192], "before the logged sector");
    assert!(
        got[8192..12288].iter().all(|b| *b == 0xEE),
        "the logged sector was not replayed"
    );
    assert_eq!(&got[12288..20000], &block0[12288..20000], "after it");
    assert!(got[20000..].iter().all(|b| *b == 0x77), "the write");
}
