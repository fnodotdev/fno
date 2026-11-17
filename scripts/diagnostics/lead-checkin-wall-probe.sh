#!/bin/bash
# Time one lead check-in beat against sandboxed copies of the real inputs.
#
# The beat writes its row journal, team-name stamp and refusal-trend ring on
# every run, so measuring the deployed command against the live fleet mutates
# state the next real beat keys on. This probe clones the agents home and the
# backlog graph (APFS clonefile: near-instant, copy-on-write), points
# FNO_AGENT_HOME at the clone, runs one beat with --no-emit, and reports the
# wall-clock seconds. Exit 0 means the beat finished inside --budget.
#
# Usage: scripts/diagnostics/lead-checkin-wall-probe.sh [--budget N]
#        [--scope SCOPE] [--binary PATH] [--keep]
#   --budget N   wall-clock ceiling in seconds (default 120, the foreground
#                Bash bound a lead beat must fit)
#   --scope      team scope string; defaults to the first key in the
#                machine's team_names.json
#   --binary     fno-agent binary to run; default resolves fno-agent on PATH
#   --keep       keep the sandbox for inspection instead of deleting it

set -u

BUDGET=120
SCOPE=""
BIN=""
KEEP=0
while [ $# -gt 0 ]; do
    case "$1" in
        --budget) BUDGET="$2"; shift 2 ;;
        --scope) SCOPE="$2"; shift 2 ;;
        --binary) BIN="$2"; shift 2 ;;
        --keep) KEEP=1; shift ;;
        *) echo "unknown flag: $1" >&2; exit 2 ;;
    esac
done

HOME_DIR="${FNO_AGENT_HOME:-$HOME/.fno/agents}"
GRAPH="${FNO_PROBE_GRAPH:-$HOME/.fno/db/graph.db}"
[ -d "$HOME_DIR" ] || { echo "probe: agents home not found: $HOME_DIR" >&2; exit 2; }
[ -f "$GRAPH" ] || { echo "probe: graph not found: $GRAPH" >&2; exit 2; }

if [ -z "$SCOPE" ]; then
    SCOPE=$(python3 -I -c '
import json, os
p = os.path.join(os.environ.get("FNO_AGENT_HOME", os.path.expanduser("~/.fno/agents")), "team_names.json")
teams = json.load(open(p)).get("teams", {})
keys = [k for k in teams if k.strip()]
if not keys:
    raise SystemExit("probe: no team scope in team_names.json; pass --scope")
print(keys[0])
') || exit 2
fi

if [ -z "$BIN" ]; then
    BIN=$(command -v fno-agent) || { echo "probe: fno-agent not on PATH; pass --binary" >&2; exit 2; }
fi

SANDBOX=$(mktemp -d "${TMPDIR:-/tmp}/lead-checkin-probe.XXXXXX")
cleanup() { [ "$KEEP" = 1 ] || rm -rf "$SANDBOX"; }
trap cleanup EXIT

# Clone, not copy: the agents home carries gigabytes of journals and the
# clonefile is copy-on-write, so the sandbox costs no real disk until the
# beat writes into it.
cp -cR "$HOME_DIR" "$SANDBOX/agents" || { echo "probe: agents home clone failed" >&2; exit 2; }
cp -c "$GRAPH" "$SANDBOX/graph.db" || { echo "probe: graph clone failed" >&2; exit 2; }
for need in registry.json team_names.json; do
    [ -e "$SANDBOX/agents/$need" ] || { echo "probe: sandbox input missing: agents/$need" >&2; exit 2; }
done

T0=$(date +%s)
FNO_AGENT_HOME="$SANDBOX/agents" "$BIN" lead-checkin --user-command \
    --scope "$SCOPE" --graph "$SANDBOX/graph.db" --no-emit --json \
    >"$SANDBOX/beat.json" 2>"$SANDBOX/beat.stderr"
RC=$?
T1=$(date +%s)

WALL=$((T1 - T0))
echo "beat wall: ${WALL}s (budget ${BUDGET}s, exit $RC)"
if [ "$RC" -ne 0 ]; then
    echo "probe: beat exited $RC; stderr tail:" >&2
    tail -5 "$SANDBOX/beat.stderr" >&2
    exit 2
fi
if [ "$WALL" -ge "$BUDGET" ]; then
    echo "probe: beat wall ${WALL}s exceeds budget ${BUDGET}s" >&2
    exit 1
fi
[ "$KEEP" = 1 ] && echo "sandbox kept: $SANDBOX"
exit 0
