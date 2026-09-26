#!/usr/bin/env bash
# tier.sh LABEL LOG-NAME MAX-LINES MAX-BYTES -- COMMAND [ARG...]
#
# One test tier, run QUIETLY and under a budget. The whole run goes to
# tmp/logs/<LOG-NAME>.log; a pass prints one verdict line naming the log, a
# failure prints the tail of it, and a run that passed but printed more than
# its budget fails with status 65.
#
# ONE WRAPPER FOR BOTH CALLERS. chores.yml runs the tiers for a person at a
# terminal and .github/workflows/ci.yml runs them for the gate, and they run
# the SAME command through the SAME budget -- so a tier that has outgrown its
# budget says so here, before the push, rather than in a CI log nobody was
# going to read. tests/ci_profile.rs checks that the two files agree on
# every tier's numbers; the duplication is deliberate (the workflow cannot
# read chores.yml without installing chore on three runner platforms) and it
# is checked rather than trusted.
#
# WHY THE BUDGET IS PART OF THE TASK. A passing run that prints three
# thousand lines hides the twenty that matter, and every reader pays for it:
# a person scrolling, a CI log viewer, and an agent working in the
# repository, which re-reads its whole transcript on each step and so pays
# for one verbose run many times over. Measured across this constellation:
# 4,661M cache-read tokens against 9.5M of output, and command output was the
# largest single contributor a repository controls.
#
# The budgets themselves are in chores.yml, next to the command each one
# bounds, and every one of them was MEASURED -- see the table there. Raise
# one deliberately when a tier grows, the way the executed-test floors are
# raised; a budget nobody can breach measures nothing.
#
# THE WRAPPER BELONGS TO rust-fs-core, AND IS RESOLVED AT RUN TIME.
#
# This repository used to carry scripts/output-budget.sh as a committed copy.
# It is gone. A committed copy is a copy that drifts: measured on 2026-09-22
# the family had several of them, reached four different ways, each
# repository internally consistent with itself and nothing comparing them.
# The canonical file is rust-fs-core's scripts/output-budget.sh, and this
# script takes a copy of it FOR THIS RUN, into tmp/, and deletes it on exit
# (tmp/ is gitignored and is where the tier logs already live). The copy
# exists so that a core checkout moving underneath a long run cannot change
# the wrapper mid-flight; it is not a vendoring.
#
# THE RESOLUTION ORDER, and it FAILS rather than falling back:
#
#   1. $FS_CORE_ROOT, when set. An explicit override for a core that is
#      somewhere else -- a second checkout in CI, or a scratch tree in
#      tests/output_budget.rs, which uses it to prove this resolver refuses
#      a core that is absent and one whose --version is wrong.
#   2. ../rust-fs-core, the sibling. SIBLING BEFORE CARGO IS LOAD-BEARING
#      HERE: this suite runs on windows-latest under Git Bash, and the
#      manifest_path cargo reports there is a Windows path (C:\...) that Git
#      Bash can neither test with -f nor hand to cp. The sibling path is
#      POSIX on every runner, and the manifest already names that directory
#      -- am-fs-core is `{ path = "../rust-fs-core" }`.
#   3. whatever `cargo metadata` says the am-fs-core package root is. With a
#      path dependency that is the same directory as 2, so this only starts
#      to answer something new if the manifest ever moves to a plain version
#      requirement against the published crate.
#
# THE FIRST CANDIDATE THAT EXISTS IS THE ONE USED, and it is then VERIFIED by
# running `--version` and requiring exactly `rust-fs-core-output-budget 1`. A
# file that is there but answers something else is FATAL -- there is no
# falling through to the next source and no falling back to anything local,
# because "the wrapper we found was wrong so we quietly used another one" is
# the drift this change removes, wearing a different hat.
#
# NO SHA-256 PIN, DELIBERATELY. A sibling repository pins the wrapper by
# digest. A digest pinned in seven repositories has to be raised in seven
# repositories for every edit to one file, which is exactly the lockstep this
# migration exists to remove. The `--version` string is the contract: it
# names the API, it is checked on every run, and it moves only when the
# interface does.
#
# THE TWO CORE PINS ARE DIFFERENT PINS IN THIS REPOSITORY, and that is not an
# oversight -- see "The pin you cannot bump" in AGENTS.md. The COMPILED
# dependency is pinned at v0.2.10, because from v0.2.11 a write past the end
# of a FileDevice is a refusal rather than an implicit extension and this
# format allocates by appending; measured on this branch, core v0.2.11 and
# v0.2.13 each fail `a_sound_log_region_still_opens_and_writes` with
# OutOfBounds at the device's exact end. The WRAPPER pin is v0.2.13, the
# first release with the quiet-failure behaviour, and ci.yml provides it as
# its own checkout through FS_CORE_ROOT. The wrapper pin cannot go below
# v0.2.11 at all: no earlier release contains the script.
#
# VERBOSE. `OUTPUT_BUDGET_VERBOSE=1`, or `--verbose`/`-v` in the chore
# invocation's CLI_ARGS (`chore test:debug -- --verbose`), streams the run as
# it happens as well as logging it. It does NOT lift the budget: the log is
# the same size either way, and a tier that has outgrown its budget should
# say so whether or not anybody was watching.
#
# THE VARIABLE WAS `FLTH_VERBOSE` until the wrapper moved to rust-fs-core.
# The canonical script does not read that name, and setting it does nothing
# at all -- it does not error, the run simply stays quiet. That is why the
# rename went through chores.yml and this file in one go, and why it is
# written down here, where somebody looking for the old name lands.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The contract. Not a digest -- see above.
OUTPUT_BUDGET_API="rust-fs-core-output-budget 1"
# The oldest core release that ships the wrapper at all.
CORE_MIN_REF="v0.2.11"

expected() {
    echo "         Expected one of:" >&2
    echo "           \$FS_CORE_ROOT/scripts/output-budget.sh   (FS_CORE_ROOT=${FS_CORE_ROOT:-unset})" >&2
    echo "           $REPO/../rust-fs-core/scripts/output-budget.sh" >&2
    echo "           <am-fs-core package root>/scripts/output-budget.sh, per cargo metadata" >&2
    echo "         answering '$OUTPUT_BUDGET_API' to --version." >&2
    echo "         The wrapper lives in antimatter-studios/rust-fs-core and is" >&2
    echo "         shipped from $CORE_MIN_REF onwards; this repository pins the" >&2
    echo "         checkout that provides it in .github/workflows/ci.yml." >&2
}

# `cargo metadata` prints one JSON document, and this needs one field out of
# it. jq is installed on every GitHub runner and python3 on all but Windows,
# so try both and say what is missing rather than guessing at the JSON with
# sed -- a parser that misreads its input reports an answer nobody checked.
core_root_from_cargo() {
    local json
    json="$(cargo metadata --format-version 1 --locked \
        --manifest-path "$REPO/Cargo.toml" 2>/dev/null)" || return 0
    [ -n "$json" ] || return 0
    if command -v jq >/dev/null 2>&1; then
        printf '%s' "$json" | jq -r \
            'first(.packages[] | select(.name == "am-fs-core") | .manifest_path) // ""' \
            | sed 's![/\\][^/\\]*$!!'
        return 0
    fi
    local python
    for python in python3 python; do
        command -v "$python" >/dev/null 2>&1 || continue
        printf '%s' "$json" | "$python" -c '
import json, re, sys
packages = json.load(sys.stdin)["packages"]
path = next((p["manifest_path"] for p in packages if p["name"] == "am-fs-core"), "")
print(re.sub(r"[/\\\\][^/\\\\]*$", "", path))
'
        return 0
    done
    echo "tier.sh: neither jq nor python3 is here to read cargo metadata's" >&2
    echo "         answer, and ../rust-fs-core does not hold the wrapper." >&2
    return 0
}

if [ -n "${FS_CORE_ROOT:-}" ]; then
    BUDGET_SRC="$FS_CORE_ROOT/scripts/output-budget.sh"
    BUDGET_FROM="FS_CORE_ROOT"
elif [ -f "$REPO/../rust-fs-core/scripts/output-budget.sh" ]; then
    BUDGET_SRC="$REPO/../rust-fs-core/scripts/output-budget.sh"
    BUDGET_FROM="the ../rust-fs-core sibling"
else
    CORE_ROOT="$(core_root_from_cargo)"
    BUDGET_SRC="${CORE_ROOT:-}/scripts/output-budget.sh"
    BUDGET_FROM="the am-fs-core package cargo metadata named"
fi

if [ ! -f "$BUDGET_SRC" ]; then
    echo "tier.sh: no output-budget wrapper at $BUDGET_SRC ($BUDGET_FROM)." >&2
    expected
    exit 1
fi

# VERIFIED BY RUNNING IT, not by reading it. `bash "$BUDGET_SRC"` rather than
# executing it directly: a Windows checkout arrives without the executable
# bit, and this suite runs on windows-latest.
FOUND_API="$(bash "$BUDGET_SRC" --version 2>/dev/null || true)"
if [ "$FOUND_API" != "$OUTPUT_BUDGET_API" ]; then
    echo "tier.sh: $BUDGET_SRC ($BUDGET_FROM) answered '$FOUND_API' to" >&2
    echo "         --version, not '$OUTPUT_BUDGET_API'. This is fatal: a" >&2
    echo "         wrapper that is present but speaks a different API is not" >&2
    echo "         something to work around by reaching for another copy." >&2
    expected
    exit 1
fi

BUDGET="$REPO/tmp/output-budget.$$.sh"
mkdir -p "$REPO/tmp"
cp "$BUDGET_SRC" "$BUDGET"
trap 'rm -f "$BUDGET"' EXIT

[ $# -ge 5 ] || { echo "tier.sh: usage: tier.sh LABEL LOG MAX-LINES MAX-BYTES -- CMD..." >&2; exit 2; }
LABEL="$1"; LOG_NAME="$2"; MAX_LINES="$3"; MAX_BYTES="$4"; shift 4
[ "${1:-}" = "--" ] && shift
[ $# -gt 0 ] || { echo "tier.sh: no command" >&2; exit 2; }

# `chore test:debug -- --verbose` arrives as CLI_ARGS. output-budget.sh reads
# OUTPUT_BUDGET_VERBOSE itself, so mapping the flag onto it is all that is
# needed -- and it means the environment variable and the flag cannot
# disagree.
case " ${CLI_ARGS:-} " in
    *" --verbose "*|*" -v "*) export OUTPUT_BUDGET_VERBOSE=1 ;;
esac

# `--tail 40` IS ASKED FOR RATHER THAN ASSUMED. rust-fs-core#164 made a
# failing run quiet by default: it prints one line naming the log and shows
# nothing of the run unless a tail is requested. This repository's contract
# -- stated in chores.yml and in every ci.yml tier step -- is that a failure
# prints its reason without anybody fetching a file, so the tail is named
# here, at the one place every tier goes through.
#
# NOT `exec`, because the EXIT trap above has to run and delete the copy.
# The command's status is handed on unchanged, which is the whole point of
# the wrapper.
set +e
bash "$BUDGET" \
    --log "$REPO/tmp/logs/$LOG_NAME.log" \
    --max-lines "$MAX_LINES" \
    --max-bytes "$MAX_BYTES" \
    --tail 40 \
    --label "$LABEL" \
    -- "$@"
status=$?
set -e
exit "$status"
