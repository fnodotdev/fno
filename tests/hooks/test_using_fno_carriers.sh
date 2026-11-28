#!/usr/bin/env bash
# The agy using-fno carrier runs the shared leg through hooks/hook-front.sh and gets the preamble only for a joined, untaught session. The opencode, pi and claude harness and answer-shape contracts are Rust tests in crates/fno-agent/src/runtime/hook/front/session_start.rs.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
AGY="$ROOT/hooks/agy-using-fno-pre-invocation.sh"
W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT

export FNO_AGENT_HOME="$W/agents" FNO_HOME="$W/fno"
unset FNO_AGENT_BIN CLAUDE_PLUGIN_ROOT CODEX_PLUGIN_ROOT CLAUDE_CODE_SESSION_ID CODEX_THREAD_ID FNO_PLATFORM || true
fail() { echo "FAIL: $*"; exit 1; }

# An unjoined session gets `{}` from the carrier itself; a joined, untaught one gets injectSteps; the leg stamps it taught, so the next call is silent again.
AGY_IN='{"conversationId":"a1","invocationNum":3}'
out="$(printf '%s' "$AGY_IN" | bash "$AGY")"
[[ "$out" == "{}" ]] || fail "an unjoined agy session got the preamble: $out"
mkdir -p "$FNO_AGENT_HOME/sessions/agy/a1" && : >"$FNO_AGENT_HOME/sessions/agy/a1/joined"
out="$(printf '%s' "$AGY_IN" | bash "$AGY")"
printf '%s' "$out" | python3 -c 'import json,sys; assert "EXTREMELY_IMPORTANT" in json.load(sys.stdin)["injectSteps"][0]["ephemeralMessage"]' \
    || fail "agy got no injectSteps: $out"
[[ -e "$FNO_AGENT_HOME/using_fno_teach/a1.json" ]] || fail "the preamble was not stamped taught"
out="$(printf '%s' "$AGY_IN" | bash "$AGY")"
[[ "$out" == "{}" ]] || fail "a taught agy session got the preamble again"

echo "using-fno carriers: agy injects only for a joined, untaught session"
