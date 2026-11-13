#!/usr/bin/env bash
# PR quality: four measures of the merged pull requests of one GitHub repo.
# The data file is the record. Anyone can fetch it again and get the same
# tables, give or take PRs merged since.
#
# Usage:
#   scripts/metrics/pr-quality.sh fetch [--repo OWNER/NAME] [--out FILE]
#       Read every merged PR over the REST API (gh api) and write one JSON
#       line per PR. Default repo fnodotdev/fno, default out stdout.
#   scripts/metrics/pr-quality.sh analyze [--now ISO-8601] FILE
#       Print the markdown tables. --now sets the end of the 90-day window;
#       the default is the last merged_at in FILE, so a rerun on the same
#       file prints the same tables.
#   scripts/metrics/pr-quality.sh --selftest
#       Run analyze on a fixture and check every figure. Reads no network.
#
# The measures (analyze):
#   first-push CI pass  share of PRs whose first commit has a SUCCESS rollup.
#                       Rollup: no check run and no status is "no run"; any
#                       failed, timed-out or startup-failed run, or a failure
#                       or error status, is FAILURE; any run not completed, or
#                       a pending status, is PENDING (counts as not passed);
#                       any cancelled run is CANCELLED, since a later push
#                       cancels the run in flight and that is no verdict; else
#                       SUCCESS. "No run" and CANCELLED leave the denominator.
#                       GitHub keeps the latest attempt of each check, so a
#                       rerun hides the first try.
#   reverted            share of merged PRs that a later merged PR reverts.
#                       A target is found three ways: a "This reverts commit
#                       <sha>" line in a commit message or the body, where
#                       <sha> is a commit of another PR or its merge commit;
#                       a "Reverts #N" or "Reverts owner/name#N" body; a title
#                       Revert "<title>" that names another PR's title. A
#                       revert inside the same PR does not count. A revert
#                       pushed straight to the branch is not seen, so this is
#                       a lower bound.
#   hours to merge      created_at to merged_at; p50 and p90 are nearest-rank.
#   changed by review   share of reviewed PRs with a commit dated after the
#                       first submitted review. The date is the committer
#                       date, which a rebase rewrites. Bot reviews count.
#
# Env:
#   PR_QUALITY_GH  gh binary (default gh); the selftest never calls it.

set -euo pipefail

GH="${PR_QUALITY_GH:-gh}"
die() { echo "pr-quality: $*" >&2; exit 2; }
command -v jq >/dev/null || die "jq is required"

# ------------------------------------------------------------------- fetch

fetch() {
    local repo="fnodotdev/fno" out=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --repo) repo="${2:?--repo needs OWNER/NAME}"; shift 2 ;;
            --out) out="${2:?--out needs a path}"; shift 2 ;;
            *) die "fetch: unknown argument: $1" ;;
        esac
    done
    local work
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' RETURN

    "$GH" api --paginate "repos/$repo/pulls?state=closed&per_page=100" --jq '
        .[] | select(.merged_at != null) | {
            number, title, created_at, merged_at,
            merge_commit_sha,
            author_type: .user.type,
            body_revert_shas: [(.body // "") | scan("This reverts commit ([0-9a-f]{7,40})") | .[0]],
            body_revert_prs: [(.body // "") | scan("(?i)reverts? (?:[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)?#([0-9]+)") | .[0] | tonumber]
        }' >"$work/pulls.jsonl"

    local total n
    total="$(wc -l <"$work/pulls.jsonl" | tr -d ' ')"
    echo "pr-quality: $total merged PRs in $repo" >&2
    while IFS= read -r n; do
        "$GH" api --paginate "repos/$repo/pulls/$n/commits?per_page=100" --jq '
            .[] | {sha, date: .commit.committer.date,
                   revert_shas: [.commit.message | scan("This reverts commit ([0-9a-f]{7,40})") | .[0]]}' \
            | jq -s . >"$work/c.json"
        local first
        first="$(jq -r '.[0].sha // empty' "$work/c.json")"
        if [[ -n "$first" ]]; then
            "$GH" api --paginate "repos/$repo/commits/$first/check-runs?per_page=100" \
                --jq '.check_runs[] | {status, conclusion}' | jq -s . >"$work/r.json"
            "$GH" api "repos/$repo/commits/$first/status" --jq '[.statuses[].state]' >"$work/s.json"
        else
            echo '[]' >"$work/r.json"; echo '[]' >"$work/s.json"
        fi
        "$GH" api --paginate "repos/$repo/pulls/$n/reviews?per_page=100" \
            --jq '.[] | select(.submitted_at != null) | {submitted_at, author_type: .user.type}' \
            | jq -s . >"$work/v.json"
        jq -c --argjson n "$n" \
            --slurpfile c "$work/c.json" --slurpfile r "$work/r.json" \
            --slurpfile s "$work/s.json" --slurpfile v "$work/v.json" '
            select(.number == $n) | {
                number, title, created_at, merged_at, merge_commit_sha, author_type,
                commit_shas: [$c[0][].sha],
                commit_dates: [$c[0][].date],
                revert_shas: ((.body_revert_shas + [$c[0][].revert_shas[]]) | unique),
                revert_prs: (.body_revert_prs | unique),
                first_checks: $r[0],
                first_statuses: $s[0],
                review_count: ($v[0] | length),
                first_review_at: ([$v[0][].submitted_at] | min),
                first_review_bot: (($v[0] | sort_by(.submitted_at) | .[0].author_type // "") == "Bot")
            }' "$work/pulls.jsonl" >>"$work/out.jsonl"
    done < <(jq -r '.number' "$work/pulls.jsonl")

    jq -s -c 'sort_by(.number) | .[]' "$work/out.jsonl" >"$work/sorted.jsonl"
    if [[ -n "$out" ]]; then cp "$work/sorted.jsonl" "$out"; else cat "$work/sorted.jsonl"; fi
}

# ----------------------------------------------------------------- analyze

# jq program: reads the PR array, prints the markdown report.
ANALYZE_JQ='
def rollup:
  if (.first_checks | length) == 0 and (.first_statuses | length) == 0 then null
  elif any(.first_checks[]; .conclusion as $c
        | ["failure","timed_out","action_required","startup_failure"]
        | index([$c]) != null)
    or any(.first_statuses[]; . == "failure" or . == "error") then "FAILURE"
  elif any(.first_checks[]; .status != "completed")
    or any(.first_statuses[]; . == "pending") then "PENDING"
  elif any(.first_checks[]; .conclusion == "cancelled" or .conclusion == "stale") then "CANCELLED"
  else "SUCCESS" end;
def ts: sub("\\.[0-9]+Z$"; "Z") | fromdateiso8601;
def rank($p): sort | if length == 0 then null else .[((length * $p) | ceil) - 1] end;
def pct($a; $b): if $b == 0 then "n/a" else "\(($a * 1000 / $b) | round / 10)%" end;
def h1: if . == null then "n/a" else "\((. * 10) | round / 10)" end;

. as $all
| ($all | map({key: .merge_commit_sha, value: .number}) | from_entries) as $by_merge
| ([$all[] | .number as $n | .commit_shas[] | {key: ., value: $n}] | from_entries) as $by_commit
| ($all | map({key: (.number | tostring), value: .}) | from_entries) as $by_num
| ($all | map({key: .title, value: .number}) | from_entries) as $by_title
| ([ $all[] | . as $p
     | ( (.revert_shas[] | . as $s
            | ($by_merge | to_entries | map(select(.key != null and (.key | startswith($s)))) | .[0].value)
              // ($by_commit | to_entries | map(select(.key | startswith($s))) | .[0].value))
       , .revert_prs[]
       , ((.title | capture("^Revert \"(?<t>.*)\"$")? | .t) as $t | $by_title[$t]) )
     | select(. != null and . != $p.number)
     | select(($by_num[tostring].merged_at | ts) < ($p.merged_at | ts))
   ] | unique) as $reverted
| ($now // ($all | map(.merged_at) | max)) as $end
| (($end | ts) - 90 * 86400) as $cut
| def row($label; $ps):
    ($ps | map(rollup)) as $r
    | ($r | map(select(. != null and . != "CANCELLED"))) as $known
    | ($ps | map(select(.review_count > 0))) as $rev
    | ($rev | map(.first_review_at as $f | select(any(.commit_dates[]; (. | ts) > ($f | ts))))) as $chg
    | ($ps | map(((.merged_at | ts) - (.created_at | ts)) / 3600)) as $hrs
    | "| \($label) | \($ps | length) | \(pct($known | map(select(. == "SUCCESS")) | length; $known | length)) of \($known | length) (\($r | map(select(. == null)) | length) no run, \($r | map(select(. == "CANCELLED")) | length) cancelled) | \(pct($ps | map(select(.number as $n | $reverted | index($n) != null)) | length; $ps | length)) | \($hrs | rank(0.5) | h1) / \($hrs | rank(0.9) | h1) | \(pct($chg | length; $rev | length)) (\($rev | length) reviewed) |";
  def head: "| Window | Merged | First-push CI pass | Reverted | Hours to merge (p50 / p90) | Changed by review |",
            "|---|---|---|---|---|---|";
  "Repo data: \($all | length) merged PRs, first merge \($all | map(.merged_at) | min | .[0:10]), last merge \($all | map(.merged_at) | max | .[0:10]).",
  "Reverted PRs: \(if ($reverted | length) == 0 then "none" else ($reverted | map("#\(.)") | join(", ")) end).",
  "Last 90 days: merged_at on or after \($cut | todate | .[0:10]).",
  "",
  head,
  row("All time"; $all),
  row("Last 90 days"; $all | map(select((.merged_at | ts) >= $cut))),
  "",
  "Per month, by merged_at:",
  "",
  head,
  ($all | group_by(.merged_at[0:7])[] | row(.[0].merged_at[0:7]; .))
'

analyze() {
    local now="" file=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --now) now="${2:?--now needs an ISO-8601 time}"; shift 2 ;;
            -*) die "analyze: unknown argument: $1" ;;
            *) file="$1"; shift ;;
        esac
    done
    [[ -n "$file" && -f "$file" ]] || die "analyze: needs a data file from fetch"
    local nowjson="null"
    [[ -n "$now" ]] && nowjson="\"$now\""
    jq -s -r --argjson now "$nowjson" "$ANALYZE_JQ" "$file"
}

# ---------------------------------------------------------------- selftest

selftest() {
    local work
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' RETURN
    # 1: first push green, reviewed, fixed after review, reverted by 3.
    # 2: first push red, reviewed, no change after review.
    # 3: reverts 1 by commit sha; no CI on the first commit; no review.
    # 4: older than 90 days; pending CI; reverts #2 by body.
    # 5: Revert "<title of 4>" merged later; a same-PR revert counts nothing.
    # 6: a cancelled run beside a green one: no verdict.
    cat >"$work/d.jsonl" <<'EOF'
{"number":1,"title":"feat: a","created_at":"2026-10-01T00:00:00Z","merged_at":"2026-10-01T02:00:00Z","merge_commit_sha":"aaa1","author_type":"User","commit_shas":["c11","c12"],"commit_dates":["2026-10-01T00:00:00Z","2026-10-01T01:30:00Z"],"revert_shas":[],"revert_prs":[],"first_checks":[{"status":"completed","conclusion":"success"},{"status":"completed","conclusion":"skipped"}],"first_statuses":[],"review_count":1,"first_review_at":"2026-10-01T01:00:00Z","first_review_bot":true}
{"number":2,"title":"fix: b","created_at":"2026-10-02T00:00:00Z","merged_at":"2026-10-02T04:00:00Z","merge_commit_sha":"bbb2","author_type":"User","commit_shas":["c21"],"commit_dates":["2026-10-02T00:00:00Z"],"revert_shas":[],"revert_prs":[],"first_checks":[{"status":"completed","conclusion":"failure"}],"first_statuses":["success"],"review_count":2,"first_review_at":"2026-10-02T01:00:00Z","first_review_bot":false}
{"number":3,"title":"Revert a","created_at":"2026-10-03T00:00:00Z","merged_at":"2026-10-03T01:00:00Z","merge_commit_sha":"ccc3","author_type":"User","commit_shas":["c31"],"commit_dates":["2026-10-03T00:00:00Z"],"revert_shas":["aaa1"],"revert_prs":[],"first_checks":[],"first_statuses":[],"review_count":0,"first_review_at":null,"first_review_bot":false}
{"number":4,"title":"chore: d","created_at":"2026-06-01T00:00:00Z","merged_at":"2026-06-01T10:00:00Z","merge_commit_sha":"ddd4","author_type":"Bot","commit_shas":["c41"],"commit_dates":["2026-06-01T00:00:00Z"],"revert_shas":[],"revert_prs":[2],"first_checks":[{"status":"in_progress","conclusion":null}],"first_statuses":[],"review_count":0,"first_review_at":null,"first_review_bot":false}
{"number":5,"title":"Revert \"chore: d\"","created_at":"2026-10-04T00:00:00Z","merged_at":"2026-10-04T08:00:00Z","merge_commit_sha":"eee5","author_type":"User","commit_shas":["c51","c52"],"commit_dates":["2026-10-04T00:00:00Z","2026-10-04T00:10:00Z"],"revert_shas":["c51"],"revert_prs":[],"first_checks":[],"first_statuses":["success"],"review_count":0,"first_review_at":null,"first_review_bot":false}
{"number":6,"title":"feat: f","created_at":"2026-10-05T00:00:00Z","merged_at":"2026-10-05T03:00:00Z","merge_commit_sha":"fff6","author_type":"User","commit_shas":["c61"],"commit_dates":["2026-10-05T00:00:00Z"],"revert_shas":[],"revert_prs":[],"first_checks":[{"status":"completed","conclusion":"cancelled"},{"status":"completed","conclusion":"success"}],"first_statuses":[],"review_count":0,"first_review_at":null,"first_review_bot":false}
EOF
    analyze "$work/d.jsonl" >"$work/out.md"
    local fail=0 want
    # #2 is reverted by #4, but #4 merged before #2, so it does not count.
    # All time: rollups S,F,no run,P,S,CANCELLED -> 2 of 4 = 50%.
    # Reverted 1,4 of 6 = 33.3%. Hours 2,4,1,10,8,3 -> p50 3, p90 10.
    # Reviewed 1,2; changed 1 = 50%.
    for want in \
        'Reverted PRs: #1, #4.' \
        'Last 90 days: merged_at on or after 2026-07-07.' \
        '| All time | 6 | 50% of 4 (1 no run, 1 cancelled) | 33.3% | 3 / 10 | 50% (2 reviewed) |' \
        '| Last 90 days | 5 | 66.7% of 3 (1 no run, 1 cancelled) | 20% | 3 / 8 | 50% (2 reviewed) |' \
        '| 2026-06 | 1 | 0% of 1 (0 no run, 0 cancelled) | 100% | 10 / 10 | n/a (0 reviewed) |' \
        '| 2026-10 | 5 | 66.7% of 3 (1 no run, 1 cancelled) | 20% | 3 / 8 | 50% (2 reviewed) |'; do
        if ! grep -qxF -- "$want" "$work/out.md"; then
            echo "selftest: missing line: $want" >&2
            fail=1
        fi
    done
    if [[ "$fail" -ne 0 ]]; then
        echo "selftest: got:" >&2
        cat "$work/out.md" >&2
        return 1
    fi
    echo "PASS pr-quality selftest"
}

case "${1:-}" in
    fetch) shift; fetch "$@" ;;
    analyze) shift; analyze "$@" ;;
    --selftest) selftest ;;
    -h|--help|"") sed -n '2,/^set -/{/^set -/q;s/^# \{0,1\}//p;}' "$0" ;;
    *) die "unknown command: $1 (fetch, analyze, --selftest)" ;;
esac
