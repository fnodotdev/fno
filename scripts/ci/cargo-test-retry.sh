#!/usr/bin/env bash
# cargo-test-retry.sh CMD... - run a cargo test invocation once; on failure,
# re-run exactly the failed tests once. A one-off timing flake then reads
# green on main instead of holding every merge behind the main-red gate,
# and each flake a retry papered over is recorded where triage reads it:
# FLAKE-RETRY lines in the job log, plus $GITHUB_STEP_SUMMARY when set.
#
# Refuse arms, both deliberate:
# - No "test <name> ... FAILED" lines means the failure is deterministic
#   (a compile or build error); a retry would only spend runner minutes.
# - Arguments with no `--` libtest section cannot build a correct retry
#   command (the failed-name filters must land in the libtest arg list),
#   so the caller contract is named and the original code kept.
#
# A test that fails the retry too keeps that exit code: a real regression
# stays red. The 60s gap before the retry is the hook_latency measurement
# (crates/fno-agent/tests/hook_latency.rs RETRY_GAP_SECS): back-to-back
# passes land in one runner burst, a spaced pass does not.
set -u

RETRY_GAP_SECS=60

# Failed test names from libtest output, sorted and deduplicated.
retry_failed_names() {
    sed -n 's/^test \(.*\) \.\.\. FAILED$/\1/p' "$1" | sort -u
}

# Split the invocation at the first `--`: SPLIT_PRE carries cargo's own
# arguments, SPLIT_POST carries the libtest flags (for example
# --test-threads=1, --nocapture); positionals are dropped because the
# retry replaces them with the failed-name filters, and a caller-supplied
# --exact is dropped so exactly one survives. rc 2 names a caller that
# gave no libtest section at all.
SPLIT_PRE=()
SPLIT_POST=()
retry_split_args() {
    SPLIT_PRE=()
    SPLIT_POST=()
    local sep_seen=0 arg
    for arg in "$@"; do
        if [ "$sep_seen" -eq 0 ]; then
            if [ "$arg" = "--" ]; then
                sep_seen=1
            else
                SPLIT_PRE+=("$arg")
            fi
        elif [ "${arg#-}" != "$arg" ]; then
            [ "$arg" = "--exact" ] && continue
            SPLIT_POST+=("$arg")
        fi
    done
    if [ "$sep_seen" -eq 0 ]; then
        echo "cargo-test-retry: no '--' libtest section in the arguments; refusing to build a retry command" >&2
        return 2
    fi
    return 0
}

cargo_test_retry_main() {
    if [ $# -eq 0 ]; then
        echo "usage: cargo-test-retry.sh CMD..." >&2
        return 2
    fi
    retry_split_args "$@" || return $?
    local log rc rc2 name
    log="$(mktemp "${TMPDIR:-/tmp}/cargo-test-retry.XXXXXX")" || return 1
    echo "cargo-test-retry: run 1: $*"
    "$@" 2>&1 | tee "$log"
    rc=${PIPESTATUS[0]}
    local -a failed=()
    while IFS= read -r name; do
        [ -n "$name" ] && failed+=("$name")
    done < <(retry_failed_names "$log")
    rm -f "$log"
    if [ "${#failed[@]}" -eq 0 ]; then
        if [ "$rc" -eq 0 ]; then
            return 0
        fi
        echo "cargo-test-retry: no 'test <name> ... FAILED' lines; refusing to retry a failure that is not a test failure" >&2
        return "$rc"
    fi
    echo "cargo-test-retry: ${#failed[@]} test(s) failed; retrying once after a ${RETRY_GAP_SECS}s gap"
    sleep "$RETRY_GAP_SECS"
    local -a retry_cmd=("${SPLIT_PRE[@]}" "--" ${SPLIT_POST[@]+"${SPLIT_POST[@]}"} "--exact" ${failed[@]+"${failed[@]}"})
    echo "cargo-test-retry: run 2 (retry the failed tests only): ${retry_cmd[*]}"
    "${retry_cmd[@]}" 2>&1 | cat
    rc2=${PIPESTATUS[0]}
    if [ "$rc2" -eq 0 ]; then
        for name in ${failed[@]+"${failed[@]}"}; do
            echo "FLAKE-RETRY: $name passed on retry (first run failed)"
        done
        if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
            {
                echo "### Flakes retried green"
                for name in ${failed[@]+"${failed[@]}"}; do
                    echo "- \`$name\` failed once, passed on the single retry"
                done
            } >> "$GITHUB_STEP_SUMMARY"
        fi
        return 0
    fi
    echo "cargo-test-retry: the retry failed too; keeping the red" >&2
    return "$rc2"
}

# Source with CARGO_TEST_RETRY_LIB=1 to load the functions without running.
if [ "${CARGO_TEST_RETRY_LIB:-0}" != "1" ]; then
    cargo_test_retry_main "$@"
fi
