#!/usr/bin/env bash
# The ci-ok verdict: `ci-verdict.sh <profile> <code>` with the workflow's
# `toJSON(needs)` on stdin. Exit 0 only when every job that must run
# succeeded, and a job that may skip either succeeded or was skipped.
#
# Strict on purpose. A missing or invalid plan fails. A job that always runs
# must be present and green. A full-profile job may be skipped only when the
# plan said it would not run (quick profile, or documentation alone); failed,
# cancelled or missing is never accepted.
set -euo pipefail
[ "$#" -eq 2 ] || { echo "::error::ci-verdict: usage: ci-verdict.sh <profile> <code>"; exit 1; }
profile="$1" code="$2"
case "$profile" in quick|full) ;; *) echo "::error::ci-verdict: invalid plan profile '$profile'"; exit 1 ;; esac
case "$code" in true|false) ;; *) echo "::error::ci-verdict: invalid plan code '$code'"; exit 1 ;; esac
if [ "$profile" = quick ] && [ "$code" = true ]; then
  echo "::error::ci-verdict: a quick plan cannot ask for the full pipeline"; exit 1
fi
always="changes fmt unit semver"
full_only="test test-release qemu-validation cli standalone"
needs="$(cat)"
echo "$needs" | jq -e 'type == "object"' >/dev/null || { echo "::error::ci-verdict: needs is not a JSON object"; exit 1; }
bad=
for j in $always; do
  r="$(echo "$needs" | jq -r --arg j "$j" '.[$j].result // "missing"')"
  echo "$j: $r"
  [ "$r" = success ] || bad="$bad $j($r)"
done
for j in $full_only; do
  r="$(echo "$needs" | jq -r --arg j "$j" '.[$j].result // "missing"')"
  echo "$j: $r"
  if [ "$r" = success ]; then continue; fi
  if [ "$r" = skipped ] && [ "$code" = false ]; then continue; fi
  bad="$bad $j($r)"
done
# A job in needs that neither list names is a job this verdict has not been
# told about: refuse it rather than ignore it.
for j in $(echo "$needs" | jq -r 'keys[]'); do
  case " $always $full_only " in *" $j "*) ;; *) bad="$bad $j(unlisted)" ;; esac
done
if [ -n "$bad" ]; then
  echo "::error::not green:$bad (profile $profile, code $code)"
  exit 1
fi
echo "all gating jobs succeeded (profile $profile, code $code)"
