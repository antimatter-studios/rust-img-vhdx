//! `scripts/tier.sh` resolves the output-budget wrapper from rust-fs-core,
//! and refuses anything else.
//!
//! # WHAT THIS FILE USED TO GUARD, AND WHY IT NO LONGER CAN
//!
//! This repository carried `scripts/output-budget.sh` as a committed copy of
//! the wrapper every test tier runs through, and this file pinned that copy's
//! BEHAVIOUR -- exit 65 for a breached budget, the command's own status for a
//! failure, the tail on failure, silence on success. That was the right guard
//! for a copy: a copy drifts, and holding it to its behaviour rather than to
//! its bytes let a rewrite be a fine copy while a regression was not.
//!
//! The copy is gone (rust-fs-core#153). The canonical wrapper lives in
//! rust-fs-core and `scripts/tier.sh` resolves it at run time, so asserting
//! the wrapper's own promises here would be this repository grading another
//! repository's file -- a test that fails when somebody else's release
//! changes, in a suite that cannot fix it. rust-fs-core owns those tests now.
//!
//! # WHAT IT GUARDS INSTEAD
//!
//! The RESOLVER, which is this repository's code and this repository's
//! problem:
//!
//! 1. a tier runs its command through a wrapper it resolved -- a pass is
//!    quiet and names its log, a failure hands on the COMMAND's status and
//!    still shows its reason;
//! 2. a tier that breaches its budget, in lines or in bytes, exits 65;
//! 3. the resolver REFUSES a core that is absent, and REFUSES one that is
//!    present but answers the wrong `--version`. It does not fall through to
//!    another source and it does not fall back to anything local: a wrapper
//!    nobody chose is the drift this arrangement removes, wearing a hat;
//! 4. `--verbose` reaches the wrapper under its NEW name and the old name is
//!    dead. That rename fails silently -- `FLTH_VERBOSE=1` does not error,
//!    the run just stays quiet -- so it is pinned rather than trusted;
//! 5. the copy the run takes is deleted when the run ends.
//!
//! # WHY THE VERSION STRING AND NOT A DIGEST
//!
//! Byte-pinning the wrapper (a SHA-256 in this repository, checked before
//! use) was considered and rejected. A digest in seven repositories has to be
//! raised in seven repositories for every edit to one file, which is the
//! lockstep this migration exists to remove, and the failure it produces
//! names a hash rather than a problem. `rust-fs-core-output-budget 1` is an
//! API version: it is checked on every run, and it moves when the interface
//! moves rather than when a comment does.
//!
//! # NOTHING HERE SKIPS
//!
//! Every test below runs `scripts/tier.sh` for real. If `bash` is missing, or
//! a core holding the wrapper cannot be resolved, these tests FAIL and say
//! what would provide it -- `../rust-fs-core` at v0.2.11 or later, or
//! `FS_CORE_ROOT` naming a checkout of one. A test that skipped when the
//! wrapper was unreachable would report protection it is not providing, in
//! exactly the situation where every tier in CI is about to break.
//!
//! The other half of the arrangement -- that every tier in `ci.yml` and
//! `chores.yml` actually GOES through `scripts/tier.sh`, under a non-zero
//! budget, with the two files agreeing on the numbers -- is in
//! `tests/ci_profile.rs`, which is the file that already parses both.

use std::path::PathBuf;
use std::process::{Command, Output};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The `bash` that can actually run a shell script.
///
/// # `bash` ON PATH IS NOT BASH ON A WINDOWS RUNNER
///
/// `C:\Windows\System32\bash.exe` is the WSL launcher, it ships with the
/// operating system, and `System32` comes early in `PATH` -- so
/// `Command::new("bash")` finds it before Git Bash. With no WSL
/// distribution installed it prints nothing useful and exits 1, which is
/// how every test in this file failed on `windows-latest` while the same
/// scripts ran perfectly in the workflow: an Actions step that says
/// `shell: bash` is handed Git Bash by name and never consults `PATH`.
///
/// So this asks for Git Bash by name on Windows and falls back to `PATH`
/// elsewhere -- and, if that file is not there, still falls back to `PATH`
/// rather than deciding the host cannot run the suite.
fn bash() -> PathBuf {
    if cfg!(windows) {
        let git_bash = PathBuf::from(r"C:\Program Files\Git\bin\bash.exe");
        if git_bash.is_file() {
            return git_bash;
        }
    }
    PathBuf::from("bash")
}

/// Run `scripts/tier.sh` from the repository root.
///
/// FROM THE ROOT, WITH RELATIVE PATHS, because this suite runs on
/// windows-latest, where `bash` is Git Bash and an absolute Windows path
/// handed to it as an argument is a path with backslashes in it. A relative
/// path plus a working directory is the one spelling that means the same
/// thing on all three runners -- which is also why every `FS_CORE_ROOT`
/// below is relative.
fn run_tier(environment: &[(&str, &str)], arguments: &[&str]) -> Output {
    let mut command = Command::new(bash());
    command
        .current_dir(repo())
        .arg("scripts/tier.sh")
        .args(arguments);
    for (name, value) in environment {
        command.env(name, value);
    }
    command.output().unwrap_or_else(|e| {
        panic!(
            "could not run `{} scripts/tier.sh`: {e}. Every test tier in this \
             repository runs through that script, so a host without `bash` \
             cannot run this suite -- which is why this is a failure and not a \
             skip. On Windows, Git Bash provides it.",
            bash().display()
        )
    })
}

fn printed(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string() + &String::from_utf8_lossy(&output.stderr)
}

/// A scratch directory under `tmp/`, which is gitignored and is where the
/// tier logs live. Returned as a path RELATIVE to the repository root.
fn scratch(name: &str) -> String {
    let directory = repo().join("tmp").join("output-budget-test").join(name);
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory)
        .unwrap_or_else(|e| panic!("could not create tmp/output-budget-test/{name}: {e}"));
    format!("tmp/output-budget-test/{name}")
}

/// A scratch core checkout holding a wrapper that answers `version` to
/// `--version`, and whose body is `body`.
///
/// `marker` goes into the file as a comment, so a copy of it can be told
/// apart from the copies other tests in this binary are taking of the real
/// wrapper at the same time.
fn fake_core(name: &str, version: &str, marker: &str, body: &str) -> String {
    let root = scratch(name);
    let scripts = repo().join(&root).join("scripts");
    std::fs::create_dir_all(&scripts).expect("could not create the fake core's scripts/");
    let script = format!(
        "#!/usr/bin/env bash\n# {marker}\nif [ \"${{1:-}}\" = --version ]; then\n\
         \x20   echo '{version}'\n    exit 0\nfi\n{body}\n"
    );
    std::fs::write(scripts.join("output-budget.sh"), script)
        .expect("could not write the fake core's wrapper");
    root
}

/// The log a tier writes, read back from `tmp/logs/`.
fn tier_log(name: &str) -> String {
    let path = repo().join("tmp").join("logs").join(format!("{name}.log"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the tier left no log at {}: {e}", path.display()))
}

/// Every `tmp/output-budget.<pid>.sh` whose text carries `marker`.
///
/// The marker is what makes looking in `tmp/` safe while the rest of this
/// binary is running: other tests are taking their own copies of the real
/// wrapper into the same directory at the same time, so counting files would
/// be a race.
fn marked_copies(marker: &str) -> Vec<PathBuf> {
    let tmp = repo().join("tmp");
    let Ok(entries) = std::fs::read_dir(&tmp) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("output-budget.") && name.ends_with(".sh"))
                && std::fs::read_to_string(path)
                    .map(|text| text.contains(marker))
                    .unwrap_or(false)
        })
        .collect()
}

/// A command that prints 40 lines and succeeds.
const LOUD: &str = "i=0; while [ $i -lt 40 ]; do echo line $i; i=$((i+1)); done";

#[test]
fn a_passing_tier_is_quiet_and_names_the_log_it_kept_everything_in() {
    let output = run_tier(
        &[],
        &[
            "quiet-tier",
            "ob-quiet",
            "5",
            "0",
            "--",
            "echo",
            "hello-from-the-command",
        ],
    );

    assert!(
        output.status.success(),
        "a tier inside its budget failed: {}\n{}",
        output.status,
        printed(&output)
    );
    let shown = printed(&output);
    assert!(
        !shown.contains("hello-from-the-command"),
        "a passing tier put the command's output on the terminal, which is \
         the whole thing the wrapper exists to stop:\n{shown}"
    );
    assert!(
        shown.contains("quiet-tier: ok"),
        "a passing tier printed no verdict line, so the reader is told \
         nothing at all -- quiet is not the same as silent:\n{shown}"
    );
    assert!(
        shown.contains("ob-quiet.log"),
        "the verdict line does not name the log, so the output that was \
         withheld cannot be found:\n{shown}"
    );
    assert_eq!(
        tier_log("ob-quiet").trim(),
        "hello-from-the-command",
        "the tier's log did not keep the command's output. The budget is not \
         a gag: everything goes to the log whatever happens to the terminal."
    );
}

#[test]
fn a_failing_tier_hands_on_the_commands_own_status_and_shows_its_reason() {
    let output = run_tier(
        &[],
        &[
            "failing-tier",
            "ob-failing",
            "5",
            "0",
            "--",
            "sh",
            "-c",
            "echo the-reason-it-failed; exit 7",
        ],
    );

    assert_eq!(
        output.status.code(),
        Some(7),
        "a failing tier must exit with the COMMAND's status, not the \
         wrapper's. This is the `cargo test | tee log` defect that \
         tests/ci_profile.rs refuses in the workflow -- a red suite reading \
         green because something else in the pipeline succeeded.\n{}",
        printed(&output)
    );
    let shown = printed(&output);
    assert!(
        shown.contains("the-reason-it-failed"),
        "a failure printed no excerpt of the run. rust-fs-core#164 made the \
         wrapper's failure path quiet unless a tail is asked for, so \
         scripts/tier.sh asks for one -- a quiet suite is only affordable if \
         a failure still shows its reason without anybody fetching a \
         file:\n{shown}"
    );
}

#[test]
fn a_tier_that_passed_but_printed_too_many_lines_exits_65() {
    let output = run_tier(
        &[],
        &["loud-tier", "ob-loud", "5", "0", "--", "sh", "-c", LOUD],
    );

    assert_eq!(
        output.status.code(),
        Some(65),
        "a tier over its line budget must exit 65 -- a status of its own, so \
         a suite that printed too much is not mistaken for a suite that \
         failed.\n{}",
        printed(&output)
    );
    assert_eq!(
        tier_log("ob-loud").lines().count(),
        40,
        "a breached budget lost the output. Everything goes to the log."
    );
}

#[test]
fn a_tier_that_passed_but_printed_too_many_bytes_exits_65_too() {
    // Lines and bytes are two budgets because one long line is not one
    // line's worth of reading. `0` is how tier.sh spells "no budget" for the
    // other one, which is why tests/ci_profile.rs refuses a tier that passes
    // 0 for both.
    let output = run_tier(
        &[],
        &[
            "fat-tier",
            "ob-fat",
            "0",
            "10",
            "--",
            "echo",
            "rather more than ten bytes",
        ],
    );

    assert_eq!(
        output.status.code(),
        Some(65),
        "a tier over its BYTE budget passed.\n{}",
        printed(&output)
    );
}

#[test]
fn the_resolver_refuses_a_core_that_does_not_hold_the_wrapper() {
    // An empty directory is a core that is absent as far as the wrapper is
    // concerned -- a checkout below v0.2.11, or a clone that never happened.
    let empty = scratch("core-without-the-wrapper");
    let output = run_tier(
        &[("FS_CORE_ROOT", &empty)],
        &[
            "absent-core",
            "ob-absent",
            "5",
            "0",
            "--",
            "echo",
            "this must never run",
        ],
    );

    assert_eq!(
        output.status.code(),
        Some(1),
        "a tier whose wrapper could not be resolved did not fail. It must: \
         every tier in CI runs through it, and a tier that silently ran \
         unwrapped is an unbudgeted, unlogged run reporting the same green as \
         a real one.\n{}",
        printed(&output)
    );
    let shown = printed(&output);
    for expected in [
        "output-budget.sh",
        "rust-fs-core",
        "rust-fs-core-output-budget 1",
        "v0.2.11",
    ] {
        assert!(
            shown.contains(expected),
            "the refusal does not mention {expected:?}, so the reader is not \
             told what would fix it -- which core, which file, which API \
             string, which minimum release:\n{shown}"
        );
    }
}

#[test]
fn the_resolver_refuses_a_wrapper_that_answers_the_wrong_version() {
    // PRESENT BUT WRONG IS FATAL, and this is the case a digest would also
    // have caught -- at the cost of having to be raised in seven
    // repositories every time one file changed. The API string is the
    // contract instead.
    // Removed first, so what is asserted below is this run's answer and not
    // a log some earlier run of this test left lying in tmp/.
    let log = repo().join("tmp").join("logs").join("ob-wrong-api.log");
    let _ = std::fs::remove_file(&log);
    let wrong = fake_core(
        "core-with-the-wrong-api",
        "rust-fs-core-output-budget 99",
        "wrong-api-marker",
        "echo \"the fake ran\"\nexit 0",
    );
    let output = run_tier(
        &[("FS_CORE_ROOT", &wrong)],
        &[
            "wrong-api",
            "ob-wrong-api",
            "5",
            "0",
            "--",
            "echo",
            "this must never run",
        ],
    );

    assert_eq!(
        output.status.code(),
        Some(1),
        "a wrapper answering the wrong --version was accepted.\n{}",
        printed(&output)
    );
    let shown = printed(&output);
    assert!(
        shown.contains("rust-fs-core-output-budget 99")
            && shown.contains("rust-fs-core-output-budget 1"),
        "the refusal names neither what it found nor what it wanted, so \
         nobody can tell a stale checkout from a moved API:\n{shown}"
    );
    assert!(
        !shown.contains("the fake ran"),
        "the resolver ran a wrapper it had already rejected:\n{shown}"
    );
    // AND IT DID NOT QUIETLY USE THE REAL ONE INSTEAD. Falling through to
    // the next source on a bad candidate would run the tier and report
    // success, having ignored the override it was given.
    assert!(
        !log.exists(),
        "a rejected wrapper fell through to another source: the tier ran \
         anyway and left tmp/logs/ob-wrong-api.log"
    );
}

#[test]
fn the_verbose_flag_reaches_the_wrapper_under_its_new_name() {
    // `chore test -- --verbose` arrives as CLI_ARGS, and tier.sh maps it onto
    // the variable the canonical wrapper reads. THIS IS THE RENAME'S RED: if
    // tier.sh still exported FLTH_VERBOSE the run would stay quiet, exit 0
    // and tell nobody.
    let output = run_tier(
        &[("CLI_ARGS", "--verbose")],
        &[
            "verbose-tier",
            "ob-verbose",
            "5",
            "0",
            "--",
            "echo",
            "streamed-while-it-happened",
        ],
    );
    let shown = printed(&output);
    assert!(
        shown.contains("streamed-while-it-happened"),
        "--verbose did not stream the run. The quiet default is only \
         acceptable because asking for everything is one flag away:\n{shown}"
    );

    // And it does not lift the budget: the log is the same size whether or
    // not anybody was watching.
    let output = run_tier(
        &[("OUTPUT_BUDGET_VERBOSE", "1")],
        &[
            "verbose-loud",
            "ob-verbose-loud",
            "5",
            "0",
            "--",
            "sh",
            "-c",
            LOUD,
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(65),
        "a verbose tier escaped its budget.\n{}",
        printed(&output)
    );
}

#[test]
fn the_old_verbose_variable_is_not_read_by_anything_any_more() {
    // FLTH_VERBOSE was the name while the wrapper was vendored from
    // fs-linux-test-harness. The canonical script does not read it, so a
    // leftover `FLTH_VERBOSE=1` in a workflow, a task or somebody's shell
    // profile now does NOTHING -- silently. Pinned here so the rename cannot
    // be half-finished: a tier.sh that still mapped --verbose onto the old
    // name would pass every other test in this file.
    let output = run_tier(
        &[("FLTH_VERBOSE", "1")],
        &[
            "old-name",
            "ob-old-name",
            "5",
            "0",
            "--",
            "echo",
            "not-streamed",
        ],
    );
    let shown = printed(&output);
    assert!(
        output.status.success(),
        "the tier failed outright: {}\n{shown}",
        output.status
    );
    assert!(
        !shown.contains("not-streamed"),
        "FLTH_VERBOSE=1 streamed the run, so something in this repository is \
         still translating the old name. It reads as harmless and is not: it \
         is the one spelling of this rename that can be half-done and \
         green:\n{shown}"
    );
}

#[test]
fn the_copy_the_run_took_is_deleted_when_the_run_ends() {
    // tier.sh copies the wrapper into tmp/ for the run rather than executing
    // core's file in place, so a core checkout moving underneath a long run
    // cannot change the wrapper mid-flight. The copy is a copy for one run,
    // which is only true if it goes away again.
    //
    // THE MARKER IS HOW THIS IS SAFE UNDER `cargo test`'s PARALLELISM: every
    // other test in this binary is taking its own copy of the REAL wrapper
    // into the same directory at the same time, so counting files would be a
    // race. Only this test's copy carries the marker.
    let marker = "the-copy-this-test-is-looking-for";
    // Any marked copy still lying in tmp/ is one an EARLIER run of this test
    // left there -- a `tier.sh` that has since been fixed would otherwise be
    // failed by its predecessor's mess.
    for path in marked_copies(marker) {
        let _ = std::fs::remove_file(path);
    }
    let core = fake_core(
        "core-for-the-copy",
        "rust-fs-core-output-budget 1",
        marker,
        // Enough of the wrapper to run the command and report its status.
        "while [ $# -gt 0 ]; do [ \"$1\" = -- ] && { shift; break; }; shift; done\n\"$@\"",
    );
    let output = run_tier(
        &[("FS_CORE_ROOT", &core)],
        &["copy-tier", "ob-copy", "5", "0", "--", "true"],
    );
    assert!(
        output.status.success(),
        "the tier failed: {}\n{}",
        output.status,
        printed(&output)
    );

    let leftovers = marked_copies(marker);
    assert!(
        leftovers.is_empty(),
        "tier.sh left its copy of the wrapper behind: {leftovers:?}. The \
         trap that removes it is what keeps `tmp/` from filling with one \
         stale wrapper per run -- each of which would be a copy nobody \
         resolved and nobody verified."
    );
}
