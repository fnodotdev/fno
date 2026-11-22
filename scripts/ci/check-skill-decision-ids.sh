#!/usr/bin/env bash
# scripts/ci/check-skill-decision-ids.sh
#
# Shipped skills, hooks and docs must not cite maintainer-local question or
# decision hashes (d-xxxxxxxx, q-xxxxxxxx).
# The law content lives in the maintainer's ~/.fno decision store; a public
# clone carries the pointer without the content, so the citation cannot be
# resolved and the instruction's authority cannot be checked. Inline the rule
# instead - the same failure one level up as .claude/rules/oss-fix-not-memory.md:
# the pointer must not ship without what it points at.
#
# The scan is a strict shape match: the word `d-` or `q-` followed by 8
# lowercase hex, in any tracked file under skills/, hooks/, docs/, or in
# AGENTS.md. A record is cited by its number (D142). Case-sensitive on
# purpose: uppercase format illustrations (d-ABCD1234) and shell-default
# tokens (ID-deadbeef) are not citations. FIXTURES names the synthetic
# examples, exempt by token and never by file. No marker hatch: if a real
# need ever appears, change the gate, not the tree it reads.
#
# Two controls, because an absence-only pass has two explanations ("clean"
# and "the instrument never matched anything"):
#   surface  each root must resolve to tracked files. A pathspec that matches
#            nothing fails silently, and the scan would read a vacuous zero.
#   tool     --self-test runs the same git grep against a scratch repo whose
#            canary file carries d-cafebabe, then against a clean fixture,
#            so a broken pattern cannot read as a clean tree.
#
# Exit 0 clean; 1 on any hit, on the surface control, or on a failed
# self-test.

set -uo pipefail

PATTERN='[dq]-[0-9a-f]{8}'
SURFACES=(skills/ hooks/ docs/ AGENTS.md)
# Rendered from crates/fno/config_schema.json by a built fno, never edited by
# hand: their text is fixed in that table, then regenerated.
GENERATED=(':(exclude)docs/configuration-guide.md' ':(exclude)docs/config.example.toml')
FIXTURES='d-1a2b3c4d|q-1a2b3c4d|q-1234abcd|q-aaaaaaaa|d-20261101|d-aaaa0001|d-deadbeef'

# The lines of `git grep` output that still cite an id once the fixtures go.
citations() {
  local line rest
  while IFS= read -r line; do
    rest=$(printf '%s\n' "$line" | sed -E "s/($FIXTURES)/fixture/g")
    if printf '%s\n' "$rest" | grep -qwE "$PATTERN"; then
      printf '%s\n' "$line"
    fi
  done
}

self_test() {
  local tmp out rc
  tmp=$(mktemp -d) || return 1
  trap 'rm -rf "$tmp"' RETURN
  git init -q "$tmp"
  printf 'law d-cafebabe says inline the rule\n' > "$tmp/canary.md"
  (cd "$tmp" && git add canary.md)
  out=$(cd "$tmp" && git grep -nE "$PATTERN" -- .)
  rc=$?
  if [ "$rc" -ne 0 ] || ! printf '%s\n' "$out" | grep -q 'canary.md.*d-cafebabe'; then
    echo "self-test failed: the canary id was not detected (rc=$rc)" >&2
    return 1
  fi
  printf 'q-0badc0de asked it\n' > "$tmp/canary.md"
  (cd "$tmp" && git add canary.md)
  out=$(cd "$tmp" && git grep -nwE "$PATTERN" -- . | citations)
  if ! printf '%s\n' "$out" | grep -q 'q-0badc0de'; then
    echo "self-test failed: the question canary was not detected" >&2
    return 1
  fi
  out=$(printf 'example.md:1:see d-1a2b3c4d\n' | citations)
  if [ -n "$out" ]; then
    echo "self-test failed: a fixture token read as a citation" >&2
    return 1
  fi
  printf 'no ids on this line\n' > "$tmp/canary.md"
  (cd "$tmp" && git add canary.md)
  if (cd "$tmp" && git grep -nE "$PATTERN" -- .); then
    echo "self-test failed: the clean fixture matched" >&2
    return 1
  fi
  return 0
}

if [ "${1:-}" = "--self-test" ]; then
  if self_test; then
    echo "self-test ok: the pattern detects a canary id and passes a clean fixture"
    exit 0
  fi
  exit 1
fi

# Surface control: a glob that matches nothing must fail loud, not pass.
for surface in "${SURFACES[@]}"; do
  if [ -z "$(git ls-files -- "$surface")" ]; then
    echo "surface control failed: no tracked files under $surface" >&2
    exit 1
  fi
done

# git grep over tracked files: case-sensitive by default, ignores untracked
# scratch files, and gives clean exit semantics (0 match, 1 none, 2 error).
found=$(git grep -nwE "$PATTERN" -- "${SURFACES[@]}" "${GENERATED[@]}")
rc=$?
if [ "$rc" -gt 1 ]; then
  echo "scan failed (git grep exit $rc)" >&2
  exit 1
fi
hits=""
[ "$rc" -eq 0 ] && hits=$(printf '%s\n' "$found" | citations)
if [ -n "$hits" ]; then
  echo "FAIL: shipped text cites question or decision hashes (d-xxxxxxxx, q-xxxxxxxx):" >&2
  echo "cite the number (D142), or inline the rule where a public clone cannot read the store" >&2
  echo "(see .claude/rules/oss-fix-not-memory.md)" >&2
  printf '%s\n' "$hits" >&2
  exit 1
fi
echo "clean: no question or decision hash under ${SURFACES[*]}"
