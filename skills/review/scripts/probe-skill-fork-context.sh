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
#      Avoid regex metacharacters; the match is literal either way.
#   2. Invoke the code-review skill via the Skill tool (any level; low is
#      cheapest) and wait for the fork to finish. The fork transcript is
#      the new agent-*.jsonl under the session's subagents/ dir, written
#      beside a *.forked-skill.marker.json sidecar.
#   3. Judge:
#      skills/review/scripts/probe-skill-fork-context.sh \
#        "<canary>" <builder-transcript.jsonl> <fork-transcript.jsonl>
#
# Three checks, three failure directions:
#
#   Identity: the marker sidecar beside the fork transcript must carry
#   forkedSkill=true and a code-review skillName (the same rule the attest
#   hook's REVIEWER_RE applies; this diagnostic re-states it). Without it
#   the verdict would be issued about a transcript that is not the
#   code-review fork.
#
#   Seed: builder rows carrying the canary BEFORE the fork's birth. Birth
#   is the first parseable fork row that carries a timestamp, and the
#   comparison is real datetime math. Naming the canary inside the judge
#   call seeds the builder transcript by itself, so the count is
#   precedence-bound, never presence-bound.
#
#   Inheritance: canary rows in the fork OUTSIDE tool-result rows. The
#   fork legitimately quotes reviewed diff and file content inside tool
#   results, so those rows are excluded; replayed builder context would
#   surface in ordinary message rows, which stay counted.
#
# Verdicts (print + exit code):
#   0  PROMPT_ONLY  marker valid, seed pre-dates the fork, zero canary
#                   rows in the fork outside tool results: the fork starts
#                   empty and the fresh stamp stays sound.
#   1  INHERITED    canary rows present in the fork outside tool results:
#                   the fork carries builder rows and the fresh stamp is
#                   unsound.
#   2  VOID         semantic failure: seed has no pre-fork builder rows,
#                   the marker is missing or does not name code-review, or
#                   the probe failed internally. Never the INHERITED code.
#   64  usage       wrong argument count or an unreadable transcript path.
#
# First measured run, 2026-11-26, claude harness:
#   CMD: bash skills/review/scripts/probe-skill-fork-context.sh \
#          CANARY-x6ca9-k9t4wq7z <session>.jsonl \
#          <session>/subagents/agent-a5cb73247eeaa1ba6.jsonl
#   OUT: forked-skill marker: present
#        pre-fork builder canary rows: 7
#        fork canary rows outside tool results: 0
#        PROMPT_ONLY: the fork starts empty; the fresh stamp stays sound
#   EXIT: 0
set -euo pipefail

# Any internal failure must land on VOID (2), never on INHERITED (1): a
# crash must read as "no verdict", not as the most alarming verdict.
trap 'rc=$?; if [[ $rc -ne 0 && $rc -ne 1 && $rc -ne 2 && $rc -ne 64 ]]; then echo "VOID: probe failed internally (exit $rc)"; exit 2; fi' EXIT

[[ $# -eq 3 ]] || {
  echo "usage: probe-skill-fork-context.sh <canary> <builder-transcript.jsonl> <fork-transcript.jsonl>" >&2
  exit 64
}
canary="$1"
builder="$2"
fork="$3"
[[ -r "$builder" ]] || { echo "usage: builder transcript not readable: $builder" >&2; exit 64; }
[[ -r "$fork" ]] || { echo "usage: fork transcript not readable: $fork" >&2; exit 64; }

# Identity arm. REVIEWER_RE mirrors hooks/code-review-attest.sh; the hook
# is the canonical spelling of the reviewer identity rule.
marker="${fork%.jsonl}.forked-skill.marker.json"
if [[ -f "$marker" ]] \
  && jq -e '(.forkedSkill == true) and ((.skillName? // "") | test("^/?code-review([[:space:]]|$)"))' \
    "$marker" >/dev/null 2>&1; then
  echo "forked-skill marker: present"
else
  echo "VOID: no valid forked-skill marker beside the fork transcript; pass the code-review Skill fork's agent-*.jsonl"
  exit 2
fi

# Seed arm + inheritance arm. Python does the per-line work: malformed
# rows are skipped (a crash-truncated line must not abort the scan), and
# timestamps compare as datetimes, not as strings.
counts="$(python3 - "$builder" "$fork" "$canary" <<'PYEOF' || true
import json
import sys
from datetime import datetime

def parse_ts(value):
    if not isinstance(value, str):
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None

builder_path, fork_path, canary = sys.argv[1], sys.argv[2], sys.argv[3]

birth = None
with open(fork_path, encoding="utf-8", errors="replace") as f:
    for line in f:
        try:
            ts = parse_ts(json.loads(line).get("timestamp"))
        except ValueError:
            continue
        if ts is not None:
            birth = ts
            break
if birth is None:
    print("VOID: no parseable timestamp in the fork transcript; cannot bound the seed window")
    sys.exit(3)

pre = 0
with open(builder_path, encoding="utf-8", errors="replace") as f:
    for line in f:
        if canary not in line:
            continue
        try:
            ts = parse_ts(json.loads(line).get("timestamp"))
        except ValueError:
            continue
        if ts is not None and ts < birth:
            pre += 1

inherited = 0
with open(fork_path, encoding="utf-8", errors="replace") as f:
    for line in f:
        if '"tool_result"' in line or canary not in line:
            continue
        inherited += 1

print(pre)
print(inherited)
PYEOF
)"
if grep -q "^VOID:" <<<"$counts"; then
  echo "$counts"
  exit 2
fi
pre="$(sed -n '1p' <<<"$counts")"
inherited="$(sed -n '2p' <<<"$counts")"

echo "pre-fork builder canary rows: $pre"
echo "fork canary rows outside tool results: $inherited"

if (( inherited > 0 )); then
  # Inheritance dominates the seed check: builder material inside the fork
  # is the load-bearing detection, and a botched seed must not mute it.
  echo "INHERITED: the fork transcript carries builder rows; the fresh stamp is unsound"
  exit 1
fi
if (( pre == 0 )); then
  echo "VOID: the canary has no pre-fork builder rows; re-seed before the Skill call and retry"
  exit 2
fi
echo "PROMPT_ONLY: the fork starts empty; the fresh stamp stays sound"
