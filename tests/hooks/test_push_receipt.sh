#!/usr/bin/env bash
# hooks/push-receipt.sh and scripts/diagnostics/ask-push-delivery.sh: the
# PushNotification result lands as one receipt row, and the read names the
# first hop that failed. The device hop is always "unconfirmed".
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1
HOOK="hooks/push-receipt.sh"
READ="scripts/diagnostics/ask-push-delivery.sh"
[[ -f "$HOOK" && -f "$READ" ]] || { echo "FAIL: hook or read script missing"; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

pass=0
fail=0

check() {
    local name="$1" want="$2" got="$3"
    if [[ "$got" == "$want" ]]; then
        echo "  PASS: $name"
        pass=$((pass + 1))
    else
        echo "  FAIL: $name"
        echo "    wanted: $want"
        echo "    got: ${got:-<empty>}"
        fail=$((fail + 1))
    fi
}

# push_payload <message> <result>
push_payload() {
    jq -n --arg m "$1" --arg r "$2" \
        '{tool_name: "PushNotification", session_id: "s1", tool_input: {message: $m}, tool_response: $r}'
}

home="$TMP/home"
rec() { printf '%s' "$(push_payload "$1" "$2")" | FNO_AGENT_HOME="$home" bash "$HOOK"; }

rec "Question q-aa11: pick one" "Terminal notification sent. Mobile push requested."
rec "Question q-bb22: pick one" "Not sent — this terminal is active, so your output here already reaches the user."
rec "Question q-cc33: pick one" "Terminal notification sent. Mobile push not sent (Remote Control inactive)."
rec "Question q-dd44: pick one" "something new"
rec "build done" "Terminal notification sent. Mobile push requested."

R="$home/ask-push/receipts.jsonl"
cls() { jq -r --arg q "$1" 'select(.qid == $q) | .class' "$R"; }
check "requested is classed" "requested" "$(cls q-aa11)"
check "active terminal is classed suppressed" "suppressed-active-terminal" "$(cls q-bb22)"
check "remote control inactive is classed" "not-sent-remote-control-inactive" "$(cls q-cc33)"
check "an unseen result text is unknown" "unknown" "$(cls q-dd44)"
check "a push with no question id keeps an empty qid" "1" "$(jq -s '[.[] | select(.qid == "")] | length' "$R")"

# The receipts file stays bounded: past 1000 rows the hook keeps the newest 500.
home3="$TMP/home3"
mkdir -p "$home3/ask-push"
R3="$home3/ask-push/receipts.jsonl"
for _ in $(seq 1 1000); do echo '{"ts":1,"qid":"q-old","class":"requested"}'; done >"$R3"
printf '%s' "$(push_payload "Question q-ee55: pick one" "Mobile push requested.")" | FNO_AGENT_HOME="$home3" bash "$HOOK"
check "receipts file is trimmed past 1000 rows" "500" "$(wc -l <"$R3" | tr -d ' ')"
check "the newest row survives the trim" "q-ee55" "$(tail -n 1 "$R3" | jq -r .qid)"

# The hook never blocks: bad input exits 0 and writes nothing.
printf 'not json' | FNO_AGENT_HOME="$TMP/home2" bash "$HOOK"
check "bad input exits 0" "0" "$?"
check "bad input writes no row" "0" "$([[ -e "$TMP/home2/ask-push/receipts.jsonl" ]] && echo 1 || echo 0)"

# The read: claim + requested push, no sink configured.
mkdir -p "$home/ask-push/q-aa11" "$home/ask-push/q-bb22"
state="$TMP/state"
mkdir -p "$state"
run_read() { FNO_AGENT_HOME="$home" FNO_ATTENTION_STATE_DIR="$state" bash "$READ" "$1"; }

out="$(run_read q-aa11)"
check "requested push passes up to the sink" "0" "$?"
check "device hop reads unconfirmed" 1 "$(printf '%s' "$out" | grep -c '^device: *unconfirmed')"

out="$(run_read q-bb22)"
check "suppressed push exits 1" "1" "$?"
check "suppressed push names the push call hop" 1 "$(printf '%s' "$out" | grep -c '^first failing hop: push call')"

out="$(run_read q-zz99)"
check "unclaimed qid names the hook claim hop" 1 "$(printf '%s' "$out" | grep -c '^first failing hop: hook claim')"

# A dropped ntfy delivery names the sink hop.
cat >"$state/deliveries.json" <<'EOF'
{"d1": {"item_id": "q-aa11", "sink": "phone", "event": "item.opened", "status": "dropped", "attempts": 2, "last_status": 503}}
EOF
out="$(run_read q-aa11)"
check "dropped sink post exits 1" "1" "$?"
check "dropped sink post names the ntfy sink hop" 1 "$(printf '%s' "$out" | grep -c '^first failing hop: ntfy sink')"

echo ""
echo "PASS=$pass FAIL=$fail"
[[ "$fail" -eq 0 ]]
