#!/usr/bin/env bash
# tests/hooks/test_bootstrap_session_start.sh
#
# Verifies the SessionStart bootstrap detector. The hook prints one
# SessionStart JSON line when the session's claude supervisor was born
# outside the Aqua GUI session (launchctl managername != Aqua), or when
# scutil --dns names no DNS configuration (a bootstrap that died after
# birth). Healthy: silence and exit 0.
#
# Isolation: stub `launchctl` and `scutil` first on PATH answer from env
# vars; the real ones never run.
#
# Run: bash tests/hooks/test_bootstrap_session_start.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT_REAL="$(cd "${SCRIPT_DIR}/../.." && pwd)"
HOOK="${REPO_ROOT_REAL}/hooks/bootstrap-session-start.sh"

log()  { printf '[bootstrap-ss] %s\n' "$*"; }
fail() { printf '[bootstrap-ss] FAIL: %s\n' "$*" >&2; exit 1; }
pass() { printf '[bootstrap-ss] PASS: %s\n' "$*"; }

[[ -f "$HOOK" ]] || fail "hook not found at $HOOK"
command -v jq >/dev/null 2>&1 || fail "jq required for these tests"

WORK=$(mktemp -d -t bootstrap-ss-XXXXXX)
trap 'rm -rf "$WORK"' EXIT

# --- Stub launchctl + scutil on PATH: answers from env vars. ---
FAKEBIN="$WORK/bin"
mkdir -p "$FAKEBIN"
cat > "$FAKEBIN/launchctl" <<'FAKE'
#!/usr/bin/env bash
if [[ "${1:-}" == "managername" ]]; then
    printf '%s\n' "${FAKE_MANAGER_NAME:-Aqua}"
fi
exit 0
FAKE
cat > "$FAKEBIN/scutil" <<'FAKE'
#!/usr/bin/env bash
if [[ "${1:-}" == "--dns" ]]; then
    if [[ "${FAKE_DNS_MODE:-ok}" == "ok" ]]; then
        printf 'DNS configuration\n\nresolver #1\n  nameserver[0] : 1.1.1.1\n'
    else
        printf '%s\n' "${FAKE_DNS_MODE}"
    fi
fi
exit 0
FAKE
chmod +x "$FAKEBIN/launchctl" "$FAKEBIN/scutil"

run_hook() {
    (export PATH="$FAKEBIN:$PATH"; bash "$HOOK")
}

# Case 1: a Background manager fires the birth-context arm.
out="$(FAKE_MANAGER_NAME=Background run_hook)"
code=$?
[[ $code -eq 0 ]] || fail "hook exited $code under a Background manager"
[[ "$(printf '%s' "$out" | wc -l | tr -d ' ')" == "1" ]] || fail "expected exactly one line, got: $out"
printf '%s' "$out" | jq -e '.hookSpecificOutput.hookEventName == "SessionStart"' >/dev/null || fail "not a SessionStart JSON line: $out"
printf '%s' "$out" | jq -e '.hookSpecificOutput.additionalContext' >/dev/null || fail "no additionalContext: $out"
printf '%s' "$out" | grep -q "claude daemon stop --any" || fail "the line must name the cure: $out"
printf '%s' "$out" | grep -q "Background" || fail "the line must name the manager: $out"
pass "Background manager fires one JSON line naming the cure"

# Case 2: Aqua manager but no DNS configuration fires the services arm.
out="$(FAKE_DNS_MODE='No DNS configuration available' run_hook)"
code=$?
[[ $code -eq 0 ]] || fail "hook exited $code under broken DNS"
printf '%s' "$out" | jq -e '.hookSpecificOutput.additionalContext' >/dev/null || fail "no additionalContext: $out"
printf '%s' "$out" | grep -q "lost its GUI bootstrap" || fail "the line must name the broken-services cause: $out"
pass "Missing DNS configuration fires one JSON line"

# Case 3: Aqua manager plus a resolver list stays silent.
out="$(run_hook)"
code=$?
[[ $code -eq 0 ]] || fail "hook exited $code when healthy"
[[ -z "$out" ]] || fail "hook must be silent when healthy, got: $out"
pass "Aqua plus resolvers stays silent"

log "all 3 cases passed"
