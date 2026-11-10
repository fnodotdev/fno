#!/usr/bin/env bash
# sandbox-leak-probe.sh - the Done probe: a fresh sandbox, created and torn
# down while the live fleet runs, must not see or touch the live fleet.
#
# Run: bash scripts/ops/sandbox-leak-probe.sh
# Exit: 0 leaks=0, 1 any leak. Prints `sandbox-leak-probe: leaks=<n> checked=<m>`.
set -u

LEAKS=0
COUNT=0
ROOT=""
NAME=""
FNO_BIN=""
MARKER=""

fail() { LEAKS=$((LEAKS + 1)); echo "leak: $1"; }
pass() { echo "ok: $1"; }

REAL_HOME="$HOME"

# Management actions resolve the sandbox registry from the REAL home; the
# sandbox env must never be active for them.
outside() {
    env -u FNO_CONFIG -u FNO_MUX_DIR -u FNO_AGENTS_HOME -u FNO_CLAIMS_ROOT \
        -u FNO_SPACES_DIR -u CLAUDE_CONFIG_DIR -u CODEX_HOME -u FNO_SANDBOX \
        HOME="$REAL_HOME" "$FNO_BIN" "$@"
}

rm_sandbox() {
    outside config project sandbox rm "$NAME" >/dev/null 2>&1
}

cleanup() {
    if [[ -n "$NAME" && -x "${FNO_BIN}" ]]; then
        rm_sandbox || true
    fi
}
trap cleanup EXIT

resolve_fno() {
    if [[ -n "${FNO_SANDBOX_PROBE_FNO:-}" ]]; then
        FNO_BIN="$FNO_SANDBOX_PROBE_FNO"
        return 0
    fi
    local wt
    wt="$(git rev-parse --show-toplevel 2>/dev/null)/target/debug/fno"
    if [[ -x "$wt" ]]; then
        FNO_BIN="$wt"
        return 0
    fi
    FNO_BIN="$(command -v fno 2>/dev/null || true)"
    [[ -n "$FNO_BIN" ]] && return 0
    echo "sandbox-leak-probe: no fno binary found" >&2
    return 2
}

# Create with stdin forced off a TTY so no login runs.
create_sandbox() {
    exec </dev/null
    "$FNO_BIN" config project sandbox create "$NAME" || return 1
}

apply_env_here() {
    local manifest="$STATE_DIR/state/sandboxes/$NAME/sandbox.toml"
    eval "$("$FNO_BIN" config project sandbox env "$NAME")" || return 1
    ROOT="$(python3 - "$manifest" <<'PY'
import sys
for line in open(sys.argv[1]):
    if line.startswith("root = "):
        print(line.split("=", 1)[1].strip().strip('"'))
        break
PY
)"
    [[ -n "$ROOT" ]]
}

# Check 1: state_dir resolves under the sandbox root.
check_state_dir() {
    local got
    got="$(fno config get state_dir 2>/dev/null | head -1)"
    got="${got/#\~/$HOME}"
    if [[ "$got" == "$ROOT"* ]]; then
        pass "state_dir under root"
    else
        fail "state_dir resolved outside the root: $got"
    fi
}

# Check 2: a sandbox backlog write lands in the sandbox store and the live
# graph never sees the marker.
check_backlog_write() {
    local out rc
    out="$(fno backlog idea "$MARKER" --difficulty low 2>&1)"
    rc=$?
    if [[ $rc -ne 0 ]]; then
        fail "sandbox backlog write failed: $(printf '%s' "$out" | tail -1)"
        return
    fi
    local live
    live="$(env -u FNO_CONFIG "$FNO_BIN" backlog find "$MARKER" 2>/dev/null | head -1)"
    if [[ -z "$live" ]]; then
        pass "backlog write is sandbox-local"
    else
        fail "the live graph sees the sandbox marker"
    fi
}

# Check 3: the live graph page never carries the marker.
check_graph_html() {
    local hits
    hits="$(grep -c "$MARKER" "$REAL_HOME/.fno/cache/pages/graph.html" 2>/dev/null || true)"
    hits="${hits:-0}"
    if [[ "$hits" == "0" ]]; then
        pass "live graph.html clean"
    else
        fail "live graph.html carries the marker: $hits"
    fi
}

# Check 4: claims in the sandbox start at zero.
check_claims() {
    local rows
    rows="$(fno agents claim list 2>/dev/null | grep -c 'holder=' || true)"
    if [[ "$rows" == "0" ]]; then
        pass "claims start at zero"
    else
        fail "claims list shows rows: $rows"
    fi
}

# Check 5: mux lists no live session and the socket dir is under the root.
check_mux() {
    fno mux ls >/dev/null 2>&1
    local sockets rows
    sockets="$(find "$ROOT/fno/mux" -name '*.sock' 2>/dev/null | head -3)"
    rows="$(fno mux ls 2>/dev/null | grep -c 'clients,' || true)"
    if [[ -z "$sockets" && "$rows" == "0" ]]; then
        pass "mux is sandbox-local and empty"
    else
        fail "mux leaks: sockets=$sockets rows=$rows"
    fi
}

# Check 6: fno's claude roster reader lists no live rows under the env.
check_roster() {
    local rows
    rows="$(fno agents status --json 2>/dev/null | sed -n '/^{/,$p' | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    print("?")
    raise SystemExit
rows = data.get("agents") or data.get("rows") or data.get("workers") or []
print(len(rows))
' 2>/dev/null)"
    if [[ "$rows" == "0" ]]; then
        pass "claude roster reads 0 rows"
    else
        fail "claude roster lists $rows rows under the sandbox env"
    fi
}

# Check 7: the sandbox accounts config starts empty.
check_accounts() {
    local count
    count="$(fno config accounts list --json 2>/dev/null | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    print("?")
    raise SystemExit
recs = data if isinstance(data, list) else (data.get("records") or [])
print(len(recs))
' 2>/dev/null)"
    if [[ "$count" == "0" ]]; then
        pass "accounts start empty"
    else
        fail "sandbox accounts list shows records: $count"
    fi
}

# Check 8: reset keeps the harness homes byte-identical and refreshes fno/.
check_reset_markers() {
    mkdir -p "$ROOT/home/.claude" "$ROOT/home/.codex"
    printf 'claude-login' >"$ROOT/home/.claude/marker"
    printf 'codex-login' >"$ROOT/home/.codex/marker"
    if ! outside config project sandbox reset "$NAME" >/dev/null 2>&1; then
        fail "reset refused"
        return
    fi
    if [[ "$(cat "$ROOT/home/.claude/marker" 2>/dev/null)" == "claude-login" \
        && "$(cat "$ROOT/home/.codex/marker" 2>/dev/null)" == "codex-login" \
        && -f "$ROOT/fno/config.toml" ]]; then
        pass "reset kept the harness homes and recreated the config"
    else
        fail "reset dropped a harness marker or the fresh config"
    fi
}

# Check 10: process readers inside the sandbox see no live fleet.
check_readers() {
    local census
    census="$("${FNO_AGENTS_BIN:-fno-agents}" census --workers --json 2>/dev/null | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
    rows = data.get("workers") or []
    print(len(rows))
except Exception:
    print("?")
' 2>/dev/null)"
    if [[ "$census" == "0" ]]; then
        pass "census sees no live fleet rows"
    else
        fail "census lists live rows: $census"
    fi
    local watchdog
    watchdog="$(fno agents fleet watchdog 2>/dev/null || true)"
    if [[ -z "$watchdog" || "$watchdog" == *"no live"* || "$watchdog" == *"0 "* ]]; then
        pass "watchdog dry run acts on no live pid"
    else
        fail "watchdog dry run named live pids: $watchdog"
    fi
}

# Check 11: both harness logins read logged out under the env.
check_logins_logged_out() {
    local claude_out codex_out
    claude_out="$(claude auth status 2>/dev/null | grep -c '"loggedIn": false')"
    codex_out="$(codex login status 2>&1 || true)"
    if [[ "$claude_out" -ge 1 && "$codex_out" == *"Not logged in"* ]]; then
        pass "logins read logged out under the env"
    else
        fail "login read under the env: claude_count=$claude_out codex=$(printf '%s' "$codex_out" | head -c 60)"
    fi
}

main() {
    resolve_fno || return 1
    # STATE_DIR names the default state root; the sandbox registry lives
    # under it. Sourced before the sandbox env exists, so the read is the
    # real one.
    stub="$("$FNO_BIN" config paths shell-stub)"
    [[ -n "$stub" && -f "$stub" ]] && source "$stub"
    : "${STATE_DIR:?sandbox-leak-probe: STATE_DIR did not resolve}"
    MARKER="fno-sandbox-probe-$(date +%s)"
    NAME="probe-$(date +%s)-$$"
    ROOT="$STATE_DIR/state/sandboxes/$NAME"
    if ! create_sandbox; then
        fail "create failed"
        echo "sandbox-leak-probe: leaks=$LEAKS checked=$COUNT"
        return 1
    fi
    if ! apply_env_here; then
        fail "env read failed"
        echo "sandbox-leak-probe: leaks=$LEAKS checked=$COUNT"
        return 1
    fi
    run_check check_state_dir
    run_check check_backlog_write
    run_check check_graph_html
    run_check check_claims
    run_check check_mux
    run_check check_roster
    run_check check_accounts
    run_check check_reset_markers
    run_check check_readers
    run_check check_logins_logged_out
    rm_sandbox || fail "rm failed"
    COUNT=$((COUNT + 1))
    if [[ ! -e "$ROOT" ]]; then
        pass "rm removed the root"
    else
        fail "the root survived rm"
    fi
    if ! pgrep -f "$ROOT" >/dev/null 2>&1; then
        pass "no process carries the root in argv"
    else
        fail "a process still carries the root in argv"
    fi
    echo "sandbox-leak-probe: leaks=$LEAKS checked=$COUNT"
    [[ "$LEAKS" -eq 0 ]]
}

# One COUNT unit per check function, leak counting stays inside.
run_check() {
    COUNT=$((COUNT + 1))
    "$@" || true
}

main "$@"
exit $?

