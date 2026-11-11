#!/usr/bin/env bash
# Contract tests for hooks/inject-control-drain-tool-boundary.sh: the shell
# gate only. A stub fno-agents logs its argv, so no build is needed; the
# command's own render and cursor contract lives in mail_control_drain.rs.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOOK="$HERE/../../hooks/inject-control-drain-tool-boundary.sh"
[[ -f "$HOOK" ]] || { echo "FAIL: hook missing at $HOOK"; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

SID="ffffabcd-1111-2222-3333-444455556666"
BUS="$TMP/state/bus"
LOG="$TMP/args.log"
EVENTS="$TMP/drain-events.jsonl"
PASS=0
FAIL=0
ok()  { PASS=$((PASS + 1)); echo "ok: $1"; }
bad() { FAIL=$((FAIL + 1)); echo "FAIL: $1"; }

mkdir -p "$TMP/bin" "$TMP/nojq"
# The stub answers the dir door the way the real binary does (FNO_BUS_DIR,
# then the layout fold: folded when it exists, legacy when it exists, folded
# on fresh) and logs every other argv as the delivery call it passed.
cat > "$TMP/bin/fno-agents" <<'STUB'
#!/usr/bin/env bash
case "$*" in
  "state path dir bus")
    [[ "${FNO_STUB_DOOR_FAIL:-0}" == "1" ]] && exit 1
    if [[ -n "${FNO_BUS_DIR:-}" ]]; then printf '%s\n' "$FNO_BUS_DIR"; exit 0; fi
    root="${FNO_STATE_DIR:-$HOME/.fno}"
    if [[ -d "$root/db/mail/bus" ]]; then printf '%s\n' "$root/db/mail/bus"
    elif [[ -d "$root/bus" ]]; then printf '%s\n' "$root/bus"
    else printf '%s\n' "$root/db/mail/bus"; fi
    exit 0
    ;;
esac
printf '%s\n' "$*" >> "${FNO_STUB_LOG:?}"
exit 0
STUB
# A stub front door: events.sh commits every row through `fno doctor event
# emit-envelope`, and a CI runner has no fno installed and no checkout build,
# so the real binary silently loses the miss row this test asserts on. The
# stub stores into the events file the same way the read stub reads it.
cat > "$TMP/bin/fno" <<'STUB'
#!/usr/bin/env bash
path=""
prev=""
for a in "$@"; do
    [[ "$prev" == "--events" ]] && path="$a"
    prev="$a"
done
case "$*" in
  *"emit-envelope"*) cat >> "$path"; exit 0 ;;
  *"event rows"*)    cat "$path" 2>/dev/null; exit 0 ;;
esac
exit 0
STUB
# A jq that answers like a missing binary: the hook must still read the id.
printf '#!/usr/bin/env bash\nexit 127\n' > "$TMP/nojq/jq"
chmod +x "$TMP/bin/fno-agents" "$TMP/bin/fno" "$TMP/nojq/jq"

fresh() {
    rm -rf "$TMP/state"
    mkdir -p "$BUS"
    : > "$LOG"
    : > "$EVENTS"
    echo '{"id":"m1"}' > "$BUS/messages.jsonl"
}

run_hook() {  # $1 extra PATH prefix
    PATH="$1:$TMP/bin:$PATH" FNO_STATE_DIR="$TMP/state" FNO_STUB_LOG="$LOG" EVENTS_FILE="$EVENTS" bash "$HOOK" \
        <<<"{\"session_id\":\"$SID\",\"tool_name\":\"Bash\",\"tool_input\":{}}"
}

called() { grep -q -- "--control-drain --bus-dir ${1:-$BUS} --session $SID" "$LOG"; }

# The miss row lives in the sibling store --events names, not the jsonl, so
# the read goes through the stub the same way the write did.
read_miss_rows() { "$TMP/bin/fno" doctor event rows --events "$EVENTS" 2>/dev/null; }

# AC4: jq missing, control flag set: the drain still runs.
fresh
mkdir -p "$BUS/control-pending" "$BUS/tool-boundary"
touch "$BUS/control-pending/${SID:0:8}.flag"
touch -t 209901010000 "$BUS/tool-boundary/$SID.stamp"
run_hook "$TMP/nojq"
called && ok "jq missing still drains a control flag" || bad "jq missing skipped the drain"

# No flag, stamp ahead of the bus: the gate skips the command.
fresh
mkdir -p "$BUS/tool-boundary"
touch -t 209901010000 "$BUS/tool-boundary/$SID.stamp"
run_hook ""
called && bad "fresh stamp still ran the command" || ok "fresh stamp skips the command"

# No flag, no stamp: the bus counts as changed, so the command runs.
fresh
run_hook ""
called && ok "missing stamp runs the command" || bad "missing stamp skipped the command"

# Folded layout: seed ONLY <state>/db/mail/bus; the drain must follow the
# door's answer there. (The pre-fix hook exited 0 and never drained.)
fresh
rm -rf "$TMP/state/bus"
mkdir -p "$TMP/state/db/mail/bus/control-pending" "$TMP/state/db/mail/bus/tool-boundary"
touch "$TMP/state/db/mail/bus/control-pending/${SID:0:8}.flag"
run_hook ""
called "$TMP/state/db/mail/bus" \
    && ok "folded layout drains through the door's answer" \
    || bad "folded layout: no drain (args=$(cat "$LOG" 2>/dev/null))"

# A door that cannot answer is a no-answer: no drain, no guessed path, and
# one miss row naming the rc so a silently-disabled hook is still visible.
fresh
FNO_STUB_DOOR_FAIL=1 run_hook ""
if called; then
    bad "door failure still drained"
elif read_miss_rows | grep -q "bus dir unresolved"; then
    ok "door failure skips the drain and lands a miss row"
else
    bad "door failure: skipped but no miss row (rows=$(read_miss_rows))"
fi

echo "passed=$PASS failed=$FAIL"
[[ "$FAIL" -eq 0 ]]
