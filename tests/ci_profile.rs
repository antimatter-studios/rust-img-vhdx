//! The debug run that lets the PR gate see an overflow guards itself.
//!
//! `overflow-checks` is on in debug and off in release, so a defect
//! whose only symptom is an arithmetic overflow panic cannot be
//! observed by a release-only test run. Measured on this crate rather
//! than inferred from `[profile.dev]` carrying no key: a temporary
//! `black_box(250u8) + black_box(10u8)` panicked under `cargo test
//! --locked` and passed under `cargo test --locked --release`.
//!
//! So the debug run in `ci.yml` -- the workflow that actually gates a
//! merge -- is what makes every arithmetic test in this crate mean
//! anything, and this file is what keeps it there. Nothing else in the
//! repository would notice if that run were deleted, or if `--release`
//! were added to it.
//!
//! THE GUARD IS NOT PROTECTION AGAINST MALICE. It is protection
//! against a tidy-up. Where a repository runs the suite twice, the
//! debug job looks like a duplicate of the release one beside it, and
//! that is exactly why someone removes it. Where it runs once, the
//! plausible edit is the opposite -- adding `--release` for
//! consistency with a sibling or to cut CI time -- which turns the
//! checks off with nothing failing. Both edits are silent; this file
//! is what makes them loud.
//!
//! # Two halves, neither redundant
//!
//! | half | asks | cannot answer |
//! |---|---|---|
//! | the scans here | is the step still in `ci.yml`, asked to check, and not disabled from the manifest | whether the build it produces actually traps |
//! | `overflow_checks` in `src/lib.rs` | does this build trap a real `u64::MAX + 1` | whether it was supposed to; it cannot notice its own absence |
//!
//! Delete the step and the runtime probe never runs at all. Keep the
//! step but drop the variable and the probe runs, finds nothing to
//! check, and passes doing nothing. Keep both and put
//! `overflow-checks = false` under `[profile.test]` and the step is
//! present, running, green and blind. Each needs its own guard.
//!
//! # Why this is an integration test and not a module under `src/`
//!
//! Cargo discovers `tests/*.rs` on its own, so there is no declaration
//! anywhere that can be deleted to switch this off, and `Cargo.toml`
//! sets no `autotests = false`. A guard living as a file under `src/`
//! behind a `#[cfg(test)] mod` line has no such protection: lose the
//! one line and the file stays, compiles into nothing, and asserts
//! nothing, with no lint to say so. That happened once already on a
//! sibling repository's version of this fix -- a `git reset --hard`
//! took the `mod` line, the suite went green, and seven assertions
//! silently ceased to exist.
//!
//! The runtime probe in `src/lib.rs` is the deliberate exception, and
//! is inline in `lib.rs` for the same reason: it must be part of the
//! library target the debug step builds, and inline there is no
//! declaration to lose.

use saphyr::{LoadableYamlNode, Yaml};
use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ci_yml() -> PathBuf {
    manifest_dir()
        .join(".github")
        .join("workflows")
        .join("ci.yml")
}

/// Read a file the guards depend on, or fail.
///
/// It panics rather than returning `None` on purpose. An
/// `if !path.exists() { return }` anywhere in this module would
/// reproduce the exact class of blindness the module exists to prevent:
/// an assertion that is present, runs, and cannot report the thing it
/// was written for. A missing workflow is a finding, not a skip.
fn read_or_panic(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}. This guard must fail rather than skip: a \
             version of it that returned early here would be the same \
             blindness it exists to prevent.",
            path.display()
        )
    })
}

/// The command lines of a shell script, with comments removed.
///
/// ONE PLACE, because two callers used to disagree about what a
/// comment is. `runs_with_overflow_checks` stripped them and
/// `step_declares_the_handshake` read `step.run` raw, so a step could
/// be armed by a line that never executes:
///
///     run: |
///       # EXPECT_OVERFLOW_CHECKS=1 -- see ci_profile.rs
///       cargo test --locked --all-targets
///
/// The handshake half said yes on the comment, the command half found
/// the real run, both workflow assertions passed, and the process got
/// no `EXPECT_OVERFLOW_CHECKS` -- so the runtime probe returned without
/// asserting anything. Two readers of one text must not have two
/// grammars.
///
/// It is shell, not YAML: [`parse_workflow`] has already removed the
/// workflow's own comments, and what reaches here is the inside of a
/// `run:` block, where `#` is the shell's comment character.
fn command_lines(script: &str) -> Vec<&str> {
    script
        .lines()
        .map(str::trim_start)
        .map(|line| match comment_start(line) {
            Some(at) => &line[..at],
            None => line,
        })
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

/// Where a shell comment begins on a line, if it does.
///
/// A `#` is the comment character only where a WORD begins: at the
/// start of the line, or after a character that ends a word. The rule
/// used to be `line.split(" #")`, which is that rule written for
/// exactly one such character, so a comment glued to a terminator
/// survived into the line and was read as part of it:
///
///     cargo test --locked --lib;# EXPECT_OVERFLOW_CHECKS=1 is set in CI
///
/// The handshake half found the variable in the comment, the command
/// half found a real debug run before it, and the step counted as
/// arming a probe the process would never receive.
///
/// THE ALPHABET IS [`shell_commands`]' OWN SEPARATOR SET, not a second
/// one invented here. That is the whole point: two readers of one text
/// must not have two grammars, which is why `command_lines` exists at
/// all. `|` and a tab are in the set and were not holes -- other
/// mechanisms already caught them -- and they are handled here because
/// the rule is "a word begins", not because either was measured
/// escaping.
///
/// A `#` inside quotes is data, and so is one inside a word:
/// `--features a#b` is one argument, and cutting at it would truncate
/// a real command and make the guard refuse a correct workflow.
///
/// A BACKSLASH ESCAPES, for the same reason and by the same rule as in
/// [`shell_commands`] -- the two must not disagree about where a quoted
/// span ends. Without it `echo "a \" # b"` closed its span at the
/// escaped quote, and the `#` that follows became a comment: the line
/// was cut and everything after it disappeared from the scan.
fn comment_start(line: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut at_word_start = true;
    let mut chars = line.char_indices();
    while let Some((index, c)) = chars.next() {
        if let Some(q) = quote {
            if c == '\\' && q == '"' {
                chars.next();
                at_word_start = false;
                continue;
            }
            if c == q {
                quote = None;
            }
            at_word_start = false;
            continue;
        }
        match c {
            '\\' => {
                // The escaped character is an ordinary word character,
                // `#` included: `echo \# not-a-comment`.
                chars.next();
                at_word_start = false;
            }
            '#' if at_word_start => return Some(index),
            '\'' | '"' => {
                quote = Some(c);
                at_word_start = false;
            }
            ';' | '&' | '|' | '(' | ')' => at_word_start = true,
            c if c.is_whitespace() => at_word_start = true,
            _ => at_word_start = false,
        }
    }
    None
}

/// Whether a line turns `set -e` off.
///
/// Actions runs a `run:` block as `bash -e`, so a failing command ends
/// the step. `set +e` withdraws that for everything after it, which
/// makes every command in the block advisory -- including the one this
/// file exists to require. Recognised in all its spellings (`set +e`,
/// `set +ex`, `set +o errexit`) rather than as a fixed string.
///
/// IT TOKENISES WITH [`shell_commands`] RATHER THAN `split_whitespace`.
/// Whitespace is not what ends a word in a shell, so the option name
/// with the next command's punctuation glued to it -- `set +o
/// errexit;`, `set +o errexit&&` -- read as `errexit;`, which is not
/// `errexit`, and the withdrawal was invisible. Only the detached
/// `set +o errexit ;` was caught, which is the spelling nobody writes.
///
/// Sharing the tokeniser also settles the quoted case for free:
/// `echo "set +o errexit"` yields the words `["echo", ""]`, so a
/// printed withdrawal withdraws nothing.
fn disables_errexit(line: &str) -> bool {
    shell_commands(line)
        .iter()
        .any(|(words, _)| set_disables_errexit(words))
}

/// Whether one tokenised command is a `set` that turns `errexit` off.
fn set_disables_errexit(words: &[String]) -> bool {
    let mut words = words.iter().map(String::as_str);
    if words.next() != Some("set") {
        return false;
    }
    let mut expecting_option_name = false;
    for word in words {
        if expecting_option_name {
            if word == "errexit" {
                return true;
            }
            expecting_option_name = false;
            continue;
        }
        if word == "+o" {
            expecting_option_name = true;
            continue;
        }
        if let Some(flags) = word.strip_prefix('+') {
            if flags.contains('e') {
                return true;
            }
        }
    }
    false
}

/// What follows a command on its line, and therefore whether the shell
/// reads the command's exit status.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sep {
    /// Nothing -- the command ends the line.
    End,
    /// `&&`: the chain SHORT-CIRCUITS on failure, and that is not the
    /// same as the failure being read. Under `bash -e` a command
    /// followed by `&&` is exempt from `errexit` -- the shell does not
    /// exit for it -- so the only way its failure surfaces is as the
    /// whole `&&` list's status, and that surfaces only when the list
    /// is the last thing the script runs. MEASURED:
    ///
    /// ```text
    /// bash -e -c 'false && echo x'              exit 1   READ
    /// bash -e -c 'false && echo x; echo R'      exit 0   discarded, R printed
    /// bash -e -c $'false && echo x\necho R'     exit 0   discarded, R printed
    /// ```
    ///
    /// So `cargo test --locked --all-targets && echo done` with ANY
    /// command after it -- on the same line or a later one -- is a step
    /// that goes green with the tests failing. See [`status_is_read`].
    And,
    /// `||`: the failure is caught and discarded.
    Or,
    /// `;`: under `bash -e` a failure still ends the step, because
    /// `-e` aborts before the next command runs. MEASURED, not
    /// reasoned: `bash -e -c 'false; echo REACHED'` prints nothing and
    /// exits 1.
    Semi,
    /// `|`: the line's status becomes the LAST command's. Actions'
    /// default `bash -e` does not set `pipefail`.
    Pipe,
    /// `&`: backgrounded, so nothing waits for it.
    Amp,
    /// The command sits inside a command substitution -- `$( … )` or
    /// backticks. Its status is not the shell's; it is replaced by the
    /// text the command printed, and only the SUBSTITUTING command's
    /// status is read. `echo $(cargo test --locked --lib)` exits 0
    /// whatever the tests do.
    ///
    /// A plain subshell is a different thing and keeps its separator:
    /// `(cargo test --locked --lib)` does propagate the failure.
    Substituted,
}

/// One shell line split into the commands it invokes, each with the
/// separator that follows it, and with the contents of quoted spans
/// dropped.
///
/// It is not a shell. It knows six things: the separators above; that
/// whitespace divides words; that what sits inside `'` or `"` is DATA
/// rather than a word; that an `&` right after `>` or `<` is part of a
/// redirection (`2>&1`) rather than a separator; that a backslash
/// escapes the character after it, so `\"` does NOT close a
/// double-quoted span; and that `$( … )` and backticks substitute a
/// command's OUTPUT, discarding its status.
///
/// THE BACKSLASH IS NOT A DETAIL. Without it,
/// `echo "a \" && cargo test --locked --lib"` closed its quoted span at
/// the escaped quote, and what the shell passes to `echo` as one string
/// read here as a real `cargo test` whose status the shell reads. A
/// printed command satisfying the guard is the exact defect this
/// function was written to end, one escape later.
///
/// Single quotes do not escape: inside `'…'` a backslash is literal
/// and only `'` closes the span, which is what the shell does.
///
/// The quoting rule is the whole difference between running a command
/// and printing it:
///
///     cargo test --locked --lib      -> [["cargo", "test", ...]]
///     echo "cargo test --locked"     -> [["echo", ""]]
///
/// The characters are the same and the substring match that used to
/// stand here could not tell them apart.
///
/// OVER-STRICT IS THE SAFE DIRECTION, as everywhere else in this file:
/// a shape it fails to recognise makes the guard refuse a correct
/// workflow, loudly, with the command quoted. The other way round is a
/// gate that went blind and said nothing.
fn shell_commands(line: &str) -> Vec<(Vec<String>, Sep)> {
    let chars: Vec<char> = line.chars().collect();
    let mut out: Vec<(Vec<String>, Sep)> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut i = 0;
    // One entry per open `(`: true if it opened a command substitution
    // (`$(`, `<(`, `>(`), false if it opened a plain subshell.
    let mut substitution: Vec<bool> = Vec::new();
    // The command a substitution is nested INSIDE, set aside while the
    // substitution's own commands are scanned. `echo $(date)` is one
    // command, and flushing `echo` at the `(` split it into two -- see
    // the note in the `$(`-opening arm below.
    let mut host: Vec<(Vec<String>, String, bool)> = Vec::new();

    macro_rules! end_word {
        () => {
            if started {
                words.push(std::mem::take(&mut word));
                started = false;
            }
        };
    }
    macro_rules! separator {
        ($sep:expr) => {
            if substitution.iter().any(|is_substitution| *is_substitution) {
                Sep::Substituted
            } else {
                $sep
            }
        };
    }

    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            // Inside a quoted span every character is data, including a
            // separator: `echo "a && cargo test"` is one command. A
            // backslash still escapes inside `"…"`, so `\"` is data and
            // does not close the span; inside `'…'` nothing escapes.
            // A BACKTICK SPAN ESCAPES LIKE `"` DOES, and without that
            // `echo `echo a\` ; cargo test --locked --lib`` split at
            // the `;` and counted the suite. Measured:
            // `bash -e -c 'echo `echo a\` ; false`'` exits 0 -- the
            // escaped backtick keeps the span open, so the whole thing
            // is one substitution whose status is thrown away.
            if c == '\\' && (q == '"' || q == '`') && i + 1 < chars.len() {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match c {
            // Outside quotes a backslash escapes the next character,
            // which is then an ordinary word character -- `\;` and `\"`
            // are not a separator and not a quote.
            '\\' if i + 1 < chars.len() => {
                word.push(chars[i + 1]);
                started = true;
                i += 2;
            }
            // A BACKTICK OPENS A SPAN WHOSE CONTENTS ARE DATA, exactly
            // as a quote does: a command substitution swallows its
            // command's status, so nothing inside one is a gate. This
            // file tracked `$( )` through the separator arm and did not
            // track backticks at all, which left the one spelling in
            // this family that no report and no review named.
            '\'' | '"' | '`' => {
                quote = Some(c);
                started = true;
                i += 1;
            }
            '&' | '|' | ';' | '(' | ')' => {
                // `2>&1` and `>&2`: an `&` bound to a redirection is
                // part of the word, and the command's status is still
                // read.
                if c == '&' && started && (word.ends_with('>') || word.ends_with('<')) {
                    word.push(c);
                    i += 1;
                    continue;
                }
                // `$( … )` substitutes the output and discards the
                // status; `<( … )` and `>( … )` likewise. A bare `(`
                // opens a subshell, whose failure the shell does read.
                // Read the pending word BEFORE `end_word!` takes it.
                let opens_a_substitution =
                    c == '(' && (word.ends_with('$') || word.ends_with('<') || word.ends_with('>'));
                if opens_a_substitution {
                    // THE HOST COMMAND CONTINUES AFTER THE `)`, so its
                    // words are set aside rather than flushed here.
                    //
                    // Flushing them made `echo $(date)` two commands,
                    // and `cargo test --locked --all-targets && echo
                    // $(date)` a three-entry list whose `&&` chain
                    // therefore no longer ended the script -- so the
                    // guard refused a workflow whose status bash does
                    // read (`bash -e -c 'false && echo $(date)'` exits
                    // 1). Over-strict is the safe direction, but only
                    // when it is chosen; this was an artifact of where
                    // the scanner happened to cut.
                    host.push((
                        std::mem::take(&mut words),
                        std::mem::take(&mut word),
                        started,
                    ));
                    started = false;
                    substitution.push(true);
                    i += 1;
                    continue;
                }
                if c == ')' && substitution.last() == Some(&true) {
                    end_word!();
                    if !words.is_empty() {
                        out.push((std::mem::take(&mut words), Sep::Substituted));
                    }
                    substitution.pop();
                    if let Some((host_words, host_word, host_started)) = host.pop() {
                        words = host_words;
                        word = host_word;
                        started = host_started;
                    }
                    i += 1;
                    continue;
                }
                let doubled = i + 1 < chars.len() && chars[i + 1] == c;
                let sep = match (c, doubled) {
                    ('&', true) => Sep::And,
                    ('&', false) => Sep::Amp,
                    ('|', true) => Sep::Or,
                    ('|', false) => Sep::Pipe,
                    _ => Sep::Semi,
                };
                // A `)` closes whatever the matching `(` opened, and
                // the command it terminates is the one whose status the
                // substitution swallows -- so the depth has to come off
                // AFTER that command is pushed, not before.
                let closing_a_substitution = c == ')' && substitution.last() == Some(&true);
                let sep = if closing_a_substitution {
                    Sep::Substituted
                } else {
                    separator!(sep)
                };
                end_word!();
                if !words.is_empty() {
                    out.push((std::mem::take(&mut words), sep));
                }
                match c {
                    // `$(` opens a substitution; a bare `(` opens a
                    // subshell, whose status the shell does read.
                    '(' => substitution.push(opens_a_substitution),
                    ')' => {
                        substitution.pop();
                    }
                    _ => {}
                }
                i += if doubled && c != ';' { 2 } else { 1 };
            }
            c if c.is_whitespace() => {
                end_word!();
                i += 1;
            }
            _ => {
                word.push(c);
                started = true;
                i += 1;
            }
        }
    }
    if started {
        words.push(word);
    }
    if !words.is_empty() {
        // An unterminated `$(` runs off the end of the line. Whatever
        // is inside it is still substituted, so its status is still
        // nobody's.
        let sep = if substitution.iter().any(|is_substitution| *is_substitution) {
            Sep::Substituted
        } else {
            Sep::End
        };
        out.push((words, sep));
    }
    out
}

/// The arguments of a `cargo test` invocation on this line, or `None`
/// if the line does not invoke one.
///
/// The scan used to ask `command.contains("cargo test")`, and a line
/// that only PRINTS the command satisfied it:
///
///     echo "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib"
///
/// That one line met both of this file's workflow assertions -- the
/// debug run and the handshake -- with no debug run behind either. A
/// check whose result does not depend on the thing it exists to detect
/// is this constellation's named defect, found inside a guard written
/// to prevent it.
///
/// Leading `NAME=value` assignments and an `env` prefix are stepped
/// over, because `EXPECT_OVERFLOW_CHECKS=1 cargo test …` is exactly the
/// spelling this guard is looking for; so is a `+toolchain` selector
/// between `cargo` and its subcommand. A wrapper -- `sudo`, `xargs`, a
/// script -- is not recognised and the command does not count, which is
/// the strict direction.
fn cargo_test_arguments(words: &[String]) -> Option<Vec<&str>> {
    let mut words = words
        .iter()
        .map(String::as_str)
        .skip_while(|w| *w == "env" || (!w.starts_with('-') && w.contains('=')));
    let program = words.next()?;
    if program != "cargo" && !program.ends_with("/cargo") {
        return None;
    }
    let mut rest = words.skip_while(|w| w.starts_with('+'));
    if rest.next()? != "test" {
        return None;
    }
    // A REDIRECTION IS NOT AN ARGUMENT. `2>&1` would otherwise read as
    // a bare test-name filter and disqualify a run that is fine, and a
    // separate `>` takes the following word with it.
    let mut arguments = Vec::new();
    let mut argument_is_a_redirection_target = false;
    for word in rest {
        if argument_is_a_redirection_target {
            argument_is_a_redirection_target = false;
            continue;
        }
        if word.contains('>') || word.contains('<') {
            argument_is_a_redirection_target = word.ends_with('>') || word.ends_with('<');
            continue;
        }
        arguments.push(word);
    }
    Some(arguments)
}

/// Cargo options that take their value as the NEXT argument.
///
/// Needed only to tell an option's value from a bare test-name filter:
/// `--features qemu-validation` is not a filter and
/// `cargo test --locked qemu` is.
const OPTIONS_TAKING_A_VALUE: [&str; 18] = [
    "-p",
    "--package",
    "--exclude",
    "-F",
    "--features",
    "--target",
    "--target-dir",
    "--manifest-path",
    "--profile",
    "--test",
    "--bin",
    "--example",
    "--bench",
    "-j",
    "--jobs",
    "--message-format",
    "--color",
    "--config",
];

/// Harness options that take their value as the NEXT argument.
///
/// `cargo test --locked --lib -- --test-threads 1` is legal and does
/// not narrow anything, and its `1` is a bare word. Without this list
/// the bare-word filter rule below reads that `1` as a test-name
/// filter and REFUSES A CORRECT WORKFLOW. That direction is the one
/// #87 is about: a guard that rejects a legitimate step gets edited
/// until it stops, and what gets edited is the guard.
const HARNESS_OPTIONS_TAKING_A_VALUE: [&str; 6] = [
    "--test-threads",
    "--logfile",
    "--color",
    "--format",
    "--shuffle-seed",
    "-Z",
];

/// Whether what follows a bare `--` runs fewer tests than the whole
/// library, and so may leave the overflow probe unrun.
///
/// The harness's own options divide cleanly, so this is a whitelist of
/// what NARROWS rather than a blacklist of what is harmless:
///
/// - a bare word is a filter, and runs only tests matching it;
/// - `--ignored` runs ONLY ignored tests, so it excludes the probe
///   outright -- unlike `--include-ignored`, which adds to the run and
///   is therefore fine;
/// - `--skip <pattern>` removes tests, and the pattern may be the
///   probe's own name;
/// - `--exact` narrows only in company with a filter, which the bare
///   word rule already catches, so it is not listed.
///
/// Everything else the harness takes -- `--test-threads`, `--nocapture`,
/// `--quiet`, `--color`, `--format` -- changes how the run is reported
/// rather than which tests it contains, and must keep counting; the
/// acceptance test pins `-- --test-threads=1` AND the space-separated
/// `-- --test-threads 1`, whose `1` is a bare word.
fn harness_arguments_narrow_the_run(arguments: &[&str]) -> bool {
    let mut expecting_a_value = false;
    for argument in arguments.iter().skip_while(|a| **a != "--").skip(1) {
        if expecting_a_value {
            expecting_a_value = false;
            continue;
        }
        // BOTH SPELLINGS OF THE OPTION, and the rule is already in
        // this file thirty lines below: `omits_the_library_unit_tests`
        // matches `*argument == *o || argument.starts_with("{o}=")`
        // because, as its doc puts it, `--test <name>` and
        // `--test=<name>` are "one of that option's two spellings".
        // This function tested `*argument == "--skip"` only, and
        // `--skip=FILTER` then missed all three of its rules at once:
        // not the equality, not `HARNESS_OPTIONS_TAKING_A_VALUE`, and
        // not the bare-word fallback because it starts with `-`. A
        // complete bypass rather than a near miss, and a lesson learnt
        // one function away and not applied.
        //
        // MEASURED, both spellings, on this crate's own library:
        //   cargo test --locked --lib                     63 passed
        //   ... -- --skip=<a real test name>   62 passed, 1 filtered
        //   ... -- --skip  <a real test name>  62 passed, 1 filtered
        // Both narrow, so both must disqualify.
        //
        // `--skip` also takes a value, and the answer comes before the
        // value is consumed either way. The `=` form needs no
        // consumption at all, which is why `--test-threads=1` already
        // behaved: its value is attached rather than a following word.
        if ["--skip", "--ignored"]
            .iter()
            .any(|o| *argument == *o || argument.starts_with(&format!("{o}=")))
        {
            return true;
        }
        if HARNESS_OPTIONS_TAKING_A_VALUE.contains(argument) {
            expecting_a_value = true;
            continue;
        }
        if !argument.starts_with('-') {
            return true;
        }
    }
    false
}

/// Whether these arguments select something OTHER than the crate's
/// library unit tests, where the overflow probe lives.
///
/// `--test <name>` builds one integration target and no library unit
/// tests. Several repositories here run a cross-validation suite that
/// way, in its own job, beside the real one.
///
/// This used to be `command.contains("--test ")`, which is one of that
/// option's two spellings, and which said nothing at all about `--doc`,
/// `--no-run`, `--bins`, or a bare filter. So four different one-line
/// edits each left the guard green with the probe unbuilt or unrun --
/// `--no-run` most starkly, since it compiles and executes nothing.
///
/// THE ANSWER IS TO COMPARE THE ARGUMENTS, NOT TO WIDEN THE SUBSTRING.
/// The trailing space in the old match was doing real work: `--tests`
/// DOES build the library unit tests and contains `--test`, so dropping
/// the space would have excluded a run that genuinely satisfies this
/// guard. Whole arguments answer both spellings and the `--tests` near
/// miss at once, with no space left load-bearing.
///
/// Everything after a bare `--` belongs to the test harness rather
/// than to cargo, and it is scanned by its own rules below, because
/// DOCUMENTING A HOLE DOES NOT CLOSE IT. The previous version of this
/// comment said a harness filter "is not covered, and the comment says
/// so rather than the code implying otherwise" -- which reads as a
/// decision, so the next reader treats it as one. It was a hole with a
/// note attached: `cargo test --locked --all-targets -- --ignored`
/// runs ONLY ignored tests, the overflow probe is not ignored, and the
/// guard counted the line.
fn omits_the_library_unit_tests(arguments: &[&str]) -> bool {
    if harness_arguments_narrow_the_run(arguments) {
        return true;
    }
    let cargo_arguments = arguments.iter().take_while(|a| **a != "--");
    let mut expecting_a_value = false;
    for argument in cargo_arguments {
        if expecting_a_value {
            expecting_a_value = false;
            continue;
        }
        if OPTIONS_TAKING_A_VALUE.contains(argument) {
            // `--test` and friends disqualify whether or not the name
            // is attached, so answer before consuming the value.
            expecting_a_value = true;
        }
        let selects_elsewhere = [
            "--test",
            "--doc",
            "--no-run",
            "--bin",
            "--example",
            "--bench",
        ]
        .iter()
        .any(|o| *argument == *o || argument.starts_with(&format!("{o}=")))
            || ["--bins", "--examples", "--benches"].contains(argument);
        if selects_elsewhere {
            return true;
        }
        // A bare word is a test-name filter, which runs only the tests
        // matching it -- the probe among the ones it may exclude.
        if !argument.starts_with('-') {
            return true;
        }
    }
    false
}

/// Whether the shell reads this command's exit status.
///
/// Nothing here used to look at status handling at all, so
/// `cargo test --locked --all-targets || true` was matched, counted as
/// gating, and gated nothing: the job goes green with the probe
/// failing. A pipe and a trailing `&` are the same edit in other
/// spellings.
///
/// This file already enumerates two levels of the same defect -- a
/// step's `if:` and `continue-on-error:`, and a job's. Suppression
/// inside the command is the third, and it was not on the list.
///
/// WHICH SEPARATORS DISCARD A STATUS IS MEASURED, NOT REASONED.
/// Actions runs a `run:` block as `bash -e` with no `pipefail`, and
/// under that shell:
///
/// ```text
/// bash -e -c 'false; echo REACHED'  prints nothing, exit 1  READ
/// bash -e -c 'false && echo x'                     exit 1   READ
/// bash -e -c 'false || true'                       exit 0   discarded
/// bash -e -c 'false | cat'                         exit 0   discarded
/// bash -e -c 'false &'                             exit 0   discarded
/// bash -e -c 'set +e; false; echo REACHED'  prints, exit 0  discarded
/// ```
///
/// `;` therefore counts. The first version of this rule refused it,
/// reasoning that the line's status becomes the next command's -- true
/// without `-e`, false with it, and the sort of claim that has to be
/// run rather than thought about.
///
/// `&&` DOES NOT BELONG WITH `;`, WHICH IS WHERE THIS RULE WAS WRONG.
/// `errexit` exempts a command that is part of a `&&` or `||` list,
/// except the one following the final operator. So a failing
/// `cargo test` on the left of `&&` does not end the step; its failure
/// can only surface as the whole list's status, and the list's status
/// is the script's only when the list is the last thing the script
/// runs. MEASURED, and the second row is the defect:
///
/// ```text
/// bash -e -c 'false && echo x'              exit 1  READ (list ends the script)
/// bash -e -c 'false && echo x; echo R'      exit 0  discarded
/// bash -e -c $'false && echo x\necho R'     exit 0  discarded
/// bash -e -c 'false; echo R'                exit 1  READ (errexit fires at once)
/// ```
///
/// `run: cargo test --locked --all-targets && echo done` followed by
/// one more line is a green step with red tests, and the previous rule
/// counted it. `is_last_command_line` is what tells the two rows apart,
/// so this cannot be decided from one line in isolation.
///
/// `set +e` is the caller's to handle, because it disqualifies the
/// whole block rather than one line.
///
/// It stays STRICTER than bash in two places, both stated rather than
/// accidental: `cargo test … & wait $!` does propagate the failure
/// (measured: exit 1) and is refused anyway, because recognising it
/// means tracking which job `$!` names; and `cargo test … || exit 1`
/// is refused, because `Or` is read as the catch-and-continue it
/// almost always is. Refusing a correct workflow loudly is this file's
/// declared direction; passing a broken one silently is the defect it
/// exists for.
fn status_is_read(
    commands: &[(Vec<String>, Sep)],
    index: usize,
    is_last_command_line: bool,
) -> bool {
    match commands[index].1 {
        // `errexit` fires on this command itself, whatever follows.
        Sep::Semi => true,
        // Nothing follows on this line: `errexit` fires, unless the
        // command is the tail of an `&&`/`||` list, in which case it is
        // the one member the exemption does not cover -- also read.
        Sep::End => true,
        // Caught, backgrounded, piped away, or substituted.
        Sep::Or | Sep::Pipe | Sep::Amp | Sep::Substituted => false,
        // Exempt from `errexit`. Only the list's own status can carry
        // the failure out, and only if nothing runs after the list.
        Sep::And => {
            let mut end = index;
            // `Sep::Substituted` entries are the commands nested INSIDE
            // a member of this list, not members of it, so they do not
            // stop the list from being the last thing the script runs.
            // Without this, `cargo test … && echo $(date)` counted the
            // substituted `date` as a fourth list member and the chain
            // stopped qualifying.
            // `Sep::Or` MUST NOT BE CROSSED. It was in this set, and an
            // `||` anywhere in the chain is what catches the failure:
            //
            //   bash -e -c 'false && echo a && echo b'   exit 1  READ
            //   bash -e -c 'false && echo a || echo c'   exit 0  DISCARDED
            //
            // The walk ran 0 -> 1 -> 2, stopped on `End`, and reported
            // the status read for a chain whose whole point is that it
            // swallows one. The refusal above only covers a command
            // whose OWN separator is `Or`; this is one further along.
            while matches!(commands[end].1, Sep::And | Sep::Substituted) && end + 1 < commands.len()
            {
                end += 1;
            }
            is_last_command_line && end + 1 == commands.len() && matches!(commands[end].1, Sep::End)
        }
    }
}

/// Every `cargo test` invocation in a shell script that would be
/// compiled with overflow checks on AND whose failure would be read.
///
/// The argument is the SHELL text of one step's `run:`, not YAML.
/// [`parse_workflow`] has already turned the workflow into a structure,
/// so a YAML comment can no longer reach this function at all -- that
/// half of the old scan is now the parser's job, by construction. Shell
/// comments still reach here, and [`command_lines`] is the one place
/// they are removed.
///
/// Seven things disqualify a command, and each one is a way the guard
/// could otherwise be satisfied by something that does not actually
/// build the library in debug and fail loudly:
///
/// - the script disables `set -e`, which makes every command in it
///   advisory ([`disables_errexit`]);
/// - it is a shell comment, or an inline trailing comment on an
///   otherwise-`--release` line ([`command_lines`]);
/// - it does not invoke `cargo test` at all, only mentions it
///   ([`cargo_test_arguments`]);
/// - it passes `--release`, or names a profile explicitly;
/// - it sets a `CARGO_PROFILE_*` variable, which can turn overflow
///   checks off for the dev or test profile from outside the manifest;
/// - it selects something other than the library unit tests
///   ([`omits_the_library_unit_tests`]);
/// - its exit status is discarded ([`status_is_read`]).
///
/// A `cargo build` is not a `cargo test` and is not considered, nor is
/// any step that invokes no cargo at all.
fn runs_with_overflow_checks(script: &str) -> Vec<String> {
    let lines = command_lines(script);
    if lines.iter().any(|line| disables_errexit(line)) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let last_line = lines.len().saturating_sub(1);
    for (line_index, line) in lines.iter().enumerate() {
        if line.contains("--release") || line.contains("--profile") {
            continue;
        }
        if line.contains("CARGO_PROFILE_") {
            continue;
        }
        let commands = shell_commands(line);
        for (index, (words, _)) in commands.iter().enumerate() {
            let Some(arguments) = cargo_test_arguments(words) else {
                continue;
            };
            if omits_the_library_unit_tests(&arguments) {
                continue;
            }
            // Whether an `&&` chain's failure reaches the step depends
            // on there being nothing after it -- see `status_is_read`.
            if !status_is_read(&commands, index, line_index == last_line) {
                continue;
            }
            out.push(line.to_string());
            break;
        }
    }
    out
}

/// WHAT ELSE DECIDES WHETHER A STEP GATES.
///
/// The first version of this guard matched the text of a `- run:` line
/// and never looked at anything else in the step. That is enough to
/// find the command and useless for deciding whether the command's
/// result is read. Measured against this repository's own workflow:
/// adding `if: false` to the step, or `continue-on-error: true`, left
/// every one of the guard's 31 tests green while the gate went blind.
/// A step that runs and whose result nothing reads is this
/// constellation's own named defect, reproduced inside the guard
/// written to prevent it.
///
/// So the list is enumerated first, rather than discovered one defeat
/// at a time. A `run:` step gates a pull request only if ALL of these
/// hold:
///
/// 1. the step carries no `if:` -- a false condition skips it;
/// 2. the step carries no `continue-on-error:` -- its failure is
///    discarded;
/// 3. its JOB carries no `if:` -- same reasoning, one level up;
/// 4. its JOB carries no `continue-on-error:`;
/// 5. the workflow's `on:` still includes `pull_request` -- a scan
///    scoped to `ci.yml` assumes `ci.yml` is what runs on a pull
///    request, and that is a fact about the file, not a given.
///
/// OVER-STRICT IS THE SAFE DIRECTION HERE, so 1 and 2 reject on the
/// key's PRESENCE rather than trying to evaluate it. `if: false`,
/// `if: ${{ false }}`, and an `if:` on an expression that happens to
/// evaluate false are distinct spellings, and this crate has already
/// been caught by four spellings of one manifest key -- enumerating
/// them is the losing game. A step that genuinely needs a condition
/// can be split out; a guard that tries to interpret conditions is a
/// guard with a new defeat every time GitHub adds syntax.
///
/// # Why this is parsed and no longer scanned
///
/// The version this replaces hand-rolled the YAML: `.lines()`, an
/// indent count, `split_once(':')` for the key, and `after != "|"` for
/// a block scalar. It was defeated three more times after the five
/// spellings above, and each defeat was the same shape -- ordinary
/// YAML the scanner had not been taught:
///
/// ```text
///   "if": false               quoted key -- matched no NON_GATING_KEYS
///                             entry, so the step counted as gating
///                             while Actions skipped it. SILENT.
///   "continue-on-error": true same.
///   # pull_request:           a substring match over the `on:` block's
///                             raw text, comments included, so
///                             commenting the trigger out left the
///                             guard green. SILENT.
///   run: |-  / run: >         only a bare `|` opened a block, so every
///                             other legal style was read as the
///                             command itself and the block's contents
///                             never parsed. LOUD -- it failed a
///                             correct workflow.
/// ```
///
/// Quoted keys, block scalar styles, comments and nested mappings are
/// not edge cases; they are the grammar. A parser handles all of them
/// by construction, and does not need to be taught the next one. The
/// sibling `rust-fs-xfs` copy patched each hole individually and its
/// own comments record the cost: the identical quote-normalisation was
/// added to its TOML key scan, and then had to be added again, a few
/// dozen lines away, to its YAML key scan. The same lesson twice in one
/// file is the argument against learning it a third time.
///
/// `saphyr` is a dev-dependency, so nothing here reaches a consumer of
/// the crate.
#[derive(Debug)]
struct Step {
    keys: Vec<String>,
    run: String,
    /// The step's `env:` mapping, as `KEY=VALUE`.
    ///
    /// The handshake is an environment variable, and an inline
    /// `VAR=1 cargo test` prefix is bash syntax. A matrix that includes
    /// `windows-latest` runs the same step under PowerShell, where that
    /// prefix is a syntax error -- so on a cross-platform crate the
    /// handshake HAS to be declared here rather than in the command,
    /// and a guard that only reads the command would refuse the only
    /// spelling that works.
    env: Vec<String>,
}

#[derive(Debug)]
struct Job {
    keys: Vec<String>,
    steps: Vec<Step>,
}

#[derive(Debug)]
struct Workflow {
    triggers: Vec<String>,
    jobs: Vec<Job>,
}

/// The value of `name` in a YAML mapping, or `None`.
///
/// By name rather than by constructing a key, because `saphyr`'s `Yaml`
/// borrows the source text and building one to hand to `get` is more
/// ceremony than the lookup is worth here.
fn field<'a, 'b>(node: &'a Yaml<'b>, name: &str) -> Option<&'a Yaml<'b>> {
    node.as_mapping()?
        .iter()
        .find(|(key, _)| key.as_str() == Some(name))
        .map(|(_, value)| value)
}

/// An env value as text, whether it was written `1`, `"1"` or `true`.
///
/// `EXPECT_OVERFLOW_CHECKS: 1` and `EXPECT_OVERFLOW_CHECKS: "1"` are
/// the same variable to Actions, and a guard that accepted only the
/// quoted spelling would be back to matching spellings.
fn scalar_text(node: &Yaml) -> Option<String> {
    if let Some(text) = node.as_str() {
        return Some(text.to_string());
    }
    if let Some(i) = node.as_integer() {
        return Some(i.to_string());
    }
    node.as_bool().map(|b| b.to_string())
}

/// The keys of a YAML mapping, as plain strings.
///
/// The parser has already resolved the quoting, so `"if"`, `'if'` and
/// `if` all arrive here as `if`. That is the whole of the quoted-key
/// fix: there is no un-quoting step to forget.
fn keys_of(node: &Yaml) -> Vec<String> {
    node.as_mapping()
        .map(|mapping| {
            mapping
                .iter()
                .filter_map(|(key, _)| key.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Structure a workflow far enough to answer the five questions above.
///
/// Panics on a workflow it cannot parse, deliberately. A guard that
/// returned an empty `Workflow` for a file it did not understand would
/// report "no debug run gates this" -- which is a failure, so that
/// direction is safe -- but a guard that returned early with a PASS
/// would be the blindness this module exists to prevent. Failing on the
/// parse error names the real problem instead of a consequence of it.
fn parse_workflow(text: &str) -> Workflow {
    let documents = Yaml::load_from_str(text).unwrap_or_else(|e| {
        panic!(
            "workflow is not valid YAML: {e}. This guard reads the workflow \
             rather than scanning its text, so a file it cannot parse is a \
             failure and never a pass."
        )
    });
    let Some(document) = documents.first() else {
        return Workflow {
            triggers: Vec::new(),
            jobs: Vec::new(),
        };
    };

    // `on:` takes three legal shapes: a mapping of trigger names, a
    // sequence of them, or a single scalar. All three are names.
    //
    // Note that `on` survives as the string key `on` and is not folded
    // into the boolean `true` -- saphyr implements the YAML 1.2 core
    // schema, where only `true`/`false` are booleans. The YAML 1.1
    // reading that would break every GitHub workflow ever written does
    // not apply.
    let triggers = match field(document, "on") {
        Some(on) if on.as_mapping().is_some() => keys_of(on),
        Some(on) if on.as_sequence().is_some() => on
            .as_sequence()
            .into_iter()
            .flatten()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        Some(on) => on.as_str().map(str::to_string).into_iter().collect(),
        None => Vec::new(),
    };

    let mut jobs = Vec::new();
    if let Some(mapping) = field(document, "jobs").and_then(Yaml::as_mapping) {
        for (_, body) in mapping.iter() {
            let steps = field(body, "steps")
                .and_then(Yaml::as_sequence)
                .into_iter()
                .flatten()
                .map(|step| Step {
                    keys: keys_of(step),
                    env: field(step, "env")
                        .and_then(Yaml::as_mapping)
                        .map(|m| {
                            m.iter()
                                .filter_map(|(k, v)| {
                                    Some(format!("{}={}", k.as_str()?, scalar_text(v)?))
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    // A `run:` block of any style -- `|`, `|-`, `|+`,
                    // `>`, `>-`, `|2` -- arrives as one string with the
                    // block folded per its own rules, so a command
                    // inside a shell loop is seen whole rather than as
                    // fragments, and no style is mistaken for the
                    // command itself.
                    run: field(step, "run")
                        .and_then(Yaml::as_str)
                        .unwrap_or_default()
                        .to_string(),
                })
                .collect();
            jobs.push(Job {
                keys: keys_of(body),
                steps,
            });
        }
    }

    Workflow { triggers, jobs }
}

/// Does this workflow still run on a pull request at all?
///
/// A whole-name comparison against the parsed trigger keys. The version
/// this replaces asked `wf.triggers.contains("pull_request")` of the
/// `on:` block's raw text -- comments and blank lines included -- so
/// the word appearing anywhere in it satisfied the guard. Commenting
/// the real key out, or deleting it and leaving a comment naming it,
/// left `ci.yml` no longer running on pull requests at all with the
/// guard still green.
///
/// `pull_request_target` DELIBERATELY DOES NOT COUNT, and the omission
/// is the point rather than an oversight. It runs against the base
/// repository with a write token and the repository's secrets, and it
/// checks out the base ref by default -- so a workflow triggered only
/// that way may never build the contributor's code at all, and
/// accepting it as proof the merge is gated is permissive in the worst
/// direction. `rust-fs-xfs#146` and `rust-fs-ext4#149` record it as a
/// live gap in the hand-rolled guard this file replaces, where the
/// clause was written by hand and then copied between repositories.
///
/// A parser has no opinion about `pull_request_target` unless someone
/// writes one. So it is not written. If this repository ever needs it
/// accepted, that is a decision with its own justification, and it
/// comes with a check that the checkout selects the pull request head.
fn runs_on_pull_request(wf: &Workflow) -> bool {
    wf.triggers.iter().any(|t| t == "pull_request")
}

/// Keys whose presence on a step or job means its result does not gate.
const NON_GATING_KEYS: [&str; 2] = ["if", "continue-on-error"];

/// Walk a workflow's steps and collect what `select` finds in each
/// `run:`.
///
/// `gating` restricts the walk to steps whose result the pull-request
/// gate actually reads: the workflow must still trigger on a pull
/// request, and neither the job nor the step may carry a key from
/// [`NON_GATING_KEYS`].
///
/// One walk rather than two. The headline assertion used the line-based
/// scan while only the handshake assertion was step-aware, so under
/// `if: false` the headline PASSED and its failure message would have
/// claimed the pull-request gate could see an overflow when the step it
/// names does not run. Every defeat spelling still turned the suite red
/// through the other assertion, so this was a precision defect rather
/// than a hole -- but it left the "runs without --release" property
/// verified line-based, and defeatable if the handshake assertion were
/// ever weakened. Both halves share this walk now and cannot drift
/// apart again. Found on the sibling `rust-fs-btrfs` copy of this guard
/// and corrected here rather than left to diverge.
fn collect_steps(workflow: &str, gating: bool) -> Vec<Step> {
    let wf = parse_workflow(workflow);
    if gating && !runs_on_pull_request(&wf) {
        return Vec::new();
    }
    let carries_a_non_gating_key =
        |keys: &[String]| keys.iter().any(|k| NON_GATING_KEYS.contains(&k.as_str()));

    let mut out = Vec::new();
    for job in wf.jobs {
        if gating && carries_a_non_gating_key(&job.keys) {
            continue;
        }
        for step in job.steps {
            if gating && carries_a_non_gating_key(&step.keys) {
                continue;
            }
            out.push(step);
        }
    }
    out
}

fn scan_steps(workflow: &str, gating: bool, select: fn(&str) -> Vec<String>) -> Vec<String> {
    collect_steps(workflow, gating)
        .iter()
        .flat_map(|step| select(&step.run))
        .collect()
}

/// Does this step ask the build to prove it traps an overflow?
///
/// Either spelling counts, and both are the same instruction to
/// Actions: the variable inline in the command, or declared in the
/// step's `env:` mapping. The mapping is not a concession -- it is the
/// ONLY spelling that works on a matrix including `windows-latest`,
/// where an inline `VAR=1 cargo test` prefix is a PowerShell syntax
/// error. A guard that read only the command would refuse the correct
/// workflow on every cross-platform crate in this constellation.
///
/// THE INLINE HALF READS COMMANDS, NOT THE RAW BLOCK. It used to be
/// `step.run.contains(...)`, and `step.run` is the block verbatim --
/// comments included -- while the other half of the scan strips them.
/// Two readers of one text with two grammars, so a step could be armed
/// by a line the shell never executes:
///
///     run: |
///       # EXPECT_OVERFLOW_CHECKS=1 -- see ci_profile.rs
///       cargo test --locked --all-targets
///
/// Both workflow assertions passed on that and the process got no
/// variable at all, leaving the runtime probe to return without
/// asserting anything. [`command_lines`] is now the single grammar.
///
/// STRIPPING COMMENTS WAS NECESSARY AND NOT SUFFICIENT. The fix above
/// replaced one text scan with a parse and then asked the parsed text
/// the same substring question, so the next spelling walked straight
/// through it:
///
///     run: |
///       echo "EXPECT_OVERFLOW_CHECKS=1"
///       cargo test --locked --all-targets
///
/// That is not a comment. It is a command, it survives
/// [`command_lines`] intact, `contains` said yes, and the process still
/// received nothing. The defect was never the grammar -- it was asking
/// whether the CHARACTERS are present instead of whether the shell
/// puts the variable in an environment. See [`assigns_the_handshake`].
fn step_declares_the_handshake(step: &Step) -> bool {
    command_lines(&step.run).iter().any(|line| {
        shell_commands(line)
            .iter()
            .any(|(words, _)| assigns_the_handshake(words))
    }) || step.env.iter().any(|entry| entry == HANDSHAKE)
}

/// The one spelling of the handshake that this file is looking for.
const HANDSHAKE: &str = "EXPECT_OVERFLOW_CHECKS=1";

/// Whether one tokenised command puts the handshake into an
/// environment, as opposed to merely containing its characters.
///
/// A shell has exactly two ways to do it, and both are short enough to
/// enumerate -- which is why this is a whitelist of what ASSIGNS and
/// not a blacklist of what PRINTS. `echo` and `printf` are the two
/// obvious printers and there is no end to the list: `cat <<EOF`,
/// `tee`, `yq`, a heredoc, a `>> $GITHUB_ENV` line whose variable name
/// is built by string concatenation. Enumerating those is the losing
/// game this file has already lost once, over manifest keys.
///
/// 1. a `NAME=value` prefix in front of a command, optionally behind
///    `env`: `EXPECT_OVERFLOW_CHECKS=1 cargo test …`;
/// 2. a builtin that EXPORTS: `export EXPECT_OVERFLOW_CHECKS=1`, or
///    `declare -x` / `typeset -x`. `declare`, `typeset` and `readonly`
///    WITHOUT `-x` set a shell variable a child never sees, and
///    `readonly` has no exporting spelling at all -- so none of them
///    arms anything. They were once in this list, unwitnessed, and the
///    comment you are reading named only `export` while the code took
///    four.
///
/// A quoted assignment -- `export "EXPECT_OVERFLOW_CHECKS=1"` -- is NOT
/// recognised, because [`shell_commands`] drops the contents of quoted
/// spans and cannot tell it from `echo "EXPECT_OVERFLOW_CHECKS=1"`.
/// That refuses a correct workflow rather than passing a broken one,
/// which is this file's declared direction, and the `env:` mapping is
/// there for anyone who hits it.
fn assigns_the_handshake(words: &[String]) -> bool {
    let mut words = words.iter().map(String::as_str).peekable();
    if let Some(first) = words.peek() {
        // ONLY THE BUILTINS THAT ACTUALLY EXPORT. This list was
        // `["export", "declare", "typeset", "readonly"]`, and three of
        // those four put the variable in the SHELL, not in the
        // environment a child receives. MEASURED, one command per row,
        // counting `EXPECT_OVERFLOW_CHECKS=1` in a child's `env`:
        //
        //   export X=1      child sees it: 1
        //   declare X=1     child sees it: 0
        //   typeset X=1     child sees it: 0
        //   readonly X=1    child sees it: 0
        //   declare -x X=1  child sees it: 1
        //   typeset -x X=1  child sees it: 1
        //
        // So `readonly EXPECT_OVERFLOW_CHECKS=1` followed by
        // `cargo test` armed the step and handed the process nothing --
        // the exact end state the printed-handshake defect produced,
        // reached by a line that really does assign something.
        // `declare` and `typeset` are also FUNCTION-LOCAL inside a
        // function, which is a second reason they are not exports.
        //
        // The `-x` spellings do export and are recognised.
        let exports = *first == "export"
            || ((*first == "declare" || *first == "typeset")
                && words.clone().any(|word| word == "-x"));
        if exports {
            return words.any(|word| word == HANDSHAKE);
        }
    }
    // A PREFIX ASSIGNMENT IS EXPORTED TO THE COMMAND IT PREFIXES, AND
    // TO NOTHING ELSE.
    //
    // This was a `take_while(...).any(...)`, which asks only whether
    // the handshake is somewhere in the leading run of assignments. It
    // was wrong twice, and both were measured against `bash -c
    // '<prefix> env'`, counting the handshake in the child's
    // environment:
    //
    //   EXPECT_OVERFLOW_CHECKS=1 env            child sees it: 1
    //   a=b=c EXPECT_OVERFLOW_CHECKS=1 env      child sees it: 1
    //   1abc=x EXPECT_OVERFLOW_CHECKS=1 env     child sees it: 0
    //
    // POSITION. A standalone `EXPECT_OVERFLOW_CHECKS=1` line is a
    // prefix with no command after it: bash sets a shell variable, and
    // a `cargo test` on the next line never sees it. Measured, a bare
    // assignment followed by `env` on its own line: 0. Reachable here
    // through `step_declares_the_handshake`, which is the step-level
    // arming -- a workflow whose only handshake is a standalone
    // assignment line made `gating_runs_that_prove_the_build_traps`
    // return the following `cargo test` as proven. It is NOT reachable
    // through `debug_runs_that_prove_the_build_traps`, which only ever
    // looks at lines that are themselves cargo-test runs, and that
    // difference is why reading one call site would have settled it
    // wrongly.
    //
    // NAME SHAPE. `1abc=x` is not a legal identifier, so bash runs it
    // as a COMMAND -- `bash: 1abc=x: command not found` -- and the
    // handshake after it is that command's argument. `word.contains('=')`
    // called it an assignment. `a=b=c` really is one: `a` takes the
    // value `b=c`.
    let mut saw = false;
    let mut rest = words.peekable();
    while let Some(word) = rest.peek() {
        if *word == "env" {
            rest.next();
            continue;
        }
        if !is_shell_assignment(word) {
            break;
        }
        if *word == HANDSHAKE {
            saw = true;
        }
        rest.next();
    }
    // AND A COMMAND FOLLOWS. `rest` empty means the whole command was
    // assignments, which is the case this was changed for.
    saw && rest.peek().is_some()
}

/// Whether one word is a shell assignment: a legal identifier, an `=`,
/// and then anything.
///
/// The name is what makes it an assignment rather than a command:
/// `1abc=x` starts with a digit, so bash treats the whole word as a
/// program name. Everything after the first `=` is the value, so
/// `a=b=c` assigns `b=c` to `a` and is a perfectly good assignment.
fn is_shell_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit())
        }
        None => false,
    }
}

/// The run commands of steps that run in debug AND actually gate a
/// pull request -- without requiring the handshake.
fn gating_runs_with_overflow_checks(workflow: &str) -> Vec<String> {
    scan_steps(workflow, true, runs_with_overflow_checks)
}

/// The run commands of steps that both run in debug with the handshake
/// AND actually gate a pull request.
///
/// Step-aware rather than text-aware, because the handshake may be
/// declared in the step's `env:` mapping rather than inline in the
/// command -- see [`step_declares_the_handshake`].
fn gating_runs_that_prove_the_build_traps(workflow: &str) -> Vec<String> {
    collect_steps(workflow, true)
        .into_iter()
        .filter(step_declares_the_handshake)
        .flat_map(|step| runs_with_overflow_checks(&step.run))
        .collect()
}

/// The debug runs that ask the build to prove it traps an overflow.
///
/// A subset of [`runs_with_overflow_checks`]: those which also set the
/// `EXPECT_OVERFLOW_CHECKS` handshake, so that
/// `overflow_checks::the_build_the_gate_asked_to_check_does_check`
/// performs an overflow and fails if the build let it through.
///
/// A run carrying the handshake but also `--release` is not counted,
/// because [`runs_with_overflow_checks`] has already excluded it. Such
/// a step is a misconfiguration and it fails loudly rather than
/// quietly: the checks are legitimately off in release, so the
/// assertion the handshake arms would fire there every time.
/// Like [`step_declares_the_handshake`], it asks whether the line
/// ASSIGNS the variable rather than whether it mentions it -- the two
/// call sites shared the substring bug and had to be fixed together.
/// `echo "EXPECT_OVERFLOW_CHECKS=1" && cargo test --locked --lib` is a
/// real gating debug run with the handshake nowhere in its environment.
fn debug_runs_that_prove_the_build_traps(script: &str) -> Vec<String> {
    runs_with_overflow_checks(script)
        .into_iter()
        .filter(|command| {
            shell_commands(command)
                .iter()
                .any(|(words, _)| assigns_the_handshake(words))
        })
        .collect()
}

/// The guard. Reads the workflow this repository's pull requests are
/// gated by and refuses if nothing in it compiles the overflow checks.
///
/// `ci.yml` specifically, not every workflow -- see
/// [`a_checking_debug_run_that_is_not_in_ci_yml_does_not_satisfy_this_guard`],
/// which is the one repository-specific decision in this file.
#[test]
fn the_pr_gate_still_tests_in_a_profile_that_can_see_an_overflow() {
    let path = ci_yml();
    let workflow = read_or_panic(&path);

    let debug_runs = gating_runs_with_overflow_checks(&workflow);
    assert!(
        !debug_runs.is_empty(),
        "no `cargo test` in {} runs without `--release`, so a defect whose \
         only symptom is an arithmetic overflow panic can merge without the \
         PR gate ever seeing it. release.yml already runs a debug suite, and \
         that does not help: it triggers on a version tag, after the change \
         has merged. If the debug step in ci.yml looked redundant beside the \
         release ones, it is not -- see the comment above it.",
        path.display()
    );
}

/// The other half of the workflow scan: the step exists, but does it
/// ask the build anything?
///
/// # Why a handshake rather than more spellings
///
/// The manifest scan below reads `Cargo.toml` and asks whether a known
/// spelling of "overflow checks are off" is present. Several spellings
/// of the key were needed before it was right, and then routes turned
/// up that are not in that file at all: a
/// `CARGO_PROFILE_TEST_OVERFLOW_CHECKS` variable set at step or job
/// level in the workflow, and a `.cargo/config.toml`, which nothing
/// here reads. All of them leave the debug step present, running, green
/// and blind.
///
/// They are all the same shape: a scanner enumerating the ways a thing
/// can be disabled, in the places it happens to look. Another pass buys
/// the next one. So the question is put to the build instead -- perform
/// an overflow, see whether you are stopped -- and this test's job
/// shrinks to making sure the gate still asks it.
#[test]
fn the_debug_run_asks_the_build_to_prove_it_traps_overflows() {
    let path = ci_yml();
    let workflow = read_or_panic(&path);

    let proving = gating_runs_that_prove_the_build_traps(&workflow);
    assert!(
        !proving.is_empty(),
        "no `cargo test` in {} runs without `--release` while setting \
         EXPECT_OVERFLOW_CHECKS=1, so nothing checks whether the profile the \
         gate builds actually traps an arithmetic overflow. Reading \
         Cargo.toml is not enough: the checks can also be turned off by a \
         CARGO_PROFILE_TEST_OVERFLOW_CHECKS variable at step or job level, \
         or by a .cargo/config.toml, neither of which is in any file this \
         test reads. The handshake is what arms the one check that cannot be \
         fooled by where the setting lives.",
        path.display()
    );
}

/// THE DISTINCTION THIS REPOSITORY NEEDS THAT A PORTED COPY WOULD MISS.
///
/// A workflow carrying a checking debug run under a name other than
/// `ci.yml` -- `release.yml`, in this repository's own case -- must not
/// satisfy the guards above. Simulated here with `release.yml`'s actual
/// step shape: a plain `cargo test --locked --all-targets` with no
/// `EXPECT_OVERFLOW_CHECKS`, because that workflow was never asked to
/// carry the handshake and is not asked to.
///
/// The scenario worth pinning is the near miss: even a hypothetical
/// debug run in `release.yml` that DID set the handshake would not make
/// `ci.yml`'s own absence of one acceptable, because `release.yml`
/// triggers too late to gate a merge. Both shapes are asserted below.
///
/// This is why the scan is scoped to one file rather than globbed over
/// `.github/workflows/`. A workflow that triggers on a version tag runs
/// after the change has already merged, so a debug run there does not
/// gate anything; a scan across every workflow would count it and
/// report the gate as sound when no pull request is covered. The
/// guard's correctness therefore comes from WHICH FILE it opens, not
/// from the parser refusing these shapes, and that is a fact worth
/// pinning rather than leaving as a comment someone could stop
/// believing.
#[test]
fn a_checking_debug_run_that_is_not_in_ci_yml_does_not_satisfy_this_guard() {
    let release_yml_as_it_is = "\
jobs:
  test:
    steps:
      - run: cargo test --locked --all-targets
      - run: cargo test --locked --lib
";
    // The second step was `-- --ignored` until the harness-argument
    // rule landed, and it is a counted debug run no longer: `--ignored`
    // runs ONLY ignored tests, so it leaves the probe unrun. It moved
    // to `a_selection_that_leaves_the_library_unit_tests_out_does_not_count`,
    // and this fixture needs a step that really is counted or it stops
    // making its own point.
    assert_eq!(
        scan_steps(release_yml_as_it_is, false, runs_with_overflow_checks),
        vec![
            "cargo test --locked --all-targets".to_string(),
            "cargo test --locked --lib".to_string(),
        ],
        "release.yml's real steps ARE debug runs -- the parser counts them, \
         and the only reason they do not satisfy the guard is that the guard \
         never opens that file"
    );
    assert!(
        scan_steps(
            release_yml_as_it_is,
            false,
            debug_runs_that_prove_the_build_traps
        )
        .is_empty(),
        "release.yml carries no handshake, and is not asked to"
    );

    let release_yml_with_a_handshake = "\
jobs:
  test:
    steps:
      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --all-targets
";
    assert_eq!(
        scan_steps(
            release_yml_with_a_handshake,
            false,
            debug_runs_that_prove_the_build_traps
        ),
        vec!["EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --all-targets".to_string()],
        "the parser itself would count this step too -- so widening the scan \
         to every workflow would silently stop catching this repository's \
         actual defect"
    );

    // And the real guards must be reading ci.yml, not one of these.
    let scanned = read_or_panic(&ci_yml());
    assert!(
        !gating_runs_that_prove_the_build_traps(&scanned).is_empty(),
        "the guards above must be satisfied by ci.yml's own content, not by \
         any of the strings in this test"
    );
}

/// The full dotted paths that switch overflow checks off for the
/// profile `cargo test` builds.
///
/// # This compares a whole path, because a key is not a word
///
/// The first version of this scan tracked the `[section]` and compared
/// the key to the literal `"overflow-checks"`. That reads correctly and
/// is defeated by ordinary TOML, because the same setting has several
/// spellings and cargo honours all of them without a warning. Measured
/// on a sibling repository with a runtime `u64::MAX + 1` unit test as
/// the probe -- `cargo test --locked --lib` EXIT=101 means the checks
/// are on, EXIT=0 means they are off, and `cargo metadata --no-deps`
/// was EXIT=0 for every one:
///
/// ```text
///   (nothing)                                          EXIT=101  on
///   [profile.test]  overflow-checks = false            EXIT=0    off
///   [profile.test]  "overflow-checks" = false          EXIT=0    off
///   [profile.test]  'overflow-checks' = false          EXIT=0    off
///   [profile]       test.overflow-checks = false       EXIT=0    off
/// ```
///
/// A bare key, a basic string, a literal string, and a dotted key that
/// puts the profile name on the key side where a section-matching scan
/// never looks. Four of those five defeated the first version, and each
/// leaves the debug step in `ci.yml` present, running, green and blind
/// -- the exact state the guard exists to refuse.
///
/// So the section and the key are joined into one path and normalised
/// per segment, and the comparison is against the whole thing. That
/// covers the spellings above, a quoted *section* (`["profile"."test"]`),
/// and a fully top-level dotted key with no section at all.
///
/// Only `profile.dev` and `profile.test` count. `cargo test` builds the
/// `test` profile, which inherits from `dev`, so either can disable the
/// checks in one line. `profile.release` is deliberately absent: the
/// checks are off there by default, that is what ships, and the release
/// steps exist to test what ships.
fn profiles_disabling_overflow_checks(manifest: &str) -> Vec<String> {
    /// Split a dotted TOML path and strip each segment's quoting, so
    /// that `"profile" . 'test'` and `profile.test` are one path.
    fn normalise(path: &str) -> String {
        path.split('.')
            .map(|segment| {
                segment
                    .trim()
                    .trim_matches(|c| c == '"' || c == '\'')
                    .trim()
            })
            .collect::<Vec<_>>()
            .join(".")
    }

    const DISABLED: [&str; 2] = [
        "profile.dev.overflow-checks",
        "profile.test.overflow-checks",
    ];

    let mut section = String::new();
    let mut found = Vec::new();
    for raw in manifest.lines() {
        let line = raw.split('#').next().unwrap_or(raw).trim();
        if line.starts_with('[') {
            section = normalise(line.trim_matches(|c| c == '[' || c == ']'));
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if value.trim() != "false" {
            continue;
        }
        let key = normalise(key);
        let path = if section.is_empty() {
            key
        } else {
            format!("{section}.{key}")
        };
        if DISABLED.contains(&path.as_str()) {
            found.push(path);
        }
    }
    found
}

/// The half of the property the workflow scans cannot see.
///
/// A debug step in `ci.yml` only buys anything while the profile it
/// builds actually checks. One line -- `overflow-checks = false` under
/// `[profile.test]`, or under this repository's existing
/// `[profile.dev]`, a plausible way to make a slow suite faster --
/// would leave that step present, running, green, and no longer able to
/// observe an overflow, with every workflow assertion above still
/// passing. A guard for half a condition is the defect it was written
/// to prevent.
///
/// The runtime probe in `src/lib.rs` would also catch this. This scan
/// is kept as defence in depth: it fails earlier in the gate and names
/// the offending manifest key, which is a better diagnostic than "the
/// build did not trap".
#[test]
fn the_profile_that_cargo_test_builds_still_checks_for_overflow() {
    let path = manifest_dir().join("Cargo.toml");
    let manifest = read_or_panic(&path);

    let disabled = profiles_disabling_overflow_checks(&manifest);
    assert!(
        disabled.is_empty(),
        "{} sets `overflow-checks = false` under {disabled:?}. `cargo test` \
         builds the `test` profile, which inherits from `dev`, so this \
         switches off the check that the debug step in ci.yml exists to run \
         -- leaving that step present, green, and blind. Put it back, or the \
         debug step is costing a compile and buying nothing.",
        path.display()
    );
}

/// The shell scanner is the part of this that can rot, so it is checked
/// against each shape it has to tell apart.
///
/// Its argument is the shell text of one step's `run:`, not YAML. What
/// used to be tested here as YAML -- a debug command quoted in a `#`
/// line of the workflow -- moved to `gating`, because the parser now
/// answers it by construction and this function never sees it.
mod shell_scan {
    use super::runs_with_overflow_checks;

    /// Whether a line's `cargo test` selects something other than the
    /// library unit tests. A line-level wrapper so these tests read as
    /// shell, the way the workflow does.
    fn selects_away_from_the_library(line: &str) -> bool {
        super::shell_commands(line)
            .iter()
            .filter_map(|(words, _)| super::cargo_test_arguments(words))
            .any(|arguments| super::omits_the_library_unit_tests(&arguments))
    }

    /// A LINE THAT PRINTS THE COMMAND IS NOT A RUN.
    ///
    /// The scan asked whether the line CONTAINED "cargo test", so this
    /// one satisfied both of the file's workflow assertions with
    /// nothing behind them: the debug run and, because the same
    /// characters carry `EXPECT_OVERFLOW_CHECKS=1`, the handshake too.
    #[test]
    fn an_echoed_command_is_not_a_run() {
        for line in [
            "echo \"EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\"",
            "echo 'cargo test --locked --all-targets'",
            "echo cargo test --locked --lib",
            "printf '%s\\n' \"cargo test --locked --lib\"",
            "echo \"running: cargo test\" && cargo build --locked",
        ] {
            assert_eq!(
                runs_with_overflow_checks(line),
                Vec::<String>::new(),
                "{line} prints the command; it does not run it"
            );
        }
    }

    /// THE ACCEPTANCE HALF OF THE SAME CHANGE.
    ///
    /// Recognising the invocation rather than the substring must not
    /// cost the spellings a real workflow uses. Each of these DOES
    /// run the suite in debug and each must still be counted --
    /// including the two where `cargo` is not the first word on the
    /// line.
    #[test]
    fn the_spellings_that_do_invoke_cargo_test_still_count() {
        for line in [
            "cargo test --locked --all-targets",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib",
            "cd .. && cargo test --locked --lib",
            "cargo test --locked --lib 2>&1",
            "cargo test --locked --lib > test.log",
            "cargo +stable test --locked --lib",
            "env EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib",
        ] {
            assert_eq!(
                runs_with_overflow_checks(line).len(),
                1,
                "{line} runs the suite in debug and must be counted"
            );
        }
    }

    /// The trap this repository actually contains, in the form that
    /// still reaches this function. `ci.yml` documents the debug step
    /// by quoting the command, and a `run: |` block can carry the same
    /// habit in shell comments, where the text survives the command's
    /// deletion.
    #[test]
    fn a_debug_run_quoted_in_a_shell_comment_does_not_count() {
        let block = "\
set -euo pipefail
# Measured on this branch:
#     cargo test --locked --release --lib   ->  EXIT=0
#     EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib   ->  EXIT=101
cargo test --locked --release
";
        assert_eq!(
            runs_with_overflow_checks(block),
            Vec::<String>::new(),
            "a debug command quoted inside a comment is documentation, not a run"
        );
    }

    #[test]
    fn a_real_debug_run_counts() {
        let block = "\
cargo test --locked --release
EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib
";
        assert_eq!(
            runs_with_overflow_checks(block),
            vec!["EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib".to_string()],
        );
    }

    /// A command that is `--release` but which carries a trailing
    /// comment mentioning the debug run.
    #[test]
    fn a_trailing_comment_does_not_promote_a_release_run() {
        let inline = "cargo test --locked --release  # not cargo test --lib\n";
        assert_eq!(
            runs_with_overflow_checks(inline),
            Vec::<String>::new(),
            "the command is --release; the comment after it is not a second run"
        );
    }

    /// A `#` THAT DOES NOT BEGIN A WORD IS NOT A COMMENT.
    ///
    /// The acceptance half of widening the comment rule from `" #"` to
    /// the separator alphabet: a `#` inside a word, or inside quotes,
    /// is data. Cutting there would truncate a real command and the
    /// guard would refuse a correct workflow.
    #[test]
    fn a_hash_that_does_not_begin_a_word_is_not_a_comment() {
        for line in [
            "cargo test --locked --features a#b --all-targets",
            "cargo test --locked --all-targets && echo \"done #1\"",
        ] {
            assert_eq!(
                runs_with_overflow_checks(line).len(),
                1,
                "{line:?} has no comment on it: the `#` is inside a word or a quoted span"
            );
        }
    }

    /// THE QUOTE TRACKING IN `comment_start`, WHICH NOTHING ELSE PINS.
    ///
    /// `a_hash_that_does_not_begin_a_word_is_not_a_comment` looks like
    /// it covers this and does not: its quoted `#` sits AFTER the
    /// `cargo test`, so cutting the line there still leaves the
    /// invocation behind and the guard still counts it. The mechanism
    /// survived deletion with every other test green.
    ///
    /// The witnessing input has to put the quoted `#` FIRST, so that
    /// treating it as a comment removes the run:
    ///
    ///     echo "step # 1"; cargo test --locked --all-targets
    ///
    /// Without quote tracking the line is cut to `echo "step`, no
    /// `cargo test` remains, and the guard REFUSES A CORRECT WORKFLOW.
    /// That is the direction this one guards -- a false rejection, not
    /// a false pass.
    ///
    /// It is asserted here rather than as a workflow because YAML ends
    /// a plain scalar at ` #` itself, so the same line written as
    /// `run: echo "step # 1"; ...` is refused before this function ever
    /// sees it -- a different defect, and an easy way to measure the
    /// wrong thing. Only a block scalar reaches here intact, and at
    /// that point the shell text is what is under test.
    #[test]
    fn a_quoted_hash_before_the_run_does_not_cut_the_line_short() {
        for line in [
            "echo \"step # 1\"; cargo test --locked --all-targets",
            "echo 'step # 1' && cargo test --locked --all-targets",
            "printf '%s\\n' \"# not a comment\"; cargo test --locked --lib",
        ] {
            assert_eq!(
                runs_with_overflow_checks(line).len(),
                1,
                "{line:?} runs the suite: the `#` is inside quotes and ends nothing"
            );
        }
    }

    /// The inline-comment strip, which nothing else here pins. A real
    /// debug run whose trailing comment happens to contain `--release`
    /// must still be counted. Without the strip that word disqualifies
    /// the command, and the guard then fails insisting there is no
    /// debug run while one is sitting in front of it.
    #[test]
    fn a_trailing_comment_naming_release_does_not_disqualify_a_debug_run() {
        let line = "cargo test --locked --lib  # deliberately not --release\n";
        assert_eq!(
            runs_with_overflow_checks(line),
            vec!["cargo test --locked --lib".to_string()],
            "the command is a debug run; --release appears only in its comment"
        );
    }

    /// The ways a run can carry no `--release` and still be built
    /// without the checks.
    #[test]
    fn a_profile_named_another_way_does_not_count() {
        let lines = [
            "cargo test --locked --profile release-with-debug --lib",
            "CARGO_PROFILE_TEST_OVERFLOW_CHECKS=false cargo test --locked --lib",
            "CARGO_PROFILE_DEV_OVERFLOW_CHECKS=false cargo test --locked --lib",
        ];
        for line in lines {
            assert_eq!(
                runs_with_overflow_checks(line),
                Vec::<String>::new(),
                "{line} does not compile the overflow checks"
            );
        }
        assert_eq!(
            lines.len(),
            3,
            "the loop above must have examined every shape"
        );
    }

    /// `cargo build` is not `cargo test`. A workflow that builds a
    /// binary and then exercises it with external tooling runs no test
    /// suite, and a scanner that counted `cargo build --release` would
    /// be looking at the wrong steps entirely.
    #[test]
    fn a_cargo_build_step_is_not_a_test_run() {
        let validate_job = "\
cargo build --locked --release
./target/release/some-tool --check /tmp/image
";
        assert_eq!(
            runs_with_overflow_checks(validate_job),
            Vec::<String>::new(),
            "building a binary is not running a test suite"
        );
    }

    /// A COMMAND WHOSE FAILURE IS DISCARDED IS NOT A GATE.
    ///
    /// Nothing here looked at status handling, so a debug run with its
    /// exit status thrown away was matched, counted as gating, and
    /// gated nothing -- the job goes green with the probe failing.
    /// This file already enumerates the same defect at two levels, a
    /// step's `if:`/`continue-on-error:` and a job's; suppression
    /// inside the command is the third.
    ///
    /// The pipe is the one worth reading twice. Actions runs a `run:`
    /// block as `bash -e` with no `pipefail`, so the line's status is
    /// the LAST stage's -- `tee` always succeeds.
    #[test]
    fn a_run_whose_status_is_discarded_does_not_count() {
        for line in [
            "cargo test --locked --all-targets || true",
            "cargo test --locked --all-targets || echo 'ignored'",
            "cargo test --locked --all-targets | tee test.log",
            "cargo test --locked --all-targets &",
            "set +e\ncargo test --locked --all-targets\n",
            "set +o errexit\ncargo test --locked --all-targets\n",
            "set +ex\ncargo test --locked --all-targets\n",
            // The option name with the next command's punctuation
            // glued to it. `split_whitespace` produced `errexit;`,
            // which is not `errexit`, so the withdrawal was invisible
            // and everything after it was counted as gating.
            "set +o errexit; cargo test --locked --all-targets\n",
            "set +o errexit&& cargo test --locked --all-targets\n",
            "set +o errexit\ncargo test --locked --all-targets | tee log\n",
        ] {
            assert_eq!(
                runs_with_overflow_checks(line),
                Vec::<String>::new(),
                "{line:?} runs the suite and throws the answer away"
            );
        }
    }

    /// AN `&&` CHAIN THAT ENDS IN AN `||` ARM SWALLOWS THE FAILURE.
    ///
    /// `status_is_read` walked the `&&` list across `Sep::Or` as well
    /// as `Sep::And`, so it ran past the recovery arm to the `End`
    /// beyond it and reported the status read. The refusal one arm up
    /// only covers a command whose OWN separator is `Or`; this is the
    /// same operator one member further along, and it is the whole
    /// point of the construct that it catches what the chain returns.
    ///
    /// TAKEN FROM BASH, NOT REASONED (`bash 5.3.15`, `-e`, no
    /// `pipefail`), with the failing command standing in for the run:
    ///
    /// ```text
    /// bash -e -c 'false && echo a && echo b'          exit 1  READ
    /// bash -e -c 'false && echo ok || echo failed'    exit 0  DISCARDED
    /// bash -e -c 'false && echo a && echo b || true'  exit 0  DISCARDED
    /// ```
    ///
    /// The first row is the acceptance half and lives in
    /// `a_run_whose_failure_still_ends_the_step_counts`, so this pair
    /// distinguishes "an `&&` chain" from "an `&&` chain with an `||`
    /// in it" rather than refusing `&&` wholesale.
    #[test]
    fn an_and_chain_ending_in_an_or_arm_does_not_count() {
        for line in [
            "cargo test --locked --all-targets && echo ok || echo failed",
            "cargo test --locked --lib && echo a && echo b || true",
            "cargo test --locked --all-targets && echo $(date) || echo failed",
            "cargo test --locked --lib 2>&1 && echo ok || true",
        ] {
            assert_eq!(
                runs_with_overflow_checks(line),
                Vec::<String>::new(),
                "{line:?} ends in a recovery arm, so the list exits 0 and the \
                 failure is never read"
            );
        }
    }

    /// A RUN INSIDE A COMMAND SUBSTITUTION IS NOT A GATE.
    ///
    /// `(` and `)` both fell into the separator arm and became
    /// `Sep::Semi`, which -- once `;` was correctly reclassified as
    /// status-read -- made every command inside `$( … )` look like one
    /// whose failure ends the step. It is the opposite: the
    /// substitution replaces the command with the text it printed and
    /// the status goes with it. `echo $(cargo test --locked --lib)`
    /// exits 0 whatever the tests do.
    ///
    /// Backticks are the same construct spelled differently, and
    /// `<( … )` is the same swallowing again.
    /// A BACKTICK SPAN IS A SUBSTITUTION TOO, AND AN ESCAPE KEEPS IT
    /// OPEN.
    ///
    /// This copy tracked `$( )` through the separator arm and did not
    /// track backticks at all, so `` ` `` was an ordinary word
    /// character. That made this the one spelling in this family of
    /// guards that neither the reports nor the review bot named
    /// anywhere: the escaped backtick did not close the span for bash,
    /// so the `cargo test` after the `;` was still inside the
    /// substitution and its status was discarded -- and the guard
    /// counted it as the gate.
    ///
    /// Every verdict here is `bash -e -c` on that line; `exit 0` means
    /// the status was swallowed, so the suite is not a gate.
    ///
    /// | line | bash |
    /// |---|---|
    /// | `` echo `echo a\` ; false` `` | 0 |
    /// | `` echo `false` `` | 0 |
    /// | `` echo `false` ; false `` | 1 — the trailing one is read |
    #[test]
    fn an_escaped_backtick_keeps_the_substitution_open() {
        for line in [
            "echo `echo a\\` ; cargo test --locked --lib`",
            "echo `cargo test --locked --lib`",
            "OUT=`cargo test --locked --lib`",
        ] {
            assert_eq!(
                runs_with_overflow_checks(line),
                Vec::<String>::new(),
                "{line}: the backtick span swallows the status, so nothing in it is a gate"
            );
        }

        // ACCEPTANCE: a span that really closes leaves a real gate
        // after it, and a fix that treated a backtick as opening a span
        // it never leaves would drop this.
        let line = "echo `date` ; cargo test --locked --lib";
        assert_eq!(
            runs_with_overflow_checks(line),
            vec![line.to_string()],
            "{line}: the substitution closes and the cargo test after it is top-level"
        );
    }

    #[test]
    fn a_run_inside_a_command_substitution_does_not_count() {
        for line in [
            "echo $(cargo test --locked --lib)",
            "echo $(cargo test --locked --all-targets) && echo done",
            "diff <(cargo test --locked --all-targets) expected.txt",
            // A BACKTICK IS REFUSED BY A DIFFERENT MECHANISM, and is
            // here so the next reader does not assume otherwise.
            // Nothing tracks backticks: the tokenizer leaves them
            // glued to the words around them, so the program is
            // "echo" and `cargo_test_arguments` returns `None`.
            // Backtick tracking WAS written and then removed --
            // measured inert for the verdict in every shape, and where
            // it did change one it refused a correct workflow.
            "echo `cargo test --locked --lib`",
        ] {
            assert_eq!(
                runs_with_overflow_checks(line),
                Vec::<String>::new(),
                "{line:?} substitutes the command's OUTPUT; nothing reads its status"
            );
        }
    }

    /// AN ESCAPED QUOTE DOES NOT CLOSE A QUOTED SPAN.
    ///
    /// The tokenizer's whole job is telling a command that RUNS from
    /// one that is PRINTED, and it did that by dropping what sits
    /// inside quotes. With no backslash handling, `\"` closed the span
    /// early and the rest of the printed string -- separators included
    /// -- was read as live shell. So a `cargo test` inside a string
    /// argument became a `cargo test` whose status the shell reads,
    /// which is the exact defect the quoting rule was added to end.
    ///
    /// Confirmed against the shell rather than reasoned:
    /// `bash -e -c 'echo "a \" && cargo test --locked --lib"'` prints
    /// one line and runs no tests.
    #[test]
    fn an_escaped_quote_does_not_end_a_printed_command() {
        for line in [
            "echo \"a \\\" && cargo test --locked --lib\"",
            "echo \"quote: \\\" ; cargo test --locked --all-targets\"",
            // OUTSIDE quotes the backslash matters too, and for the
            // same reason: `\;` is a literal semicolon in echo's
            // argument list, not a separator. Without it the line
            // splits and the tail reads as a run whose status is read.
            "echo \\; cargo test --locked --lib",
        ] {
            assert_eq!(
                runs_with_overflow_checks(line),
                Vec::<String>::new(),
                "{line:?} is one string handed to echo; the shell runs no tests"
            );
        }
    }

    /// AN ESCAPED QUOTE DOES NOT MOVE WHERE THE COMMENT STARTS EITHER.
    ///
    /// `comment_start` and `shell_commands` must agree about where a
    /// quoted span ends -- that agreement is the whole reason
    /// `command_lines` exists -- so the backslash rule has to be in
    /// both or the two readers have two grammars again.
    ///
    /// This one guards BOTH directions, which is why the acceptance
    /// case is in the same test rather than beside it:
    ///
    /// - without the rule inside quotes, `echo "a \" # b"` closes at
    ///   the escaped quote and the ` #` after it cuts the line, so the
    ///   real `cargo test` behind the `;` disappears and the guard
    ///   refuses a correct workflow;
    /// - without the rule outside quotes, `echo \" # x` opens a span
    ///   at what is really a literal quote, no comment is found, and
    ///   the commented-out `cargo test` is read as live.
    #[test]
    fn a_backslash_decides_where_a_comment_can_start() {
        let rejected = "echo \\\" # x; cargo test --locked --lib";
        assert_eq!(
            runs_with_overflow_checks(rejected),
            Vec::<String>::new(),
            "{rejected:?}: the `\\\"` is a literal quote, so `#` opens a comment and \
             the shell runs no tests"
        );
        let accepted = "echo \"a \\\" # b\"; cargo test --locked --lib";
        assert_eq!(
            runs_with_overflow_checks(accepted).len(),
            1,
            "{accepted:?}: the `#` is inside the string, and the run after the `;` \
             is real -- cutting the line here refuses a correct workflow"
        );
    }

    /// AN `&&` CHAIN WITH ANYTHING AFTER IT IS NOT A GATE.
    ///
    /// `errexit` exempts a command that is part of a `&&` list, so the
    /// left side's failure does not end the step. It can only surface
    /// as the whole list's status, and that is the script's status only
    /// when the list is the last thing the script runs. MEASURED:
    ///
    /// ```text
    /// bash -e -c 'false && echo x'          exit 1
    /// bash -e -c 'false && echo x; echo R'  exit 0, R printed
    /// ```
    ///
    /// The rule this replaces put `&&` with `;` on the strength of
    /// `&&` "propagating a failure". It does, when it ends the script,
    /// and the second row is a step that goes green with the suite red.
    #[test]
    fn an_and_chain_that_is_not_the_last_thing_the_step_does_does_not_count() {
        for line in [
            "cargo test --locked --all-targets && echo done\necho more\n",
            "cargo test --locked --all-targets && echo done; echo more",
            "cargo test --locked --all-targets && echo ok\ncargo build --locked\n",
        ] {
            assert_eq!(
                runs_with_overflow_checks(line),
                Vec::<String>::new(),
                "{line:?}: errexit exempts the left of `&&`, and something runs after \
                 the chain, so the step exits on that instead"
            );
        }
    }

    /// THE ACCEPTANCE HALF OF THE STATUS RULE.
    ///
    /// `&&` propagates a failure, `set -e` is the default rather than
    /// something to opt into, and a redirection is not a separator --
    /// the `&` in `2>&1` binds to the `>` before it. A status rule that
    /// refused these would refuse most real workflows.
    ///
    /// The subshell is the acceptance half of the substitution rule:
    /// `( … )` without a `$` in front of it does propagate its failure,
    /// so a fix that made every parenthesis discard would fail here.
    /// The escaped quotes are the acceptance half of the backslash
    /// rule, and the `&&` chains the acceptance half of the `errexit`
    /// exemption -- when the chain IS the last thing the step does, its
    /// status is the step's.
    #[test]
    fn a_run_whose_failure_still_ends_the_step_counts() {
        for line in [
            "cargo test --locked --all-targets",
            "cargo test --locked --all-targets && echo ok",
            "cd .. && cargo test --locked --all-targets && echo ok",
            "set -euo pipefail\ncargo test --locked --all-targets && echo ok\n",
            "(cargo test --locked --all-targets)",
            // A SUBSTITUTION IN A LATER MEMBER OF THE CHAIN DOES NOT
            // BREAK IT. These were refused once: flushing the host
            // command at the `(` made the list one entry longer, so it
            // stopped being the last thing the script does. bash reads
            // the status of both -- `bash -e -c 'false && echo $(date)'`
            // exits 1 -- and a guard that refuses them refuses a
            // correct workflow.
            "cargo test --locked --all-targets && echo $(date)",
            "cargo test --locked --all-targets && echo `date`",
            "cargo test --locked --all-targets && diff <(echo a) <(echo b)",
            "echo \"he said \\\"hi\\\"\" && cargo test --locked --all-targets",
            "cargo test --locked --all-targets && echo \"done \\\" quoted\"",
            "cargo test --locked --all-targets 2>&1",
            // `;` is NOT a discard under `bash -e`: the shell aborts
            // before the next command runs. Measured, and the reason
            // the first version of this rule was wrong.
            "cargo test --locked --all-targets; echo done",
            "cargo test --locked --all-targets ; true",
            "set -euo pipefail\ncargo test --locked --all-targets\n",
            // THE ACCEPTANCE HALF OF THE GLUED-PUNCTUATION RULE.
            // `-o errexit` turns the option ON, and a `set +o errexit`
            // that is only PRINTED withdraws nothing -- the tokenizer
            // drops what is inside quotes, so the word is `echo`.
            "set -o errexit; cargo test --locked --all-targets\n",
            "echo \"set +o errexit\"\ncargo test --locked --all-targets\n",
        ] {
            assert_eq!(
                runs_with_overflow_checks(line).len(),
                1,
                "{line:?} fails the step when the suite fails"
            );
        }
    }

    /// SELECTIONS THAT BUILD OR RUN SOMETHING OTHER THAN THE LIBRARY.
    ///
    /// `--doc`, `--no-run` and a bare filter each leave the overflow
    /// probe unbuilt or unrun while the old scan counted the line as
    /// the required debug suite. `--no-run` is the starkest: it
    /// compiles and executes nothing.
    #[test]
    fn a_selection_that_leaves_the_library_unit_tests_out_does_not_count() {
        for line in [
            "cargo test --locked --doc",
            "cargo test --locked --no-run",
            "cargo test --locked --bins",
            "cargo test --locked --examples",
            "cargo test --locked --bin some_tool",
            "cargo test --locked --example inspect",
            "cargo test --locked some_filter",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --features qemu-validation qemu",
        ] {
            assert!(
                selects_away_from_the_library(line),
                "{line} selects something other than the library unit tests"
            );
            assert_eq!(
                runs_with_overflow_checks(line),
                Vec::<String>::new(),
                "{line} does not build and run the library unit tests"
            );
        }
    }

    /// A HARNESS ARGUMENT PAST THE `--` CAN SELECT THE PROBE AWAY.
    ///
    /// The walk was `take_while(|a| **a != "--")`, so everything after
    /// a bare `--` was skipped, and the comment above it said as much:
    /// "that is not covered, and the comment says so rather than the
    /// code implying otherwise". DOCUMENTING A HOLE DOES NOT CLOSE IT
    /// -- a reader takes a sentence like that for a decision, and
    /// `cargo test --locked --all-targets -- --ignored` runs ONLY
    /// ignored tests while the guard counts the line.
    ///
    /// Three shapes narrow: `--ignored` (not `--include-ignored`,
    /// which widens), `--skip <pattern>`, and a bare word, which is a
    /// test-name filter.
    ///
    /// The last two rows are the reason `HARNESS_OPTIONS_TAKING_A_VALUE`
    /// consumes rather than ignores: a consumed value must not swallow
    /// the argument behind it.
    /// BOTH SPELLINGS OF A NARROWING HARNESS OPTION.
    ///
    /// `--skip=FILTER` missed all three of this function's rules at
    /// once -- not the equality against `"--skip"`, not the
    /// value-taking list, and not the bare-word fallback because it
    /// starts with `-`. A complete bypass, and the rule that closes it
    /// was already thirty lines below on the cargo side.
    ///
    /// MEASURED on this crate's own library, so "narrows" is not a
    /// reading of the harness's documentation:
    ///
    /// ```text
    /// cargo test --locked --lib                        63 passed
    /// cargo test --locked --lib -- --skip=<a test>      62 passed, 1 filtered out
    /// cargo test --locked --lib -- --skip  <a test>     62 passed, 1 filtered out
    /// ```
    #[test]
    fn both_spellings_of_a_narrowing_harness_option_disqualify() {
        for line in [
            "cargo test --locked --lib -- --skip foo",
            "cargo test --locked --lib -- --skip=foo",
            "cargo test --locked --lib -- --ignored",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --all-targets -- --skip=overflow",
        ] {
            assert_eq!(
                runs_with_overflow_checks(line),
                Vec::<String>::new(),
                "{line}: the harness runs fewer tests than the library, so the probe may \
                 not run and this cannot be the gate"
            );
        }
    }

    /// ACCEPTANCE, AND IT IS WHAT A WIDER MATCH WOULD BREAK. These
    /// change how the run is REPORTED, not which tests it contains, and
    /// `--include-ignored` ADDS to the run. All must still count --
    /// including the `=` spellings, since the fix above starts reading
    /// them.
    #[test]
    fn harness_options_that_do_not_narrow_still_count() {
        for line in [
            "cargo test --locked --lib -- --test-threads=1",
            "cargo test --locked --lib -- --test-threads 1",
            "cargo test --locked --lib -- --nocapture",
            "cargo test --locked --lib -- --include-ignored",
            "cargo test --locked --lib -- --color=always",
            "cargo test --locked --lib -- --format=terse",
        ] {
            assert_eq!(
                runs_with_overflow_checks(line),
                vec![line.to_string()],
                "{line} reports differently and runs the whole library, so it still gates"
            );
        }
    }

    #[test]
    fn a_harness_argument_that_narrows_the_run_does_not_count() {
        for line in [
            "cargo test --locked --all-targets -- --ignored",
            "cargo test --locked --lib -- --skip overflow",
            "cargo test --locked --all-targets -- some_filter",
            "cargo test --locked --lib -- --test-threads=1 --ignored",
            "cargo test --locked --lib -- --test-threads 1 some_filter",
            "cargo test --locked --all-targets -- --color never --skip overflow",
        ] {
            assert!(
                selects_away_from_the_library(line),
                "{line} runs fewer tests than the library holds"
            );
            assert_eq!(
                runs_with_overflow_checks(line),
                Vec::<String>::new(),
                "{line} may leave the overflow probe unrun"
            );
        }
    }

    /// BOTH SPELLINGS OF `--test` ARE THE SAME OPTION.
    ///
    /// The scan matched the substring `"--test "`, so the `=` form was
    /// invisible: a cross-validation job building one integration
    /// target counted as a full debug run, and the real one could then
    /// be deleted with this guard still green. Nothing noticed because
    /// every such job here is written the long way today; the guard is
    /// what stops the short way from being silently equivalent.
    #[test]
    fn an_equals_spelled_single_target_does_not_count_either() {
        for line in [
            "cargo test --locked --features qemu-validation --test=qemu_validation",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --test=some_oracle",
            "cargo test --locked --test=\"qemu_validation\"",
        ] {
            assert_eq!(
                runs_with_overflow_checks(line),
                Vec::<String>::new(),
                "{line} builds one integration target and no library unit tests"
            );
            assert!(
                selects_away_from_the_library(line),
                "{line} names a single integration target"
            );
        }
    }

    /// The acceptance half: options that merely START with `--test`
    /// are not the option, and every one of these builds the library
    /// unit tests.
    #[test]
    fn options_that_only_look_like_test_do_not_disqualify_a_run() {
        for line in [
            "cargo test --locked --tests",
            "cargo test --locked --all-targets",
            "cargo test --locked --lib -- --test-threads=1",
            // The rest of the harness's reporting options, which change
            // how a run is described rather than which tests it holds.
            "cargo test --locked --all-targets -- --nocapture",
            "cargo test --locked --lib -- --include-ignored",
            "cargo test --locked --all-targets -- --format=terse --color=never",
            // THE SPACE-SEPARATED SPELLING, whose value is a bare word.
            // `HARNESS_OPTIONS_TAKING_A_VALUE` exists for these three
            // lines: without it the `1`, the `terse` and the path are
            // read as test-name filters and a correct workflow is
            // refused.
            "cargo test --locked --lib -- --test-threads 1",
            "cargo test --locked --all-targets -- --format terse --color never",
            "cargo test --locked --lib -- --logfile results.txt",
        ] {
            assert!(
                !selects_away_from_the_library(line),
                "{line} does not restrict the run to one integration target"
            );
            assert_eq!(
                runs_with_overflow_checks(line).len(),
                1,
                "{line} builds the library unit tests and must be counted"
            );
        }
    }

    /// A single integration target is not the crate's arithmetic.
    /// Several repositories here run a cross-validation suite as
    /// `--test <name>` in its own job, and counting it would let the
    /// real debug run be deleted with the guard still green.
    #[test]
    fn a_single_integration_target_does_not_count() {
        for line in [
            "cargo test --locked --features qemu-validation --test qemu_validation",
            "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --test some_oracle",
        ] {
            assert_eq!(
                runs_with_overflow_checks(line),
                Vec::<String>::new(),
                "{line} builds one integration target and no library unit tests"
            );
        }
    }

    /// The near miss that the trailing space protects: `--tests` DOES
    /// build the library unit tests and must still count. Without the
    /// space this is excluded and the guard refuses a correct workflow.
    #[test]
    fn a_tests_flag_run_counts_which_is_what_the_trailing_space_protects() {
        assert_eq!(
            runs_with_overflow_checks("cargo test --locked --tests"),
            vec!["cargo test --locked --tests".to_string()],
        );
        assert_eq!(
            runs_with_overflow_checks("cargo test --locked --all-targets").len(),
            1,
            "`--all-targets` builds the library too"
        );
    }
}

/// The handshake half of the shell scanner.
mod handshake {
    use super::debug_runs_that_prove_the_build_traps;
    use super::runs_with_overflow_checks;

    #[test]
    fn a_debug_run_carrying_the_handshake_counts() {
        let script = "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n";
        assert_eq!(
            debug_runs_that_prove_the_build_traps(script),
            vec!["EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib".to_string()],
        );
    }

    /// A debug run that exists and asks the build nothing. Buys a
    /// compile and no information.
    #[test]
    fn a_debug_run_without_the_handshake_does_not_count() {
        let script = "cargo test --locked --lib\n";
        assert_eq!(
            debug_runs_that_prove_the_build_traps(script),
            Vec::<String>::new(),
            "the step is there but nothing checks the build it produced"
        );
    }

    /// A handshake on a release run proves nothing and must not satisfy
    /// this: the checks are off in release on purpose.
    #[test]
    fn the_handshake_on_a_release_run_does_not_count() {
        let script = "EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --release\n";
        assert_eq!(
            debug_runs_that_prove_the_build_traps(script),
            Vec::<String>::new(),
        );
    }

    /// And quoted inside a shell comment, which is where a `run: |`
    /// block would explain it.
    #[test]
    fn the_handshake_quoted_in_a_comment_does_not_count() {
        let script = "#     EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n";
        assert_eq!(
            debug_runs_that_prove_the_build_traps(script),
            Vec::<String>::new(),
        );
    }

    /// AND PRINTED ON THE SAME LINE AS A REAL RUN.
    ///
    /// This is the script-level twin of
    /// `gating::a_handshake_that_is_only_printed_does_not_arm_a_step`,
    /// and it needs its own fixture because the two readers are
    /// separate functions that shared one substring bug. Fixing only
    /// the step-level one left this call site answering `contains` on
    /// a whole line, and a line that both prints the variable and runs
    /// the suite satisfies that while the process receives nothing.
    ///
    /// The `&&` chain is the last thing the script does, so the run
    /// itself genuinely gates -- the only thing wrong with it is the
    /// handshake, which is what keeps this fixture pointed at the
    /// defect rather than at the status rule.
    #[test]
    fn the_handshake_merely_printed_beside_a_real_run_does_not_count() {
        let script = "echo \"EXPECT_OVERFLOW_CHECKS=1\" && cargo test --locked --lib\n";
        assert_eq!(
            runs_with_overflow_checks(script).len(),
            1,
            "the control: this line really does run the suite and end the step"
        );
        assert_eq!(
            debug_runs_that_prove_the_build_traps(script),
            Vec::<String>::new(),
            "printing the variable does not put it in the run's environment"
        );
    }
}

/// The manifest scanner, held to the shapes it has to tell apart. These
/// do not depend on this repository's own `Cargo.toml`, so they keep
/// meaning something after it changes.
mod manifest_parser {
    use super::profiles_disabling_overflow_checks;

    #[test]
    fn the_test_profile_disabling_the_checks_is_caught() {
        let manifest = "\
[profile.release]
lto = true

[profile.test]
overflow-checks = false
";
        assert_eq!(
            profiles_disabling_overflow_checks(manifest),
            vec!["profile.test.overflow-checks".to_string()],
        );
    }

    /// This repository has an explicit `[profile.dev]`, so this is the
    /// likeliest place the setting would actually arrive.
    #[test]
    fn the_dev_profile_disabling_the_checks_is_caught() {
        let manifest = "[profile.dev]\nopt-level = 1\noverflow-checks   =   false\n";
        assert_eq!(
            profiles_disabling_overflow_checks(manifest),
            vec!["profile.dev.overflow-checks".to_string()],
        );
    }

    /// Release is expected to have them off. Flagging it would make the
    /// guard fail on every correct manifest, which is the fastest way
    /// to get a guard deleted.
    #[test]
    fn the_release_profile_disabling_the_checks_is_not_flagged() {
        let manifest = "[profile.release]\noverflow-checks = false\n";
        assert_eq!(
            profiles_disabling_overflow_checks(manifest),
            Vec::<String>::new(),
        );
    }

    /// A commented-out line is not a setting -- the same trap as the
    /// workflow parser's, in the other file this module reads.
    #[test]
    fn a_commented_out_setting_is_not_a_setting() {
        let manifest = "[profile.test]\n# overflow-checks = false\nopt-level = 1\n";
        assert_eq!(
            profiles_disabling_overflow_checks(manifest),
            Vec::<String>::new(),
        );
    }

    /// The comment strip, which nothing else here pins. The realistic
    /// way this setting arrives is with its excuse on the same line,
    /// and it must still be caught: unstripped, the value reads
    /// `false  # speeds the suite up`, which is not `false`, and the
    /// guard waves through the exact edit it exists to catch.
    #[test]
    fn a_disabling_line_with_a_trailing_comment_is_still_caught() {
        let manifest = "[profile.test]\noverflow-checks = false  # speeds the suite up\n";
        assert_eq!(
            profiles_disabling_overflow_checks(manifest),
            vec!["profile.test.overflow-checks".to_string()],
        );
    }

    /// A different setting being `false` is not this setting being
    /// `false`. Without this the scanner could be keying on the value
    /// alone -- flagging any `= false` under those two sections -- and
    /// every other test here would still pass.
    #[test]
    fn another_setting_being_false_is_not_this_one() {
        let manifest = "[profile.test]\ndebug-assertions = false\nopt-level = 1\n";
        assert_eq!(
            profiles_disabling_overflow_checks(manifest),
            Vec::<String>::new(),
        );
    }

    /// THE SPELLINGS THAT DEFEATED THE FIRST VERSION. Each of these was
    /// measured to genuinely switch the checks off, with no warning
    /// from cargo -- see the table on
    /// `profiles_disabling_overflow_checks`. A guard that reads one
    /// spelling of a setting is a guard against typing it one way.
    #[test]
    fn a_double_quoted_key_is_the_same_key() {
        let manifest = "[profile.test]\n\"overflow-checks\" = false\n";
        assert_eq!(
            profiles_disabling_overflow_checks(manifest),
            vec!["profile.test.overflow-checks".to_string()],
        );
    }

    #[test]
    fn a_literal_quoted_key_is_the_same_key() {
        let manifest = "[profile.dev]\n'overflow-checks' = false\n";
        assert_eq!(
            profiles_disabling_overflow_checks(manifest),
            vec!["profile.dev.overflow-checks".to_string()],
        );
    }

    /// The one a section-matching scan cannot see at all: the profile
    /// name is on the key side, so the section is only `profile`.
    #[test]
    fn a_dotted_key_putting_the_profile_on_the_key_side_is_caught() {
        let manifest = "[profile]\ntest.overflow-checks = false\n";
        assert_eq!(
            profiles_disabling_overflow_checks(manifest),
            vec!["profile.test.overflow-checks".to_string()],
        );
    }

    /// And with no section header at all, which is still valid TOML.
    #[test]
    fn a_top_level_dotted_key_is_caught() {
        let manifest = "profile.test.overflow-checks = false\n";
        assert_eq!(
            profiles_disabling_overflow_checks(manifest),
            vec!["profile.test.overflow-checks".to_string()],
        );
    }

    #[test]
    fn a_quoted_section_is_the_same_section() {
        let manifest = "[\"profile\".'test']\noverflow-checks = false\n";
        assert_eq!(
            profiles_disabling_overflow_checks(manifest),
            vec!["profile.test.overflow-checks".to_string()],
        );
    }

    /// Release stays exempt in the dotted spelling too, or normalising
    /// the path would have quietly widened what the guard refuses.
    #[test]
    fn the_release_profile_is_exempt_in_the_dotted_spelling_too() {
        let manifest = "[profile]\nrelease.overflow-checks = false\n";
        assert_eq!(
            profiles_disabling_overflow_checks(manifest),
            Vec::<String>::new(),
        );
    }

    /// `true` is the state we want and must not be reported as the
    /// state we do not. Without this the scanner could be keying on the
    /// word `overflow-checks` alone and nothing here would notice.
    #[test]
    fn enabling_the_checks_explicitly_is_not_flagged() {
        let manifest = "[profile.test]\noverflow-checks = true\n";
        assert_eq!(
            profiles_disabling_overflow_checks(manifest),
            Vec::<String>::new(),
        );
    }
}

/// WHAT ELSE DECIDES WHETHER THE STEP GATES -- one test per item on the
/// enumerated list, because each is a separate way for the gate to go
/// blind with the command still present and still matching.
///
/// The version of this guard these replace matched the `- run:` line in
/// isolation. Measured against this repository's own workflow, `if:
/// false` and `continue-on-error: true` each left all 31 of its tests
/// green while the gate stopped gating.
mod gating {
    use super::gating_runs_that_prove_the_build_traps;

    /// A HANDSHAKE IN A SHELL COMMENT DOES NOT ARM THE STEP.
    ///
    /// `step_declares_the_handshake` read `step.run` verbatim while the
    /// command scan stripped comments, so this workflow satisfied both
    /// assertions and handed the process no `EXPECT_OVERFLOW_CHECKS` at
    /// all -- leaving the runtime probe to return without asserting
    /// anything.
    ///
    /// The existing `handshake::the_handshake_quoted_in_a_comment_does_not_count`
    /// looks like this test and is not: it exercises the script-level
    /// path, which was already comment-stripped, and its fixture has no
    /// real command after the comment, so it could not diverge on the
    /// defect even pointed at the right function.
    #[test]
    fn a_handshake_in_a_shell_comment_does_not_arm_a_step() {
        for block in [
            "      - run: |\n          # EXPECT_OVERFLOW_CHECKS=1 -- see ci_profile.rs\n          cargo test --locked --lib\n",
            "      - run: |\n          cargo test --locked --lib  # EXPECT_OVERFLOW_CHECKS=1 is set in CI\n",
        ] {
            let yaml = GATING.replace(
                "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
                block,
            );
            assert!(
                gating_runs_that_prove_the_build_traps(&yaml).is_empty(),
                "the variable is named in a comment, so the process never receives \
                 it and the runtime probe asserts nothing: {block:?}"
            );
        }
    }

    /// A HANDSHAKE GLUED TO A TERMINATOR DOES NOT ARM THE STEP EITHER.
    ///
    /// `command_lines` cut the comment at `" #"`, which is the rule
    /// "a `#` that begins a word" written for exactly one of the
    /// characters that end a word. The three it missed are the ones
    /// `shell_commands` already splits on, so the two readers of one
    /// text had two grammars again -- the defect this file records
    /// having fixed once already, at a different character.
    ///
    /// `&&#` is not valid bash, and is here anyway: the guard must not
    /// depend on the evasion being a shape bash would accept, and
    /// over-strict is the safe direction everywhere in this file.
    #[test]
    fn a_handshake_glued_to_a_terminator_does_not_arm_a_step() {
        for block in [
            "      - run: |\n          cargo test --locked --lib;# EXPECT_OVERFLOW_CHECKS=1 is set in CI\n",
            "      - run: |\n          (cargo test --locked --lib)# EXPECT_OVERFLOW_CHECKS=1 is set in CI\n",
            "      - run: |\n          cargo test --locked --lib && echo ok&&# EXPECT_OVERFLOW_CHECKS=1 is set in CI\n",
        ] {
            let yaml = GATING.replace(
                "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
                block,
            );
            assert!(
                gating_runs_that_prove_the_build_traps(&yaml).is_empty(),
                "the variable is named in a comment, so the process never receives \
                 it and the runtime probe asserts nothing: {block:?}"
            );
        }
    }

    /// A HANDSHAKE THAT IS ONLY PRINTED DOES NOT ARM THE STEP.
    ///
    /// The comment fix replaced a raw-text scan with a parse and then
    /// asked the parsed text the same substring question, so the next
    /// spelling walked straight through it. `echo` is not a comment: it
    /// survives `command_lines` intact, `contains` said yes, and the
    /// process still received nothing -- which is the same end state as
    /// the comment defect, reached by a line the shell really does run.
    ///
    /// Parsing was necessary and not sufficient. The predicate is where
    /// the defect was: whether the characters are PRESENT, rather than
    /// whether the shell puts the variable in an environment.
    ///
    /// The last fixture is the one that also defeats the script-level
    /// reader, because the same line carries a qualifying `cargo test`.
    #[test]
    fn a_handshake_that_is_only_printed_does_not_arm_a_step() {
        for block in [
            "      - run: |\n          echo \"EXPECT_OVERFLOW_CHECKS=1\"\n          cargo test --locked --lib\n",
            "      - run: |\n          echo EXPECT_OVERFLOW_CHECKS=1\n          cargo test --locked --lib\n",
            "      - run: |\n          echo \"setting EXPECT_OVERFLOW_CHECKS=1 for the run below\"\n          cargo test --locked --lib\n",
            "      - run: |\n          echo \"EXPECT_OVERFLOW_CHECKS=1\" && cargo test --locked --lib\n",
        ] {
            let yaml = GATING.replace(
                "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
                block,
            );
            assert!(
                gating_runs_that_prove_the_build_traps(&yaml).is_empty(),
                "printing the variable does not put it in the environment, so the \
                 runtime probe asserts nothing: {block:?}"
            );
        }
    }

    /// ASSIGNING IS NOT EXPORTING.
    ///
    /// The whitelist read `["export", "declare", "typeset",
    /// "readonly"]`, and three of those four put the variable in the
    /// SHELL rather than in the environment a child receives. The doc
    /// comment above the function named only `export`, so the other
    /// three were an unwitnessed widening rather than a decision.
    ///
    /// `readonly EXPECT_OVERFLOW_CHECKS=1` followed by `cargo test`
    /// therefore armed the step and handed the process nothing --
    /// exactly the end state the printed-handshake defect produced,
    /// reached by a line that really does assign something.
    ///
    /// MEASURED, one command per row, counting the name in a child's
    /// `env` (`bash 5.3.15`):
    ///
    /// ```text
    /// export X=1      1     declare -x X=1  1
    /// declare X=1     0     typeset -x X=1  1
    /// typeset X=1     0
    /// readonly X=1    0
    /// ```
    ///
    /// The `-x` spellings are the acceptance half and live in
    /// `both_real_spellings_of_the_handshake_still_arm_a_step`, so
    /// this pair turns on the export, not on the builtin's name.
    /// AN ASSIGNMENT WITH NO COMMAND AFTER IT ARMS NOTHING, AND IT IS
    /// REACHABLE HERE.
    ///
    /// Whether this copy had the defect was an open question, and the
    /// answer depends on which call site you read -- which is why it
    /// was settled by running both.
    ///
    /// `debug_runs_that_prove_the_build_traps` only ever looks at lines
    /// that are themselves cargo-test runs, so a bare assignment on its
    /// own line never reaches it: measured, that path does NOT arm.
    /// `step_declares_the_handshake` is the step-level arming and has
    /// no such precondition, so `gating_runs_that_prove_the_build_traps`
    /// returned the following `cargo test` as proven while the process
    /// received nothing. Measured on a whole synthetic workflow, which
    /// is why this test is written against the gating path rather than
    /// the predicate.
    ///
    /// `bash -c 'EXPECT_OVERFLOW_CHECKS=1<newline>env'` shows the
    /// handshake in the child's environment 0 times.
    #[test]
    fn a_standalone_assignment_does_not_arm_the_step() {
        let bare = concat!(
            "on:\n  pull_request:\n",
            "jobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n",
            "      - run: |\n",
            "          EXPECT_OVERFLOW_CHECKS=1\n",
            "          cargo test --locked --lib\n",
        );
        assert_eq!(
            super::gating_runs_that_prove_the_build_traps(bare),
            Vec::<String>::new(),
            "the assignment is on its own line, so the cargo test runs without it"
        );

        // ACCEPTANCE: the prefix spelling really does export, and is
        // the spelling a workflow is written in.
        let prefix = concat!(
            "on:\n  pull_request:\n",
            "jobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n",
            "      - run: |\n",
            "          EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
        );
        assert_eq!(
            super::gating_runs_that_prove_the_build_traps(prefix).len(),
            1,
            "a prefix assignment is exported to the command it prefixes"
        );
    }

    /// A NAME THE SHELL WOULD REJECT IS NOT AN ASSIGNMENT.
    ///
    /// `word.contains('=')` called `1abc=x` an assignment. bash does
    /// not: `bash: 1abc=x: command not found`, so the handshake after
    /// it is that command's ARGUMENT and nothing is exported --
    /// measured, child sees it 0 times. `a=b=c` really is an
    /// assignment, `a` taking the value `b=c`, measured at 1.
    #[test]
    fn only_a_legal_identifier_makes_an_assignment_prefix() {
        let w = |line: &str| -> Vec<String> {
            super::shell_commands(line)
                .into_iter()
                .next()
                .map(|(words, _)| words)
                .unwrap_or_default()
        };
        assert!(
            !super::assigns_the_handshake(&w("1abc=x EXPECT_OVERFLOW_CHECKS=1 cargo test")),
            "`1abc=x` is a command name, so the handshake is its argument"
        );
        assert!(
            super::assigns_the_handshake(&w("a=b=c EXPECT_OVERFLOW_CHECKS=1 cargo test")),
            "`a=b=c` assigns `b=c` to `a`, so the prefix is still a prefix"
        );
    }

    #[test]
    fn an_assignment_that_does_not_export_does_not_arm_a_step() {
        for block in [
            "      - run: |\n          declare EXPECT_OVERFLOW_CHECKS=1\n          cargo test --locked --lib\n",
            "      - run: |\n          typeset EXPECT_OVERFLOW_CHECKS=1\n          cargo test --locked --lib\n",
            "      - run: |\n          readonly EXPECT_OVERFLOW_CHECKS=1\n          cargo test --locked --lib\n",
        ] {
            let yaml = GATING.replace(
                "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
                block,
            );
            assert!(
                gating_runs_that_prove_the_build_traps(&yaml).is_empty(),
                "this builtin sets a shell variable the child never sees, so the \
                 runtime probe asserts nothing: {block:?}"
            );
        }
    }

    /// THE ACCEPTANCE HALF: both real spellings still arm the step.
    ///
    /// The `env:` mapping is not a concession -- it is the only
    /// spelling that works on a matrix including `windows-latest`.
    ///
    /// `export` and an `env` prefix are here because the rule that
    /// stops `echo` has to be a whitelist of what ASSIGNS rather than a
    /// blacklist of what PRINTS, and a whitelist that named only the
    /// bare `NAME=value` prefix would refuse these two.
    #[test]
    fn both_real_spellings_of_the_handshake_still_arm_a_step() {
        for block in [
            "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
            "      - run: cargo test --locked --lib\n        env:\n          EXPECT_OVERFLOW_CHECKS: \"1\"\n",
            "      - run: |\n          # the guard is armed below, not here\n          EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
            "      - run: |\n          export EXPECT_OVERFLOW_CHECKS=1\n          cargo test --locked --lib\n",
            "      - run: env EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
            // `-x` IS WHAT MAKES THEM EXPORTS, and refusing these would
            // refuse a correct workflow. Measured: `declare -x` 1,
            // `typeset -x` 1.
            "      - run: |\n          declare -x EXPECT_OVERFLOW_CHECKS=1\n          cargo test --locked --lib\n",
            "      - run: |\n          typeset -x EXPECT_OVERFLOW_CHECKS=1\n          cargo test --locked --lib\n",
        ] {
            let yaml = GATING.replace(
                "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
                block,
            );
            assert_eq!(
                gating_runs_that_prove_the_build_traps(&yaml).len(),
                1,
                "this step really does ask the build to prove it traps: {block:?}"
            );
        }
    }

    /// The shape that does gate, as a control. Every test below is this
    /// with one thing added, so a failure here would mean the fixture
    /// is wrong rather than the property.
    const GATING: &str = "\
on:
  pull_request:
    branches: [main]
jobs:
  test:
    steps:
      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib
";

    #[test]
    fn the_control_shape_gates() {
        assert_eq!(
            gating_runs_that_prove_the_build_traps(GATING).len(),
            1,
            "the control must be counted, or every test below passes for the wrong reason"
        );
    }

    #[test]
    fn a_step_carrying_if_does_not_gate() {
        for condition in [
            "if: false",
            "if: ${{ false }}",
            "if: github.event_name == 'push'",
            "if: ${{ env.SOMETHING == 'yes' }}",
        ] {
            let yaml = GATING.replace(
                "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
                &format!(
                    "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n        {condition}\n"
                ),
            );
            assert!(
                gating_runs_that_prove_the_build_traps(&yaml).is_empty(),
                "a step carrying `{condition}` may or may not run, so it cannot be what \
                 makes the gate able to see an overflow. Rejected on the key's presence \
                 rather than by evaluating it -- the spellings are open-ended."
            );
        }
    }

    #[test]
    fn a_step_carrying_continue_on_error_does_not_gate() {
        let yaml = GATING.replace(
            "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
            "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n        continue-on-error: true\n",
        );
        assert!(
            gating_runs_that_prove_the_build_traps(&yaml).is_empty(),
            "the step runs and its failure is discarded, which is the project's own named \
             defect: a step that runs and whose result nothing reads"
        );
    }

    #[test]
    fn a_job_carrying_if_does_not_gate() {
        let yaml = GATING.replace("  test:\n", "  test:\n    if: false\n");
        assert!(
            gating_runs_that_prove_the_build_traps(&yaml).is_empty(),
            "the same reasoning one level up: a job that may not run cannot gate"
        );
    }

    #[test]
    fn a_job_carrying_continue_on_error_does_not_gate() {
        let yaml = GATING.replace("  test:\n", "  test:\n    continue-on-error: true\n");
        assert!(
            gating_runs_that_prove_the_build_traps(&yaml).is_empty(),
            "a job whose failure is discarded cannot gate, however sound its steps"
        );
    }

    /// The assumption the `ci.yml`-only scan rests on, which is a fact
    /// about the file rather than a given.
    #[test]
    fn a_workflow_that_no_longer_runs_on_pull_request_does_not_gate() {
        let yaml = GATING.replace(
            "  pull_request:\n    branches: [main]\n",
            "  push:\n    branches: [main]\n",
        );
        assert!(
            gating_runs_that_prove_the_build_traps(&yaml).is_empty(),
            "scoping the scan to ci.yml assumes ci.yml is what runs on a pull request; if its \
             triggers stop including pull_request, the step gates nothing no matter how it looks"
        );
    }

    /// A `run: |` block is read whole, so a command inside a loop is
    /// visible. This repository has TWO such loops in kernel-gate, and
    /// a line-range extraction drops the second.
    #[test]
    fn a_run_block_is_read_whole() {
        let yaml = "\
on:
  pull_request:
    branches: [main]
jobs:
  test:
    steps:
      - name: a block
        run: |
          set -euo pipefail
          EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib
";
        assert_eq!(
            gating_runs_that_prove_the_build_traps(yaml).len(),
            1,
            "a command inside a `run: |` block must be seen; the kernel-gate loops live in \
             blocks like this one"
        );
    }

    /// THE QUOTED SPELLINGS, WHICH WERE SILENT DEFEATS. Measured on
    /// `main` at `57cf1b6`: `if: false` correctly turned the suite red,
    /// and `"if": false` -- the same key, quoted -- left all 34 tests
    /// green while Actions skipped the step. The old parser took its
    /// key as `cur.split(':').next()` with no un-quoting, so the key
    /// read `"if"` and matched no entry in `NON_GATING_KEYS`.
    ///
    /// Nothing un-quotes anything now: the key arrives from the parser
    /// already resolved, so every spelling of it is the same key by
    /// construction.
    #[test]
    fn a_quoted_key_is_the_same_key() {
        for spelling in [
            "\"if\": false",
            "'if': false",
            "\"continue-on-error\": true",
            "'continue-on-error': true",
        ] {
            let yaml = GATING.replace(
                "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
                &format!(
                    "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n        {spelling}\n"
                ),
            );
            assert!(
                gating_runs_that_prove_the_build_traps(&yaml).is_empty(),
                "`{spelling}` is the same key as its bare spelling; quoting it must not \
                 make a skipped step count as the thing gating the merge"
            );
        }
    }

    /// And one level up, on the job.
    #[test]
    fn a_quoted_key_on_the_job_is_the_same_key() {
        for spelling in ["\"if\": false", "\"continue-on-error\": true"] {
            let yaml = GATING.replace("  test:\n", &format!("  test:\n    {spelling}\n"));
            assert!(
                gating_runs_that_prove_the_build_traps(&yaml).is_empty(),
                "`{spelling}` on the job is the same key as its bare spelling"
            );
        }
    }

    /// THE COMMENTED-OUT TRIGGER, ALSO A SILENT DEFEAT. The old check
    /// asked whether the `on:` block's raw text -- comments included --
    /// contained the characters `pull_request`, so commenting the
    /// trigger out left the guard green on a workflow that no longer
    /// ran on pull requests at all. Measured on `main`: 34 passed,
    /// both arms.
    #[test]
    fn a_commented_out_pull_request_trigger_does_not_gate() {
        let commented_with_another_trigger_left = GATING.replace(
            "  pull_request:\n    branches: [main]\n",
            "  # pull_request:\n  #   branches: [main]\n  push:\n    branches: [main]\n",
        );
        let only_a_comment_naming_it = GATING.replace(
            "  pull_request:\n    branches: [main]\n",
            "  # pull_request disabled while we investigate flaky runners\n  push:\n    branches: [main]\n",
        );
        for yaml in [
            &commented_with_another_trigger_left,
            &only_a_comment_naming_it,
        ] {
            assert!(
                gating_runs_that_prove_the_build_traps(yaml).is_empty(),
                "a trigger named only in a comment is not a trigger; the parser drops \
                 comments before anything compares a name, so there is no `#` to strip \
                 and none to forget:\n{yaml}"
            );
        }
    }

    /// A whole-name comparison, so a trigger that merely begins with
    /// those characters is a different trigger. `pull_request_review`
    /// fires on a review, not on the pull request, and cannot be what
    /// gates the merge.
    #[test]
    fn a_trigger_that_merely_begins_with_pull_request_does_not_gate() {
        let yaml = GATING.replace(
            "  pull_request:\n    branches: [main]\n",
            "  pull_request_review:\n    types: [submitted]\n",
        );
        assert!(
            gating_runs_that_prove_the_build_traps(&yaml).is_empty(),
            "pull_request_review is not pull_request; a substring match cannot tell \
             them apart and this comparison must"
        );
    }

    /// `pull_request_target` is not `pull_request`, and is refused on
    /// purpose. It runs against the base repository with a write token
    /// and the repository's secrets, and checks out the base ref by
    /// default, so a workflow triggered only that way may never build
    /// the contributor's code. `rust-fs-xfs#146` and
    /// `rust-fs-ext4#149` record it as a live gap in the hand-rolled
    /// guard this file replaces.
    ///
    /// Pinned as a test rather than left to the comparison, because the
    /// clause is one line and was previously written by hand and copied
    /// between repositories. This is what stops it coming back.
    #[test]
    fn pull_request_target_does_not_gate() {
        let yaml = GATING.replace(
            "  pull_request:\n    branches: [main]\n",
            "  pull_request_target:\n    branches: [main]\n",
        );
        assert!(
            gating_runs_that_prove_the_build_traps(&yaml).is_empty(),
            "pull_request_target runs with the base repository's token and secrets \
             and checks out the base ref; it is not proof that the merge is gated"
        );
    }

    /// `on:` may be a sequence of names rather than a mapping, in
    /// either the flow or the block spelling, and all three are
    /// ordinary workflows.
    #[test]
    fn a_sequence_of_triggers_is_read() {
        for spelling in [
            "on: [push, pull_request]\n",
            "on:\n  - push\n  - pull_request\n",
        ] {
            let yaml = GATING.replace("on:\n  pull_request:\n    branches: [main]\n", spelling);
            assert_eq!(
                gating_runs_that_prove_the_build_traps(&yaml).len(),
                1,
                "this workflow triggers on a pull request as surely as the mapping \
                 spelling does:\n{yaml}"
            );
        }
    }

    /// THE ARM THAT WAS A FALSE ALARM RATHER THAN A DEFEAT, AND SO
    /// CANNOT BE WITNESSED BY THE SUITE GOING RED -- it already did.
    /// The witness is that legal YAML now passes.
    ///
    /// The old parser treated only a bare `|` as a block opener
    /// (`after != "|"`), so `|-`, `|+`, `>`, `>-` and `|2` were read as
    /// the command itself and the block's contents never parsed at all.
    /// Measured on `main`: `run: |` 34 passed, `run: |-` and `run: >`
    /// each EXIT=101 with 2 failed -- the guard refusing a completely
    /// correct workflow, which is the fastest way to get a guard
    /// deleted.
    ///
    /// A parser knows all five styles because they are the grammar.
    #[test]
    fn every_block_scalar_style_is_read_whole() {
        for style in ["|", "|-", "|+", ">", ">-", "|2"] {
            let yaml = GATING.replace(
                "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
                &format!(
                    "      - name: a block\n        run: {style}\n          EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n"
                ),
            );
            assert_eq!(
                gating_runs_that_prove_the_build_traps(&yaml).len(),
                1,
                "`run: {style}` is a legal block scalar carrying the gating command; \
                 failing here is the guard refusing a correct workflow:\n{yaml}"
            );
        }
    }

    /// A command quoted in a YAML comment is not a run. This used to be
    /// the shell scanner's job and is the parser's now: comments do not
    /// survive parsing, so there is no `#` handling here to get wrong.
    /// It is asserted at this level because that is where the property
    /// now lives -- `ci.yml` really does quote the gating command
    /// verbatim in the comment block above it, so a scan that missed
    /// this would stay green after the step itself was deleted.
    #[test]
    fn a_debug_run_quoted_in_a_yaml_comment_does_not_gate() {
        let yaml = "\
on:
  pull_request:
    branches: [main]
jobs:
  test:
    steps:
      # Do not remove this as a duplicate of the runs above it:
      #     - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib
      - run: cargo test --locked --release
";
        assert!(
            gating_runs_that_prove_the_build_traps(yaml).is_empty(),
            "the gating command appears only inside a comment, and the step that \
             remains is a release run"
        );
    }

    /// A workflow the parser cannot read is a failure, never a pass.
    /// The direction matters: a guard that swallowed the error and
    /// returned an empty structure would report "no debug run gates
    /// this", which is also a failure and therefore safe -- but one
    /// that returned early with a pass would be the blindness this
    /// whole module exists to refuse.
    #[test]
    #[should_panic(expected = "not valid YAML")]
    fn a_workflow_that_does_not_parse_is_a_failure() {
        super::parse_workflow("jobs:\n  test:\n   - broken: [unclosed\n");
    }

    /// THE CONTROL THAT STOPS THE REFUSAL OVER-CORRECTING.
    ///
    /// `pull_request_target` is refused as insufficient ON ITS OWN.
    /// That is not the same as refusing any workflow that mentions it,
    /// and until this test existed nothing in the file could tell the
    /// two apart: every fixture carried at most one trigger, so this
    /// mutation survived the whole suite --
    ///
    /// ```text
    ///   any(t == "pull_request")
    ///       && !any(t == "pull_request_target")
    /// ```
    ///
    /// -- while refusing a perfectly gated workflow. Carrying both
    /// triggers is the ordinary way to reach repository secrets from a
    /// job without giving up the pull-request gate, and such a workflow
    /// IS gated, by its `pull_request:` key.
    ///
    /// An assertion whose result does not depend on the thing it claims
    /// to check is this project's own recurring defect; this one was in
    /// the test pinning the refusal rather than in the refusal itself.
    #[test]
    fn a_workflow_carrying_both_triggers_still_gates() {
        let yaml = GATING.replace(
            "  pull_request:\n    branches: [main]\n",
            "  pull_request:\n    branches: [main]\n  pull_request_target:\n    branches: [main]\n",
        );
        assert_ne!(yaml, GATING, "the mutation must actually apply");
        assert_eq!(
            gating_runs_that_prove_the_build_traps(&yaml).len(),
            1,
            "the workflow still triggers on pull_request, so it still gates; refusing it \
             because pull_request_target is also present would be the over-correction"
        );
    }

    /// THE HANDSHAKE MAY BE DECLARED IN THE STEP'S `env:` MAPPING.
    ///
    /// Not a concession: on a matrix including `windows-latest` it is
    /// the only spelling that works, because an inline
    /// `VAR=1 cargo test` prefix is bash syntax and a PowerShell syntax
    /// error. A guard that read only the command would refuse the
    /// correct workflow on every cross-platform crate here -- the loud
    /// direction, but wrong, and the fastest way to get a guard
    /// deleted.
    #[test]
    fn the_handshake_declared_in_an_env_mapping_counts() {
        for value in ["\"1\"", "1", "'1'"] {
            let yaml = GATING.replace(
                "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
                &format!(
                    "      - run: cargo test --locked --lib\n        env:\n          EXPECT_OVERFLOW_CHECKS: {value}\n"
                ),
            );
            assert_eq!(
                gating_runs_that_prove_the_build_traps(&yaml).len(),
                1,
                "`EXPECT_OVERFLOW_CHECKS: {value}` in an env mapping is the same \
                 instruction to Actions as the inline prefix, and on a Windows \
                 matrix it is the only one that works:\n{yaml}"
            );
        }
    }

    /// And it must not rescue a `--release` run. The checks are off in
    /// release deliberately, so a handshake there arms an assertion
    /// that would fire on every green run.
    #[test]
    fn the_handshake_in_an_env_mapping_does_not_count_on_a_release_run() {
        let yaml = GATING.replace(
            "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
            "      - run: cargo test --locked --release --lib\n        env:\n          EXPECT_OVERFLOW_CHECKS: \"1\"\n",
        );
        assert!(
            gating_runs_that_prove_the_build_traps(&yaml).is_empty(),
            "a release run cannot prove the build traps, however it is labelled"
        );
    }

    /// A step carrying an `env:` mapping is still a gating step. Over-
    /// strictness here would cost something real: the handshake itself
    /// lives in such a mapping.
    #[test]
    fn a_step_carrying_an_env_mapping_still_gates() {
        let yaml = GATING.replace(
            "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n",
            "      - run: EXPECT_OVERFLOW_CHECKS=1 cargo test --locked --lib\n        env:\n          SOMETHING_ELSE: \"1\"\n",
        );
        assert_eq!(
            gating_runs_that_prove_the_build_traps(&yaml).len(),
            1,
            "`env:` says nothing about whether the step's result is read"
        );
    }
}
