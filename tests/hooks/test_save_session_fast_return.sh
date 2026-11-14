#!/usr/bin/env bash
# test_save_session_fast_return.sh
#
# The SessionEnd save-session hook returns fast and still saves. Claude Code
# gives all SessionEnd hooks one shared 1.5 s budget, so hooks/save-session.sh
# must hand the work to a detached child instead of running it inline.
# Covers:
#   AC1  the hook exits 0, writes nothing, and returns well under 200 ms
#   AC2  the detached child still writes the transcript under saved-sessions
#   AC3  the stdin temp file is cleaned up
#   AC4  a machine with no fno-agents exits 0 and saves nothing

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
HOOK="$REPO_ROOT/hooks/save-session.sh"

[[ -f "$HOOK" ]] || { echo "FAIL: hook not found at $HOOK" >&2; exit 1; }

AGENTS_BIN="${FNO_AGENTS_BIN:-$REPO_ROOT/crates/fno-agents/target/debug/fno-agents}"
if [[ ! -x "$AGENTS_BIN" ]]; then
    (cd "$REPO_ROOT/crates/fno-agents" && cargo build --bins --quiet) || {
        echo "FAIL: could not build fno-agents" >&2
        exit 1
    }
fi

fail() { echo "FAIL - $1" >&2; exit 1; }
pass() { echo "ok - $1"; }

SBX="$(mktemp -d "${TMPDIR:-/tmp}/fno-save-session-fast.XXXXXX")"
trap 'rm -rf "$SBX"' EXIT

# A minimal one-round Claude Code transcript.
cat > "$SBX/transcript.jsonl" <<'EOF'
{"type":"user","message":{"role":"user","content":"hello from the fast-return test"}}
{"type":"assistant","message":{"role":"assistant","content":"world from the fast-return test"}}
EOF

cat > "$SBX/payload.json" <<EOF
{"session_id":"fastreturntest","transcript_path":"$SBX/transcript.jsonl","cwd":"$SBX","hook_event_name":"SessionEnd","reason":"clear"}
EOF

export HOME="$SBX"
export FNO_HOME="$SBX/fno-home"
export CLAUDE_PLUGIN_ROOT="$REPO_ROOT"
export FNO_HOOK_CONTEXT=1
export TMPDIR="$SBX/tmp"
mkdir -p "$TMPDIR"
PATH="$(dirname "$AGENTS_BIN"):$PATH"
export PATH

# Warm the binary once: page-in and first-touch dir creation are cold-start
# cost, not the contract under test. A different session id, so its artifact
# never dedups or collides with the one AC2 polls for.
sed 's/fastreturntest/warmupsid/' "$SBX/payload.json" \
    | "$AGENTS_BIN" hook save-session >/dev/null 2>&1 || true

# AC1: fast, silent, exit 0.
t0=$(perl -MTime::HiRes=time -e 'printf "%.6f", time')
out=$(bash "$HOOK" < "$SBX/payload.json")
rc=$?
t1=$(perl -MTime::HiRes=time -e 'printf "%.6f", time')
elapsed_ms=$(perl -e "printf '%d', ($t1 - $t0) * 1000")

[[ "$rc" -eq 0 ]] || fail "hook exit $rc, want 0"
[[ -z "$out" ]] || fail "hook wrote to stdout: $out"
(( elapsed_ms < 200 )) || fail "hook took ${elapsed_ms}ms, budget 200ms"
pass "hook returned in ${elapsed_ms}ms with no output"

# AC2: the detached child saves. Poll - the child runs after the hook returns.
artifact=""
for _ in $(seq 1 100); do
    artifact=$(find "$FNO_HOME" -name 'fastreturntest-*.md' 2>/dev/null | head -1)
    [[ -n "$artifact" ]] && break
    sleep 0.1
done
[[ -n "$artifact" ]] || fail "no saved transcript under $FNO_HOME after 10s"
grep -q "world from the fast-return test" "$artifact" || fail "artifact lacks transcript text"
pass "detached save landed: $artifact"

# A clean save's stderr log carries the confirmation line, not an error.
err_log="$FNO_HOME/db/saved-sessions/save-session.err"
[[ -f "$err_log" ]] || fail "save-session.err not written"
grep -q "^Session saved:" "$err_log" || fail "save-session.err lacks confirmation: $(cat "$err_log")"
grep -Eq "^Failed to save session:|^save-session: " "$err_log" \
    && fail "save-session.err carries an error: $(cat "$err_log")"
pass "save-session.err holds the confirmation line"

# AC3: the stdin temp file is gone.
leftover=$(find "$TMPDIR" -name 'fno-save-session.*' 2>/dev/null | head -1)
[[ -z "$leftover" ]] || fail "stdin temp file left behind: $leftover"
pass "stdin temp file cleaned up"

# AC4: no fno-agents on PATH exits 0 and saves nothing.
t0=$(perl -MTime::HiRes=time -e 'printf "%.6f", time')
env PATH=/usr/bin:/bin bash "$HOOK" < "$SBX/payload.json" >/dev/null 2>&1
rc=$?
t1=$(perl -MTime::HiRes=time -e 'printf "%.6f", time')
elapsed_ms=$(perl -e "printf '%d', ($t1 - $t0) * 1000")
[[ "$rc" -eq 0 ]] || fail "no-fno-agents run exited $rc"
(( elapsed_ms < 200 )) || fail "no-fno-agents run took ${elapsed_ms}ms"
saved=$(find "$FNO_HOME" -name 'fastreturntest-*.md' 2>/dev/null | wc -l)
[[ "$saved" -eq 1 ]] || fail "no-fno-agents run changed the save count: $saved"
pass "no fno-agents: exit 0 in ${elapsed_ms}ms, nothing saved"

echo "PASS: save-session hook returns fast and still saves"
