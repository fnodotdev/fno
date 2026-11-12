#!/usr/bin/env bash
# A spawn-gate refusal must leave a machine-readable trace.
#
# An earlier Python-leg guard of this name held the gate's one _refuse()
# seam; the Python leg and the guard died together. The Rust gate carried
# the same silent shape, and this guard re-opens the contract against the
# Rust gate: run_gate already emitted friction rows +
# agent_spawn_refused + a machine-sample stamp, but no spawn_gate_refused
# row - the kind the feed's dedicated handler keys on. The state-root
# pre-gate (bin/client.rs) returned its exit code with no trace at all.
#
# The fix is one emitter, emit_refusal_event, called at the two refusal
# crossings. A convention nothing checks is a convention that decays.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
GATE="crates/fno-agents/src/spawn/spawn_gate.rs"
CLIENT="crates/fno-agents/src/bin/client.rs"

fail=0
note() {
  if [[ $fail -eq 0 ]]; then
    echo "check-gate-refusals-emit: FAIL - a gate refusal can exit without a trace." >&2
    fail=1
  fi
}

# 1. One emitter per door. Readers (feed, fleet_load, schema) and tests are
#    not emitters; lib.rs carries the hand-maintained KNOWN_EVENT_KINDS
#    registry, definitionally non-emitting; route_slot.rs is the routing
#    door's own guarded emitter (gate:"routing"), separate by contract. The
#    spawn gate keeps ONE.
sites=$(git grep -n '"spawn_gate_refused"' -- crates/ | grep -v 'feed\.rs\|fleet_load\.rs\|events_schema\.json\|tests/\|fixtures/\|_tests\.rs\|lib\.rs\|assert' || true)
count=$(printf '%s\n' "$sites" | grep -c '"spawn_gate_refused"' || true)
if [[ $count -ne 2 ]]; then
  print_sites() { printf '%s\n' "$sites" >&2; }
  note
  echo "  Expected exactly two emitter sites: the spawn gate seam (spawn_gate.rs" >&2
  echo "  emit_refusal_event) and the routing door (route_slot.rs). Found $count:" >&2
  print_sites
  echo "  Fix: emit through spawn_gate::emit_refusal_event instead." >&2
fi

# 2. run_gate crosses the seam.
if ! grep -q 'emit_refusal_event(&r, &name, &substrate, node.as_deref())' "$REPO_ROOT/$GATE"; then
  note
  echo "  run_gate's map_err must call emit_refusal_event (spawn_gate.rs)." >&2
fi

# 3. The state-root pre-gate door crosses the seam.
if ! grep -A 10 'state_root_grant_gate(provider, declared_substrate, &roots)' "$CLIENT" | grep -q 'emit_refusal_event'; then
  note
  echo "  bin/client.rs's state_root_grant_gate call must cross emit_refusal_event" >&2
  echo "  before returning the refusal exit code." >&2
  exit 1
fi

if [[ $fail -ne 0 ]]; then
  echo "" >&2
  echo "  One emitter keeps the refusal trace uniform. See spawn_gate.rs" >&2
  echo "  emit_refusal_event's doc comment." >&2
  exit 1
fi
echo "check-gate-refusals-emit: ok"
