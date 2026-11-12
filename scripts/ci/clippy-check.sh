#!/usr/bin/env bash
# clippy-check.sh - the crates/ clippy gate (rust-ci.yml `clippy` job).
#
# Nine surfaces, one mechanism:
#   clippy::correctness        any new hit fails (denied)
#   clippy::invalid_regex      ratcheted: an untouched flagged line passes,
#   clippy::indexing_slicing   an edited flagged line fails
#   clippy::disallowed_methods same; bare Command::output via clippy.toml
# Four more are ratcheted on product code only (a second `--lib --bins` pass,
# so #[cfg(test)] code and tests/ never count; rust-style.md says tests may
# unwrap):
#   clippy::unwrap_used                R8   no unwrap in product code
#   clippy::let_underscore_must_use    R9   never drop an error in silence
#   clippy::undocumented_unsafe_blocks R14  no unsafe without a SAFETY comment
#   clippy::too_many_lines             R15  split a long function (clippy's
#                                           default threshold, 100 lines)
#
# A hit is keyed `<crate>/<path><TAB><lint><TAB>trimmed primary-line text`.
# Because the key carries the line's text, editing a flagged line produces a
# NEW key and fails, so deny-on-changed-lines needs no merge-base diff and
# holds on push and nightly runs too.
#
# Existing hits live in tests/ci/clippy-baseline.txt. The file sits under
# tests/ because it is generated fixture data: a projection of raw source
# lines, so it can carry token shapes (node-id-like strings) the tree gates
# must not read as shipped breadcrumbs. Fixing a line turns
# its entry stale (harmless, reported as a ratchet-down); re-ratchet with
# --write-baseline after a fix or a toolchain bump. CLIPPY_TOOLCHAIN in
# rust-ci.yml names the toolchain that wrote the baseline; bump them
# together, never apart (same contract as the fmt job's RUSTFMT_TOOLCHAIN).
#
# Platform: CI runs on Linux. A baseline written on macOS misses cfg(linux)
# code (and --write-baseline there drops the Linux-only rows as stale), so
# union a macOS write with the rows the Linux job reports, never replace.
#
# Known limit: keys are unique lines, so a new flagged line whose trimmed text
# equals an already-baselined line in the same file passes (common for
# `let _ = tx.send(..)` and `.lock().unwrap()`). The gate sees the first
# occurrence only; a duplicate is review's to catch.
#
# Escape hatch: a targeted `#[allow(clippy::indexing_slicing)]` silences a
# site for good; the diagnostic and its key disappear together.
#
# Usage:
#   bash scripts/ci/clippy-check.sh [--baseline FILE] [--write-baseline] CRATE_DIR...
#   bash scripts/ci/clippy-check.sh --extract <clippy.jsonl> <crate>     (print keys)
#   bash scripts/ci/clippy-check.sh --hits-file <keys.tsv> [--baseline FILE] [--write-baseline] CRATE_DIR...
#
# `--extract` and `--hits-file` exist for tests/ci/test_clippy_check.sh: the
# parser and the verdict run without a cargo build.

set -euo pipefail
export LC_ALL=C

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

BASELINE="tests/ci/clippy-baseline.txt"
WRITE=0
EXTRACT_SRC=""
EXTRACT_CRATE=""
HITS_FILE=""
CRATES=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --baseline) BASELINE="$2"; shift 2 ;;
        --write-baseline) WRITE=1; shift ;;
        --extract) EXTRACT_SRC="$2"; EXTRACT_CRATE="$3"; shift 3 ;;
        --hits-file) HITS_FILE="$2"; shift 2 ;;
        -h|--help) sed -n '2,50p' "$0"; exit 0 ;;
        *) CRATES+=("$1"); shift ;;
    esac
done

LINT_FLAGS=(-W clippy::correctness -W clippy::invalid_regex -W clippy::indexing_slicing -W clippy::disallowed_methods)
PRODUCT_LINT_FLAGS=(-W clippy::unwrap_used -W clippy::let_underscore_must_use -W clippy::undocumented_unsafe_blocks -W clippy::too_many_lines)
# The one list extract_keys filters on: every lint either pass can emit.
RATCHET_LINTS='["clippy::correctness", "clippy::invalid_regex", "clippy::indexing_slicing", "clippy::disallowed_methods", "clippy::unwrap_used", "clippy::let_underscore_must_use", "clippy::undocumented_unsafe_blocks", "clippy::too_many_lines"]'

# One clippy JSON stream -> sorted unique TSV hit keys for one crate. Paths
# outside the crate (macro-expanded registry sources, /rustc shims) are not
# ours to ratchet; deps are cap-linted anyway. Non-clippy diagnostics are
# rustc's own and stay with the `warnings` job.
extract_keys() {
    local jsonl="$1" crate="$2"
    jq -r --arg crate "$crate" --argjson lints "$RATCHET_LINTS" '
        select(.reason == "compiler-message")
        | select(.message.code.code as $c | $lints | index($c))
        | (.message.spans // [] | map(select(.is_primary == true)) | .[0] // .message.spans[0]) as $s
        | select($s != null)
        | select(($s.file_name | startswith("/") or startswith("..")) | not)
        | [$crate + "/" + $s.file_name, .message.code.code,
           (($s.text[0].text // "") | gsub("^[ \t]+|[ \t]+$"; ""))]
        | @tsv
    ' "$jsonl" | sort -u
}

if [[ -n "$EXTRACT_SRC" ]]; then
    extract_keys "$EXTRACT_SRC" "$EXTRACT_CRATE"
    exit 0
fi

if [[ ${#CRATES[@]} -eq 0 ]]; then
    echo "usage: clippy-check.sh [--baseline FILE] [--write-baseline] CRATE_DIR..." >&2
    exit 2
fi

CURRENT="$(mktemp)"
baseline="$(mktemp)"
trap 'rm -f "$CURRENT" "$baseline"' EXIT

if [[ -n "$HITS_FILE" ]]; then
    # comm below requires sorted input: an unsorted hits file can hide a new
    # hit from the merge and pass a gate that should fail.
    sort -u -o "$CURRENT" "$HITS_FILE"
else
    for crate in "${CRATES[@]}"; do
        if [[ ! -d "$REPO_ROOT/$crate" ]]; then
            echo "clippy-check: no such crate dir: $crate" >&2
            exit 2
        fi
        local_tmp="$(mktemp)"
        err_tmp="$(mktemp)"
        if ! ( cd "$REPO_ROOT/$crate" && cargo clippy --all-targets --locked --message-format=json -- "${LINT_FLAGS[@]}" ) >"$local_tmp" 2>"$err_tmp"; then
            # A compile error is not a lint verdict: fail loudly with the
            # readable stderr, never a silent pass.
            echo "clippy-check: cargo clippy failed in $crate; stderr tail:" >&2
            tail -30 "$err_tmp" >&2
            rm -f "$local_tmp" "$err_tmp"
            exit 1
        fi
        extract_keys "$local_tmp" "$crate" >>"$CURRENT"
        # Product-only pass: --lib --bins leaves #[cfg(test)] and tests/ out.
        # A crate with neither target (cargo exits non-zero) has no product
        # code to ratchet.
        if ( cd "$REPO_ROOT/$crate" && cargo clippy --lib --bins --locked --message-format=json -- "${PRODUCT_LINT_FLAGS[@]}" ) >"$local_tmp" 2>"$err_tmp"; then
            extract_keys "$local_tmp" "$crate" >>"$CURRENT"
        elif ! grep -q "no library targets found" "$err_tmp"; then
            echo "clippy-check: cargo clippy (product pass) failed in $crate; stderr tail:" >&2
            tail -30 "$err_tmp" >&2
            rm -f "$local_tmp" "$err_tmp"
            exit 1
        fi
        rm -f "$local_tmp" "$err_tmp"
    done
    sort -u -o "$CURRENT" "$CURRENT"
fi

if [[ $WRITE -eq 1 ]]; then
    {
        echo "# clippy ratchet baseline: one <crate>/<path><TAB><lint><TAB>line text> per line."
        echo "# Written by scripts/ci/clippy-check.sh --write-baseline; regen after fixing"
        echo "# flagged lines or bumping CLIPPY_TOOLCHAIN in .github/workflows/rust-ci.yml."
    } >"$BASELINE"
    cat "$CURRENT" >>"$BASELINE"
    echo "clippy-check: wrote $(grep -c -v '^#' "$BASELINE") baseline entries to $BASELINE"
    exit 0
fi

if [[ ! -s "$BASELINE" ]]; then
    echo "clippy-check: baseline missing or empty: $BASELINE; write it first:" >&2
    echo "  bash scripts/ci/clippy-check.sh --write-baseline crates/fno-agents crates/fno" >&2
    exit 2
fi

# Baseline rows for the crates this invocation owns; other rows (none today)
# belong to another invocation and must not mask its misses.
crate_prefixes="$(printf '%s/\n' "${CRATES[@]}")"
awk -F'\t' -v prefixes="$crate_prefixes" '
    BEGIN { n = split(prefixes, p, "\n") }
    /^#/ || /^[[:space:]]*$/ { next }
    {
        for (i = 1; i <= n; i++) {
            if (p[i] != "" && index($1, p[i]) == 1) { print; next }
        }
    }
' "$BASELINE" >"$baseline" || true
sort -o "$baseline" "$baseline"

new="$(comm -13 "$baseline" "$CURRENT" || true)"
stale="$(comm -23 "$baseline" "$CURRENT" || true)"

if [[ -n "$new" ]]; then
    echo "clippy-check: FAILED - new clippy hits (not in $BASELINE):" >&2
    while IFS= read -r key; do
        echo "  $key" >&2
    done <<<"$new"
    echo "The rendered diagnostics are in the clippy job log above. Locally:" >&2
    echo "  cargo clippy --all-targets -- -W clippy::correctness -W clippy::invalid_regex -W clippy::indexing_slicing -W clippy::disallowed_methods" >&2
    echo "  cargo clippy --lib --bins -- -W clippy::unwrap_used -W clippy::let_underscore_must_use -W clippy::undocumented_unsafe_blocks -W clippy::too_many_lines" >&2
    exit 1
fi

total="$(wc -l <"$CURRENT" | tr -d ' ')"
echo "clippy-check: OK - $total ratcheted hits, no new hits"
if [[ -n "$stale" ]]; then
    echo "clippy-check: $(printf '%s\n' "$stale" | grep -c .) stale baseline entries; ratchet down with --write-baseline" >&2
fi
