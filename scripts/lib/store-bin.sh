#!/usr/bin/env bash
# Resolve the fno store binary for test reads: FNO_BIN first, then the
# checkout build (debug, then release), then PATH - the same order the append
# in scripts/lib/events.sh makes. A smoke runner has no PATH fno (the checkout
# build is deliberately off PATH), so a reader that trusts PATH alone sees an
# empty store on CI. One sourced copy: the per-test copies this replaces
# drifted once, and that drift was a red main run.
# Sets ROWS_BIN, the name every caller reads.

_store_bin_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)" || return 1
ROWS_BIN="${FNO_BIN:-}"
if [[ -z "$ROWS_BIN" ]]; then
    for _profile in debug release; do
        [[ -x "$_store_bin_root/crates/fno/target/$_profile/fno" ]] && { ROWS_BIN="$_store_bin_root/crates/fno/target/$_profile/fno"; break; }
    done
fi
[[ -n "$ROWS_BIN" ]] || ROWS_BIN=$(command -v fno 2>/dev/null)
unset _store_bin_root _profile
