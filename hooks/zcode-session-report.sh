#!/usr/bin/env bash
# fno hook: zcode session adoption - report this session's presence to the
# registry (mail and liveness stop guessing). zcode fires no SessionStart, so
# the session report rides its first PreToolUse or Stop instead (upstream
# zai-org/feedback #841), and the mint command updates an existing
# row, so a repeated Stop re-report is harmless. The session id is minted by
# zcode as sess_<uuid>, read defensively from the payload - never from a
# claude env name, because a zcode session inherits the claude plugin's env.
# Standalone on purpose: wiring it as its own hooks.json command would tax
# every claude-plugin harness's Stop fan-out, so hooks/session-state.sh
# invokes this script behind the zcode guard and nothing else calls it.
PATH="${PATH:+$PATH:}/usr/bin:/bin"
export PATH
HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
stdin=$(cat)
sid=""
for key in session_id sessionId conversationId; do
    sid=$(printf '%s' "$stdin" | sed -n "s/.*\"${key}\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" | head -1)
    [[ -n "$sid" ]] && break
done
bin=""
for b in "$(command -v fno-agent 2>/dev/null)" "${FNO_AGENT_BIN:-}"; do
    [[ -n "$b" && -x "$b" ]] || continue
    bin="$b"
    break
done
if [[ -n "$bin" && -n "$sid" ]]; then
    args=(report --kind session --harness zcode --session-id "$sid")
    # A keeper-hosted spawn stamps FNO_AGENT_SELF on the child; absent on a
    # hand-started session, where the daemon mints an operator row.
    if [[ -n "${FNO_AGENT_SELF:-}" ]]; then
        args+=(--agent-self "$FNO_AGENT_SELF")
    fi
    if source "$HOOK_DIR/../scripts/lib/with-timeout.sh" 2>/dev/null && command -v with_timeout >/dev/null 2>&1; then
        with_timeout 2 "$bin" "${args[@]}" >/dev/null 2>&1 || true
    else
        "$bin" "${args[@]}" >/dev/null 2>&1 || true
    fi
fi
exit 0
