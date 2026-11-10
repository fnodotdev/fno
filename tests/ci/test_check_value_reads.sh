#!/usr/bin/env bash
# tests/ci/test_check_value_reads.sh
#
# Exercises scripts/ci/check-value-reads.sh against a scratch repo: a local
# origin holding main, and a feature branch whose commit is the case. Each
# case asserts the exit code AND a line only that outcome prints.
#
# Run: bash tests/ci/test_check_value_reads.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$(cd "${SCRIPT_DIR}/../.." && pwd)/scripts/ci/check-value-reads.sh"
[[ -f "$GATE" ]] || { echo "gate not found at $GATE" >&2; exit 1; }

export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
unset PR_BASE_REF PR_REMOTE VALUE_READS_BASE_SHA

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
PASS=0; FAIL=0

fresh() {
  rm -rf "$TMP/origin.git" "$TMP/repo"
  git init -q --bare "$TMP/origin.git"
  git init -q "$TMP/repo"
  cd "$TMP/repo" || exit 1
  git symbolic-ref HEAD refs/heads/main
  mkdir -p crates/fno/src crates/fno/src/tests
  printf 'fn a(v: &Value) { v.get("id"); }\n' > crates/fno/src/a.rs
  git add -A && git commit -qm base
  git remote add origin "$TMP/origin.git"
  git push -q origin main
  git checkout -q -b feature
}

commit() { git add -A && git commit -qm change; }

check() {
  local label="$1" want="$2" marker="$3" out got
  out="$(bash "$GATE" 2>&1)"; got=$?
  if [[ "$got" -eq "$want" && "$out" == *"$marker"* ]]; then
    PASS=$((PASS + 1))
  else
    FAIL=$((FAIL + 1))
    printf 'FAIL: %s\n  want exit %s with: %s\n  got exit %s:\n%s\n' "$label" "$want" "$marker" "$got" "$out"
  fi
}

fresh
check "head equal to base passes and prints the counts" 0 'fno          .get("field")                 1        1'

fresh
printf 'fn b(v: &Value) { v.get("x"); }\n' > crates/fno/src/b.rs
commit
check "one added read refuses and names the rule" 1 'crates/fno grew .get("field") from 1 to 2. Remedy: read a typed field: deserialize the JSON once at its boundary (docs/rust-style.md R1)'

fresh
printf 'pub fn c() -> Result<(), String> { Ok(()) }\n' > crates/fno/src/c.rs
commit
check "one added string error refuses and names R6" 1 'docs/rust-style.md R6'

fresh
printf 'fn t(v: &Value) { v.get("x"); }\n' > crates/fno/src/a_tests.rs
printf 'fn t(v: &Value) { v.get("x"); }\n' > crates/fno/src/tests/t.rs
commit
check "test files never move the count" 0 'check-value-reads: ok'

fresh
printf '\n' > crates/fno/src/a.rs
commit
check "a removed read passes" 0 'fno          .get("field")                 1        0'

echo "check-value-reads selftest: $PASS passed, $FAIL failed"
[[ "$FAIL" -eq 0 ]]
