#!/usr/bin/env bash
# fno hook: StopFailure - the primary logout detector, plus the target loop's circuit breaker.
# Claude Code fires StopFailure instead of Stop when a turn ends on an API
# error. The documented input carries `error` (rate_limit | overloaded |
# authentication_failed | ... | unknown), optional `error_details`, optional
# `last_assistant_message`. The old hook read .error_type/.status_code,
# fields the input does not carry, and exited unless a target loop ran, so
# leads and plain sessions were never covered. Now every failure appends a
# real event, and on authentication_failed the hook pages the operator and
# asks the daemon to respawn the session (deduped by stamp so 13 failed
# prompts cost one page). The transcript scan (runtime/synthetic_tail.rs)
# is the backstop for a hook that never fired.
set -uo pipefail

# Survive a caller env with no usable PATH (see worktree-write-protect.sh).
PATH="${PATH:+$PATH:}/usr/bin:/bin:/usr/sbin:/sbin"
export PATH

STATE_FILE=".fno/target-state.md"

INPUT=$(cat)

# The documented fields, one jq call; @tsv keeps spaces in the rendered
# error text alive through the read.
read -r ERROR SESSION_ID LAST_MSG < <(
    printf '%s\n' "$INPUT" | jq -r '[
        (.error // "unknown"),
        (.session_id // ""),
        ((.last_assistant_message // "") | gsub("[\\t\\n]"; " ") | .[0:160])
    ] | @tsv' 2>/dev/null || echo "unknown"
)
ERROR_DETAILS=$(printf '%s\n' "$INPUT" | jq -r '(.error_details // "") | gsub("[\\t\\n]"; " ") | .[0:200]' 2>/dev/null || true)

STATUS=$(grep '^status:' "$STATE_FILE" 2>/dev/null | awk '{print $2}')
# The breaker and its model-fallback flag are target-loop bookkeeping; a
# plain lead session never writes them.
IN_TARGET_LOOP=0
[[ -f "$STATE_FILE" && "$STATUS" == "IN_PROGRESS" ]] && IN_TARGET_LOOP=1

if [[ "$IN_TARGET_LOOP" == 1 ]]; then
    # Circuit breaker: increment consecutive_same_error
    # Normalize error into a signature for duplicate detection
    ERROR_SIG=$(printf '%s' "${ERROR}:${LAST_MSG}" | tr '[:upper:]' '[:lower:]')

    LAST_SIG=$(grep 'last_error_signature:' "$STATE_FILE" 2>/dev/null | head -1 | sed 's/.*last_error_signature: *//' | tr -d '"')

    # Ensure fields exist in state file (append if missing)
    if ! grep -q 'consecutive_same_error:' "$STATE_FILE" 2>/dev/null; then
        echo "consecutive_same_error: 0" >> "$STATE_FILE"
    fi
    if ! grep -q 'last_error_signature:' "$STATE_FILE" 2>/dev/null; then
        echo "last_error_signature: null" >> "$STATE_FILE"
    fi

    if [[ "$ERROR_SIG" == "$LAST_SIG" && -n "$LAST_SIG" && "$LAST_SIG" != "null" ]]; then
        # Same error - increment counter
        CURRENT=$(grep 'consecutive_same_error:' "$STATE_FILE" 2>/dev/null | head -1 | awk '{print $2}')
        CURRENT=${CURRENT:-0}
        NEW=$((CURRENT + 1))
        sed -i.bak "s/^consecutive_same_error:.*/consecutive_same_error: $NEW/" "$STATE_FILE" 2>/dev/null
        rm -f "${STATE_FILE}.bak"
    else
        # Different error - reset counter, update signature
        sed -i.bak "s/^last_error_signature:.*/last_error_signature: \"$ERROR_SIG\"/" "$STATE_FILE" 2>/dev/null
        sed -i.bak "s/^consecutive_same_error:.*/consecutive_same_error: 1/" "$STATE_FILE" 2>/dev/null
        rm -f "${STATE_FILE}.bak"
    fi

    # Model fallback: flag rate limits for the skill to handle. The documented
    # error enum carries no status_code; `overloaded` rides the same flag.
    if [[ "$ERROR" == "rate_limit" || "$ERROR" == "overloaded" ]]; then
        if grep -q 'model_fallback_needed:' "$STATE_FILE" 2>/dev/null; then
            sed -i.bak "s/^model_fallback_needed:.*/model_fallback_needed: true/" "$STATE_FILE" 2>/dev/null
        else
            echo "model_fallback_needed: true" >> "$STATE_FILE"
        fi
        rm -f "${STATE_FILE}.bak"
    fi
fi

# Log to events. REPO_ROOT-anchored (not bare cwd-relative), per the
# placement rule (ab-f063 Wave 2) - avoids the nested ~/.fno/.fno accident
# class when this hook fires with an unexpected cwd.
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || echo "$PWD")
PLUGIN_ROOT="${CLAUDE_PLUGIN_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
# shellcheck source=../scripts/lib/events.sh
source "$PLUGIN_ROOT/scripts/lib/events.sh" 2>/dev/null || true
TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EVENT_LINE=$(printf '{"ts":"%s","type":"stop_failure","source":"hook","data":{"error":"%s","error_details":"%s"}}' "$TS" "$ERROR" "$(printf '%s' "${ERROR_DETAILS:-}" | tr -d '"\\')")
if declare -F _append_bounded_event >/dev/null 2>&1; then
    _append_bounded_event target_stopfailure "$EVENT_LINE" "${REPO_ROOT}/.fno/events.jsonl" || true
else
    echo "target stop-failure: event helper unavailable; skipping stop_failure" >&2
fi

# ---------------------------------------------------------------------------
# The auth lane: a logout is the one failure a restart fixes. The stamp is
# the dedupe: StopFailure fires per failed turn, and 13 failed prompts must
# cost one page, not thirteen (30 minutes).
# ---------------------------------------------------------------------------
if [[ "$ERROR" == "authentication_failed" && -n "$SESSION_ID" ]]; then
    CACHE_DIR="${FNO_HOOK_CACHE_DIR:-}"
    if [[ -z "$CACHE_DIR" ]]; then
        if [[ -n "${FNO_CACHE_HOME:-}" ]]; then
            CACHE_DIR="${FNO_CACHE_HOME/#\~/$HOME}/hook-budget"
        elif [[ "$(uname -s)" == "Darwin" ]]; then
            CACHE_DIR="$HOME/Library/Caches/fno/hook-budget"
        else
            CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/fno/hook-budget"
        fi
    fi
    SAFE_ID=$(printf '%s' "$SESSION_ID" | tr -cd 'A-Za-z0-9-')
    STAMP="$CACHE_DIR/stopfailure-auth-$SAFE_ID.stamp"
    NOW=$(date +%s)
    MTIME=$(stat -c %Y "$STAMP" 2>/dev/null || stat -f %m "$STAMP" 2>/dev/null || printf 0)
    if (( NOW - MTIME >= 1800 )); then
        mkdir -p "$CACHE_DIR" 2>/dev/null || true
        : > "$STAMP"
        # shellcheck source=../scripts/lib/with-timeout.sh
        source "$PLUGIN_ROOT/scripts/lib/with-timeout.sh" 2>/dev/null || true
        FNO_BIN=""
        # shellcheck source=../hooks/lib/fno-bin.sh
        source "$PLUGIN_ROOT/hooks/lib/fno-bin.sh" 2>/dev/null || true
        declare -F fno_bin >/dev/null 2>&1 && FNO_BIN="$(fno_bin)"
        if [[ -n "$FNO_BIN" ]]; then
            # One line to the operator through the dispatch owner.
            with_timeout 15 "$FNO_BIN" inbox notify \
                "fno: session signed out" \
                "$SESSION_ID signed out: ${LAST_MSG:-authentication_failed}" \
                >/dev/null 2>&1 || true
            # The respawn ask, detached so the hook returns: the door runs
            # claude respawn and truth-confirms the landing, which outlives
            # a hook. A refused door (a pane worker with no respawn target)
            # is the honest answer; the watchdog backstop and the operator
            # page stand behind it.
            ( with_timeout 150 "$FNO_BIN" agent resume "$SESSION_ID" >/dev/null 2>&1 & ) 2>/dev/null
        fi
    fi
fi

exit 0
