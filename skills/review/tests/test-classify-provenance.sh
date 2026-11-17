#!/usr/bin/env bash
# Provenance contract tests for classify-provenance.sh (skills/review/scripts/).
#
# A finding must read pre-existing only when its cited line provably lies
# outside every changed hunk of the merge-base diff. These tests pin the
# four tag outcomes and the fail-closed refusal on a malformed payload.
set -uo pipefail

#CHUNK2
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$SCRIPT_DIR/../scripts/classify-provenance.sh"

PASS=0
FAIL=0
pass() { echo "  PASS: $*"; PASS=$((PASS + 1)); }
fail() { echo "  FAIL: $*"; FAIL=$((FAIL + 1)); }

TMP="$(mktemp -d -t fno-prov-test-XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

# ---- fixture: a base commit, then a change commit
export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=t@t
git -C "$TMP" init -q -b main 2>/dev/null || git -C "$TMP" init -q
printf '%s\n' line1 line2 line3 line4 line5 line6 line7 line8 line9 line10 > "$TMP/touched.txt"
printf '%s\n' other1 other2 other3 > "$TMP/untouched.txt"
git -C "$TMP" add . && git -C "$TMP" commit -qm base
printf '%s\n' line1 line2 line3 line4b line5b line6 line7 line8 line9 line10 line11 > "$TMP/touched.txt"
git -C "$TMP" add . && git -C "$TMP" commit -qm change

# HEAD lines: 4 and 5 are the rewritten lines (inside the + hunk), line 2 is
# unchanged but the file is touched, untouched.txt is not in the diff.
cat > "$TMP/findings.json" <<EOF
[{"file": "touched.txt", "line": 4, "summary": "in-hunk", "failure_scenario": "f"},
 {"file": "touched.txt", "line": 2, "summary": "touched-file-unchanged-line", "failure_scenario": "f"},
 {"file": "untouched.txt", "line": 1, "summary": "untouched-file", "failure_scenario": "f"}]
EOF

echo "== AC-P-HP: in-hunk line reads new, others read pre-existing"
OUT=$(cd "$TMP" && bash "$SCRIPT" findings.json out.json HEAD~1)
TAG0=$(jq -r '.[0].pre_existing' "$TMP/out.json")
TAG1=$(jq -r '.[1].pre_existing' "$TMP/out.json")
TAG2=$(jq -r '.[2].pre_existing' "$TMP/out.json")
[ "$TAG0" = "false" ] && pass "in-hunk line reads new" || fail "in-hunk line read pre_existing=$TAG0"
[ "$TAG1" = "true" ] && pass "unchanged line of touched file reads pre-existing" || fail "unchanged line read pre_existing=$TAG1"
[ "$TAG2" = "true" ] && pass "untouched file reads pre-existing" || fail "untouched file read pre_existing=$TAG2"
echo "$OUT" | grep -q 'provenance: 1 new, 2 pre-existing' && pass "summary line counts both tags" || fail "summary line wrong: $OUT"

echo "== AC-P-WRAP: an object payload with a findings key unwraps"
echo '{"findings": [{"file": "untouched.txt", "line": 2, "summary": "s", "failure_scenario": "f"}]}' > "$TMP/wrapped.json"
if (cd "$TMP" && bash "$SCRIPT" wrapped.json out2.json HEAD~1) >/dev/null 2>&1 \
   && [ "$(jq -r '.[0].pre_existing' "$TMP/out2.json")" = "true" ]; then
  pass "object payload unwraps and tags"
else
  fail "object payload refused or mistagged"
fi

echo "== AC-P-ERR: a bare object payload fails closed with a refusal"
echo '{"file": "untouched.txt", "line": 2, "summary": "s", "failure_scenario": "f"}' > "$TMP/bare.json"
if (cd "$TMP" && bash "$SCRIPT" bare.json out3.json HEAD~1) >/dev/null 2>&1; then
  fail "bare object payload was accepted"
else
  pass "bare object payload refused (fail closed)"
fi

echo "== AC-P-NOBASE: an unresolvable base tags everything new and says so"
MISSING=$(cd "$TMP" && bash "$SCRIPT" findings.json out4.json no-such-ref 2>&1 >/dev/null | grep -c 'unresolvable')
[ "$MISSING" -ge 1 ] && pass "unresolvable base warned" || fail "unresolvable base did not warn"
ALLNEW=$(jq '[.[] | select(.pre_existing == false)] | length' "$TMP/out4.json")
[ "$ALLNEW" = "3" ] && pass "unresolvable base tagged all findings new" || fail "unresolvable base mistagged: $ALLNEW"

echo
echo "classify-provenance: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
