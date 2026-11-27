#!/usr/bin/env bash
# fno hook: cursor sessionStart - report this session's presence to the
# registry (mail and liveness stop guessing). cursor's sessionStart is
# fire-and-forget and its output fields are optional, so a silent {} is the
# only safe output. The payload's session_id is the id cursor itself calls
# conversation_id (cursor.com/docs/hooks); hooks/cursor-hooks.json does not
# exist - the install arm in crates/fno/src/setup/cli_hooks.rs merges the
# resolved command into ~/.cursor/hooks.json, following install_gemini.
PATH="${PATH:+$PATH:}/usr/bin:/bin"
export PATH
HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
stdin=$(cat)
sid=$(printf '%s' "$stdin" | sed -n 's/.*"session_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
bin=""
for b in "$(command -v fno-agent 2>/dev/null)" "${FNO_AGENT_BIN:-}"; do
    [[ -n "$b" && -x "$b" ]] || continue
    bin="$b"
    break
done
if [[ -n "$bin" && -n "$sid" ]]; then
    args=(report --kind session --harness cursor-agent --session-id "$sid")
    # A keeper-hosted spawn stamps FNO_AGENT_SELF on the child; absent on a
    # hand-started session, where the daemon mints an operator row.
    if [[ -n "${FNO_AGENT_SELF:-}" ]]; then
        args+=(--agent-self "$FNO_AGENT_SELF")
    fi
    # Bounded like the rest of the hook family; the helper ships beside the
    # hooks in the installed stage, and the fallback stays fire-and-forget.
    if source "$HOOK_DIR/../scripts/lib/with-timeout.sh" 2>/dev/null && command -v with_timeout >/dev/null 2>&1; then
        with_timeout 2 "$bin" "${args[@]}" >/dev/null 2>&1 || true
    else
        "$bin" "${args[@]}" >/dev/null 2>&1 || true
    fi
fi
printf '{}\n'
