#!/usr/bin/env bash
# check-fixture-dates.sh - flags hard-coded calendar dates in test fixtures
# that age past the max-age window.
#
# A sample payload with a fixed creation date reads as live data until it
# silently crosses an age gate: the specimen fixture aged past
# pr_watch.max_age_days (14) on 2026-11-12 and flipped the behavior under
# test. This lint flags every whole-token ISO date (YYYY-MM-DD) in the
# fixture homes older than the window, so a fixed date is either historical
# enough to be stable or explicitly marked.
#
# A date embedded in a longer token (a plan slug like
# 2026-05-05-provider-rotation.md) is an identifier, not sample data, and
# never flags. A line carrying `fixture-date-ok:` opts itself out.
#
# Run: bash scripts/ci/check-fixture-dates.sh [--max-age-days N] [file...]
#   --max-age-days N  the window (default 14, pr_watch.max_age_days)
#   file...           check exactly these files (local test mode)
# Exit: 0 clean, 1 a stale unmarked date or an unusable scan, 2 misuse.

set -euo pipefail

MAX_AGE=14
FILES=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --max-age-days)
      [[ $# -ge 2 ]] || { echo "check-fixture-dates: --max-age-days needs a number" >&2; exit 2; }
      MAX_AGE="$2"; shift 2 ;;
    --max-age-days=*)
      MAX_AGE="${1#--max-age-days=}"; shift ;;
    -*)
      echo "check-fixture-dates: unknown flag $1" >&2; exit 2 ;;
    *)
      FILES+=("$1"); shift ;;
  esac
done

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

if [[ ${#FILES[@]} -eq 0 ]]; then
  while IFS= read -r f; do
    FILES+=("$f")
  done < <(git -C "$REPO_ROOT" ls-files 'tests/fixtures/**' 'tests/py/fixtures/**')
fi

if [[ ${#FILES[@]} -eq 0 ]]; then
  echo "check-fixture-dates: no fixture files tracked"
  exit 0
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
: > "$TMP/hits"

for f in ${FILES[@]+"${FILES[@]}"}; do
  orig="$f"
  [[ "$f" = /* ]] || f="$REPO_ROOT/$f"
  if [[ ! -f "$f" ]]; then
    echo "check-fixture-dates: fixture missing from the tree: $orig" >&2
    exit 1
  fi
  # Whole-token dates only: a slug-embedded date is an identifier. The
  # date math rides python3 because BSD and GNU date disagree on parsing.
  python3 - "$f" "$MAX_AGE" >> "$TMP/hits" <<'PY'
import re, sys
from datetime import date, timedelta

path, max_age = sys.argv[1], int(sys.argv[2])
tok = re.compile(r"(?<![\w/.-])([12][0-9]{3}-[01][0-9]-[0-3][0-9])(?![\w-])")
today = date.today()
for n, line in enumerate(open(path, encoding="utf-8", errors="replace"), 1):
    if "fixture-date-ok" in line:
        continue
    for m in tok.finditer(line):
        age = (today - date.fromisoformat(m.group(1))).days
        if age > max_age:
            print(f"stale fixture date: {path}:{n}: {m.group(1)} "
                  f"({age} days old; window {max_age})")
PY
done

if [[ -s "$TMP/hits" ]]; then
  cat "$TMP/hits" >&2
  echo "check-fixture-dates: stale date(s) above; make the fixture historical enough" >&2
  echo "  to be stable, or mark the line 'fixture-date-ok: <why it is deliberate>'" >&2
  exit 1
fi
echo "check-fixture-dates: ${#FILES[@]} fixture file(s) clean past ${MAX_AGE} days"
