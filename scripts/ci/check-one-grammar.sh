#!/usr/bin/env bash
# check-one-grammar.sh - the search grammar exists once.
#
# The mux client and the fno-agent feed reader evaluate one query grammar,
# held in crates/fno-proto/src/feed/grammar.rs. A second copy would let a
# key mean two things. The gate counts what only a grammar copy carries, the
# key table and the tokenizer, across every crate's src. Each must appear
# exactly once, in that file. A pattern that finds nothing is a broken gate,
# not a pass, so a count of zero fails too.
#
# Run: bash scripts/ci/check-one-grammar.sh
# Exit: 0 one copy, 1 zero or several.
set -euo pipefail
cd "$(dirname "$0")/../.."

home="crates/fno-proto/src/feed/grammar.rs"
fail=0
for pattern in '^pub static KEYS: &\[KeyDef\]' '^fn tokenize\(input: &str\)'; do
    hits="$(grep -rlE --include='*.rs' "$pattern" crates/*/src || true)"
    count="$(printf '%s' "$hits" | grep -c . || true)"
    if [[ "$count" -ne 1 || "$hits" != "$home" ]]; then
        echo "check-one-grammar: '$pattern' found in $count file(s), want 1 in $home:" >&2
        printf '  %s\n' $hits >&2
        fail=1
    fi
done
[[ "$fail" -eq 0 ]] && echo "check-one-grammar: ok (one grammar, in $home)"
exit "$fail"
