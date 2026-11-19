#!/usr/bin/env bash
# fno hook: SessionStart - daemon liveness + heal
# With the fno-agent daemon down, nothing wakes a
# lead on a daemon beat. The daemon lazy-starts on the next daemon-routed
# command, so the gap is exactly the quiescent machine: no session, no tick,
# no lead wake until someone types. SessionStart is the one surface that
# fires anyway, so it heals the gap: when the probe reads down, it fires
# the gated restart detached and says so.
#
# The probe is a raw socket connect with the client's own down predicate
# (missing or refused reads down; anything else says nothing), not
# `fno-agent status`: the status command also scans the tick journals and
# costs ~1.9s, over the session-start hook budget. The connect itself
# lazy-starts nothing; the heal goes through `fno-agent restart --if-down`,
# whose gate re-measures the down state before any swap.
set -uo pipefail

# Survive a caller env with no usable PATH (see worktree-write-protect.sh).
PATH="${PATH:+$PATH:}/usr/bin:/bin:/usr/sbin:/sbin"
export PATH

HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HOOK_DIR/.." && pwd)"
# shellcheck source=lib/agents-bin.sh
source "$HOOK_DIR/lib/agents-bin.sh"
BIN="$(fno_agent_bin "$ROOT")"
[[ -z "$BIN" ]] && exit 0

SOCK="${FNO_AGENT_HOME:-${HOME:-}/.fno/agents}/supervisor.sock"
# A missing socket file reads down with no connect at all. A socket file
# that refuses the connect is a stale one a dead daemon left behind; `nc -U`
# connects, sends nothing (stdin is /dev/null) and closes. No nc says
# nothing, like any other probe failure.
DOWN=""
if [[ ! -e "$SOCK" ]]; then
    DOWN="down"
elif [[ -S "$SOCK" ]] && command -v nc >/dev/null 2>&1; then
    nc -U -w 1 "$SOCK" </dev/null >/dev/null 2>&1 || DOWN="down"
fi

[[ "$DOWN" == "down" ]] || exit 0

# The heal: fire the gated restart detached, no wait, so the hook budget
# never sees the ensure loop (a daemon that serves within the budget reads
# up anyway; a slow boot finishes in the background). The client command, not
# a direct daemon spawn: ensure_daemon scrubs this session's identity env
# off the new daemon, and the client's own --if-down gate re-measures the
# down state, so a daemon that came up mid-hook is never bounced.
if [[ -n "$BIN" ]]; then
    nohup "$BIN" restart --if-down >/dev/null 2>&1 </dev/null &
    disown 2>/dev/null || true
    printf '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"[fno] the fno-agent daemon is down: fired fno-agent restart --if-down in the background to start it; it serves within a few seconds. Run that command by hand if it stays down."}}\n'
    exit 0
fi

printf '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"[fno] the fno-agent daemon is down: lead-wake, watchdog and daemon beats are not firing. Run: fno-agent restart --if-down"}}\n'
exit 0
