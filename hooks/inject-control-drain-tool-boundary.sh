#!/usr/bin/env bash
# fno hook: PreToolUse - mail drain at the tool boundary.
#
# A control body that demoted durable waits on the turn-boundary mail push,
# which fires only at a prompt boundary; a worker holding one long turn never
# reaches one, so a merge freeze could not stop it. This hook lands CONTROL
# mail at the next tool call instead, and ordinary mail too when
# `inbox.tool_boundary_ordinary` is on. The shell layer is a cheap gate only:
# three stat calls against the sender-stamped control flags, plus one builtin
# `-nt` test of the bus log against this session's stamp, which the command sets
# after each pass. Then the fno-agents command owns config, scan, cursors, dedup,
# and rendering. The bus dir comes from the binary's dir door, the one
# ladder the readers share, so a config-redirected bus follows the redirect.

set -uo pipefail

# Survive a caller env with no usable PATH (see worktree-write-protect.sh).
PATH="${PATH:+$PATH:}/usr/bin:/bin:/usr/sbin:/sbin"
export PATH

HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/with-timeout.sh
source "$HOOK_DIR/../scripts/lib/with-timeout.sh" 2>/dev/null || true

IFS= read -r -d '' INPUT || true
SID="$(jq -r '.session_id // empty' 2>/dev/null <<<"$INPUT")" || SID=""
if [[ -z "$SID" ]]; then
  # No jq, or jq failed: the drain must still run, so read the leading key by
  # hand. The harness writes session_id first; anchoring at the opening brace
  # keeps a "session_id" inside tool_input from faking the identity.
  re='^[[:space:]]*\{[[:space:]]*"session_id"[[:space:]]*:[[:space:]]*"([0-9a-f-]{16,})"'
  [[ "$INPUT" =~ $re ]] && SID="${BASH_REMATCH[1]}"
fi
[[ "$SID" =~ ^[0-9a-f-]{16,}$ ]] || exit 0

# The binary gate runs first: with no binary nothing drains, and the bus
# dir is the binary's answer.
command -v fno-agents >/dev/null 2>&1 || exit 0

# The bus dir is the dir door's answer - the one ladder the mail readers
# share (FNO_BUS_DIR, config.paths.bus_dir, the layout table) - so the db
# fold retargets this hook without touching it. A door miss is a no-answer,
# never a guessed path: record one miss row naming the rc, then exit 0 and
# stay silent; the tool call proceeds and pending mail stays pending.
BUS="$(with_timeout 5 fno-agents state path dir bus 2>/dev/null)" && BUS_rc=0 || BUS_rc=$?
if [[ "$BUS_rc" -ne 0 || -z "$BUS" ]]; then
    source "$HOOK_DIR/../scripts/lib/events.sh" 2>/dev/null \
        && emit_event_raw_literal mail_notify_self_missed \
            "{\"rc\":$BUS_rc,\"stderr_tail\":\"bus dir unresolved\"}" \
            "hook" 2>/dev/null
    exit 0
fi

FIRST8="${SID:0:8}"
LAST8="${SID: -8}"
if [[ ! -f "$BUS/control-pending/$FIRST8.flag" && ! -f "$BUS/control-pending/$SID.flag" && ! -f "$BUS/control-pending/$LAST8.flag" ]] \
  && [[ ! "$BUS/messages.jsonl" -nt "$BUS/tool-boundary/$SID.stamp" ]]; then
  exit 0
fi

# Stdout of the command IS the hook payload: it streams through fd 3 untouched,
# byte-for-byte (same discipline as inject-mail-notify.sh). A miss never
# blocks the tool call it rode in on.
exec 3>&1
drain_err="$(with_timeout 5 fno-agents mail-inject --control-drain --bus-dir "$BUS" --session "$SID" 2>&1 1>&3 3>&-)"
exec 3>&-
exit 0
