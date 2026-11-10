#!/usr/bin/env bash
# fno hook: UserPromptSubmit and PostToolUse Skill - session join trigger
# A prompt that names an fno command (/fno:<command> or $fno:<command>), or a Skill
# call of an fno:* skill, joins this session to fno and lands the using-fno
# preamble on the same turn. A joined session's later prompt carries the
# preamble when it is due again (after a compaction, or after a join that the
# fno CLI made, which cannot inject context itself).
#
# Every other prompt in an unjoined session costs one stat and emits `{}`.
# Hooks never block: every failure path emits `{}`.
set -uo pipefail

PATH="${PATH:+$PATH:}/usr/bin:/bin:/usr/sbin:/sbin"
export PATH
HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

noop() { echo '{}'; exit 0; }
# The delegate bounds its fno-agents call; sourcing here keeps this hook
# under the same wall-clock helper every daemon-shelling hook carries.
# shellcheck source=scripts/lib/with-timeout.sh
source "$HOOK_DIR/../scripts/lib/with-timeout.sh" 2>/dev/null || noop

PAYLOAD=""
[[ ! -t 0 ]] && PAYLOAD="$(cat 2>/dev/null || true)"
[[ -z "$PAYLOAD" ]] && noop
command -v jq >/dev/null 2>&1 || noop

# One jq pass answers every question the hook asks of the payload: two to
# three jq forks per prompt was a real share of a batch the 2026-11-08 audit
# showed dying at 3 to 5 s harness caps. TSV keeps the fields apart; a jq
# failure leaves them empty and every path below falls through to its noop.
EVENT="" SKILL_TRIG="" PROMPT_TRIG="" SID_FIELD=""
IFS=$'\t' read -r EVENT SKILL_TRIG PROMPT_TRIG SID_FIELD < <(
    printf '%s' "$PAYLOAD" | jq -r '
        if (.hook_event_name // "UserPromptSubmit") == "PostToolUse"
        then ["PostToolUse",
              ((.tool_input.skill // "") | startswith("fno:")),
              "false",
              (.session_id // "")]
        else ["UserPromptSubmit",
              "false",
              ((.prompt // "") | test("(^|\\s)[/$]fno:[a-z]")),
              (.session_id // "")]
        end | @tsv' 2>/dev/null
)
EVENT="${EVENT:-UserPromptSubmit}"
if [[ "$EVENT" == "PostToolUse" ]]; then
    TRIGGER="$SKILL_TRIG"
else
    EVENT="UserPromptSubmit"
    TRIGGER="$PROMPT_TRIG"
fi

if [[ "$TRIGGER" == "true" ]]; then
    printf '%s' "$PAYLOAD" | bash "$HOOK_DIR/session-start-using-fno.sh" "$EVENT" join
    exit 0
fi

# Not a trigger: only a joined session goes on to the due check. The marker
# layout is crates/fno-agents/src/backlog/join_marker.rs::marker_path.
if [[ "${FNO_PLATFORM:-}" == "codex" ]]; then
    HARNESS="codex"; SID="${CODEX_THREAD_ID:-${CODEX_SESSION_ID:-}}"
else
    HARNESS="claude"; SID="${CLAUDE_CODE_SESSION_ID:-}"
fi
[[ -z "$SID" ]] && SID="$SID_FIELD"
[[ -z "$SID" ]] && noop
AGENTS_HOME="${FNO_AGENTS_HOME:-${HOME:-}/.fno/agents}"
[[ -e "$AGENTS_HOME/sessions/$HARNESS/$SID/joined" ]] || noop
printf '%s' "$PAYLOAD" | bash "$HOOK_DIR/session-start-using-fno.sh" "$EVENT"
exit 0
