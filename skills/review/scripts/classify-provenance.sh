#!/usr/bin/env bash
# Tag each finding new or pre-existing by whether the cited line lies inside
# a changed hunk of the merge-base diff. A pre-existing finding is filed as
# a backlog node by the lane and costs the author no round; the classifier
# (review_findings.rs) reads the tag as nonblocking.
#
# Usage: classify-provenance.sh <findings.json> <out.json> [base]
#   <findings.json>  the lane's findings payload: a JSON array, or an object
#                    carrying {"findings": [...]}
#   <out.json>       where the annotated array is written
#   [base]           diff base; default BASE env, then origin/main (the same
#                    default the lane's Phase 0 and empty-diff guard use)
#
# The rule: the cited line inside a + side range of the merge-base diff
# (committed range plus the working-tree delta, so a pre-commit review tags
# correctly) reads new (pre_existing false). Outside every range reads
# pre-existing (true). A finding with no resolvable line reads new (false),
# fail closed: only a positively-proven pre-existing line gets the tag. An
# unresolvable base tags everything new and says so.
set -euo pipefail

usage() {
  sed -n '/^# Usage:/,/^# The rule:/p' "$0" | head -n -1
  exit "${1:-0}"
}
[ $# -ge 1 ] && [ $# -le 3 ] || usage 1
[ "${1:-}" = "-h" ] && usage 0

FINDINGS="$1"
OUT="$2"
BASE="${3:-${BASE:-origin/main}}"
REPO="${REPO:-$(pwd)}"

command -v jq >/dev/null || { echo "classify-provenance: jq is required" >&2; exit 1; }
[ -r "$FINDINGS" ] || { echo "classify-provenance: findings file not readable: $FINDINGS" >&2; exit 1; }
jq -e . "$FINDINGS" >/dev/null 2>&1 || { echo "classify-provenance: findings file is not valid JSON: $FINDINGS" >&2; exit 1; }

TMPD="$(mktemp -d -t fno-prov-XXXXXX)"
trap 'rm -rf "$TMPD"' EXIT

# The changed-line ranges: + side of both the committed range and the
# working-tree delta, as path<TAB>start<TAB>end rows.
: > "$TMPD/ranges.tsv"
append_ranges() {
  # `|| true` keeps an unresolvable ref from killing the script under
  # set -e; the rev-parse guard below owns the fail-closed message.
  { git -C "$REPO" diff -U0 --no-color "$@" 2>/dev/null | awk -v OFS='\t' '
    /^\+\+\+ b\// { sub(/^\+\+\+ b\//, "", $0); f = $0; next }
    /^\+\+\+ /    { next }
    /^@@/ {
      if (f != "" && match($0, /\+[0-9]+(,[0-9]+)?/)) {
        s = substr($0, RSTART + 1, RLENGTH - 1)
        if (idx = index(s, ",")) {
          e = substr(s, idx + 1)
          s = substr(s, 1, idx - 1)
        } else {
          e = 1
        }
        if (e + 0 > 0) printf "%s\t%s\t%s\n", f, s + 0, s + e - 1
      }
    }
  ' >> "$TMPD/ranges.tsv"; } || true
}
# An unmeasurable base cannot prove any line pre-existing: tag everything
# new and say so, before any range is read.
if ! git -C "$REPO" rev-parse -q --verify "$BASE^{commit}" >/dev/null 2>&1; then
  echo "classify-provenance: base $BASE unresolvable; tagging every finding new (fail closed)" >&2
  jq -n --rawfile raw "$FINDINGS" '
    ($raw | fromjson | if type == "object" then .findings else . end)
    | if type != "array" then error("findings payload must be an array or {\"findings\": [...]}") end
    | map(. + {pre_existing: false})
  ' > "$OUT" || { echo "classify-provenance: jq refused the payload" >&2; exit 1; }
  NEW=$(jq '[.[] | select(.pre_existing == false)] | length' "$OUT")
  echo "provenance: $NEW new, 0 pre-existing"
  exit 0
fi

append_ranges "$BASE...HEAD"
append_ranges "HEAD"

# pre_existing per finding: no resolvable line -> false; file with no
# changed hunk -> true; line inside a range -> false; outside all -> true.
awk -F'\t' '{ printf "{\"p\": \"%s\", \"s\": %s, \"e\": %s}\n", $1, $2, $3 }' "$TMPD/ranges.tsv" | jq -s '.' > "$TMPD/ranges.json"

jq -n --rawfile raw "$FINDINGS" --slurpfile rows "$TMPD/ranges.json" '
  ($rows[0] // []) as $r
  | ($raw | fromjson | if type == "object" then .findings else . end) as $payload
  | if ($payload | type) != "array" then
      error("findings payload must be an array or {\"findings\": [...]}")
    end
  | $payload
  | map(
      (.line | (tonumber? // -1)) as $l
      | (.file // "") as $p
      | [ $r[] | select(.p == $p) ] as $mine
      | if $l < 1 then false
        elif ($mine | length) == 0 then true
        else (any($mine[]; $l >= .s and $l <= .e) | not)
        end as $pre
      | . + {pre_existing: $pre}
    )
' > "$OUT" || { echo "classify-provenance: jq refused the payload" >&2; exit 1; }

NEW=$(jq '[.[] | select(.pre_existing == false)] | length' "$OUT")
PRE=$(jq '[.[] | select(.pre_existing == true)] | length' "$OUT")
echo "provenance: $NEW new, $PRE pre-existing"
