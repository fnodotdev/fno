#!/usr/bin/env bash
# The verified-facts contract (verify-footnotes sections 2 and 3) reaches the
# delivery surfaces: the using-fno SessionStart preamble, the lead skill's
# beat report, and the style doc's machine rule list. The mail lint and the
# stop hook gate carry their own Rust tests.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
USING_FNO="$ROOT/skills/using-fno/SKILL.md"
LEAD_SKILL="$ROOT/skills/lead/SKILL.md"
STYLE_DOC="$ROOT/docs/style-rules.md"
HOOK="$ROOT/hooks/session-start-using-fno.sh"

grep -Fq "## Verified facts only" "$USING_FNO"
grep -Fq "name each one tried in the claim" "$USING_FNO"
grep -Fq "skills/verify-footnotes/SKILL.md" "$USING_FNO"
grep -Fq "State only facts this beat verified" "$LEAD_SKILL"
grep -Fq "name each one tried" "$LEAD_SKILL"
grep -Fq "cannot claim that names no route tried" "$STYLE_DOC"
grep -Fq "12. Mail prose refuses a cannot claim" "$STYLE_DOC"

# The rule block must survive injection: on a fresh install (no session joined yet) the preamble is due, and the SessionStart additionalContext carries the rule.
HOME_DIR="$(mktemp -d)"
trap 'rm -rf "$HOME_DIR"' EXIT
unset FNO_AGENT_BIN FNO_HOOK_HARNESS CODEX_PLUGIN_ROOT FNO_PLATFORM || true
out="$(FNO_AGENT_HOME="$HOME_DIR/agents" FNO_HOME="$HOME_DIR/fno" \
    CLAUDE_CODE_SESSION_ID=verified-facts-contract \
    CLAUDE_PLUGIN_ROOT="$ROOT" bash "$HOOK" </dev/null)"
grep -Fq "## Verified facts only" <<<"$out"
grep -Fq "name each one tried in the claim" <<<"$out"

echo "verified-facts contract reaches SessionStart, lead, and style-doc surfaces"
