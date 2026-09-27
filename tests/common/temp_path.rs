//! A fixture path that removes itself, panic or not — the one definition.
//!
//! A CLEANUP AT THE END OF A TEST NEVER RUNS ON THE FAILURE PATH, and the
//! failure path is exactly when a fixture is most likely to be left behind
//! and least likely to be noticed, because attention is on the failure.
//! `tests/corruption.rs` had 38 bare `let _ = std::fs::remove_file(&path);`
//! lines at the ends of its tests; three of those tests build 64 MiB sparse
//! images, and a red test is the one a developer then reruns in a loop
//! (#107). `Drop` runs during unwind, which is the whole difference.
//!
//! WHY ITS OWN FILE. `tests/qemu_validation.rs` had a second copy of this
//! type, written out again because it does not use the fixture builders in
//! `common/mod.rs` and importing that module for one RAII wrapper would drag
//! them in. A `#[path]` module reaches this file alone, so the two targets
//! share the definition without sharing anything else — and the copy that
//! would have drifted is gone.
//!
//! NAMING STAYS WITH THE CALLER. The two targets write different file names
//! (`vhdx_synth_…vhdx`, `vhdx_qemu_…raw`) and that is their business;
//! [`TempPath::named`] takes the whole file name and this file has no opinion
//! about it.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// A path in the system temp directory whose file is removed on drop.
///
/// Derefs to `Path` and implements `AsRef<Path>`, so a call site passes it
/// wherever it passed a `PathBuf` before.
pub struct TempPath(pub PathBuf);

impl TempPath {
    /// `file_name` inside the system temp directory.
    ///
    /// The caller is responsible for making it unique: a name carrying the
    /// process id and a per-process counter means a crashed run leaves an
    /// identifiable corpse rather than colliding with the next run.
    pub fn named(file_name: String) -> Self {
        let mut p = std::env::temp_dir();
        p.push(file_name);
        TempPath(p)
    }
}

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

/// THE GUARD, BECAUSE A NEUTERED `Drop` BODY LEAVES THE SUITE GREEN.
///
/// Every test in this repository cleans up on its way out whether the
/// removal works or not, so nothing else in the suite depends on it. Empty
/// the body of `Drop` and the only thing that changes is the contents of the
/// temp directory — an absence that reads exactly like a pass, which is the
/// shape of defect this repository keeps meeting.
///
/// It runs in every target that includes this file, which is the point: each
/// one gets the assertion rather than trusting another target's.
#[cfg(test)]
mod fixture_cleanup {
    use super::TempPath;
    use std::path::PathBuf;

    /// A name unique to this process and this call.
    fn probe_name() -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        format!("vhdx_drop_probe_{}_{n}.tmp", std::process::id())
    }

    #[test]
    fn a_panicking_test_still_removes_its_fixture() {
        let left = panicking_probe_left_behind();
        assert!(
            left.is_none(),
            "a panicking test left its fixture behind: {left:?}"
        );
    }

    /// Make a fixture inside a closure that panics, and report whether
    /// **that exact file** outlived it.
    ///
    /// Exact, not a scan of the temp directory for the prefix. Several test
    /// binaries include this file and share one temp directory, and a crashed
    /// run leaves its probe behind; a prefix scan would fail every later run
    /// on that corpse. Narrowing to this pid is not enough either — pids are
    /// reused and the per-process counter restarts at zero, so a stale probe
    /// can carry this very name. Only the path this call created answers the
    /// question.
    fn panicking_probe_left_behind() -> Option<PathBuf> {
        let created = std::sync::Mutex::new(None);
        let outcome = std::panic::catch_unwind(|| {
            let p = TempPath::named(probe_name());
            *created.lock().unwrap() = Some(p.0.clone());
            std::fs::write(&p.0, b"x").expect("write fixture");
            assert!(p.0.exists(), "the fixture was created");
            panic!("deliberate");
        });
        assert!(outcome.is_err(), "the closure must have panicked");
        let path = created
            .into_inner()
            .unwrap()
            .expect("the closure recorded its fixture");
        path.exists().then_some(path)
    }

    /// Corpses in the temp directory — another process's, and one carrying
    /// this process's pid as a reused pid would — are not this test's
    /// fixture and must not fail it.
    #[test]
    fn probes_left_by_other_runs_do_not_fail_the_drop_test() {
        let pid = std::process::id();
        // Both names are unique to this process, so binaries running this
        // test side by side never plant or remove each other's.
        let foreign = TempPath::named("vhdx_drop_probe_0_9999.tmp".to_string());
        let reused_pid = TempPath::named(format!("vhdx_drop_probe_{pid}_4000000000.tmp"));
        std::fs::write(&foreign.0, b"corpse").unwrap();
        std::fs::write(&reused_pid.0, b"corpse").unwrap();

        assert!(
            panicking_probe_left_behind().is_none(),
            "a corpse from another run failed the drop test"
        );
        assert!(
            foreign.0.exists() && reused_pid.0.exists(),
            "the probe's own removal took another run's corpse with it"
        );
    }
}
