#!/usr/bin/env bash
# check-value-reads.sh - string-key JSON reads and string errors are
# shrink-only, per crate.
#
# The Rust crates began as ports of Python that kept JSON shapes, so rows
# travel as serde_json::Value and readers pick fields by string key. A
# misspelled key compiles and reads None. docs/rust-style.md holds the rules
# this gate enforces:
#   R1  parse at the boundary into a #[derive(Deserialize)] struct; no code
#       past it reads .get("field") on serde_json::Value.
#   R6  an expected failure returns a typed error enum; no new public
#       Result<_, String>.
#
# This is a ratchet, not a ceiling, the shape of check-file-budget.sh. The
# merge base is the baseline. Per crate, over tracked non-test files under
# crates/<crate>/src, the gate counts two patterns at the base and at HEAD:
#   .get("<ident>")             a literal-key read (R1)
#   Result<..., String>         a string error type (R6)
# A count may stay or fall. A rise refuses the change and names the crate,
# the pattern, both counts and the rule. The count is textual: a HashMap
# keyed by &str also matches. That is fine for a ratchet, since only the
# change between two trees decides. Inline #[cfg(test)] modules count;
# *_tests.rs files and tests/ directories do not. Committed work only.
#
# Run: bash scripts/ci/check-value-reads.sh
# Exit: 0 no crate grew, 1 a crate grew, 2 the base cannot be established.
#
# Env (all optional):
#   PR_BASE_REF            base branch name, no remote prefix. Default: main.
#   PR_REMOTE              remote holding the base. Default: origin.
#   VALUE_READS_BASE_SHA   explicit base sha in place of the merge base (the
#                          push-to-main alarm). All zeros counts as unset.

set -euo pipefail

if [[ $# -gt 0 ]]; then
    case "$1" in
        -h|--help) sed -n '2,/^set -/{/^set -/q;s/^# \{0,1\}//p;}' "$0"; exit 0 ;;
        *) echo "check-value-reads: unknown arg: $1 (configured by env; see --help)" >&2; exit 2 ;;
    esac
fi

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$REPO_ROOT"

REMOTE="${PR_REMOTE:-origin}"
BASE_REF="${PR_BASE_REF:-main}"
BASE_SHA="${VALUE_READS_BASE_SHA:-}"
[[ "$BASE_SHA" == "0000000000000000000000000000000000000000" ]] && BASE_SHA=""

# A shallow checkout holds neither the previous tip nor the merge base.
heal_shallow() {
    [[ "$(git rev-parse --is-shallow-repository 2>/dev/null)" == "true" ]] || return 0
    git fetch --quiet --unshallow "$REMOTE" 2>/dev/null || true
}

if [[ -n "$BASE_SHA" ]]; then
    BASE="$(git rev-parse --verify --quiet "$BASE_SHA^{commit}")" || {
        heal_shallow
        BASE="$(git rev-parse --verify --quiet "$BASE_SHA^{commit}")" || true
    }
    if [[ -z "${BASE:-}" ]]; then
        echo "check-value-reads: VALUE_READS_BASE_SHA $BASE_SHA does not resolve" >&2
        exit 2
    fi
else
    if ! git fetch --quiet "$REMOTE" "+refs/heads/$BASE_REF:refs/remotes/$REMOTE/$BASE_REF"; then
        echo "check-value-reads: cannot fetch $REMOTE/$BASE_REF (set PR_BASE_REF/PR_REMOTE)" >&2
        exit 2
    fi
    BASE="$(git merge-base "$REMOTE/$BASE_REF" HEAD 2>/dev/null)" || {
        heal_shallow
        BASE="$(git merge-base "$REMOTE/$BASE_REF" HEAD 2>/dev/null)" || true
    }
    if [[ -z "${BASE:-}" ]]; then
        echo "check-value-reads: cannot establish a merge base between $REMOTE/$BASE_REF and HEAD" >&2
        exit 2
    fi
fi

READ_PATTERN='\.get\("[A-Za-z_]+"\)'
ERROR_PATTERN='Result<[^;{]*, ?String>'

# count <rev> <crate> <pattern>: matches (not lines) in the crate's non-test
# sources at that tree. A crate absent at the rev counts zero.
count() {
    { git grep -o -h -E "$3" "$1" -- "crates/$2/src/*.rs" \
        ':(exclude)*_tests.rs' ':(exclude)**/tests/**' 2>/dev/null || true; } \
        | wc -l | tr -d ' '
}

crates="$(git ls-tree -d --name-only HEAD crates/ | sed 's|^crates/||')"
fails=0
printf '%-12s %-22s %8s %8s\n' crate pattern base head
for crate in $crates; do
    for kind in read error; do
        if [[ "$kind" == read ]]; then pattern="$READ_PATTERN"; label='.get("field")'; rule=R1
        else pattern="$ERROR_PATTERN"; label='Result<_, String>'; rule=R6; fi
        base_n="$(count "$BASE" "$crate" "$pattern")"
        head_n="$(count HEAD "$crate" "$pattern")"
        printf '%-12s %-22s %8s %8s\n' "$crate" "$label" "$base_n" "$head_n"
        if (( head_n > base_n )); then
            fails=$((fails + 1))
            if [[ "$rule" == R1 ]]; then
                remedy='read a typed field: deserialize the JSON once at its boundary'
            else
                remedy='return a typed error enum on the seam'
            fi
            echo "check-value-reads: crates/$crate grew $label from $base_n to $head_n." \
                "Remedy: $remedy (docs/rust-style.md $rule)." >&2
        fi
    done
done

if (( fails > 0 )); then
    exit 1
fi
if [[ -n "$(git status --porcelain -- 'crates/*.rs' 2>/dev/null)" ]]; then
    echo "check-value-reads: note: uncommitted .rs changes are not counted; commit, then re-run" >&2
fi
echo "check-value-reads: ok (no crate grew a string-key read or a string error)"
