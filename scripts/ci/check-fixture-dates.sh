#!/usr/bin/env bash
# check-fixture-dates.sh - forbids a hard-coded calendar date on any line a
# change adds where fixtures live.
#
# A sample payload with a fixed date reads as live data until it silently
# crosses an age gate: the specimen fixture aged past pr_watch.max_age_days
# (14) on 2026-11-12 and flipped the behavior under test. An age window does
# not stop that; it only moves the failure 14 days later onto an unrelated
# PR. So the rule is plain: a line added under a fixture home carries no
# whole-token ISO date (YYYY-MM-DD). Build the date relative to now in the
# test, or mark the line `fixture-date-ok: <why it is deliberate>`.
#
# Fixture homes: tests/fixtures, tests/py/fixtures, every crates/**/tests
# tree, and every golden tree. Dates already on main stay: the 276 lines the
# wider scope found were never measured as age-sensitive one by one, and a
# line is judged when it is added.
#
# A date embedded in a longer token (a plan slug like
# 2026-05-05-provider-rotation.md) is an identifier, never sample data, and
# never flags. An ISO timestamp's date part (2026-01-01T...) never flags
# either: golden captures hold schema-locked timestamps that cannot carry a
# marker.
#
# Run: bash scripts/ci/check-fixture-dates.sh [file...]
#   (no args)  the added lines between the merge base with origin/main (or
#              FIXTURE_DATES_BASE_SHA, the push alarm's previous tip) and HEAD
#   file...    every line of exactly these files (local test mode)
# Exit: 0 clean, 1 a fixed unmarked date or an unusable scan, 2 misuse.

set -euo pipefail

FILES=()
for arg in "$@"; do
  case "$arg" in
    -*) echo "check-fixture-dates: unknown flag $arg" >&2; exit 2 ;;
    *) FILES+=("$arg") ;;
  esac
done

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$REPO_ROOT"
SCOPE=('tests/fixtures/**' 'tests/py/fixtures/**' 'crates/**/tests/**' '**/golden/**')

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Feed lines as "path<TAB>line<TAB>text" to one judge.
if [[ ${#FILES[@]} -gt 0 ]]; then
  for f in "${FILES[@]}"; do
    [[ -f "$f" ]] || { echo "check-fixture-dates: file missing: $f" >&2; exit 1; }
    awk -v p="$f" '{print p "\t" NR "\t" $0}' "$f" >> "$TMP/lines"
  done
  LABEL="${#FILES[@]} file(s)"
else
  BASE_SHA="${FIXTURE_DATES_BASE_SHA:-}"
  [[ "$BASE_SHA" == "0000000000000000000000000000000000000000" ]] && BASE_SHA=""
  if [[ -n "$BASE_SHA" ]]; then
    BASE="$(git rev-parse --verify --quiet "$BASE_SHA^{commit}")" || {
      echo "check-fixture-dates: base $BASE_SHA does not resolve in this checkout" >&2
      exit 1
    }
  else
    git fetch --quiet origin "+refs/heads/main:refs/remotes/origin/main" 2>/dev/null || true
    BASE="$(git merge-base origin/main HEAD 2>/dev/null)" || {
      echo "check-fixture-dates: no merge base with origin/main; fetch it" >&2
      exit 1
    }
  fi
  git diff --unified=0 --no-color --diff-filter=AM "$BASE" HEAD -- "${SCOPE[@]}" \
    | awk '
        /^\+\+\+ b\// { path = substr($0, 7); next }
        /^@@/ { split($3, a, ","); n = substr(a[1], 2) + 0; next }
        /^\+/ { print path "\t" n "\t" substr($0, 2); n++ }
      ' > "$TMP/lines"
  LABEL="added lines since ${BASE:0:10}"
fi

touch "$TMP/lines"
python3 - "$TMP/lines" > "$TMP/hits" <<'PY'
import re, sys
tok = re.compile(r"(?<![\w/.-])([12][0-9]{3}-[01][0-9]-[0-3][0-9])(?![\w-])")
for raw in open(sys.argv[1], encoding="utf-8", errors="replace"):
    path, n, text = raw.rstrip("\n").split("\t", 2)
    if "fixture-date-ok:" in text:
        continue
    for m in tok.finditer(text):
        print(f"fixed fixture date: {path}:{n}: {m.group(1)}")
PY

if [[ -s "$TMP/hits" ]]; then
  cat "$TMP/hits" >&2
  echo "check-fixture-dates: a fixed date in a fixture ages into a different test." >&2
  echo "  Build it relative to now, or mark the line 'fixture-date-ok: <why it is deliberate>'." >&2
  exit 1
fi
echo "check-fixture-dates: clean ($LABEL)"
