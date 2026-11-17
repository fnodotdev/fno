#!/usr/bin/env bash
# Selftest for scripts/ci/ci-required.sh --classify: each shape is a real head
# the aggregate must read right.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

tmp="$(mktemp -d)"
# The sustained-loop scenario kills its background pid, but a straggler
# grandchild can still hold a write into the dir when the EXIT trap runs,
# and the runner's ephemeral tmpdir is not worth a red job. Never fail the
# script from cleanup.
trap 'rm -rf "$tmp" 2>/dev/null || true' EXIT
fails=0

expect() {
  local name="$1" want="$2" runs="$3" checks="$4" labels="${5:-[]}"
  printf '%s' "$runs" >"$tmp/runs.json"
  printf '%s' "$checks" >"$tmp/checks.json"
  printf '%s' "$labels" >"$tmp/labels.json"
  local got
  got="$(bash scripts/ci/ci-required.sh --classify "$tmp/runs.json" "$tmp/checks.json" "$tmp/labels.json" | head -1 || true)"
  if [[ "$got" == "$want" ]]; then
    echo "ok   $name: $got"
  else
    echo "FAIL $name: want $want, got $got"
    fails=$((fails + 1))
  fi
}

# PR 255 at fb6cd77e: guards and stacked-base-guard each had a run cancelled
# by its concurrency group 3s before a fresh run started. Superseded history.
expect "superseded cancel" pass \
  '[{"id":10,"workflow_id":1,"name":"guards","status":"completed","conclusion":"cancelled","check_suite_id":100},
    {"id":20,"workflow_id":1,"name":"guards","status":"completed","conclusion":"success","check_suite_id":200},
    {"id":30,"workflow_id":2,"name":"ci-required","status":"in_progress","conclusion":null,"check_suite_id":300}]' \
  '[{"id":1,"name":"plugin-hooks","status":"completed","conclusion":"cancelled","check_suite":{"id":100}},
    {"id":2,"name":"plugin-hooks","status":"completed","conclusion":"success","check_suite":{"id":200}},
    {"id":3,"name":"ci-required","status":"in_progress","conclusion":null,"check_suite":{"id":300}}]'

# PR 251: clippy failed and its siblings were cancelled in the latest run.
expect "failure with cancelled siblings" fail \
  '[{"id":20,"workflow_id":1,"name":"rust-ci","status":"completed","conclusion":"failure","check_suite_id":200}]' \
  '[{"id":1,"name":"cargo clippy","status":"completed","conclusion":"failure","check_suite":{"id":200}},
    {"id":2,"name":"cargo test","status":"completed","conclusion":"cancelled","check_suite":{"id":200}}]'

# A tier-keeper cancel of the only run: no result yet, so wait for a rerun.
expect "latest run cancelled" wait \
  '[{"id":20,"workflow_id":1,"name":"cli-ci","status":"completed","conclusion":"cancelled","check_suite_id":200}]' \
  '[{"id":1,"name":"smoke","status":"completed","conclusion":"cancelled","check_suite":{"id":200}}]'

# A rerun attempt keeps the run id and suite and adds a newer check run.
expect "rerun replaces a failed attempt" pass \
  '[{"id":20,"workflow_id":1,"name":"cli-ci","status":"completed","conclusion":"success","check_suite_id":200}]' \
  '[{"id":1,"name":"smoke","status":"completed","conclusion":"failure","check_suite":{"id":200}},
    {"id":5,"name":"smoke","status":"completed","conclusion":"success","check_suite":{"id":200}}]'

# A job still running keeps the aggregate waiting.
expect "check still running" wait \
  '[{"id":20,"workflow_id":1,"name":"cli-ci","status":"in_progress","conclusion":null,"check_suite_id":200}]' \
  '[{"id":1,"name":"smoke","status":"in_progress","conclusion":null,"check_suite":{"id":200}}]'

# A live pr-hold decision holds by label, whatever CI did. The specimens
# (D3376, D3379) merged because no door read the hold; this is the CI-side
# half of the stop.
expect "hold label fails all-green ci" fail \
  '[{"id":20,"workflow_id":1,"name":"cli-ci","status":"completed","conclusion":"success","check_suite_id":200}]' \
  '[{"id":1,"name":"smoke","status":"completed","conclusion":"success","check_suite":{"id":200}}]' \
  '["pr-hold"]'

# No label and a green head keeps the old verdicts.
expect "no hold label keeps the pass" pass \
  '[{"id":20,"workflow_id":1,"name":"cli-ci","status":"completed","conclusion":"success","check_suite_id":200}]' \
  '[{"id":1,"name":"smoke","status":"completed","conclusion":"success","check_suite":{"id":200}}]' \
  '["ci","docs"]'

# --- poll-loop scenarios through a gh stub --------------------------------
# The live loop reads GitHub until the head settles, and a flaky gh must
# never turn into a red check (the 2026-11-16 flake: one (HTTP 502) read
# failed the whole job). The stub fails its first N calls with a GitHub
# error, then serves the green fixture below, applying the --jq program
# with real jq. State and sleeps ride env knobs so the scenarios are fast
# and deterministic.
STUBDIR="$(mktemp -d)"
cat >"$STUBDIR/gh" <<'STUB'
#!/usr/bin/env bash
state="${STUB_STATE:?}"
n=$(cat "$state" 2>/dev/null || echo 0); n=$((n + 1)); printf '%s' "$n" >"$state"
if [ "${FAIL_MODE:-}" = "403" ]; then
  printf 'gh: Resource not accessible by integration (HTTP 403)\n' >&2
  exit 1
fi
if [ "$n" -le "${FAIL_FIRST:-0}" ]; then
  printf 'gh: Server Error (HTTP 502)\n' >&2
  exit 1
fi
url=""
prog="."
while [ $# -gt 0 ]; do
  case "$1" in
    api|--paginate) shift ;;
    --jq) prog="$2"; shift 2 ;;
    *) if [ -z "$url" ]; then url="$1"; fi; shift ;;
  esac
done
case "$url" in
  *actions/runs*) exec jq "$prog" "$FIXTURE_DIR/runs.json" ;;
  *check-runs*) exec jq "$prog" "$FIXTURE_DIR/checks.json" ;;
  *) exit 0 ;;
esac
STUB
chmod +x "$STUBDIR/gh"
printf '%s' '{"total_count":1,"workflow_runs":[{"id":20,"workflow_id":1,"name":"rust-ci","status":"completed","conclusion":"success","check_suite_id":200}]}' >"$STUBDIR/runs.json"
printf '%s' '{"total_count":1,"check_runs":[{"id":1,"name":"cargo clippy","status":"completed","conclusion":"success","check_suite":{"id":200}}]}' >"$STUBDIR/checks.json"
export FIXTURE_DIR="$STUBDIR"

loop_expect() {
  local name="$1" want_rc="$2" fail_first="$3" mode="${4:-}"
  local state="$STUBDIR/state-$name" log="$STUBDIR/log-$name" got_rc=0
  : >"$state"
  PATH="$STUBDIR:$PATH" STUB_STATE="$state" FAIL_FIRST="$fail_first" FAIL_MODE="$mode" \
    CI_REQUIRED_POLL_S=0 CI_REQUIRED_SETTLE_READS=1 CI_REQUIRED_RETRY_ATTEMPTS=3 CI_REQUIRED_RETRY_BACKOFF_S=0 \
    bash scripts/ci/ci-required.sh acme/widget deadbeef999 12345 >"$log" 2>&1 || got_rc=$?
  if [[ "$got_rc" -eq "$want_rc" ]]; then
    echo "ok   loop $name: exit $got_rc"
  else
    echo "FAIL loop $name: want exit $want_rc, got $got_rc; log: $(tail -2 "$log")"
    fails=$((fails + 1))
  fi
}

# Two 502 reads then clean: the flake that failed PR heads before the retry
# helper. The script must recover and never print a failure verdict.
loop_expect "recovers after 502s" 0 2
if grep -q "FAILED" "$STUBDIR/log-recovers after 502s"; then
  echo "FAIL loop recovers after 502s: log names FAILED"
  fails=$((fails + 1))
fi

# A green head through clean reads still passes with its rows counted.
loop_expect "clean head passes" 0 0
grep -q "2 rows green or skipped" "$STUBDIR/log-clean head passes" || {
  echo "FAIL loop clean head passes: rows line missing"
  fails=$((fails + 1))
}

# A named 403 is GitHub refusing, not failing: fail fast with the refusal.
loop_expect "403 refusal fails fast" 1 0 403
grep -q "(HTTP 403)" "$STUBDIR/log-403 refusal fails fast" || {
  echo "FAIL loop 403 refusal fails fast: refusal not shown"
  fails=$((fails + 1))
}

# A sustained 5xx is an absent result, never a verdict: the script keeps
# waiting (alive at the bound, log names the read failure, never self-exited).
sust_state="$STUBDIR/state-sustained" sustained_log="$STUBDIR/log-sustained"
: >"$sust_state"
PATH="$STUBDIR:$PATH" STUB_STATE="$sust_state" FAIL_FIRST=1000 \
  CI_REQUIRED_POLL_S=0 CI_REQUIRED_SETTLE_READS=1 CI_REQUIRED_RETRY_ATTEMPTS=3 CI_REQUIRED_RETRY_BACKOFF_S=0 \
  bash scripts/ci/ci-required.sh acme/widget deadbeef999 12345 >"$sustained_log" 2>&1 &
sust_pid=$!
sleep 2
sust_alive=1
kill -0 "$sust_pid" 2>/dev/null || sust_alive=0
kill "$sust_pid" 2>/dev/null || true
wait "$sust_pid" 2>/dev/null || true
if [[ "$sust_alive" -eq 1 ]] && grep -q "read failed" "$sustained_log"; then
  echo "ok   loop sustained 5xx waits: alive at the 2s bound"
else
  echo "FAIL loop sustained 5xx waits: alive=$sust_alive, log: $(tail -2 "$sustained_log")"
  fails=$((fails + 1))
fi

rm -rf "$STUBDIR" 2>/dev/null || true

[[ "$fails" -eq 0 ]]
