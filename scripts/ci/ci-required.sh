#!/usr/bin/env bash
# The `ci-required` aggregate: one check that goes green only when the latest
# run of every other workflow at the PR head, and every check run inside those
# runs, finished without a failure or a timeout.
#
# When this job fails, its own log names the failing row and stops: the verdict
# lines at the end carry no error text. The row's real log lives in that row's
# workflow run, so fetch it there (`gh run view <run-id> --log-failed`, or
# `gh api --allow-escape-sequences repos/<owner/repo>/actions/jobs/<job-id>/logs`).
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
# A failed GitHub read is also an absent result, never a verdict. Each read
# retries what GitHub itself is failing: a named (HTTP 5xx), a (HTTP 429),
# or an error with no HTTP status at all (DNS, reset, timeout). An exhausted
# read waits for the next tick with the settle counter reset, and the job
# timeout fails the head if GitHub never heals. A named 4xx is GitHub
# answering "no" (auth, permissions, a missing ref); it will not heal, so it
# exits red on the first tick and the real error lands in the log.
#
# Usage: ci-required.sh <owner/repo> <head-sha> <own-run-id> [pr-number]
#        ci-required.sh --classify <runs.json> <check-runs.json> [labels.json]
#          prints `fail`, `wait` or `pass` on the first line, then the rows
#          that decided it. The first two files hold a JSON array of API
#          objects; the optional third holds a JSON array of label names.
# Exit:  0 pass, 1 fail (a latest run or check failed or timed out, or the
#        pull request carries the pr-hold label)

set -euo pipefail

SELF_NAME="ci-required"

classify() {
  local runs="$1" checks="$2" labels="${3:-}"
  if [[ -z "$labels" ]]; then
    labels="$(mktemp)"
    printf '[]\n' > "$labels"
  fi
  jq -nr --slurpfile runs "$runs" --slurpfile checks "$checks" \
    --slurpfile labels "$labels" \
    --arg self "$SELF_NAME" --arg holdlabel "pr-hold" \
    --arg holdmsg "pr-hold: a live pr-hold decision holds this pull request; release: fno backlog decide 'pr-hold:<owner/repo>#<n>' 'release' --authority operator --supersedes <decision id>" '
    def bad: . == "failure" or . == "timed_out" or . == "action_required" or . == "startup_failure";
    ($labels[0] // []) as $names
    | if ($names | index($holdlabel)) then "fail", ("  " + $holdmsg)
      else
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
      end
      end'
}

if [[ "${1:-}" == "--classify" ]]; then
  out="$(classify "$2" "$3" "${4:-}")"
  printf '%s\n' "$out"
  [[ "$(head -1 <<<"$out")" != "fail" ]]
  exit
fi

REPO="${1:?owner/repo}"
SHA="${2:?head sha}"
SELF_RUN="${3:?own run id}"
PR="${4:-}"
POLL_S="${CI_REQUIRED_POLL_S:-30}"
# A run starts its downstream jobs only when the upstream ones finish, so one
# all-complete read can fall in the gap between stages. Require this many
# consecutive passing reads before the verdict.
SETTLE_READS="${CI_REQUIRED_SETTLE_READS:-3}"
# Per-read retry budget: RETRY_ATTEMPTS tries total, backing off
# RETRY_BACKOFF_S seconds and doubling between tries (integers).
RETRY_ATTEMPTS="${CI_REQUIRED_RETRY_ATTEMPTS:-5}"
RETRY_BACKOFF_S="${CI_REQUIRED_RETRY_BACKOFF_S:-2}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# A missing or failed labels read answers empty, never blocks: the draft and
# the merge gate hold the PR already, so the label here is defense-in-depth.
# The read sits inside the poll loop, so a hold recorded after this run
# started still fails the head within one tick, and a released hold clears
# on the next tick of a rerun.
fetch_labels() {
  printf '[]\n' >"$tmp/labels.json"
  [[ -n "$PR" ]] || return 0
  gh api "repos/$REPO/issues/$PR/labels" --jq '.[].name' 2>/dev/null \
    | jq -R -s 'split("\n") | map(select(length > 0))' >"$tmp/labels.json" \
    || printf '[]\n' >"$tmp/labels.json"
}

# One GitHub read, retried while GitHub itself is failing. pipefail makes a
# partial-page failure fail the whole pipeline, so a tick never classifies
# on truncated data. Returns:
#   0  complete read written to $1
#   1  still failing after the last retry: the tick is absent, not a verdict
#   2  a named 4xx refusal: GitHub said no and will not heal
gh_read_retry() {
  local dest="$1" url="$2" prog="$3"
  local attempt=1 delay="$RETRY_BACKOFF_S" failed err
  while :; do
    failed=0
    gh api --paginate "$url" --jq "$prog" 2>"$tmp/gh.err" | jq -s . >"$dest" || failed=1
    if [[ "$failed" -eq 0 ]]; then
      return 0
    fi
    err="$(cat "$tmp/gh.err")"
    if [[ "$err" =~ \(HTTP\ 5[0-9][0-9]\) || "$err" =~ \(HTTP\ 429\) ]]; then
      : # transient: GitHub itself is failing; retry below
    elif [[ "$err" =~ \(HTTP\ [0-9]{3}\) ]]; then
      return 2
    fi
    # No named status in the stderr: transport error (DNS, reset, timeout);
    # transient too, so fall through to the retry below.
    if [[ "$attempt" -ge "$RETRY_ATTEMPTS" ]]; then
      return 1
    fi
    sleep "$delay"
    delay=$((delay * 2))
    attempt=$((attempt + 1))
  done
}

settled=0
while :; do
  runs_rc=0
  gh_read_retry "$tmp/runs.json" "repos/$REPO/actions/runs?head_sha=$SHA&per_page=100" \
    ".workflow_runs[] | select(.id != $SELF_RUN)" || runs_rc=$?
  checks_rc=0
  gh_read_retry "$tmp/checks.json" "repos/$REPO/commits/$SHA/check-runs?per_page=100" \
    '.check_runs[]' || checks_rc=$?
  fetch_labels
  if [[ "$runs_rc" -eq 2 || "$checks_rc" -eq 2 ]]; then
    echo "ci-required: GitHub refused a read; failing fast at $SHA"
    cat "$tmp/gh.err" >&2
    exit 1
  fi
  if [[ "$runs_rc" -ne 0 || "$checks_rc" -ne 0 ]]; then
    settled=0
    echo "ci-required: GitHub read failed after $RETRY_ATTEMPTS attempt(s); waiting at $SHA"
    sleep "$POLL_S"
    continue
  fi
  out="$(classify "$tmp/runs.json" "$tmp/checks.json" "$tmp/labels.json")"
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
