#!/usr/bin/env bash
# Self-test for scripts/ci/cargo-test-retry.sh: the failed-name parse, the
# `--` split the retry rebuild relies on, and the wrapper's two arms plus
# both refuse arms, driven through the real entry point with a stub
# command. No cargo needed, so it runs locally and as a CI step.
set -u

here="$(cd "$(dirname "$0")" && pwd)"
wrapper="$here/../../scripts/ci/cargo-test-retry.sh"
if [ ! -f "$wrapper" ]; then
    echo "self-test: wrapper missing at $wrapper" >&2
    exit 1
fi

fails=0
check() {
    if [ "$2" = "$3" ]; then
        echo "ok: $1"
    else
        echo "FAIL: $1 (expected [$2], got [$3])"
        fails=$((fails + 1))
    fi
}

CARGO_TEST_RETRY_LIB=1 . "$wrapper"
RETRY_GAP_SECS=0

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# The parse can match its own target before it is trusted on real output.
printf 'test a ... FAILED\ntest b ... ok\ntest a ... FAILED\ntest result: FAILED. 1 passed; 1 failed\n' > "$tmp/log"
check "positive control: fixture carries FAILED lines" 2 "$(grep -c '\.\.\. FAILED$' "$tmp/log")"
check "parse extracts the deduplicated name" "a" "$(retry_failed_names "$tmp/log")"

# Split: the shard shape keeps cargo args, keeps libtest flags, and drops
# the old names plus the caller's --exact, so exactly one survives.
retry_split_args cargo test --test '*' --no-fail-fast -- --test-threads=1 --exact n1 n2 n3
check "split rc (shard shape)" 0 "$?"
check "pre kept verbatim" "cargo test --test * --no-fail-fast" "${SPLIT_PRE[*]}"
check "post keeps flags, drops --exact and names" "--test-threads=1" "${SPLIT_POST[*]}"

# Split: the mux shape (flags only after --) and an empty post section both
# assemble a retry command without tripping set -u on the empty array.
retry_split_args cargo test --test x -- --test-threads=1
check "split rc (mux shape)" 0 "$?"
check "mux pre kept" "cargo test --test x" "${SPLIT_PRE[*]}"
retry_split_args cargo test --test x --
check "split rc (empty post)" 0 "$?"
retry_cmd=("${SPLIT_PRE[@]}" "--" ${SPLIT_POST[@]+"${SPLIT_POST[@]}"} "--exact" a)
check "empty post assembles" "cargo test --test x -- --exact a" "${retry_cmd[*]}"

# Refuse arm: no libtest section, no correct retry command.
retry_split_args cargo test --test x --test-threads=1
check "split rc (no -- section)" 2 "$?"

cat > "$tmp/stub.sh" <<'EOF'
count_file="$1"; shift
mode="$1"; shift
n="$(cat "$count_file" 2>/dev/null || echo 0)"
n=$((n + 1))
echo "$n" > "$count_file"
case "$mode" in
  green_retry)
    if [ "$n" -eq 1 ]; then
      echo "test flaky_one ... FAILED"
      echo "test steady ... ok"
      echo "test result: FAILED. 1 passed; 1 failed"
      exit 1
    fi
    echo "test flaky_one ... ok"
    echo "test result: ok"
    exit 0
    ;;
  still_red)
    echo "test flaky_two ... FAILED"
    echo "test result: FAILED. 0 passed; 1 failed"
    exit 1
    ;;
  compile_error)
    echo "error: could not compile \`stub\` (lib test)"
    exit 101
    ;;
esac
exit 1
EOF

# Admit arm: a one-off flake passes on the retry and is recorded.
echo 0 > "$tmp/c1"
out="$(cargo_test_retry_main bash "$tmp/stub.sh" "$tmp/c1" green_retry --test stub -- --test-threads=1 2>&1)"; rc=$?
check "green retry exits 0" 0 "$rc"
check "green retry ran twice" 2 "$(cat "$tmp/c1")"
check "flake line names the test" 1 "$(printf '%s\n' "$out" | grep -c '^FLAKE-RETRY: flaky_one passed on retry')"
check "retry command carries the --exact filter" 1 "$(printf '%s\n' "$out" | grep -c 'run 2 .* --exact flaky_one')"

# A real regression: the retry fails too, so the red stays.
echo 0 > "$tmp/c2"
out="$(cargo_test_retry_main bash "$tmp/stub.sh" "$tmp/c2" still_red --test stub -- --test-threads=1 2>&1)"; rc=$?
check "still-red retry exits nonzero" 1 "$rc"
check "still-red ran twice" 2 "$(cat "$tmp/c2")"
check "still-red prints no flake line" 0 "$(printf '%s\n' "$out" | grep -c 'FLAKE-RETRY')"

# Refuse arm: a compile failure is deterministic and buys no retry.
echo 0 > "$tmp/c3"
out="$(cargo_test_retry_main bash "$tmp/stub.sh" "$tmp/c3" compile_error --test stub -- --test-threads=1 2>&1)"; rc=$?
check "compile error exits with the original code" 101 "$rc"
check "compile error ran once (no retry)" 1 "$(cat "$tmp/c3")"
check "compile error names the refuse arm" 1 "$(printf '%s\n' "$out" | grep -c 'refusing to retry a failure that is not a test failure')"

# Refuse arm: no libtest section refuses before the first run.
echo 0 > "$tmp/c4"
out="$(cargo_test_retry_main bash "$tmp/stub.sh" "$tmp/c4" anymode --test-threads=1 2>&1)"; rc=$?
check "missing -- section exits 2" 2 "$rc"
check "missing -- section ran nothing" 0 "$(cat "$tmp/c4")"
check "missing -- section names the contract" 1 "$(printf '%s\n' "$out" | grep -c "no '--' libtest section")"

if [ "$fails" -eq 0 ]; then
    echo "self-test: all green"
    exit 0
fi
echo "self-test: $fails check(s) failed" >&2
exit 1
