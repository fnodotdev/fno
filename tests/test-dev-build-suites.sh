#!/usr/bin/env bash
# Runs every tests/py test that takes the native_backlog_door fixture or the
# dev_build marker with this checkout's fno-agents build present. The smoke
# pytest legs delete crates/fno-agents/target/debug/fno-agents and
# target/release/fno-agents, so those tests skip there. Discovery runs this
# file after the build step.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# The conftest skips these tests when no dev build exists. Check the same
# paths first, so a missing build fails here instead of passing as skips.
if [ ! -x crates/fno-agents/target/debug/fno-agents ] \
   && [ ! -x crates/fno-agents/target/release/fno-agents ]; then
  echo "test-dev-build-suites: no fno-agents dev build; run: cargo build --manifest-path crates/fno-agents/Cargo.toml" >&2
  exit 1
fi

# Two door tests journal through the native event store, whose client resolves
# FNO_BIN or the checkout's own crates/fno build. The team fixture pins PATH to
# a fake bin, so a PATH fno cannot answer at test time.
if [ -z "${FNO_BIN:-}" ] && [ ! -x crates/fno/target/debug/fno ] \
   && [ ! -x crates/fno/target/release/fno ]; then
  echo "test-dev-build-suites: no fno front-door build; run: cargo build --manifest-path crates/fno/Cargo.toml" >&2
  exit 1
fi

# Select by the fixture name or the dev_build marker, so a new file that
# takes either joins with no edit.
# grep exits 1 on no candidate and pytest exits 5 on an empty -m selection;
# both fail this harness.
files=$(grep -rl --include='test_*.py' -e native_backlog_door -e 'mark\.dev_build' tests/py)
# shellcheck disable=SC2086  # repo test paths carry no spaces
uv run --no-project --with pytest --with pytest-timeout --with pyyaml --with psutil \
  pytest --tb=short -q -m dev_build $files
