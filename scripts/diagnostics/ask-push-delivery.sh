#!/usr/bin/env bash
# ask-push-delivery.sh <qid>: where a filed question's push stopped.
#
# Hops, in order: the hook claim (ask-push.sh fired), the push call
# (push-receipt.sh row), the ntfy sink post (deliveries.json), the device.
# The device hop is never "delivered": no push path returns a device receipt,
# so the best a read can say is "unconfirmed". The first hop that failed is
# named on the last line. Exit 0 when every hop up to the sink holds, 1 when a
# hop failed, 2 on bad usage.

set -uo pipefail

qid="${1:-}"
[[ "$qid" =~ ^[A-Za-z0-9_.-]+$ ]] || { echo "usage: ask-push-delivery.sh <qid>" >&2; exit 2; }
command -v jq >/dev/null 2>&1 || { echo "ask-push-delivery: jq is required" >&2; exit 2; }

agent_home="${FNO_AGENT_HOME:-$HOME/.fno/agents}"
state_dir="${FNO_ATTENTION_STATE_DIR:-$HOME/.fno/state/attention}"
failed=""

fail() { [[ -n "$failed" ]] || failed="$1"; }

if [[ -d "$agent_home/ask-push/$qid" ]]; then
    echo "hook claim:   ok (ask-push.sh fired for $qid)"
else
    echo "hook claim:   none (no claim dir; the ask never printed a claude-push line)"
    fail "hook claim"
fi

receipts="$agent_home/ask-push/receipts.jsonl"
rows=""
[[ -f "$receipts" ]] && rows="$(jq -c --arg q "$qid" 'select(.qid == $q)' "$receipts" 2>/dev/null)"
if [[ -z "$rows" ]]; then
    echo "push call:    none (the model never called PushNotification, or push-receipt.sh is not installed)"
    fail "push call"
else
    classes="$(jq -r '.class' <<<"$rows" | sort | uniq -c | tr -s ' ' | tr '\n' ';')"
    if jq -e 'select(.class == "requested")' <<<"$rows" >/dev/null 2>&1; then
        echo "push call:    requested ($classes)"
    else
        echo "push call:    not sent ($classes)"
        fail "push call"
    fi
fi

deliveries="$state_dir/deliveries.json"
if [[ ! -f "$deliveries" ]]; then
    echo "ntfy sink:    not configured here (no deliveries.json)"
else
    drow="$(jq -c --arg q "$qid" '[to_entries[] | .value | select(.item_id == $q and .event == "item.opened")] | .[0] // empty' "$deliveries" 2>/dev/null)"
    if [[ -z "$drow" ]]; then
        echo "ntfy sink:    no delivery row for $qid"
        fail "ntfy sink"
    else
        status="$(jq -r '.status' <<<"$drow")"
        echo "ntfy sink:    $status (attempts $(jq -r '.attempts' <<<"$drow"), last http $(jq -r '.last_status // "none"' <<<"$drow")); a 2xx is a server accept, not a device receipt"
        [[ "$status" == "delivered" ]] || fail "ntfy sink"
    fi
fi

echo "device:       unconfirmed (no push path returns a device receipt)"
if [[ -n "$failed" ]]; then
    echo "first failing hop: $failed"
    exit 1
fi
echo "first failing hop: none up to the sink"
exit 0
