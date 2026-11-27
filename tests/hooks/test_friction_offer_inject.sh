#!/usr/bin/env bash
# Test for hooks/friction-offer-inject.sh.
#
# Verifies:
#   Skip    : no .offer file -> exit 0, no output.
#   Once    : <sid>.offer -> printed once as additionalContext for the event the
#             payload names, then deleted; a second run prints nothing.
#   Scope   : another session's offer is never printed and never deleted.
#   Wiring  : codex-hooks.json registers it under PostToolUse and
#             UserPromptSubmit. Claude runs it as a leg of hooks/hook-front.sh.
#
# Exit codes: 0 pass, 1 assertion failed, 77 skipped (missing deps).

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
HOOK="${REPO_ROOT}/hooks/friction-offer-inject.sh"

fail() { printf '[friction-offer-hook] FAIL: %s\n' "$*" >&2; exit 1; }
pass() { printf '[friction-offer-hook] PASS: %s\n' "$*"; }
skip() { printf '[friction-offer-hook] SKIP: %s\n' "$*" >&2; exit 77; }

command -v python3 &>/dev/null || skip "python3 not on PATH"
[[ -x "$HOOK" ]] || fail "hook not executable at $HOOK"
bash -n "$HOOK" || fail "bash -n rejected $HOOK"
# The hook is a shim into `fno-agent hook front`; it needs a binary.
source "$REPO_ROOT/hooks/lib/agents-bin.sh"
FNO_AGENT_BIN="$(fno_agent_bin "$REPO_ROOT")"
[[ -n "$FNO_AGENT_BIN" ]] || skip "no fno-agent binary"
export FNO_AGENT_BIN

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
export FNO_AGENT_HOME="$WORK/agents"
DIR="$FNO_AGENT_HOME/friction"
mkdir -p "$DIR"
SID="cccccccc-3333-4333-8333-333333333333"
OTHER="bbbbbbbb-2222-4222-8222-222222222222"

# Skip: no offer file -> exit 0, silent.
out="$(printf '%s' '{"session_id":"x"}' | bash "$HOOK")" || fail "no-offer run exited non-zero"
[[ -z "$out" ]] || fail "no-offer run printed: $out"
pass "no offer: exit 0, silent"

printf 'line one "quoted"\n  - x-0002: a title\n' > "$DIR/$SID.offer"
printf 'not yours\n' > "$DIR/$OTHER.offer"
# A nested session_id that comes first must not claim that session's offer.
nested="{\"tool_input\": {\"session_id\": \"$OTHER\"}, \"session_id\": \"$SID\", \"hook_event_name\": \"UserPromptSubmit\"}"
payload="$nested"
first="$(printf '%s' "$payload" | bash "$HOOK")"
ctx="$(printf '%s' "$first" | python3 -c '
import json, sys
d = json.load(sys.stdin)["hookSpecificOutput"]
print(d["hookEventName"]); print(d["additionalContext"], end="")')"
[[ "$(head -1 <<<"$ctx")" == "UserPromptSubmit" ]] || fail "event name not echoed: $first"
grep -q 'line one "quoted"' <<<"$ctx" || fail "offer text lost: $first"
grep -q 'x-0002: a title' <<<"$ctx" || fail "offer second line lost: $first"
[[ ! -e "$DIR/$SID.offer" ]] || fail "offer not deleted after surfacing"
second="$(printf '%s' "$payload" | bash "$HOOK")"
[[ -z "$second" ]] || fail "offer surfaced twice: $second"
[[ -f "$DIR/$OTHER.offer" ]] || fail "another session's offer was touched"
pass "offer printed once for its own top-level session, then deleted"

python3 - "$REPO_ROOT" <<'PY' || fail "hook not registered under PostToolUse and UserPromptSubmit"
import json, sys
root = sys.argv[1]
for name in ("codex-hooks.json",):
    d = json.load(open(f"{root}/hooks/{name}"))["hooks"]
    for event in ("PostToolUse", "UserPromptSubmit"):
        cmds = [h.get("command", "") for e in d.get(event, []) for h in e.get("hooks", [])]
        assert any("friction-offer-inject.sh" in c for c in cmds), (name, event)
PY
pass "registered in codex-hooks.json"
