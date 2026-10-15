#!/usr/bin/env bash
# The CI profile for one run: `ci-plan.sh <event> <code>` prints two lines,
# `profile=quick|full` and `code=true|false`, for $GITHUB_OUTPUT.
#
# A pull request gets the quick profile: only the cheap jobs that always run
# (unit, fmt and its script tests, semver). Every other event -- the merge
# queue, a push to main, a manual dispatch, a workflow call, and any event not
# named here -- gets the full profile, so an unknown event can never shrink the
# pipeline. `code` is what the path-gated jobs read: true only when the run is
# full and the change is not documentation alone.
set -euo pipefail
[ "$#" -eq 2 ] || { echo "usage: ci-plan.sh <event> <code>" >&2; exit 2; }
event="$1" code="$2"
case "$code" in true|false) ;; *) echo "ci-plan: code must be true or false, got '$code'" >&2; exit 2 ;; esac
case "$event" in
  pull_request) profile=quick ;;
  *) profile=full ;;
esac
[ "$profile" = full ] || code=false
echo "profile=$profile"
echo "code=$code"
