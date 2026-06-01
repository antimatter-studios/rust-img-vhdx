//! Cross-validation against `qemu-img`.
//!
//! Gated behind the `qemu-validation` feature so regular `cargo test`
//! does not require qemu-img on PATH. Run with:
//!
//!     cargo test --features qemu-validation --test qemu_validation
//!
//! Licensing posture: `qemu-img` is invoked as a separate OS process.
//! No QEMU source or binary is linked into this crate, and `qemu-img`
//! is never bundled into a release artifact. Reading bytes that a GPL
//! tool happens to produce, or feeding it bytes for validation, does
//! not create a derivative work.
//!
//! These tests cross-check three directions that a self-consistent
//! reader/writer cannot validate on its own:
//!
//!   1. cross-read   — qemu-img *produces* a VHDX, our reader consumes
//!      it. Catches header/region/metadata/BAT fields we mis-parse from
//!      a real Microsoft-format emitter rather than our own builder.
//!   2. cross-write  — our writer *mutates* a VHDX, qemu-img replays its
//!      log and validates structure, then extracts the bytes. Catches
//!      log/BAT encodings that look valid to us but not to qemu.
//!   3. metadata     — qemu-img info reports the same virtual-size /
//!      block-size we read.
//!
//! NOTE on the log: our writer commits through the VHDX log and leaves
//! it pending for replay-on-next-open (the log is the durability
//! mechanism). qemu refuses a *read-only* open of a log-dirty image, so
//! every cross-write check first runs `qemu-img check -r all`, which
//! replays our log and then reports a clean image.

#![cfg(feature = "qemu-validation")]

use std::path::{Path, PathBuf};
use std::process::Command;

use vhdx::VhdxReader;

const QEMU_IMG: &str = "qemu-img";

fn run_qemu(args: &[&str]) -> std::process::Output {
    Command::new(QEMU_IMG)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to invoke `{QEMU_IMG}` ({e}); install qemu-utils?"))
}

fn assert_qemu(args: &[&str]) {
    let out = run_qemu(args);
    assert!(
        out.status.success(),
        "`qemu-img {}` failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

fn tmp_path(name: &str) -> TempPath {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!("vhdx_qemu_{}_{n}_{name}.vhdx", std::process::id()));
    TempPath(p)
}

/// RAII temp-file path: removes the backing file on drop so a panicking
/// assertion can't leak fixtures into the temp dir across CI runs.
struct TempPath(PathBuf);
impl std::ops::Deref for TempPath {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}
impl AsRef<Path> for TempPath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn raw_path(name: &str) -> TempPath {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!("vhdx_qemu_{}_{n}_{name}.raw", std::process::id()));
    TempPath(p)
}

fn qemu_create(path: &Path, size: &str) {
    assert_qemu(&["create", "-f", "vhdx", path.to_str().unwrap(), size]);
}

fn qemu_check(path: &Path) {
    assert_qemu(&["check", "-f", "vhdx", path.to_str().unwrap()]);
}

/// Replay any pending log and repair, then assert the image is clean.
/// Used after our writer mutates an image: qemu treats the pending log
/// as a repairable inconsistency, replays it, and the *second* plain
/// check must then find no errors.
fn qemu_replay_then_check_clean(path: &Path) {
    // First pass: allow replay/repair. This may report "N corruptions
    // ... repaired" purely from replaying our log — that is expected
    // and not a failure.
    let repair = run_qemu(&["check", "-f", "vhdx", "-r", "all", path.to_str().unwrap()]);
    assert!(
        repair.status.success(),
        "`qemu-img check -r all` failed:\n{}",
        String::from_utf8_lossy(&repair.stderr),
    );
    // Second pass: after replay the image must be pristine.
    qemu_check(path);
}

fn qemu_convert_raw_to_vhdx(raw: &Path, vhdx: &Path) {
    assert_qemu(&[
        "convert",
        "-f",
        "raw",
        "-O",
        "vhdx",
        raw.to_str().unwrap(),
        vhdx.to_str().unwrap(),
    ]);
}

fn qemu_convert_vhdx_to_raw(vhdx: &Path, raw: &Path) {
    assert_qemu(&[
        "convert",
        "-f",
        "vhdx",
        "-O",
        "raw",
        vhdx.to_str().unwrap(),
        raw.to_str().unwrap(),
    ]);
}

fn qemu_info_json(path: &Path) -> serde_json::Value {
    let out = run_qemu(&["info", "--output=json", path.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "qemu-img info failed:\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("qemu-img info JSON must parse")
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Sanity: qemu-img is reachable. If this fails every other test here
/// would fail uselessly, so it gives the clearest diagnostic.
#[test]
fn qemu_img_is_callable() {
    let out = run_qemu(&["--version"]);
    assert!(
        out.status.success(),
        "qemu-img --version exited non-zero — qemu-utils not installed?"
    );
}

/// Direction 1 (structural): qemu's own empty VHDX passes its own check
/// on this host — establishes the baseline.
#[test]
fn qemu_check_passes_on_empty_qemu_image() {
    let p = tmp_path("empty");
    qemu_create(&p, "4M");
    qemu_check(&p);
}

/// Direction 1 (cross-read, trivial): a blank qemu VHDX reads as all
/// zeros through our reader, and we report the geometry qemu encoded.
/// Misparsing the header/metadata would corrupt the BAT walk and
/// surface as non-zero garbage or a wrong virtual size.
#[test]
fn our_reader_returns_zeros_and_geometry_for_empty_qemu_image() {
    let p = tmp_path("zeros");
    qemu_create(&p, "4M");

    let r = VhdxReader::open(&p).unwrap();
    assert_eq!(r.virtual_size(), 4 * 1024 * 1024);
    assert_eq!(r.sector_size(), 512);
    assert!(!r.has_parent());

    let mut buf = vec![0u8; 65_536];
    r.read_at(0, &mut buf).unwrap();
    assert!(
        buf.iter().all(|&b| b == 0),
        "expected all-zero read from empty qemu image"
    );
}

/// Direction 1 (cross-read, populated): convert a raw file with a known
/// pattern into VHDX via qemu, then read it back with our reader and
/// compare byte-for-byte. Validates our FullyPresent-block decode
/// against a real qemu layout.
#[test]
fn our_reader_matches_qemu_populated_pattern() {
    let raw = raw_path("pat-src");
    let vhdx = tmp_path("pat-dst");

    let data = pattern(256 * 1024);
    std::fs::write(&raw, &data).unwrap();
    qemu_convert_raw_to_vhdx(&raw, &vhdx);

    let r = VhdxReader::open(&vhdx).unwrap();
    let mut buf = vec![0u8; data.len()];
    r.read_at(0, &mut buf).unwrap();
    assert_eq!(buf, data, "byte mismatch reading qemu-produced image");
}

/// Direction 1 (cross-read, multi-block): a pattern larger than qemu's
/// default 8 MiB block forces reads across block boundaries, exercising
/// `data_bat_index` against a real multi-block layout.
#[test]
fn our_reader_matches_qemu_pattern_across_multiple_blocks() {
    let raw = raw_path("multi-src");
    let vhdx = tmp_path("multi-dst");

    // 20 MiB > 2 default blocks (8 MiB each).
    let data = pattern(20 * 1024 * 1024);
    std::fs::write(&raw, &data).unwrap();
    qemu_convert_raw_to_vhdx(&raw, &vhdx);

    let r = VhdxReader::open(&vhdx).unwrap();
    // Read a window straddling the first block boundary (8 MiB).
    let start = 8 * 1024 * 1024 - 4096;
    let mut buf = vec![0u8; 8192];
    r.read_at(start as u64, &mut buf).unwrap();
    assert_eq!(buf, data[start..start + 8192]);

    // And a window straddling the second boundary (16 MiB).
    let start2 = 16 * 1024 * 1024 - 1000;
    let mut buf2 = vec![0u8; 4000];
    r.read_at(start2 as u64, &mut buf2).unwrap();
    assert_eq!(buf2, data[start2..start2 + 4000]);
}

/// Direction 3 (metadata): qemu-img info reports the same virtual size
/// and block (cluster) size our reader sees.
#[test]
fn qemu_info_matches_our_reader_geometry() {
    let p = tmp_path("info");
    qemu_create(&p, "8M");

    let info = qemu_info_json(&p);
    assert_eq!(info["format"], "vhdx");
    let qemu_vsize = info["virtual-size"].as_u64().unwrap();
    let qemu_block = info["cluster-size"].as_u64().unwrap();

    let r = VhdxReader::open(&p).unwrap();
    assert_eq!(r.virtual_size(), qemu_vsize);
    assert_eq!(r.block_size() as u64, qemu_block);
}

/// Direction 2 (cross-write, structural): create with qemu, mutate with
/// our writer, then have qemu replay our log and validate. Catches
/// log-entry / BAT encodings that look valid to us but not to qemu.
#[test]
fn qemu_replays_and_validates_image_we_wrote() {
    let p = tmp_path("we-wrote-check");
    qemu_create(&p, "4M");

    let r = VhdxReader::open_rw(&p).unwrap();
    r.write_at(0, b"vhdx written by our crate").unwrap();
    r.flush().unwrap();
    drop(r);

    qemu_replay_then_check_clean(&p);
}

/// Direction 2 (cross-write, content): the strongest single check —
/// write bytes via our crate, let qemu replay the log and convert the
/// image to raw, and verify the bytes survived. Fails if our writer
/// produced spec-valid-looking bytes that qemu interprets differently.
#[test]
fn qemu_extracts_bytes_we_wrote() {
    let vhdx = tmp_path("we-wrote-convert");
    let raw = raw_path("we-wrote-convert");
    qemu_create(&vhdx, "4M");

    let payload = b"bytes-qemu-must-see-back-0123456789";
    let r = VhdxReader::open_rw(&vhdx).unwrap();
    r.write_at(4096, payload).unwrap();
    r.flush().unwrap();
    drop(r);

    // Replay our pending log so qemu will open the image, then extract.
    qemu_replay_then_check_clean(&vhdx);
    qemu_convert_vhdx_to_raw(&vhdx, &raw);

    let out = std::fs::read(&raw).unwrap();
    assert_eq!(&out[4096..4096 + payload.len()], payload);
    assert!(
        out[..4096].iter().all(|&b| b == 0),
        "bytes before the write offset should be zero"
    );
}
