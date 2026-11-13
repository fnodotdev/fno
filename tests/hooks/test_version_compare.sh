#!/usr/bin/env bash
# test_version_compare.sh
#
# Direct tests of hooks/lib/version-compare.sh: the floor nag's one compare.
# Regression cases from the 2026-11-13 false nag: an install at or above the
# floor (0.4.8 vs 0.4.5, 0.4.9rc1 vs 0.4.5) must not warn; a genuinely stale
# install must; equal versions and empty inputs must not.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
LIB="$REPO_ROOT/hooks/lib/version-compare.sh"

[[ -f "$LIB" ]] || { echo "FAIL: lib not found at $LIB" >&2; exit 1; }
# shellcheck source=../../hooks/lib/version-compare.sh
source "$LIB"

PASS=0
FAIL=0
pass() { echo "  PASS: $*"; PASS=$((PASS + 1)); }
fail() { echo "  FAIL: $*"; FAIL=$((FAIL + 1)); }

expect_below() { # expect_below <version> <floor> <label>
  if version_below_floor "$1" "$2"; then
    pass "$3"
  else
    fail "$3 (expected below, got not-below)"
  fi
}
expect_not_below() { # expect_not_below <version> <floor> <label>
  if version_below_floor "$1" "$2"; then
    fail "$3 (expected not-below, got below)"
  else
    pass "$3"
  fi
}

echo "== version-compare.sh =="

# The false-nag regression: installs at or above the floor stay silent.
expect_not_below "0.4.8" "0.4.5" "0.4.8 vs floor 0.4.5 is not below"
expect_not_below "0.4.9rc1" "0.4.5" "0.4.9rc1 vs floor 0.4.5 is not below"
expect_not_below "1.0.0" "0.4.5" "1.0.0 vs floor 0.4.5 is not below"
expect_not_below "0.4.5" "0.4.5" "equal version and floor is not below"

# The cases the inverted compare missed: stale installs warn.
expect_below "0.4.4" "0.4.5" "0.4.4 vs floor 0.4.5 is below"
expect_below "0.3.9" "0.4.5" "0.3.9 vs floor 0.4.5 is below"
expect_below "0.4.4rc2" "0.4.5" "0.4.4rc2 vs floor 0.4.5 is below"

# Empty inputs fail closed to silence.
expect_not_below "" "0.4.5" "empty version is not below"
expect_not_below "0.4.5" "" "empty floor is not below"

echo ""
if [[ $FAIL -eq 0 ]]; then
  echo "version-compare: $PASS passed, 0 failed"
  exit 0
fi
echo "version-compare: FAILED ($FAIL of $((PASS + FAIL)) failed)" >&2
exit 1
