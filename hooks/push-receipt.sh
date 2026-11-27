#!/usr/bin/env bash
# fno hook: PostToolUse - record what a PushNotification call returned
#
# The ask-push hook only asks the model to push. The PushNotification tool
# answers with a request, never a receipt, and it suppresses the push when the
# terminal is active. This hook writes one row per call to
# ${FNO_AGENT_HOME:-$HOME/.fno/agents}/ask-push/receipts.jsonl so a read can
# tell "hook fired" from "push requested" from "push suppressed". No row says
# the device got it. Always exits 0; never blocks the call it rides on.

set -uo pipefail

command -v jq >/dev/null 2>&1 || exit 0

payload="$(cat 2>/dev/null || true)"
[ -n "$payload" ] || exit 0

message="$(jq -r '.tool_input.message // empty' <<<"$payload" 2>/dev/null)"
[ -n "$message" ] || exit 0

result="$(jq -r '(.tool_response // empty) | if type == "string" then . else (.content // .text // .stdout // tostring) end' <<<"$payload" 2>/dev/null)"

case "$result" in
    *"Mobile push requested"*) class="requested" ;;
    *"Not sent"*"terminal is active"*) class="suppressed-active-terminal" ;;
    *"Mobile push not sent"*"Remote Control inactive"*) class="not-sent-remote-control-inactive" ;;
    *) class="unknown" ;;
esac

# The ask-push text opens with "Question <qid>:". Any other push has no qid.
qid=""
if [[ "$message" =~ ^Question\ ([A-Za-z0-9_.-]+): ]]; then
    qid="${BASH_REMATCH[1]}"
fi

dir="${FNO_AGENT_HOME:-$HOME/.fno/agents}/ask-push"
mkdir -p "$dir" 2>/dev/null || exit 0

session="$(jq -r '.session_id // empty' <<<"$payload" 2>/dev/null)"
now="$(date +%s)"
jq -cn --arg ts "$now" --arg qid "$qid" --arg class "$class" --arg session "$session" --arg result "${result:0:200}" \
    '{ts: ($ts | tonumber), qid: $qid, class: $class, session: $session, result: $result}' \
    >>"$dir/receipts.jsonl" 2>/dev/null || true

# Bound the file: past 1000 rows keep the newest 500.
if [ "$(wc -l <"$dir/receipts.jsonl" 2>/dev/null || echo 0)" -gt 1000 ]; then
    tail -n 500 "$dir/receipts.jsonl" >"$dir/receipts.jsonl.tmp" 2>/dev/null \
        && mv "$dir/receipts.jsonl.tmp" "$dir/receipts.jsonl" 2>/dev/null || true
fi
exit 0
