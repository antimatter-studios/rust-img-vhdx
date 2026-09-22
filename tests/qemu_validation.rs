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

const HEADER_SLOTS: [u64; 2] = [64 * 1024, 128 * 1024];
const HEADER_SIZE: usize = 4096;
const LOG_VERSION_OFFSET: usize = 64;
const LOG_OFFSET_OFFSET: usize = 72;

/// Overwrite a two-byte field in **both** header slots and repair each
/// slot's CRC-32C, so the image fails for the reason the test is about
/// rather than for a checksum. Repairing the CRC is the whole point: the
/// header checksum covers this field, so a valid-CRC image carrying an
/// unknown value is exactly the shape a future revision of the format
/// would have.
fn patch_both_headers_u16(path: &Path, field_offset: usize, value: u16) {
    let mut bytes = std::fs::read(path).unwrap();
    for slot in HEADER_SLOTS {
        let at = slot as usize;
        assert_eq!(&bytes[at..at + 4], b"head", "slot {slot} is not a header");
        bytes[at + field_offset..at + field_offset + 2].copy_from_slice(&value.to_le_bytes());
        let crc = vhdx::header::compute_crc(&bytes[at..at + HEADER_SIZE])
            .expect("a full-size header slot");
        bytes[at + 4..at + 8].copy_from_slice(&crc.to_le_bytes());
    }
    std::fs::write(path, &bytes).unwrap();
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

/// `log_version` is the format's forward-compatibility latch: version 0
/// is the only log format defined, and a reader that meets another value
/// is required to stop rather than guess.
///
/// The consequence of guessing is a **write**. `open` replays the log
/// before anything else, so descriptors decoded by the version-0 parser
/// out of a log written in some other format get their payloads written
/// into the file's data zones, on top of real data.
///
/// qemu is the arbiter here: it refuses such a file outright, and the
/// test asserts that both implementations agree.
#[test]
fn a_log_version_we_do_not_know_is_refused_like_qemu_refuses_it() {
    let p = tmp_path("log-version");
    qemu_create(&p, "8M");

    // Baseline: qemu wrote log_version 0 and both of us accept it.
    qemu_check(&p);
    VhdxReader::open(&p).expect("the unpatched qemu image must open");

    patch_both_headers_u16(&p, LOG_VERSION_OFFSET, 1);

    let refusal = run_qemu(&["info", p.to_str().unwrap()]);
    assert!(
        !refusal.status.success(),
        "precondition: qemu must refuse log_version 1, but it accepted the file"
    );

    match VhdxReader::open(&p) {
        Err(vhdx::Error::Unsupported(msg)) => assert!(
            msg.contains("log version"),
            "the refusal must name the log version, got {msg:?}"
        ),
        Err(other) => panic!("expected Unsupported, got {other:?}"),
        Ok(_) => panic!(
            "opened a file qemu refuses: {}",
            String::from_utf8_lossy(&refusal.stderr).trim()
        ),
    }
}

/// Add a region entry with `guid`, `required` as given, to **both**
/// region-table copies of an image, repairing each table's CRC-32C.
///
/// Repairing the CRC is the whole point: the check under test is about a
/// well-formed table that names a region the reader does not know, not
/// about a damaged one.
fn add_region_entry(path: &Path, guid: [u8; 16], required: bool) {
    const TABLE_OFFSETS: [usize; 2] = [192 * 1024, 256 * 1024];
    const TABLE_SIZE: usize = 64 * 1024;
    let mut bytes = std::fs::read(path).unwrap();
    for at in TABLE_OFFSETS {
        assert_eq!(
            &bytes[at..at + 4],
            b"regi",
            "table at {at} is not a region table"
        );
        let count = u32::from_le_bytes(bytes[at + 8..at + 12].try_into().unwrap()) as usize;
        let off = at + 16 + count * 32;
        bytes[off..off + 16].copy_from_slice(&guid);
        // A zero-length region at the very end of the file: never read,
        // and never meant to be — the question is whether its flag is
        // honoured, not what is in it.
        bytes[off + 16..off + 24].copy_from_slice(&0u64.to_le_bytes());
        bytes[off + 24..off + 28].copy_from_slice(&0u32.to_le_bytes());
        bytes[off + 28..off + 32].copy_from_slice(&(if required { 1u32 } else { 0 }).to_le_bytes());
        bytes[at + 8..at + 12].copy_from_slice(&((count + 1) as u32).to_le_bytes());
        bytes[at + 4..at + 8].fill(0);
        let crc = vhdx::region_table::compute_crc(&bytes[at..at + TABLE_SIZE])
            .expect("a full-size region table");
        bytes[at + 4..at + 8].copy_from_slice(&crc.to_le_bytes());
    }
    std::fs::write(path, &bytes).unwrap();
}

/// A region's `Required` flag is a hard gate. A region whose GUID the
/// implementation does not recognise, with the flag set, means the file
/// must not be loaded — it is how the format reserves room for a region
/// that *transforms* the payload, an encryption region or a dedup map,
/// without older readers quietly returning the untransformed bytes.
///
/// The flag was parsed onto `RegionEntry` and read by nothing outside
/// the module's own tests, so such an image was read as though the
/// region were not there: BAT found, metadata found, payload blocks
/// returned raw, and no error, because nothing looked.
///
/// qemu refuses such a file. The test asserts both halves — that it
/// refuses, and that we do — so it cannot pass by the patch failing to
/// take effect.
#[test]
fn an_unknown_required_region_is_refused_like_qemu_refuses_it() {
    let p = tmp_path("required-region");
    qemu_create(&p, "8M");
    VhdxReader::open(&p).expect("the unpatched image must open");

    add_region_entry(&p, [0xDEu8; 16], true);

    let refusal = run_qemu(&["info", p.to_str().unwrap()]);
    assert!(
        !refusal.status.success(),
        "precondition: qemu must refuse an unknown required region"
    );
    match VhdxReader::open(&p) {
        Err(vhdx::Error::Unsupported(msg)) => assert!(
            msg.contains("region"),
            "the refusal must name the region, got {msg:?}"
        ),
        Err(other) => panic!("expected Unsupported, got {other:?}"),
        Ok(_) => panic!(
            "opened a file qemu refuses: {}",
            String::from_utf8_lossy(&refusal.stderr).trim()
        ),
    }
}

/// The same region with the flag *clear* is the format saying "ignore
/// me if you do not know me", and the image must still open and read.
/// This is what makes the check a gate rather than a blanket refusal of
/// unknown regions.
#[test]
fn an_unknown_optional_region_is_ignored() {
    let raw = raw_path("optional-region");
    let p = tmp_path("optional-region");
    let data = pattern(1024 * 1024);
    std::fs::write(&raw, &data).unwrap();
    qemu_convert_raw_to_vhdx(&raw, &p);

    add_region_entry(&p, [0xDEu8; 16], false);

    let r = VhdxReader::open(&p).expect("an unknown optional region must be ignored");
    let mut buf = vec![0u8; 4096];
    r.read_at(0, &mut buf).unwrap();
    assert_eq!(buf, data[..4096]);
}

/// The metadata region's file offset, read out of the first region
/// table rather than assumed.
fn metadata_region_offset(bytes: &[u8]) -> usize {
    let at = 192 * 1024;
    assert_eq!(&bytes[at..at + 4], b"regi", "no region table at {at}");
    let count = u32::from_le_bytes(bytes[at + 8..at + 12].try_into().unwrap()) as usize;
    (0..count)
        .map(|i| at + 16 + i * 32)
        .find(|off| bytes[*off..*off + 16] == vhdx::region_table::guids::METADATA)
        .map(|off| u64::from_le_bytes(bytes[off + 16..off + 24].try_into().unwrap()) as usize)
        .expect("the region table names a metadata region")
}

/// The metadata table's entries as `(item_id, flags)`.
fn metadata_items(path: &Path) -> Vec<([u8; 16], u32)> {
    let bytes = std::fs::read(path).unwrap();
    let at = metadata_region_offset(&bytes);
    assert_eq!(&bytes[at..at + 8], b"metadata", "no metadata table at {at}");
    let count = u16::from_le_bytes([bytes[at + 10], bytes[at + 11]]) as usize;
    (0..count)
        .map(|i| {
            let off = at + 32 + i * 32;
            let id: [u8; 16] = bytes[off..off + 16].try_into().unwrap();
            let flags = u32::from_le_bytes(bytes[off + 24..off + 28].try_into().unwrap());
            (id, flags)
        })
        .collect()
}

/// Append a metadata entry to a qemu image, reusing the last entry's
/// item data so the table stays in bounds. The metadata table carries no
/// checksum, so nothing else needs repair.
fn add_metadata_item(path: &Path, item_id: [u8; 16], flags: u32) {
    let mut bytes = std::fs::read(path).unwrap();
    let at = metadata_region_offset(&bytes);
    let count = u16::from_le_bytes([bytes[at + 10], bytes[at + 11]]) as usize;
    let last = at + 32 + (count - 1) * 32;
    let off = at + 32 + count * 32;
    let data_location: [u8; 8] = bytes[last + 16..last + 24].try_into().unwrap();
    bytes[off..off + 16].copy_from_slice(&item_id);
    bytes[off + 16..off + 24].copy_from_slice(&data_location);
    bytes[off + 24..off + 28].copy_from_slice(&flags.to_le_bytes());
    bytes[at + 10..at + 12].copy_from_slice(&((count + 1) as u16).to_le_bytes());
    std::fs::write(path, &bytes).unwrap();
}

/// Metadata entry flag bit 2.
const METADATA_IS_REQUIRED: u32 = 0x4;

/// THE ORDERING PROOF FOR #44: an image qemu writes carries required
/// metadata items beyond the three this crate decodes -- Page 83 Data and
/// PhysicalSectorSize -- and must still open. A required-item gate that
/// landed before those two were recognised would refuse every image the
/// reference tool writes; this is the test that says so.
#[test]
fn a_stock_qemu_image_with_required_items_we_do_not_decode_still_opens() {
    const PAGE_83_DATA: [u8; 16] = [
        0xAB, 0x12, 0xCA, 0xBE, 0xE6, 0xB2, 0x23, 0x45, 0x93, 0xEF, 0xC3, 0x09, 0xE0, 0x00, 0xC7,
        0x46,
    ];
    const PHYSICAL_SECTOR_SIZE: [u8; 16] = [
        0xC7, 0x48, 0xA3, 0xCD, 0x5D, 0x44, 0x71, 0x44, 0x9C, 0xC9, 0xE9, 0x88, 0x52, 0x51, 0xC5,
        0x56,
    ];
    let p = tmp_path("stock-required-items");
    qemu_create(&p, "8M");

    // Precondition: the fixture really does carry both, marked required,
    // or this test proves nothing about the gate.
    let items = metadata_items(&p);
    for (name, id) in [
        ("Page 83 Data", PAGE_83_DATA),
        ("PhysicalSectorSize", PHYSICAL_SECTOR_SIZE),
    ] {
        assert!(
            items
                .iter()
                .any(|(item, flags)| *item == id && flags & METADATA_IS_REQUIRED != 0),
            "precondition: qemu's image must carry {name} marked required, got {items:?}"
        );
    }

    VhdxReader::open(&p).expect("an image qemu writes must open");
}

/// A metadata item qemu does not recognise, marked required, is a file
/// qemu refuses -- and so do we (#44). Both halves are asserted, so the
/// test cannot pass by the patch failing to take effect.
#[test]
fn an_unknown_required_metadata_item_is_refused_like_qemu_refuses_it() {
    let p = tmp_path("required-metadata-item");
    qemu_create(&p, "8M");
    VhdxReader::open(&p).expect("the unpatched image must open");

    add_metadata_item(&p, [0xFF; 16], 0x2 | METADATA_IS_REQUIRED);

    let refusal = run_qemu(&["info", p.to_str().unwrap()]);
    assert!(
        !refusal.status.success(),
        "precondition: qemu must refuse an unknown required metadata item"
    );
    match VhdxReader::open(&p) {
        Err(vhdx::Error::Unsupported(msg)) => assert!(
            msg.contains("metadata item"),
            "the refusal must name the metadata item, got {msg:?}"
        ),
        Err(other) => panic!("expected Unsupported, got {other:?}"),
        Ok(_) => panic!(
            "opened a file qemu refuses: {}",
            String::from_utf8_lossy(&refusal.stderr).trim()
        ),
    }
}

/// The same item with the flag clear: qemu opens it, and so do we, and
/// the payload still reads.
#[test]
fn an_unknown_optional_metadata_item_is_ignored_like_qemu_ignores_it() {
    let raw = raw_path("optional-metadata-item");
    let p = tmp_path("optional-metadata-item");
    let data = pattern(1024 * 1024);
    std::fs::write(&raw, &data).unwrap();
    qemu_convert_raw_to_vhdx(&raw, &p);

    add_metadata_item(&p, [0xFF; 16], 0x2);

    let info = run_qemu(&["info", p.to_str().unwrap()]);
    assert!(
        info.status.success(),
        "precondition: qemu must accept an unknown optional metadata item: {}",
        String::from_utf8_lossy(&info.stderr).trim()
    );
    let r = VhdxReader::open(&p).expect("an unknown optional item must be ignored");
    let mut buf = vec![0u8; 4096];
    r.read_at(0, &mut buf).unwrap();
    assert_eq!(buf, data[..4096]);
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

/// Overwrite the eight-byte `log_offset` in **both** header slots and
/// repair each slot's CRC-32C, for the same reason
/// [`patch_both_headers_u16`] does: the check under test is about a
/// well-formed header carrying a value it should not, not a damaged one.
fn patch_both_headers_log_offset(path: &Path, value: u64) {
    let mut bytes = std::fs::read(path).unwrap();
    for slot in HEADER_SLOTS {
        let at = slot as usize;
        assert_eq!(&bytes[at..at + 4], b"head", "slot {slot} is not a header");
        bytes[at + LOG_OFFSET_OFFSET..at + LOG_OFFSET_OFFSET + 8]
            .copy_from_slice(&value.to_le_bytes());
        let crc = vhdx::header::compute_crc(&bytes[at..at + HEADER_SIZE])
            .expect("a full-size header slot");
        bytes[at + 4..at + 8].copy_from_slice(&crc.to_le_bytes());
    }
    std::fs::write(path, &bytes).unwrap();
}

/// The file offset and length of the BAT region, read out of a real
/// image's own region table.
fn bat_region_of(path: &Path) -> (u64, u32) {
    const TABLE_OFFSET: usize = 192 * 1024;
    const BAT_GUID: [u8; 16] = [
        0x66, 0x77, 0xC2, 0x2D, 0x23, 0xF6, 0x00, 0x42, 0x9D, 0x64, 0x11, 0x5E, 0x9B, 0xFD, 0x4A,
        0x08,
    ];
    let bytes = std::fs::read(path).unwrap();
    let count = u32::from_le_bytes(
        bytes[TABLE_OFFSET + 8..TABLE_OFFSET + 12]
            .try_into()
            .unwrap(),
    ) as usize;
    for i in 0..count {
        let off = TABLE_OFFSET + 16 + i * 32;
        if bytes[off..off + 16] == BAT_GUID {
            return (
                u64::from_le_bytes(bytes[off + 16..off + 24].try_into().unwrap()),
                u32::from_le_bytes(bytes[off + 24..off + 28].try_into().unwrap()),
            );
        }
    }
    panic!("no BAT region in {}", path.display());
}

/// A log region that lands on another region is refused, and the
/// reference tool refuses it by name.
///
/// `log_offset` and `log_length` were validated only inside the branch
/// that runs when the log is dirty, so a clean image — which is what
/// every properly-closed image is — never had them looked at, and
/// `journal_sector_write` then zeroed `log_length` bytes at `log_offset`
/// before splicing its entry in. One 4 KiB write to such a file erased
/// whatever the two fields named. Measured on our own fixture with the
/// log pointed at the metadata region:
///
/// ```text
/// metadata before: [6d, 65, 74, 61, 64, 61, 74, 61]   "metadata"
/// write_at -> Ok(())
/// metadata after:  [6c, 6f, 67, 65, 88, ee, ce, c8]   "loge"
/// reopen -> BadMetadata("signature mismatch")
/// ```
///
/// qemu is the arbiter, and this asserts both halves so the test cannot
/// pass by the patch failing to take effect.
#[test]
fn a_log_region_on_top_of_another_region_is_refused_like_qemu_refuses_it() {
    let p = tmp_path("log-over-region");
    qemu_create(&p, "8M");
    qemu_check(&p);
    VhdxReader::open(&p).expect("the unpatched qemu image must open");

    let (bat_offset, _) = bat_region_of(&p);
    patch_both_headers_log_offset(&p, bat_offset);

    let refusal = run_qemu(&["info", p.to_str().unwrap()]);
    assert!(
        !refusal.status.success(),
        "precondition: qemu must refuse a log region over the BAT, but it accepted the file"
    );

    match VhdxReader::open_rw(&p) {
        Err(vhdx::Error::Corrupt(msg)) => assert!(
            msg.contains("log region"),
            "the refusal must name the log region, got {msg:?}"
        ),
        Err(other) => panic!("expected Corrupt, got {other:?}"),
        Ok(_) => panic!(
            "opened a file qemu refuses: {}",
            String::from_utf8_lossy(&refusal.stderr).trim()
        ),
    }
}
