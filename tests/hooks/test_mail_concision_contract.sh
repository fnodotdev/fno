#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
USING_FNO="$ROOT/skills/using-fno/SKILL.md"
MAIL_SKILL="$ROOT/skills/mail/SKILL.md"
HOOK="$ROOT/hooks/session-start-using-fno.sh"

for file in "$USING_FNO" "$MAIL_SKILL"; do
  grep -Fq "## Relay compression contract" "$file"
  grep -Fq 'fno mux pane send' "$file"
  grep -Fq "80 words or fewer" "$file"
  grep -Fq "Keep technical terms" "$file"
done

# The hook injects only for a joined session, or on a fresh install where no session has joined yet.
HOME_DIR="$(mktemp -d)"
trap 'rm -rf "$HOME_DIR"' EXIT
export FNO_AGENT_HOME="$HOME_DIR/agents" FNO_HOME="$HOME_DIR/fno"
unset FNO_AGENT_BIN FNO_HOOK_HARNESS CODEX_PLUGIN_ROOT FNO_PLATFORM || true
mkdir -p "$FNO_AGENT_HOME/sessions/claude/other" && : >"$FNO_AGENT_HOME/sessions/claude/other/joined"
unjoined="$(CLAUDE_CODE_SESSION_ID=s1 CLAUDE_PLUGIN_ROOT="$ROOT" bash "$HOOK" </dev/null)"
[[ "$unjoined" == "{}" ]] || { echo "an unjoined session got a preamble: $unjoined"; exit 1; }

mkdir -p "$FNO_AGENT_HOME/sessions/claude/s1" && : >"$FNO_AGENT_HOME/sessions/claude/s1/joined"
CLAUDE_CODE_SESSION_ID=s1 CLAUDE_PLUGIN_ROOT="$ROOT" bash "$HOOK" </dev/null | python3 -c '
import json
import sys

payload = json.load(sys.stdin)
context = payload["hookSpecificOutput"]["additionalContext"]
required = (
    "## Relay compression contract",
    "fno agent mail send",
    "fno agent mail reply",
    "fno mux pane send",
    "80 words or fewer",
    "Keep technical terms",
)
missing = [item for item in required if item not in context]
if missing:
    raise SystemExit(f"missing from SessionStart context: {missing}")
'

echo "mail concision contract reaches SessionStart, mail, and mux surfaces"
