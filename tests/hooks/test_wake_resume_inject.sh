#!/usr/bin/env bash
# Smoke test for the wake-resume protocol shim (x-3bb2).
#
# Verifies:
#   WIRING  : hooks.json registers the hook under UserPromptSubmit.
#   AC1-HP  : payloads the shim relays verbatim.
#   AC1-ERR : a prompt containing but not equal to a wake word -> silent.
#   SILENT  : malformed payload -> rc=0, no output.
#   TIMEOUT : a door that overruns the 3s bound -> silent rc=0.
#
# The door is stubbed via FNO_AGENTS_BIN; the real door rides the smoke
# repro and CI. Exit codes: 0 pass, 1 fail, 77 skip.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
HOOK="${REPO_ROOT}/hooks/wake-resume-inject.sh"
HOOKS_JSON="${REPO_ROOT}/hooks/hooks.json"

log()  { printf '[wake-inject] %s\n' "$*"; }
fail() { printf '[wake-inject] FAIL: %s\n' "$*" >&2; exit 1; }
pass() { printf '[wake-inject] PASS: %s\n' "$*"; }
skip() { printf '[wake-inject] SKIP: %s\n' "$*"; exit 77; }

command -v jq >/dev/null 2>&1 || skip "jq not on PATH"
command -v git >/dev/null 2>&1 || command -v python3 >/dev/null 2>&1 || skip "no jq validator"
[[ -f "$HOOK" ]] || fail "hook not found at $HOOK"
[[ -x "$HOOK" ]] || fail "hook not executable at $HOOK"
bash -n "$HOOK" || fail "bash -n rejected $HOOK"

# WIRING: the entry lands under UserPromptSubmit.
if command -v python3 >/dev/null 2>&1; then
  python3 -c "
import json
hooks = json.load(open('$HOOKS_JSON'))
rows = [h for m in hooks['hooks'].get('UserPromptSubmit', [])
        for h in m.get('hooks', [])
        if 'wake-resume-inject' in h.get('command', '')]
assert rows, 'wake-resume-inject not registered'
print('wiring ok')
" || fail "hooks.json wiring"
fi

# A stub door via FNO_AGENTS_BIN: echoes the sentinel, exit 0.
STUB_DIR=$(mktemp -d)
trap 'rm -rf "$STUB_DIR"' EXIT
STUB="$STUB_DIR/fno-agents"
printf '#!/bin/sh\necho "wake: target run x-3bb2 is still open (input: x-3bb2)"\n' > "$STUB"
chmod +x "$STUB"
export FNO_AGENTS_BIN="$STUB"

payload() {
  printf '{"session_id": "3afe9590-0496-4ee7-89be-cdc23dced8a2", "prompt": "%s"}' "$1"
}

# AC1-HP: bare go reaches the door and the relay lands.
out="$(payload go | "$HOOK")"
rc=$?
[[ "$rc" -eq 0 ]] || fail "bare go: rc=$rc"
[[ "$out" == *"target run x-3bb2"* ]] || fail "bare go: no protocol: $out"
pass "AC1-HP bare go injects"

# AC1-ERR: contains but is not bare -> the word gate stops it, silent.
out="$(payload 'go fix the line-flake' | "$HOOK")"; rc=$?
[[ "$rc" -eq 0 ]] || fail "non-bare: rc=$rc"
[[ -z "$out" ]] || fail "non-bare injected: $out"
pass "AC1-ERR containing word stays silent"

# SILENT: malformed payload, no session id -> rc 0 silent.
out="$(printf '{"prompt": "go"}' | "$HOOK")"; rc=$?
[[ "$rc" -eq 0 && -z "$out" ]] || fail "malformed: rc=$rc out=$out"
pass "silent on malformed payload"

# TIMEOUT: stub sleeps 5s, the shim's 3s bound fires, silent.
printf '#!/bin/sh\nsleep 5\n' > "$STUB"
chmod +x "$STUB"
out="$(payload go | "$HOOK")"; rc=$?
[[ "$rc" -eq 0 && -z "$out" ]] || fail "timeout case: rc=$rc out=$out"
pass "AC-timeout: door overrun stays silent"

pass "all wake-inject shim checks green"
