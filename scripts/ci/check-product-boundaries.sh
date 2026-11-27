#!/usr/bin/env bash
# Product boundary gate: the inventory doc's claims, checked against the
# manifests and the source. Fails when the compile edge stops pointing one
# way (fno to fno-agent), when the availability classifier or the PATH walk gains
# a second definition, or when the recorded duplicate-decision debt entries
# disappear from the doc.
set -uo pipefail

REPO_ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$REPO_ROOT" || exit 1

fail() {
    echo "FAIL: $*"
    exit 1
}

command -v jq >/dev/null 2>&1 || fail "jq is required (brew install jq)"

# ── 1. the compile edge points one way ────────────────────────
AGENTS_JSON=$(cargo metadata --no-deps --offline --format-version 1 \
    --manifest-path crates/fno-agent/Cargo.toml 2>/dev/null) ||
    fail "cargo metadata failed for fno-agent"
FNO_JSON=$(cargo metadata --no-deps --offline --format-version 1 \
    --manifest-path crates/fno/Cargo.toml 2>/dev/null) ||
    fail "cargo metadata failed for fno"

echo "$AGENTS_JSON" | jq -e '.packages | length > 0' >/dev/null ||
    fail "positive control: parsed no packages from fno-agent metadata"

# fno-agent names no fno dependency of any kind: fno links it, never the
# other way.
echo "$AGENTS_JSON" | jq -e '
    [.packages[] | select(.name == "fno-agent")
        | .dependencies[] | select(.name == "fno")] | length == 0' >/dev/null ||
    fail "fno-agent names fno as a dependency; the edge points from fno to fno-agent only"

echo "$FNO_JSON" | jq -e '.packages | length > 0' >/dev/null ||
    fail "positive control: parsed no packages from fno metadata"
# Positive control: the one real edge is present, optional, and normal.
echo "$FNO_JSON" | jq -e '
    [.packages[] | select(.name == "fno")
        | .dependencies[] | select(.name == "fno-agent")
        | select(.optional and .kind == null)] | length == 1' >/dev/null ||
    fail "fno must link fno-agent once, as an optional normal dependency behind the client feature"

# ── 2. the classifier and its doc exist ─────────────────────────────────────
[ -f crates/fno/src/product_boundary.rs ] ||
    fail "crates/fno/src/product_boundary.rs is missing"
[ -f docs/architecture/product-boundaries.md ] ||
    fail "docs/architecture/product-boundaries.md is missing"

# ── 3. one availability classifier ──────────────────────────────────────────
# Scoped grep, not rg: ubuntu-latest runners ship no ripgrep, and both paths
# are source dirs with no build artifacts or nested checkouts beneath them.
CLASSIFIER_FILES=$(grep -rl --include='*.rs' "not found (set FNO_AGENT_WORKER" \
    crates/fno/src crates/fno-agent/src | wc -l | tr -d ' ')
[ "$CLASSIFIER_FILES" = "1" ] ||
    fail "the worker-missing refusal is defined in $CLASSIFIER_FILES files, expected 1 (product_boundary.rs)"

# ── 4. one PATH walk for paired binaries ────────────────────────────────────
WALK_FILES=$(grep -rl --include='*.rs' "fn find_on_path" crates/fno/src | wc -l | tr -d ' ')
[ "$WALK_FILES" = "1" ] ||
    fail "find_on_path is defined in $WALK_FILES files, expected 1 (product_boundary.rs)"

# ── 5. the recorded debt stays recorded ─────────────────────────────────────
for token in canonical_root_with canonical_repo_root resolve_canonical_repo_root paired_bin; do
    grep -q "$token" docs/architecture/product-boundaries.md ||
        fail "the debt inventory no longer records $token"
done

echo "product-boundaries: ok (one-way edge, one classifier, one PATH walk, debt recorded)"
