#!/usr/bin/env bash
# The `ci-required` aggregate: one check that goes green only when the latest
# run of every other workflow at the PR head, and every check run inside those
# runs, finished without a failure or a timeout.
#
# A GitHub ruleset that requires one context treats a PR as mergeable the
# moment that context passes, so a merge lands while the rest of CI still
# runs, and a check that fails a minute later is already on main. A `needs:`
# list cannot reach jobs in other workflows, so this job polls the head.
#
# Only the latest run per workflow counts. A concurrency group cancels an
# older run when a newer one starts at the same head, and a rerun replaces an
# attempt; those superseded cancels are history, not verdicts. A cancelled
# latest run is an absent result, never a pass: the job keeps waiting for a
# rerun, and the job timeout fails it if none comes.
#
# Usage: ci-required.sh <owner/repo> <head-sha> <own-run-id>
#        ci-required.sh --classify <runs.json> <check-runs.json>
#          prints `fail`, `wait` or `pass` on the first line, then the rows
#          that decided it. Both files hold a JSON array of API objects.
# Exit:  0 pass, 1 fail (a latest run or check failed or timed out)

set -euo pipefail

SELF_NAME="ci-required"

classify() {
  jq -nr --slurpfile runs "$1" --slurpfile checks "$2" --arg self "$SELF_NAME" '
    def bad: . == "failure" or . == "timed_out" or . == "action_required" or . == "startup_failure";
    ($runs[0] | map(select(.name != $self))) as $all_runs
    | ($all_runs | group_by(.workflow_id // .name) | map(max_by(.id))) as $latest
    | ([$all_runs[].check_suite_id] - [$latest[].check_suite_id]) as $superseded
    | ($checks[0]
        | map(select(.name != $self and ((.check_suite.id) as $s | $superseded | index($s) | not)))
        | group_by(.name) | map(max_by(.id))) as $latest_checks
    | ([$latest[] | {name, status, conclusion: (.conclusion // "")}]
       + [$latest_checks[] | {name, status, conclusion: (.conclusion // "")}]) as $rows
    | [$rows[] | select(.status == "completed" and (.conclusion | bad))] as $failed
    | [$rows[] | select(.status != "completed" or .conclusion == "cancelled" or .conclusion == "stale")] as $open
    | if ($failed | length) > 0 then "fail", ($failed[] | "  \(.name): \(.conclusion)")
      elif ($open | length) > 0 then "wait", ($open[] | "  \(.name): \(.status) \(.conclusion)")
      else "pass", "  \($rows | length) rows green or skipped"
      end'
}

if [[ "${1:-}" == "--classify" ]]; then
  out="$(classify "$2" "$3")"
  printf '%s\n' "$out"
  [[ "$(head -1 <<<"$out")" != "fail" ]]
  exit
fi

REPO="${1:?owner/repo}"
SHA="${2:?head sha}"
SELF_RUN="${3:?own run id}"
POLL_S="${CI_REQUIRED_POLL_S:-30}"
# A run starts its downstream jobs only when the upstream ones finish, so one
# all-complete read can fall in the gap between stages. Require this many
# consecutive passing reads before the verdict.
SETTLE_READS="${CI_REQUIRED_SETTLE_READS:-3}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

settled=0
while :; do
  gh api --paginate "repos/$REPO/actions/runs?head_sha=$SHA&per_page=100" \
    --jq ".workflow_runs[] | select(.id != $SELF_RUN)" | jq -s . >"$tmp/runs.json"
  gh api --paginate "repos/$REPO/commits/$SHA/check-runs?per_page=100" \
    --jq '.check_runs[]' | jq -s . >"$tmp/checks.json"
  out="$(classify "$tmp/runs.json" "$tmp/checks.json")"
  case "$(head -1 <<<"$out")" in
    fail)
      echo "ci-required: FAILED at $SHA"
      tail -n +2 <<<"$out"
      exit 1
      ;;
    pass)
      settled=$((settled + 1))
      if [[ "$settled" -ge "$SETTLE_READS" ]]; then
        echo "ci-required: every latest run at $SHA passed"
        tail -n +2 <<<"$out"
        exit 0
      fi
      ;;
    *)
      settled=0
      echo "ci-required: waiting on $(($(wc -l <<<"$out") - 1)) row(s) at $SHA"
      ;;
  esac
  sleep "$POLL_S"
done
