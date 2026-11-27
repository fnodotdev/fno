#!/usr/bin/env bash
# fno hook: runtime helper - context run
# One group of the context producers in hooks/context-hooks.json, stale-while-revalidate: the answer comes from the cached pass and a detached pass refreshes it. The leg lives in crates/fno-agent/src/runtime/hook/front/session_start.rs.
# No fno-agent usually means a plugin-only install with no fno yet. Then no leg can answer, so this shim runs the front-door hook, which prints the install notice.
HOOK_DIR="${BASH_SOURCE[0]%/*}"
source "$HOOK_DIR/lib/agents-bin.sh"
if [[ -z "$(fno_agent_bin "$HOOK_DIR/..")" ]]; then
    if [[ "${1:-}" == "claude-session-start" || "${1:-}" == "codex-session-start" ]]; then
        bash "$HOOK_DIR/frontdoor-nudge-session-start.sh"
    fi
    echo "fno: fno-agent not found; install footnote: curl -fsSL fno.sh | sh" >&2
    exit 0
fi
exec "$HOOK_DIR/hook-front.sh" SessionStart --leg context-run "$@"
