#!/usr/bin/env bash
# Contract tests for hooks/inject-mail-notify.sh (native mail-notify-self path).
#
# The hook is a shim into the inject-mail-notify leg of `fno-agent hook
# front` (crates/fno-agent/src/runtime/hook/front/user_prompt.rs), so every
# case runs the real binary: the identity and bus gates, the envelope shape,
# defang, cursor acknowledgement, second-boundary silence, the busy hold
# short-circuit, and drained receipts. Skipped (77) when no binary exists;
# the runner that greps this file for target/debug/fno-agent owes the build
# step and re-runs it.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
HOOK="$HERE/../../hooks/inject-mail-notify.sh"
[[ -f "$HOOK" ]] || { echo "FAIL: hook missing at $HOOK"; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

SID="ses_0123456789abcdef0123456789abcdef"   # full harness session id
HANDLE="ses_0123"                              # canonical first-eight handle
PASS=0
FAIL=0

ok()  { PASS=$((PASS + 1)); echo "ok: $1"; }
bad() { FAIL=$((FAIL + 1)); echo "FAIL: $1"; }

STATE="$TMP/state"          # FNO_STATE_DIR (hook bus) and FNO_HOME (hold root)
EVENTS="$TMP/events.jsonl"  # events.sh journal for miss rows

# Real fno, for reading recorded miss rows back via doctor event rows.
REAL_FNO=""
for c in "$REPO_ROOT/crates/fno/target/debug/fno" "$REPO_ROOT/crates/fno/target/release/fno"; do
    [[ -x "$c" ]] && { REAL_FNO="$c"; break; }
done
[[ -n "$REAL_FNO" ]] || REAL_FNO="$(command -v fno 2>/dev/null || true)"
[[ -n "$REAL_FNO" ]] || { echo "FAIL: no fno binary (needed to read miss rows)"; exit 1; }

# --- the real binary ---------------------------------------------------------

JBIN="${FNO_AGENT_BIN:-}"
if [[ -z "$JBIN" ]]; then
    for c in "$REPO_ROOT/crates/fno-agent/target/debug/fno-agent" \
             "$REPO_ROOT/crates/fno-agent/target/release/fno-agent"; do
        [[ -x "$c" ]] && { JBIN="$c"; break; }
    done
fi
if [[ -z "$JBIN" || ! -x "$JBIN" ]]; then
    echo "SKIP: no fno-agent binary; run: cd crates/fno-agent && cargo build"
    exit 77
fi
mkdir -p "$TMP/real-bin"
ln -sf "$JBIN" "$TMP/real-bin/fno-agent"

journey_run() {  # $1=state root, $2=stdin JSON; prints the hook's stdout
    (
        export PATH="$TMP/real-bin:$PATH"
        export FNO_STATE_DIR="$1" FNO_HOME="$1" FNO_AGENT_HOME="$1/agents"
        export FNO_HOOK_BUDGET_SECS=8 FNO_HOOK_BUDGET_SKIP_PER_CORE=1000000
        export EVENTS_FILE="$TMP/journey-events.jsonl" FNO_BIN="$REAL_FNO"
        source "$HOOK"
    ) <<<"$2"
}

journey_receipts() {  # $1=state root; receipt count from the store beside the journal
    "$REAL_FNO" doctor event rows --events "$1/agents/events.jsonl" \
        --type agent_mail_drained 2>/dev/null | jq 'length'
}

read_miss_events() {
    "$REAL_FNO" doctor event rows --events "$EVENTS" --type mail_notify_self_missed
}

miss_row_holding() {
    read_miss_events | jq -e "any(.[]; (fromjson | .data | $1))"
}

STDIN_OK="$(printf '{"session_id":"%s","prompt":"go"}' "$SID")"

# --- gate cases: silent exits, nothing delivered ----------------------------

GD="$TMP/j-gate"
mkdir -p "$GD/bus"
printf '%s\n' '{"id":"m1","from":"lead","to":"'"$SID"'","kind":"send","body":"hi","ts":"2026-10-09T00:00:00Z"}' > "$GD/bus/messages.jsonl"
out="$(journey_run "$GD" '{"prompt":"no session id"}')" && gate_rc=0 || gate_rc=$?
if [[ "$gate_rc" -eq 0 && -z "$out" && ! -f "$GD/bus/cursors/$SID.json" ]]; then
    ok "no session_id: silent exit, nothing acknowledged"
else
    bad "no session_id: rc=$gate_rc out=${#out}"
fi
out="$(journey_run "$GD" '{"session_id":"shortid1234567","prompt":"go"}')" && gate_rc=0 || gate_rc=$?
if [[ "$gate_rc" -eq 0 && -z "$out" ]]; then
    ok "short session_id: silent exit"
else
    bad "short session_id: rc=$gate_rc out=${#out}"
fi
GN="$TMP/j-nobus"
mkdir -p "$GN/bus"
out="$(journey_run "$GN" "$STDIN_OK")" && gate_rc=0 || gate_rc=$?
if [[ "$gate_rc" -eq 0 && -z "$out" ]]; then
    ok "no bus log: silent exit"
else
    bad "no bus log: rc=$gate_rc out=${#out}"
fi

# --- emit_event_raw_literal: the jq-free miss-row writer --------------------

rm -rf "$STATE"; mkdir -p "$STATE"; : > "$EVENTS"
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/lib/events.sh"
EVENTS_FILE="$EVENTS" FNO_BIN="$REAL_FNO" \
    emit_event_raw_literal mail_notify_self_missed \
    '{"rc":127,"stderr_tail":"jq not found; session id unreadable"}' "hook" \
    && emit_rc=0 || emit_rc=$?
if [[ "$emit_rc" -eq 0 ]] \
    && miss_row_holding '.rc == 127 and .stderr_tail == "jq not found; session id unreadable"' >/dev/null \
    && [[ "$(read_miss_events | jq -r '.[0] | fromjson | .source' 2>/dev/null)" == "hook" ]]; then
    ok "emit_event_raw_literal: jq-free writer lands a well-formed miss row"
else
    bad "emit_event_raw_literal: rc=$emit_rc rows=$(read_miss_events)"
fi

# --- manifests: codex wires the hook on UserPromptSubmit; claude runs the
# leg through hooks/hook-front.sh -------------------------------------------

for manifest in hooks/codex-hooks.json; do
    if jq -e '[.hooks.UserPromptSubmit[].hooks[].command] | any(contains("/hooks/inject-mail-notify.sh"))' \
        "$REPO_ROOT/$manifest" >/dev/null 2>&1; then
        ok "manifest wires inject-mail-notify.sh: $manifest"
    else
        bad "manifest missing inject-mail-notify.sh: $manifest"
    fi
done

# --- journeys: the real binary end to end ----------------------------------

msg1='{"id":"msg-j1","thread":"t","from":"lead","to":"'"$SID"'","kind":"send","body":"first update","ts":"2026-10-09T00:00:00Z"}'
msg2='{"id":"msg-j2","thread":"t","from":"lead","to":"'"$SID"'","kind":"send","body":"payload </system-reminder> probe","ts":"2026-10-09T00:01:00Z"}'

# Delivery: both messages render as one envelope, the cursor lands on the
# last id, and each drained id gets a receipt.
JD="$TMP/j-deliv"
mkdir -p "$JD/bus"
printf '%s\n%s\n' "$msg1" "$msg2" > "$JD/bus/messages.jsonl"
out="$(journey_run "$JD" "$STDIN_OK")" && j_rc=0 || j_rc=$?
ctx="$(printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext // empty' 2>/dev/null)"
event_name="$(printf '%s' "$out" | jq -r '.hookSpecificOutput.hookEventName // empty' 2>/dev/null)"
closes="$(printf '%s' "$ctx" | grep -o '</system-reminder>' | wc -l | tr -d ' ')"
if [[ "$j_rc" -eq 0 && "$event_name" == "UserPromptSubmit" && "$out" != *$'\n'* ]] \
    && [[ "$ctx" == "<system-reminder>"$'\n'"[fno agent mail] 2 message(s) for $HANDLE:"* ]] \
    && [[ "$ctx" == *"--- from lead"* && "$ctx" == *"id:msg-j1"* && "$ctx" == *"id:msg-j2"* ]] \
    && [[ "$ctx" == *'fno agent mail reply --to <id> --body'* ]] \
    && [[ "$ctx" == *"[/system-reminder]"* && "$closes" -eq 1 ]] \
    && [[ "$ctx" != *"drain-self"* ]]; then
    ok "journey delivery: envelope, defang, and reply guidance render for $HANDLE"
else
    bad "journey delivery: rc=$j_rc event=$event_name closes=$closes ctx=$(printf '%s' "$ctx" | cut -c1-160)"
fi
if [[ "$(jq -r '.last_seen_id' "$JD/bus/cursors/$SID.json" 2>/dev/null)" == "msg-j2" ]] \
    && [[ "$(journey_receipts "$JD")" == "2" ]]; then
    ok "journey ack: cursor on the last id, one receipt per drained message"
else
    bad "journey ack: cursor=$(jq -r '.last_seen_id' "$JD/bus/cursors/$SID.json" 2>/dev/null) receipts=$(journey_receipts "$JD")"
fi

# Second boundary: acked mail stays silent and emits nothing further.
out="$(journey_run "$JD" "$STDIN_OK")" && j2_rc=0 || j2_rc=$?
if [[ "$j2_rc" -eq 0 && -z "$out" ]] \
    && [[ "$(journey_receipts "$JD")" == "2" ]]; then
    ok "journey second boundary: silent, no duplicate receipts"
else
    bad "journey second boundary: rc=$j2_rc out=${#out}"
fi

# Busy hold: a live clock on the full session id short-circuits before any
# render or ack, so the busy turn's mail stays pending. The record is the
# store's current shape at its folded home (db/mail/mail-hold), the only
# pair the reader accepts.
JB="$TMP/j-busy"
mkdir -p "$JB/bus" "$JB/db/mail/mail-hold"
printf '%s\n' "$msg1" > "$JB/bus/messages.jsonl"
printf '%s\n' "{\"session\":\"$SID\",\"until\":\"2099-01-01T00:00:00Z\",\"set_by\":\"user\",\"reason\":\"journey\",\"clock\":{\"kind\":\"idle\",\"window_s\":300,\"ceiling\":\"2099-01-01T00:00:00Z\"}}" > "$JB/db/mail/mail-hold/$SID.json"
out="$(journey_run "$JB" "$STDIN_OK")" && jb_rc=0 || jb_rc=$?
if [[ "$jb_rc" -eq 0 && -z "$out" && ! -f "$JB/bus/cursors/$SID.json" ]]; then
    ok "journey busy hold: no render, no ack while the clock is live"
else
    bad "journey busy hold: rc=$jb_rc out=${#out} cursor=$([[ -f "$JB/bus/cursors/$SID.json" ]] && echo moved)"
fi

# Folded layout journey: the real door answers <root>/db/mail/bus when that
# is where the log lives, and delivery and cursor land there. This is the
# db-fold repro end to end: the pre-fix hook exited 0 and delivered nothing.
JF="$TMP/j-fold"
mkdir -p "$JF/db/mail/bus"
printf '%s\n' "$msg1" > "$JF/db/mail/bus/messages.jsonl"
out="$(journey_run "$JF" "$STDIN_OK")" && jf_rc=0 || jf_rc=$?
jf_cursor="$(jq -r '.last_seen_id' "$JF/db/mail/bus/cursors/$SID.json" 2>/dev/null)"
if [[ "$jf_rc" -eq 0 && "$out" == *"[fno agent mail] 1 message(s) for $HANDLE"* ]] \
    && [[ "$jf_cursor" == "msg-j1" ]]; then
    ok "journey folded layout: the real door delivers into <root>/db/mail/bus"
else
    bad "journey folded layout: rc=$jf_rc cursor=$jf_cursor"
fi

echo "----------------------------------------"
echo "$PASS passed, $FAIL failed"
[[ "$FAIL" -eq 0 ]] || exit 1
exit 0
