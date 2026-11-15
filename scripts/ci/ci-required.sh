#!/usr/bin/env bash
# The `ci-required` aggregate: one check that goes green only when every other
# workflow run and check run at the PR head finished without a failure, a
# cancel or a timeout.
#
# A GitHub ruleset that requires one context treats a PR as mergeable the
# moment that context passes, so `gh pr merge --auto` lands it while the rest
# of CI still runs, and a check that fails a minute later is already on main.
# A `needs:` list cannot reach jobs in other workflows, so this job polls the
# head instead. A cancel fails it on purpose: a cancelled run is an absent
# result, never a pass.
#
# Usage: ci-required.sh <owner/repo> <head-sha> <own-run-id>
# Exit:  0 every other run at the head completed green or skipped
#        1 a run failed, was cancelled or timed out, or the read never settled

set -euo pipefail

REPO="${1:?owner/repo}"
SHA="${2:?head sha}"
SELF_RUN="${3:?own run id}"
SELF_NAME="ci-required"
POLL_S="${CI_REQUIRED_POLL_S:-30}"
# A run starts its downstream jobs only when the upstream ones finish, so one
# all-complete read can fall in the gap between stages. Require this many
# consecutive settled reads before the verdict.
SETTLE_READS="${CI_REQUIRED_SETTLE_READS:-3}"

# Bad conclusions, as GitHub spells them for check runs and workflow runs.
BAD='failure|cancelled|timed_out|action_required|startup_failure|stale'

settled=0
while :; do
  runs="$(gh api --paginate "repos/$REPO/actions/runs?head_sha=$SHA&per_page=100" \
    --jq ".workflow_runs[] | select(.id != $SELF_RUN) | [.name, .status, (.conclusion // \"\")] | @tsv")"
  checks="$(gh api --paginate "repos/$REPO/commits/$SHA/check-runs?per_page=100" \
    --jq ".check_runs[] | select(.name != \"$SELF_NAME\") | [.name, .status, (.conclusion // \"\")] | @tsv")"
  rows="$(printf '%s\n%s\n' "$runs" "$checks" | sed '/^$/d')"

  bad="$(printf '%s\n' "$rows" | awk -F'\t' -v bad="^($BAD)\$" '$2 == "completed" && $3 ~ bad' || true)"
  if [[ -n "$bad" ]]; then
    echo "ci-required: FAILED - a run at $SHA did not pass:"
    printf '%s\n' "$bad" | awk -F'\t' '{print "  " $1 ": " $3}'
    exit 1
  fi

  open="$(printf '%s\n' "$rows" | awk -F'\t' '$2 != "completed"' || true)"
  if [[ -z "$open" ]]; then
    settled=$((settled + 1))
    if [[ "$settled" -ge "$SETTLE_READS" ]]; then
      echo "ci-required: every run at $SHA completed green or skipped ($(printf '%s\n' "$rows" | grep -c .) rows)"
      exit 0
    fi
  else
    settled=0
    echo "ci-required: waiting on $(printf '%s\n' "$open" | grep -c .) run(s) at $SHA"
  fi
  sleep "$POLL_S"
done
