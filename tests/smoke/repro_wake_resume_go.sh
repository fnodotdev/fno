#!/usr/bin/env bash
# Scripted end-to-end repro for x-3bb2: a parked worker gets "go" and runs
# to its next real gate. Stages a temp home/spaces root, seeds a bound
# registry row with a live manifest, sends bare "go" through the REAL hook
# + door, asserts the protocol injects and the wake row journals. Exit: 0
# pass, 1 fail, 77 skip (no jq, or fno-agents predates the door).

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

fail() { printf '[wake-repro] FAIL: %s\n' "$*" >&2; exit 1; }
pass() { printf '[wake-repro] PASS: %s\n' "$*"; }
skip() { printf '[wake-repro] SKIP: %s\n' "$*" >&2; exit 77; }

command -v jq >/dev/null 2>&1 || skip "jq not on PATH"
source "$REPO_ROOT/scripts/lib/with-timeout.sh" 2>/dev/null || true

# Resolve the binary most-local first, same as the hooks do.
source "$REPO_ROOT/hooks/lib/agents-bin.sh" 2>/dev/null || true
BIN="$(fno_agents_bin "$REPO_ROOT")"
[[ -n "$BIN" ]] || skip "no fno-agents binary resolvable"

# Door probe: an unknown hidden door falls through to the routable table and
# refuses; a binary with the door answers silent-empty (rc 0) on no stdin.
probe_out="$(printf '' | "$BIN" wake-context 2>/dev/null)"; probe_rc=$?
if [[ "$probe_rc" -ne 0 ]]; then
  skip "resolved fno-agents predates the wake-context door"
fi

# Stage an isolated world. HOME isolation keeps the global journal out of
# the operator's ~/.fno.
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
REPO="$TMP/repo"
mkdir -p "$REPO/.git"
export FNO_AGENTS_HOME="$TMP/agents-home"
export FNO_SPACES_DIR="$TMP/spaces"
export FNO_STATE_DIR="$TMP/state"
export HOME="$TMP/home"
mkdir -p "$HOME/.fno"

# Seed the registry row: claude session bound to the node, cwd = the repo.
mkdir -p "$FNO_AGENTS_HOME"
SID="3afe9590-0496-4ee7-89be-cdc23dced8a2"
REG="$FNO_AGENTS_HOME/registry.json"
python3 - "$REG" "$SID" "$REPO" <<'PYEOF' || fail "registry seed"
import json, sys
path, sid, repo = sys.argv[1], sys.argv[2], sys.argv[3]
doc = {"entries": [{
    "name": "w-wake-test", "harness": "claude",
    "harness_session_id": sid,
    "cwd": repo, "node": "x-3bb2", "node_basis": "wake-test",
}]}
json.dump(doc, open(path, "w"))
PYEOF

# Write the live manifest where the door resolves it: worktree_space_dir
# of the payload cwd. A non-repo... repo-rooted cwd keys the space on the
# canonical repo root, slashes swapped.
CANON="$(cd "$REPO" && pwd -P)"
SLUG="${CANON//\//-}"
MANIFEST="$TMP/spaces/$SLUG/target-state.md"
mkdir -p "$(dirname "$MANIFEST")"
cat > "$MANIFEST" <<MANIFEST_EOF
---
fno_id: 11111111-2222-4333-8444-555566667777
session_id: 11111111-2222-4333-8444-555566667777
harness_session_id: $SID
harness: claude
graph_node_id: x-3bb2
input: "x-3bb2"
plan_path: /tmp/nowhere-plan.md
attended: true
---
manifest body
MANIFEST_EOF

# AC3-HP: bare go through the REAL hook + door injects the protocol.
HOOK="$REPO_ROOT/hooks/wake-resume-inject.sh"
payload='{"session_id": "'"$SID"'", "prompt": "go", "cwd": "'"$REPO"'"}'
out="$(printf '%s' "$payload" | bash "$HOOK" 2>/dev/null)"
rc=$?
[[ "$rc" -eq 0 ]] || fail "hook rc=$rc"

# The protocol names the node and the turn-end rules.
[[ "$out" == *"x-3bb2"* ]] || fail "protocol missing node: $out"
[[ "$out" == *"MISSION COMPLETE"* ]] || fail "protocol missing turn rules"
pass "AC3-HP bare go injects the resume protocol"

# The wake row landed in the project journal (type wake, our session).
PROJ_JOURNAL="$TMP/spaces/$SLUG/events.jsonl"
if [[ -f "$PROJ_JOURNAL" ]]; then
  python3 -c "
import json, sys
rows = [json.loads(l) for l in open('$PROJ_JOURNAL') if l.strip()]
wakes = [r for r in rows if r.get('type') == 'wake'
         and r.get('data', {}).get('session_id') == '11111111-2222-4333-8444-555566667777']
assert wakes, 'no wake row for the fno session id'
print('wake row ok')
" || fail "wake row missing or wrong key"
else
  fail "project journal missing at $PROJ_JOURNAL"
fi
pass "wake row journaled"

pass "all wake-repro checks green"
