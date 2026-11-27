#!/usr/bin/env bash
# Probe whether the harness's Skill-tool code-review fork inherits the
# builder transcript. The attest hook (hooks/code-review-attest.sh) stamps
# reviewer_context=fresh on every code-review Skill fork from the sidecar
# marker alone; the marker carries no inheritance state, so this probe is
# the read that keeps that stamp honest.
#
# Recipe (steps 1-2 run from the dispatching session; step 3 judges):
#
#   1. Seed a unique canary string in the builder transcript: mention it in
#      an assistant message AND echo it from a Bash call, so it lands in
#      both assistant rows and tool-result rows of the transcript JSONL.
#   2. Invoke the code-review skill via the Skill tool (any level; low is
#      cheapest) and wait for the fork to finish. The fork transcript is
#      the new agent-*.jsonl under the session's subagents/ dir, written
#      beside a *.forked-skill.marker.json sidecar.
#   3. Judge:
#      skills/review/scripts/probe-skill-fork-context.sh \
#        "<canary>" <builder-transcript.jsonl> <fork-transcript.jsonl>
#
# The builder-side check counts PRE-FORK rows only: builder rows carrying
# the canary before the fork transcript's first-row timestamp. Naming the
# canary inside the judge call seeds the builder transcript by itself, so
# an unfiltered count would never void. Rows without a timestamp (rare in
# live transcripts) are skipped by the precedence filter.
#
# Verdicts (print + exit code):
#   0 PROMPT_ONLY  zero pre-fork builder canary rows, zero fork canary
#                  rows, control present in the fork: the fork starts
#                  empty and the fresh stamp stays sound.
#   1 INHERITED    canary present in the fork: the fork carries builder
#                  rows and the fresh stamp is unsound.
#   2 VOID         the seed has no pre-fork builder rows, the control
#                  found no skill rows in the fork (wrong file), or a
#                  transcript is missing or unreadable.
#
# First measured run, 2026-11-26, claude harness:
#   CMD: bash skills/review/scripts/probe-skill-fork-context.sh \
#          CANARY-x6ca9-k9t4wq7z <session>.jsonl \
#          <session>/subagents/agent-a5cb73247eeaa1ba6.jsonl
#   OUT: pre-fork builder canary rows: 7
#        fork transcript canary rows: 0
#        fork transcript control rows: 78
#        PROMPT_ONLY: the fork starts empty; the fresh stamp stays sound
#   EXIT: 0
set -euo pipefail

canary="${1:?usage: probe-skill-fork-context.sh <canary> <builder-transcript.jsonl> <fork-transcript.jsonl>}"
builder="${2:?usage: probe-skill-fork-context.sh <canary> <builder-transcript.jsonl> <fork-transcript.jsonl>}"
fork="${3:?usage: probe-skill-fork-context.sh <canary> <builder-transcript.jsonl> <fork-transcript.jsonl>}"

[[ -r "$builder" ]] || { echo "VOID: builder transcript not readable: $builder"; exit 2; }
[[ -r "$fork" ]] || { echo "VOID: fork transcript not readable: $fork"; exit 2; }

fork_birth="$(head -1 "$fork" | jq -r '.timestamp // empty' 2>/dev/null || true)"
if [[ -n "$fork_birth" ]]; then
  builder_hits="$(jq -r --arg birth "$fork_birth" \
    'select((.timestamp // "") != "" and (.timestamp < $birth))' "$builder" 2>/dev/null \
    | /usr/bin/grep -c -- "$canary" || true)"
else
  # No birth timestamp: precedence cannot be computed, so the weaker
  # presence check runs and says so.
  builder_hits="$(/usr/bin/grep -c -- "$canary" "$builder" || true)"
  echo "note: fork row 1 carries no timestamp; builder count is presence, not precedence"
fi
fork_hits="$(/usr/bin/grep -c -- "$canary" "$fork" || true)"
# The control must be a string every code-review fork transcript carries
# regardless of inheritance; it proves the fork file is the right file.
control_hits="$(/usr/bin/grep -c "code-review" "$fork" || true)"
echo "pre-fork builder canary rows: $builder_hits"
echo "fork transcript canary rows: $fork_hits"
echo "fork transcript control rows: $control_hits"

if (( builder_hits == 0 )); then
  echo "VOID: the canary has no pre-fork builder rows; re-seed before the Skill call and retry"
  exit 2
fi
if (( control_hits == 0 )); then
  echo "VOID: zero control hits; the fork file carries no code-review rows"
  exit 2
fi
if (( fork_hits > 0 )); then
  echo "INHERITED: the fork transcript carries builder rows; the fresh stamp is unsound"
  exit 1
fi
echo "PROMPT_ONLY: the fork starts empty; the fresh stamp stays sound"
