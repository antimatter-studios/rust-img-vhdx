# Working in rust-img-vhdx (agent guide)

Pure-Rust VHDX reader and writer, validated against `qemu-img`. This file is the fast path
for an agent picking up work here, so the workflow does not have to be
re-derived each time. It points at the existing docs rather than duplicating
them:

- **README** → what the crate does, how it is built, and what does not work yet.
- **`chores.yml`** → every task named below, and what each one actually runs.
- **`.github-guard`** → what must pass before `main` takes a merge.

The section between the BEGIN/END markers below is **shared, byte-identical,
with every repository in this family**. Do not edit it here: change the
canonical copy and propagate it, or `scripts/agents-core-check.sh` will fail.
Everything after the END marker is specific to this repository.

<!-- BEGIN SHARED BLOCK: agent-core v2 sha256:8e0e4d55b026cff6cc3476160ed112117ca8ea64ad456408b9f1fe2a1dcf308b -->
## Claiming work

Several agents work these repositories at the same time. Before you start on
an issue, claim it, so nobody else spends a session on what you are already
doing. The lock is a **GitHub label**, because labels are shared state that
every agent can read and change without posting comments into the thread.

**Before starting.** Check, claim, then read back:

```sh
gh issue view <N> --json labels                      # holds `claimed`? pick another
gh issue edit <N> --add-label claimed --add-label claim/<session>
gh issue view <N> --json labels                      # read back and confirm
```

`<session>` is your session name — `agent-<random4>-<isodate>`, e.g.
`agent-3f7c-2026-09-22`. Create the `claim/<session>` label if it does not
exist.

**Resolving a race.** Adding a label is not compare-and-swap: two agents can
both add `claimed` and both believe they won. That is what the read-back is
for. If it shows more than one `claim/*` label, the **lexically lowest**
session keeps the issue; every other agent removes its own `claim/*` label and
picks different work. Each racer computes the same answer independently, so no
further coordination is needed.

**When you finish or stop.** Remove both labels — on merge, or the moment you
abandon the work:

```sh
gh issue edit <N> --remove-label claimed --remove-label claim/<session>
```

Delete your `claim/<session>` label from the repository at the end of your
session so they do not accumulate.

**Reclaiming a stale claim.** An agent that dies holding a claim would block an
issue forever. If `claimed` was applied more than 12 hours ago and the holder's
branch has no commits since, any agent may take it: remove the stale `claim/*`,
add your own, and say so in the issue.

**This is a convention, not a fence.** Nothing enforces it. An agent that
ignores it duplicates work; it cannot corrupt anything. Honour it anyway.

## Work in a worktree

Every working copy is a **git worktree** of an existing checkout, made with
`git worktree add`. Never `git clone` a second, unlinked copy — not for a
branch, a PR, a review, or a sibling you need at another ref:

```sh
git -C <checkout> fetch origin
git -C <checkout> worktree add <path> -b <type>/<name> origin/main   # new work
git -C <checkout> worktree add --detach <path> <tag>                 # a sibling at a pinned ref
git -C <checkout> worktree remove <path>                             # when done
```

A worktree shares the checkout's objects and remotes, and `git worktree list`
shows it to every agent on the machine, so nobody else mistakes it for
abandoned work or loses track of it. An unlinked clone copies all the history
again, is invisible to that list, and gets left behind in `/tmp` long after the
work that made it is merged. Remove your worktree when you finish.

## Skills to use

- **`dev-loop`** — the required loop for any non-trivial change: baseline the
  full suite → change → re-run (no baseline test may regress) → enhance tests →
  vet. Always run it.
- **`commit`** / **`pr`** — for grouping commits and opening pull requests.

Each repository names any further skills of its own below.

## A bug fix starts with a red

**Prove it is broken first** — a failing check or test — *then* fix it, *then*
prove that same check is green, *then* confirm the full baseline still passes.
Never write the fix before you have a red. A fix with no failing test to its
name is a claim, not a result.

## Nothing skips

A test that cannot run **fails**, naming the task that would provide what it
needed. Never add an early return for a missing fixture, tool or VM: a skipped
test reads exactly like a passing one, and a suite that quietly declines to run
is indistinguishable from a suite that passes.

Where a tier reports skips or ignored tests, that is a gate, not a note.

## Validate against something that is not us

A driver's own readers share its interpretation of the format, so they cannot
catch a misreading: the mistake is baked into the fixture *and* the parser, and
they agree with each other while disagreeing with every real filesystem. Unit
tests over self-built fixtures prove self-consistency, not correctness.

Every structure that is parsed or written gets a cross-validation test against
an **independent oracle** — the platform's own tools, a real kernel, or a third
implementation — before it is considered done. Each repository names its
oracles below.

## Output is budgeted

Test tiers run through `scripts/tier.sh`, which runs the suite **quietly**: the
whole run goes to `tmp/logs/<tier>.log`, a pass prints one verdict line naming
that log, and a failure prints its tail. CI keeps the logs as an artifact, so
the detail is always retrievable.

The budget caps the log, not merely what is shown, and every number in the
table was measured. A run that passes but prints more than its budget **fails**.

The reader who pays most for a noisy suite is an agent that re-reads its whole
transcript on every step, and so pays for one loud run many times over. If a
tier legitimately grows, raise its row **with the measurement that justifies
it**. Do not silence output to fit, and do not route around `tier.sh`.

## Commits and branches

- Branches are `<type>/<name>`, matching the commit type: `fix/`, `feat/`,
  `ci/`, `docs/`, `chore/`, `test/`.
- A commit is a subject plus flat one-sentence bullets. Subjects are
  declarative, not imperative: "the run-end bound is checked", not "check the
  run-end bound".
- **No AI attribution and no co-author trailers**, in commits or in pull
  request descriptions.
- `main` takes **squash merges only**.

## Project rules

- **No GPL/LGPL/AGPL dependencies.** Permissive only (MIT/BSD/Apache).
  Shelling out to a copyleft CLI as a *test oracle* is fine — linking or
  copying it is not.
- **Each of these is a standalone project.** Never mention a consuming
  application in the README, the source, or CLI help.
<!-- END SHARED BLOCK: agent-core v2 -->
## What this is

Pure-Rust VHDX reader and writer over `am-fs-core`, linked into the app as a
staticlib.

## Running tests

```sh
chore test          # the suite
chore testqemu      # against qemu-img
chore testrelease   # release profile
chore lint          # fmt, the agent-core check, clippy
chore staticlib     # what the app links
```

CI runs `test`, `test-release`, `qemu-validation`, `fmt`, aggregated by `ci-ok`.

## The oracle is qemu-img

An image this crate writes must be one `qemu-img` reads identically, and one
`qemu-img` wrote must read identically here. If `qemu-img` is missing the job
**fails**; it does not skip.

## The BAT entry is a packed field, and there is exactly one decode

A VHDX BAT entry packs three things into one 64-bit word: payload **state** in
the low 3 bits, bits 3-19 reserved, and the file **offset in whole MiB** above
bit 20. `BatEntry::from_u64` (`src/bat.rs:98-105`) is the **only** decode — both
reader call sites go through it — and
`bat_entry_ignores_reserved_bits_between_state_and_offset` (`src/bat.rs:197`)
pins the reserved bits. The encode is open-coded at `src/reader.rs:886`.

If you add a second decode site, you have created the class of bug that audit
exists to prevent. Go through `from_u64`.

## Checksums are defined over an exact size

`compute_crc` in `src/header.rs` and `src/region_table.rs` return `Result<u32>`
and **refuse a short buffer** with `Error::Corrupt` (#113). They are not given a
fixed-size array parameter on purpose: the fuzz targets hand them raw bytes, and
a signature that cannot take a short slice cannot be fuzzed with one.

## How this format allocates, and why it asks first

Appending is the only way VHDX allocates: put a block at the tail, then record
where it went. That used to work because a write past the end of a
`FileDevice` grew the file underneath it — and rust-fs-core#75 stopped it,
correctly: `size_bytes()` went on reporting the construction-time length while
the file grew, so `CachingDevice` could serve bytes no cached read could reach
(rust-fs-core#70). The pin sat at `v0.2.10` for six releases because of it.

`BlockDevice::set_len` (rust-fs-core#161, v0.2.12) is the replacement, and
this crate calls it in the two places that grow the file (#117):

- `VhdxReader::allocate_block_for` — extends to the new tail **before** the
  zero-init write, under the `dev_size` lock, so the lock never names bytes
  the device does not have. Order matters: the file grows first, the bound
  second, and a refusal leaves the bound describing what is really there.
- `log::apply_chain` — one extension, up front, to the bound the descriptor
  check has already enforced. That bound *is* `last_file_offset`, so the file
  ends where the log says it ends, and the one-byte write at `wanted - 1` that
  used to put it there is gone.

Both methods are **defaulted** on the trait — `set_len` to `Err(ReadOnly)`,
`can_grow` to `false` — so a wrapping device that does not forward them turns
a growable device into one that cannot allocate, silently. Every double in
`tests/` that wraps a `FileDevice` forwards both; `CutAfter` deliberately
spends its crash budget on `set_len` too, because a lost extension is a crash
point of its own.

`VhdxReader`'s own `impl BlockDevice` answers `can_grow() == false`
explicitly, not by omission: the guest disk's length is in the metadata
region, so growing it is not something a caller can ask for by writing.

## The output budget comes from rust-fs-core, at run time

Every tier goes through `scripts/tier.sh`, and the wrapper it runs — the
thing that keeps a run quiet, logs all of it and fails a run that printed
more than its budget — is **rust-fs-core's `scripts/output-budget.sh`**.
There is no copy of it in this repository and there must not be one again
(rust-fs-core#153): the family had several copies, reached four different
ways, each internally consistent and nothing comparing them.

`tier.sh` resolves it in this order, takes the **first candidate that
exists**, and **refuses** rather than falling through:

1. `$FS_CORE_ROOT/scripts/output-budget.sh`, when that variable is set;
2. `../rust-fs-core/scripts/output-budget.sh`, the sibling. Sibling before
   cargo is load-bearing: this suite runs on `windows-latest` under Git Bash,
   where `cargo metadata` answers with a `C:\...` path that Git Bash can
   neither test nor copy;
3. the `am-fs-core` package root `cargo metadata` names.

Whichever it finds is then **verified by running it**: `--version` must
answer exactly `rust-fs-core-output-budget 1`. A wrapper that is present and
answers something else is fatal. It is not pinned by SHA-256 — a digest in
seven repositories has to be raised in seven repositories for every edit to
one file, which is the lockstep this arrangement removes.

**`OUTPUT_BUDGET_VERBOSE`, not `FLTH_VERBOSE`.** The canonical script does
not read the old name, and setting it does nothing at all — no error, the run
simply stays quiet. If `--verbose` ever stops streaming, that is the first
thing to check.

### One core pin now, where there used to be two

`ci.yml` clones `../rust-fs-core` once, at **v0.2.13**, and exports
`FS_CORE_ROOT` at it. That is both the crate this one compiles against and the
checkout the wrapper comes from.

It was two clones at two numbers: the dependency held at v0.2.10 by the
allocation problem above, and the wrapper needing v0.2.13 — the first release
with the quiet-failure behaviour, where v0.2.11 is the first that ships the
script at all. A shell script this repository runs is not code it links, so
the two pins were genuinely independent. #117 moved the dependency to the
number the tooling already needed, and they became one.

## What gates a merge

One required check, `ci-ok`, declared in `.github-guard` and aggregating every
job in `ci.yml`. `fuzz.yml` (nightly cron plus dispatch) and `release.yml`
(tag-driven) never report on a pull request and must never be required.

`chore check:ci-gate` holds both halves of that mechanically — every job in
`ci.yml` must appear in `ci-ok`'s `needs:`, and `.github-guard` must require
`ci-ok` and nothing else. The task names `scripts/ci-gate.sh` and nothing else,
so the script is what can be tested, reviewed and run without `chore` at all.
It replaced `tests/ci_aggregate_gate.rs`: that parsed a YAML file and compared
strings, exercising nothing this crate ships, and as a `cargo test` it counted
towards the executed-test floor the gate itself enforces.

Judging mergeability from check **conclusions** is unreliable: an in-progress
`CheckRun` reports its conclusion as an empty string, and a `StatusContext` has
no conclusion field at all. Read `mergeStateStatus` and
`statusCheckRollup.state`.

## Never grow a shared tool to solve a problem here

**Never grow a shared tool to solve a problem in this repository.** `chore` is
a general-purpose task runner this project merely consumes; the same goes for
`github-guard` and the agent-skills hooks. If something needed here looks like
it belongs inside one of them, it does not. Solve it here, or ask first. The
tell is a release: if a shared tool needs a new version cut whose only purpose
is to unblock this project, the code is in the wrong repository.
