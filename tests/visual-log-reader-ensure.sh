#!/bin/bash
# tests/visual-log-reader-ensure.sh
#
# Verdict contract of skills/visual-log-reader/scripts/ensure-reader.sh.
# The probe is how any session checks the fleet-owned Visual Log tap
# reader: exit 0 when the registry row exists and its served last activity
# is inside the freshness bound; exit 1 with a repair line otherwise. Every
# case here runs the script through --status-file, so no live roster and no
# reader session is needed. The spawn-mode case stubs `fno` on PATH and
# asserts the recorded argv, which pins the spawn line's shape.

set -euo pipefail
cd "$(dirname "$0")/.."

SCRIPT="skills/visual-log-reader/scripts/ensure-reader.sh"
if [[ ! -f "$SCRIPT" ]]; then
    echo "FAIL: $SCRIPT does not exist"
    exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fixture() {
    # fixture <path> <json>
    printf '%s' "$2" > "$1"
}

FRESH_FIXTURE="$TMP/fresh.json"
fixture "$FRESH_FIXTURE" '{"agents": [{"name": "visual-log-reader", "last_activity_age_s": 120}]}'

STALE_FIXTURE="$TMP/stale.json"
fixture "$STALE_FIXTURE" '{"agents": [{"name": "visual-log-reader", "last_activity_age_s": 3600}]}'

ABSENT_FIXTURE="$TMP/absent.json"
fixture "$ABSENT_FIXTURE" '{"agents": [{"name": "other-worker", "last_activity_age_s": 10}]}'

UNKNOWN_AGE_FIXTURE="$TMP/unknown-age.json"
fixture "$UNKNOWN_AGE_FIXTURE" '{"agents": [{"name": "visual-log-reader", "last_activity_age_s": null}]}'

fail_count=0
expect() {
    # expect <desc> <want_exit> <want_output_pattern> <cmd...>
    local desc="$1" want_exit="$2" want_pattern="$3"
    shift 3
    local out rc=0
    out="$("$@" 2>&1)" || rc=$?
    if [[ "$rc" != "$want_exit" ]]; then
        echo "FAIL: $desc: exit $rc, want $want_exit"
        echo "  output: $out"
        fail_count=$((fail_count + 1))
        return 0
    fi
    if [[ -n "$want_pattern" ]] && ! grep -q "$want_pattern" <<< "$out"; then
        echo "FAIL: $desc: output misses pattern '$want_pattern'"
        echo "  output: $out"
        fail_count=$((fail_count + 1))
        return 0
    fi
    echo "ok: $desc"
}

expect "fresh reader exits 0" 0 "" \
    bash "$SCRIPT" check --status-file "$FRESH_FIXTURE"
expect "stale reader exits 1 with resume repair" 1 "resume visual-log-reader" \
    bash "$SCRIPT" check --status-file "$STALE_FIXTURE"
expect "absent reader exits 1 with spawn repair" 1 "fno agents spawn visual-log-reader" \
    bash "$SCRIPT" check --status-file "$ABSENT_FIXTURE"
expect "absent repair names the thread lane" 1 "\\-\\-as thread" \
    bash "$SCRIPT" check --status-file "$ABSENT_FIXTURE"
expect "unknown last activity exits 1" 1 "" \
    bash "$SCRIPT" check --status-file "$UNKNOWN_AGE_FIXTURE"
expect "freshness bound is overridable" 1 "" \
    bash "$SCRIPT" check --status-file "$FRESH_FIXTURE" --fresh-seconds 60

# spawn mode: stub `fno` so no real spawn fires; the check inside spawn mode
# reads the absent fixture, so the stub must record the spawn argv.
STUB_BIN="$TMP/bin"
mkdir -p "$STUB_BIN"
cat > "$STUB_BIN/fno" << 'STUB'
#!/bin/bash
printf '%s\n' "$*" >> "$FNO_STUB_LOG"
exit 0
STUB
chmod +x "$STUB_BIN/fno"
STUB_LOG="$TMP/fno-argv.log"
expect "spawn mode fires the thread spawn on an absent row" 0 "spawned visual-log-reader" \
    env PATH="$STUB_BIN:$PATH" FNO_STUB_LOG="$STUB_LOG" \
    bash "$SCRIPT" spawn --status-file "$ABSENT_FIXTURE"
if ! grep -q -- "--as thread" "$STUB_LOG"; then
    echo "FAIL: spawn argv lost the thread lane flag"
    fail_count=$((fail_count + 1))
else
    echo "ok: spawn argv carries --as thread"
fi
if ! grep -q "visual-log-reader" "$STUB_LOG"; then
    echo "FAIL: spawn argv lost the reader name"
    fail_count=$((fail_count + 1))
else
    echo "ok: spawn argv carries the reader name"
fi

# spawn mode on a fresh reader must not spawn again.
: > "$STUB_LOG"
expect "spawn mode on a fresh reader spawns nothing" 0 "already" \
    env PATH="$STUB_BIN:$PATH" FNO_STUB_LOG="$STUB_LOG" \
    bash "$SCRIPT" spawn --status-file "$FRESH_FIXTURE"
if [[ -s "$STUB_LOG" ]]; then
    echo "FAIL: fresh spawn mode still called fno with: $(cat "$STUB_LOG")"
    fail_count=$((fail_count + 1))
else
    echo "ok: fresh spawn mode called fno only for the check"
fi

if [[ "$fail_count" -gt 0 ]]; then
    echo "FAIL: $fail_count assertion(s) failed"
    exit 1
fi
echo "PASS: visual-log-reader ensure contract"
