#!/usr/bin/env bash
# fno hook: SessionStart - claude supervisor bootstrap
# A claude supervisor born outside the user's Aqua GUI session hosts sessions
# that can lose DNS, gh, ps and the keychain for its whole life, and the
# breakage lands in every session it forks. The in-Rust birth guard
# only sees births fno mediates; a bare `claude` typed in a non-GUI pane
# never reaches fno, so SessionStart is the one surface that names the state
# where a hosted session can see it.
#
# Evidence, never a self-heal: like daemon-liveness-session-start.sh, this
# prints at most one line and exits 0.
set -uo pipefail

# Survive a caller env with no usable PATH (see worktree-write-protect.sh).
PATH="${PATH:+$PATH:}/usr/bin:/bin:/usr/sbin:/sbin"
export PATH

# macOS only: launchctl and scutil do not exist elsewhere.
if [[ "$(uname -s)" != "Darwin" ]]; then
    exit 0
fi

# One bounded probe: run a command with its output in a file, kill it at the
# 1s bound. A wedged launchctl or configd must not wedge session start.
bounded_probe() {
    local out="$1"
    shift
    "$@" >"$out" 2>/dev/null </dev/null &
    local pid=$!
    local n=0
    while kill -0 "$pid" 2>/dev/null && ((n < 10)); do
        sleep 0.1
        n=$((n + 1))
    done
    kill "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
    return 0
}

WORK="$(mktemp -d "${TMPDIR:-/tmp}/fno-bootstrap-XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

bounded_probe "$WORK/mgr" launchctl managername
bounded_probe "$WORK/dns" scutil --dns

MGR="$(head -n 1 "$WORK/mgr" 2>/dev/null | tr -d '[:space:]')"
DNS_BROKEN=1
if grep -q "^DNS configuration" "$WORK/dns" 2>/dev/null; then
    DNS_BROKEN=0
fi

# Arm 1: the birth context. Aqua is the GUI session; anything else names a
# supervisor whose hosted sessions can lose services. An empty
# answer is unknown, never a warning.
# Arm 2: the services. A session whose supervisor lost its bootstrap prints
# no DNS configuration at all (the 2026-11-10 incident symptom), even when
# the manager name still reads Aqua.
REASON=""
if [[ -n "$MGR" && "$MGR" != "Aqua" ]]; then
    REASON="this session runs under a claude supervisor born outside the GUI session (launchctl managername reads ${MGR}): DNS, gh, ps and the keychain can fail here, and every session the supervisor hosts inherits the same."
elif [[ "$DNS_BROKEN" == "1" ]]; then
    REASON="this session's claude supervisor lost its GUI bootstrap: scutil --dns names no DNS configuration, so DNS, gh, ps and the keychain fail here."
fi

if [[ -n "$REASON" ]]; then
    printf '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"[fno] %s Cure: from a GUI terminal, run `claude daemon stop --any`, then start any `claude --bg` command; the recipe is the claude supervisor entry in docs/fleet-faq.md."}}\n' "$REASON"
fi

exit 0
