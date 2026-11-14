#!/usr/bin/env bash
# fno hook: UserPromptSubmit - wake resume protocol
# A bare wake word ("go", "continue", ...) on a session whose target run is
# still open injects the resume protocol: run state plus the turn-end rules.
# This is the wake half of node x-3bb2's fix: the stop gate already governs
# the turn end; nothing governed the wake.
# The fno-agents `wake-context` door owns the whole decision (wake word,
# registry row, live manifest, session match, closed-run refusal); the shim
# adds only the cheap bash word gate so most prompts skip the binary call,
# and relays the door's one block as additionalContext. Every failure exits
# 0 and prints nothing: nothing optional sits on the foreground path
# (law d-bad5a42f).
#
# Claude config only: the prompt-submit entry is claude hooks.json, the same
# one-release limitation node-bind-prompt-submit.sh documents.
#
# SILENT unless the door injects: UserPromptSubmit hook stdout is added to
# model context, so per-prompt chatter is context cost.

set -uo pipefail

# Survive a caller env with no usable PATH (see worktree-write-protect.sh).
PATH="${PATH:+$PATH:}/usr/bin:/bin:/usr/sbin:/sbin"
export PATH

HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# The per-turn guard (tests/hooks/test_with_timeout.sh) requires every
# UserPromptSubmit hook that names a binary to carry the shared bound.
source "$HOOK_DIR/../scripts/lib/with-timeout.sh" 2>/dev/null || exit 0
source "$HOOK_DIR/lib/agents-bin.sh" 2>/dev/null || true

# The payload arrives once; capture it before anything consumes stdin.
payload="$(cat 2>/dev/null)" || exit 0
[[ -n "$payload" ]] || exit 0

# The wake-word gate: bare go/continue/resume/go on/keep going (case-folded,
# trimmed) reads as a wake; anything else is not this hook's business.
prompt="$(printf '%s' "$payload" | jq -r '.prompt // empty' 2>/dev/null)"
case "$(printf '%s' "$prompt" | tr '[:upper:]' '[:lower:]' | xargs)" in
  go | continue | resume | go\ on | keep\ going) ;;
  *) exit 0 ;;
esac

# The session key, read from the payload's real key (a planted "session_id"
# inside prompt text is data, never identity). Charset + length gate like
# the bind hook.
SID="$(printf '%s' "$payload" | jq -r '.session_id // empty' 2>/dev/null)"
case "$SID" in
'' | *[!A-Za-z0-9_-]*)
  exit 0
  ;;
esac
[[ "${#SID}" -ge 16 ]] || exit 0

# A FIXED 3s bound like the bind hook: a wake inject that overruns its bound
# is skipped for this prompt, never blocks the session.
BIN="$(fno_agents_bin "$PWD")"
[[ -n "$BIN" ]] || exit 0
out="$(printf '%s' "$payload" | with_timeout 3 "$BIN" wake-context 2>/dev/null)"
rc=$?
if [[ "$rc" -eq 124 ]]; then
  exit 0
fi
if [[ "$rc" -eq 0 && -n "$out" ]]; then
  printf '%s\n' "$out"
fi
exit 0
