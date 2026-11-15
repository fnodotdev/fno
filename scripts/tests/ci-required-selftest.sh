#!/usr/bin/env bash
# Selftest for scripts/ci/ci-required.sh --classify: each shape is a real head
# the aggregate must read right.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fails=0

expect() {
  local name="$1" want="$2" runs="$3" checks="$4"
  printf '%s' "$runs" >"$tmp/runs.json"
  printf '%s' "$checks" >"$tmp/checks.json"
  local got
  got="$(bash scripts/ci/ci-required.sh --classify "$tmp/runs.json" "$tmp/checks.json" | head -1 || true)"
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

[[ "$fails" -eq 0 ]]
