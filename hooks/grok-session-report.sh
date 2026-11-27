#!/usr/bin/env bash
# fno hook: grok SessionStart - report this session's presence to the
# registry (mail and liveness stop guessing). grok's runner injects
# GROK_SESSION_ID into every hook process and reserves it against overrides,
# so the env value is authoritative and the stdin payload's sessionId is only
# a fallback. Output stays empty: the grok SessionStart hook is
# fire-and-forget and no output field is documented. Registration is not a
# hooks/grok-hooks.json template - the install arm in
# crates/fno/src/setup/cli_hooks.rs writes ~/.grok/hooks/fno-session.json
# directly, because each file in that directory is one standalone config.
PATH="${PATH:+$PATH:}/usr/bin:/bin"
export PATH
HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
sid="${GROK_SESSION_ID:-}"
stdin=""
if [[ -z "$sid" ]]; then
    stdin=$(cat)
    sid=$(printf '%s' "$stdin" | sed -n 's/.*"sessionId"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
fi
bin=""
for b in "$(command -v fno-agent 2>/dev/null)" "${FNO_AGENT_BIN:-}"; do
    [[ -n "$b" && -x "$b" ]] || continue
    bin="$b"
    break
done
if [[ -n "$bin" && -n "$sid" ]]; then
    args=(report --kind session --harness grok --session-id "$sid")
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
