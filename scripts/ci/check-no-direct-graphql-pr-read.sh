#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

matches=$(python3 scripts/ci/graphql-caller-inventory.py)

classified=$(printf '%s\n' "$matches" | awk 'NF {n++} END {print n+0}')
unclassified=$(printf '%s\n' "$matches" | awk -F'|' '$1 == "unclassified" {n++} END {print n+0}')
digest=$(printf '%s\n' "$matches" | LC_ALL=C sort | shasum -a 256 | awk '{print $1}')
# Re-pinned 2026-11-07 (guard diet, law d-9d4e95e5): the hook's GraphQL
# reserve gate is deleted, so its caller rows leave the inventory; the
# remaining rows are documentation and the enforcement-boundary checks
# below still hold.
# Re-pinned 2026-11-20: six scripts/ops rows classify
# operator-one-shot, one loopcheck argv row left when the commits read moved
# to the REST commits endpoint, and the proxy boundary pin migrated to
# run_proxy_main.
expected_digest="dd58a466f573d0ed4c2e29b6c6e0b9ee305399b9baeb8cc846a2e3ea2f7b0b70"

if [[ "${1:-}" == "--print-digest" ]]; then
  printf 'classified_graphql_callers=%s digest=%s\n' "$classified" "$digest"
  exit 0
fi

if [[ "$classified" -eq 0 ]]; then
  echo "direct-graphql-pr-read: classification instrument matched zero callers" >&2
  exit 1
fi
if [[ "$unclassified" -ne 0 ]]; then
  printf '%s\n' "$matches" | awk -F'|' '$1 == "unclassified"' >&2
  echo "direct-graphql-pr-read: executable callers lack a routing disposition" >&2
  exit 1
fi
if ! printf '%s\n' "$matches" | grep -q '^agents-rest-delegate|argv-pr|crates/fno-agent/src/gate/pr_status/proxy.rs|'; then
  echo "direct-graphql-pr-read: argv inventory missed the pr_status proxy op door" >&2
  exit 1
fi
# Pin the BOUNDARY, never the line that happens to express it today. Both of
# the first two pins broke on ordinary edits that changed nothing about the
# boundary: a Python proxy call changed its argument spelling,
# and the `gh_bin` default was wrapped across two lines. A whole-line grep then
# reports "a named enforcement boundary is missing" for a boundary that is
# present and working, which is a false red nobody can act on. Match the
# assignment and the callee, or the binary name, and let formatting move.
# The proxy boundary migrated with the pr port: the front door's old
# protect() call in cli/bootstrap.rs is gone; today plain gh reaches the
# broker through the shim chain (setup wrapper() execs `<fno> gh`, the gh
# door routes it into run_proxy_main), so the pin is the proxy's own entry.
if ! grep -q 'pub fn run_proxy_main' crates/fno-agent/src/gate/pr_status/proxy.rs \
    || ! grep -q '"fno-gh-coverage"' crates/fno-agent/src/gate/finalize.rs \
    || ! grep -q '"fno-gh-loopcheck"' crates/fno-agent/src/gate/loopcheck/args.rs; then
  echo "direct-graphql-pr-read: a named enforcement boundary is missing" >&2
  exit 1
fi
if [[ "$digest" != "$expected_digest" ]]; then
  echo "direct-graphql-pr-read: caller inventory changed; classify the new or removed path" >&2
  echo "classified_graphql_callers=$classified unclassified=$unclassified digest=$digest" >&2
  exit 1
fi

echo "classified_graphql_callers=$classified unclassified=0"
