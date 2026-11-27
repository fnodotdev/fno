#!/usr/bin/env bash
# fno hook: PreToolUse (Read) - count reads of memory entries in a sidecar.
# The payload carries the same fields the other PreToolUse hooks
# parse: .tool_input.file_path (worktree-write-protect.sh:44),
# .cwd and .session_id (capture-plan-mode.sh:42,52).
#
# Posture: passive observer, fail open. Every path exits 0; nothing here
# may refuse a Read or print to stdout. One jq call plus one append keeps
# the hook far under the 150ms pretooluse budget (D1839). Lines are jsonl:
# {"file","ts","kind","session"}; the only reader is the retirement pass
# beside the writer (scripts/memory/retire-unread-pass.sh).
set -uo pipefail

IFS= read -r -d '' INPUT || true
[[ -n "${INPUT:-}" ]] || exit 0

read -r FP CWD SID <<< "$(printf '%s' "$INPUT" | jq -r '[
    .tool_input.file_path // "",
    .cwd // "",
    .session_id // ""
] | @tsv' 2>/dev/null)"

[[ -n "${FP:-}" && -n "${CWD:-}" ]] || exit 0

# Free-form fields must not carry a quote into the jsonl line. @tsv already
# escapes tabs and newlines; quotes it leaves alone.
FP="${FP//\"/}"
SID="${SID//\"/}"

# The projects root mirrors Claude Code itself: $CLAUDE_CONFIG_DIR/projects
# when the dir override is set (account aliases rely on it), else
# $HOME/.claude/projects. The slug is the cwd with every non-alphanumeric
# byte replaced by '-' (verified against live dirs: /a/b -> -a-b,
# /a/b/.claude/worktrees/w -> -a-b--claude-worktrees-w).
ROOT="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
SLUG="$(printf '%s' "$CWD" | LC_ALL=C tr -c '[:alnum:]' '-')"
MEM="$ROOT/projects/$SLUG/memory"

case "$FP" in
    "$MEM"/*.md) ;;
    *) exit 0 ;;
esac
BASENAME="${FP##*/}"
case "$BASENAME" in
    MEMORY.md|CLAUDE.md|ARCHIVE-INDEX.md) exit 0 ;;
esac

TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
printf '{"file":"%s","ts":"%s","kind":"read","session":"%s"}\n' \
    "$BASENAME" "$TS" "$SID" >> "$MEM/.reads.jsonl" 2>/dev/null || exit 0
exit 0
