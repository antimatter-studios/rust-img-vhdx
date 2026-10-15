#!/usr/bin/env bash
# The CI profile's event plan and ci-ok verdict, as a truth table.
#
# scripts/ci-plan.sh decides quick or full from the event; scripts/ci-verdict.sh
# decides whether ci-ok is green from the plan and every job's result. A wrong
# row in either is silent: a quick plan on the merge queue lands untested code,
# and a lenient verdict turns a cancelled job green. Each row here names one.
set -uo pipefail
cd "$(dirname "$0")/../.."
fails=0
ok() { echo "ok: $1"; }
fail() { echo "FAIL: $1"; fails=$((fails + 1)); }

command -v jq >/dev/null || { echo "FAIL: jq is not installed; the CI fmt job provisions it"; exit 1; }

plan() { bash scripts/ci-plan.sh "$1" "$2" | tr '\n' ' '; }
expect_plan() {
  got="$(plan "$1" "$2")"
  [ "$got" = "$3" ] && ok "plan $1 code=$2 -> $3" || fail "plan $1 code=$2: got '$got', want '$3'"
}
expect_plan pull_request true "profile=quick code=false "
expect_plan pull_request false "profile=quick code=false "
expect_plan merge_group true "profile=full code=true "
expect_plan merge_group false "profile=full code=false "
expect_plan push true "profile=full code=true "
expect_plan workflow_dispatch true "profile=full code=true "
expect_plan workflow_call true "profile=full code=true "
expect_plan some_future_event true "profile=full code=true "
bash scripts/ci-plan.sh pull_request maybe >/dev/null 2>&1 && fail "plan accepted an invalid code" || ok "plan refuses an invalid code"
bash scripts/ci-plan.sh >/dev/null 2>&1 && fail "plan accepted no arguments" || ok "plan refuses no arguments"

# needs JSON: always-run jobs, then full-only jobs, each with a result.
needs() {
  local a="$1" f="$2"
  printf '{"changes":{"result":"%s"},"fmt":{"result":"%s"},"unit":{"result":"%s"},"semver":{"result":"%s"},' "$a" "$a" "$a" "$a"
  printf '"test":{"result":"%s"},"test-release":{"result":"%s"},"qemu-validation":{"result":"%s"},"cli":{"result":"%s"},"standalone":{"result":"%s"}}' "$f" "$f" "$f" "$f" "$f"
}
verdict() { printf '%s' "$3" | bash scripts/ci-verdict.sh "$1" "$2" >/dev/null 2>&1; }
expect_green() { verdict "$1" "$2" "$3" && ok "$4" || fail "$4 (want green)"; }
expect_red() { verdict "$1" "$2" "$3" && fail "$4 (want red)" || ok "$4"; }

expect_green full true "$(needs success success)" "full run, every job green"
expect_green quick false "$(needs success skipped)" "quick run, full-only jobs skipped"
expect_green full false "$(needs success skipped)" "documentation-only full run, full-only jobs skipped"
expect_red full true "$(needs success skipped)" "full code run refuses a skipped full-only job"
expect_red full true "$(needs success failure)" "full run refuses a failed job"
expect_red full true "$(needs success cancelled)" "full run refuses a cancelled job"
expect_red quick false "$(needs success failure)" "quick run refuses a failed full-only job"
expect_red quick false "$(needs success cancelled)" "quick run refuses a cancelled full-only job"
expect_red quick false "$(needs skipped skipped)" "quick run refuses a skipped always-run job"
expect_red quick false "$(needs failure skipped)" "quick run refuses a failed always-run job"
expect_red quick false "$(needs cancelled skipped)" "quick run refuses a cancelled always-run job"
expect_red quick true "$(needs success skipped)" "a quick plan asking for the full pipeline is invalid"
expect_red "" false "$(needs success skipped)" "a missing profile fails"
expect_red full "" "$(needs success success)" "a missing code fails"
expect_red bogus true "$(needs success success)" "an invalid profile fails"
expect_red full true '{"changes":{"result":"success"}}' "missing jobs fail"
expect_red full true "$(needs success success | jq -c 'del(.unit)')" "a missing always-run job fails"
expect_red full true "$(needs success success | jq -c '. + {"extra":{"result":"success"}}')" "an unlisted job fails"
expect_red full true "not json" "unparseable needs fails"

# The workflow uses both scripts, and ci-ok needs every job the verdict names.
grep -q 'bash scripts/ci-plan.sh' .github/workflows/ci.yml && ok "ci.yml plans with ci-plan.sh" || fail "ci.yml does not call ci-plan.sh"
grep -q 'bash scripts/ci-verdict.sh' .github/workflows/ci.yml && ok "ci-ok decides with ci-verdict.sh" || fail "ci-ok does not call ci-verdict.sh"
grep -q 'needs: \[changes, test, test-release, fmt, qemu-validation, cli, semver, standalone, unit\]' .github/workflows/ci.yml \
  && ok "ci-ok needs every planned job" || fail "ci-ok needs list differs from the verdict's jobs"
grep -qE '^  workflow_dispatch:' .github/workflows/ci.yml && ok "ci.yml has a manual full run" || fail "ci.yml lost workflow_dispatch"
grep -qE '^  merge_group:' .github/workflows/ci.yml && ok "ci.yml still runs in the merge queue" || fail "ci.yml lost merge_group"

[ "$fails" -eq 0 ] || { echo "test-ci-profile.sh: $fails check(s) failed"; exit 1; }
echo "test-ci-profile.sh: all checks passed"
