#!/usr/bin/env bash
# Propagate ONE release version to every version-bearing manifest in the repo.
#
# The plugin/extension manifests drifted (claude/marketplace/gemini/codex all
# lagged the CLI) precisely because nothing propagated the version to them. This
# is that single point: the release workflow (release.yml) and any manual bump
# both call it, so all surfaces stay in lockstep by construction.
#
# Accepts one PEP 440 version; cargo refuses PEP 440 spellings, so the
# Cargo.toml files get the semver form while the JSON manifests keep the
# PEP 440 string:
#   X.Y.Z        -> JSON: X.Y.Z                 Cargo.toml: X.Y.Z
#   X.Y.ZrcN     -> JSON: X.Y.ZrcN              Cargo.toml: X.Y.Z-rc.N
#   X.Y.Z.devN   -> JSON: X.Y.Z.devN            Cargo.toml: X.Y.Z-dev.N
# Any other shape exits 2. --check compares every other surface against
# .claude-plugin/plugin.json (the PEP 440 anchor), so a hand-patched crate
# version fails the drift guard.
#
# Version-agnostic: it overwrites whatever is there with $1, so it works from any
# starting version and imposes no zero-padding (0.3.9 -> 0.3.10 -> 0.3.100 is
# fine; we expect to ride 0.3.x deep into 3-digit patches).
#
# perl -i (not `sed -i`) so it is identical on the ubuntu runner and a
# maintainer's macOS: BSD and GNU `sed -i` take their backup-suffix argument
# differently, which silently corrupts one or the other.
#
# Usage:
#   scripts/release/sync-version.sh 0.4.0      # set every surface to 0.4.0
#   scripts/release/sync-version.sh 0.4.0rc1   # pre-release; cargo gets 0.4.0-rc.1
#   scripts/release/sync-version.sh --check    # assert every surface already agrees
set -euo pipefail

arg="${1:?usage: sync-version.sh <X.Y.Z | X.Y.ZrcN | X.Y.Z.devN> | --check}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

# PEP 440 -> semver: 0.4.0rc1 -> 0.4.0-rc.1, 0.4.0.dev20260925 -> 0.4.0-dev.20260925.
pep_to_semver() {
  local ver="$1" base tail
  case "$ver" in
    *rc[0-9]*) tail="${ver##*rc}"; base="${ver%rc*}"; echo "${base}-rc.${tail}" ;;
    *.dev[0-9]*) tail="${ver##*.dev}"; base="${ver%.dev*}"; echo "${base}-dev.${tail}" ;;
    *) echo "$ver" ;;
  esac
}

# Strict shape: plain X.Y.Z, or X.Y.Z followed by rc<digits> or .dev<digits>.
is_release_version() {
  printf '%s' "$1" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(rc[0-9]+|\.dev[0-9]+)?$'
}

# The manifests whose top-level `"version"` must track the release. --check
# compares every one of them against the plugin.json anchor.
# buddy is deliberately absent: it versions on its own (0.0.1 today); a release
# is a PR that bumps its plugin.json, never fno's stable promotion.
JSON_MANIFESTS=(
  .claude-plugin/plugin.json
  .claude-plugin/marketplace.json
  gemini-extension.json
  .codex-plugin/plugin.json
  .opencode/package.json
  plugins/openclaw/promise-tag-reader/package.json
)

# The crate manifests the stamp moves: their [package] version and their
# lockfile's own entry both read from this one list. fno-mail is absent: it
# versions on its own API, and fno and fno-agents take it by caret range.
CRATE_MANIFESTS=(
  crates/fno/Cargo.toml
  crates/fno-agents/Cargo.toml
)

# fno-app versions on its own line (its Cargo.toml is never stamped here), but
# its lockfile pins fno and fno-agents, so each stamp refreshes that lock.
LOCK_ONLY_MANIFESTS=(
  crates/fno-app/Cargo.toml
)

# --check: fail (exit 1) if any surface disagrees with the anchored version.
# The drift guard that would have caught the plugins stranded at 0.2.x. Read-only.
# The anchor is .claude-plugin/plugin.json: since the wheel went frozen it is
# the only PEP 440 source of truth in the tree.
if [[ "$arg" == "--check" ]]; then
  want_pep="$(perl -ne 'if (/"version"\s*:\s*"([^"]*)"/) { print $1; exit }' .claude-plugin/plugin.json)"
  is_release_version "$want_pep" || {
    echo "sync-version: .claude-plugin/plugin.json does not hold an X.Y.Z, X.Y.ZrcN or X.Y.Z.devN version: '$want_pep'" >&2
    exit 1
  }
  want_semver="$(pep_to_semver "$want_pep")"
  bad=0
  # Cargo carries the semver spelling; everything else carries the PEP 440 one.
  for cf in "${CRATE_MANIFESTS[@]}"; do
    got="$(perl -ne 'if(/^version = "(.*)"/){print $1; exit}' "$cf")"
    [[ "$got" == "$want_semver" ]] || { echo "drift: $cf = $got (want $want_semver)"; bad=1; }
  done
  # The crate's own entry in its lockfile must carry the stamped version: the
  # `cargo update` that moves it runs || true, so a silent failure (a
  # resolution conflict, for example) would strand the lock a --locked build
  # refuses.
  for cf in "${CRATE_MANIFESTS[@]}"; do
    c="$(basename "$(dirname "$cf")")"
    lock="$(dirname "$cf")/Cargo.lock"
    got="$(awk -v pkg="$c" '$0 == "name = \"" pkg "\"" { seen = 1; next } seen && /^version = / { sub(/^version = "/, ""); sub(/"$/, ""); print; exit }' "$lock" 2>/dev/null)"
    [[ "$got" == "$want_semver" ]] || { echo "drift: $lock ($c) = ${got:-missing} (want $want_semver)"; bad=1; }
  done
  for j in "${JSON_MANIFESTS[@]}"; do
    matches="$(perl -ne 'print "$1\n" while /"version"\s*:\s*"([^"]*)"/g' "$j")"
    # Zero version fields is itself drift: a dropped/renamed key would otherwise
    # pass silently (the while loop just never runs).
    if [[ -z "$matches" ]]; then
      echo "drift: $j has no \"version\" field (want $want_pep)"; bad=1; continue
    fi
    while IFS= read -r got; do
      [[ "$got" == "$want_pep" ]] || { echo "drift: $j = $got (want $want_pep)"; bad=1; }
    done <<< "$matches"
  done
  [[ "$bad" == 0 ]] && echo "sync-version: all surfaces agree at ${want_pep}"
  exit "$bad"
fi

ver="$arg"
is_release_version "$ver" || {
  echo "sync-version: '$ver' is not an X.Y.Z, X.Y.ZrcN or X.Y.Z.devN version" >&2
  exit 2
}
semver="$(pep_to_semver "$ver")"

# 1. Rust crate manifests, in the semver form. `^version = ` matches
#    only the [package] version (rust-version and dependency lines never start
#    with `version `). Manifest text only: the lockfile pass below runs after
#    every manifest edit, never interleaved.
for cf in "${CRATE_MANIFESTS[@]}"; do
  perl -i -pe "s/^version = \".*\"/version = \"${semver}\"/" "$cf"
done

# 1b. Lockfiles: cargo update per crate, after EVERY manifest edit above.
#     fno-app's lock walks both stamped crates, so an update run while one
#     manifest is still stale fails - and the failure hides under || true,
#     stranding the lock. --check's lock drift guard catches a stranded lock;
#     this ordering is what keeps it from firing on every stamp.
for cf in "${CRATE_MANIFESTS[@]}"; do
  c="$(basename "$(dirname "$cf")")"
  cargo update --manifest-path "$cf" -p "$c" --precise "${semver}" >/dev/null 2>&1 || true
done
for cf in "${LOCK_ONLY_MANIFESTS[@]}"; do
  for dep in fno fno-agents; do
    cargo update --manifest-path "$cf" -p "$dep" >/dev/null 2>&1 || true
  done
done

# 3. JSON plugin / extension manifests. Every `"version"` key in each of these is
#    the package version (verified: no nested/dependency `"version"` keys), so a
#    global key-anchored replace is safe and touches only the version line(s) -
#    marketplace.json legitimately carries two (listing metadata + the plugin row).
for j in "${JSON_MANIFESTS[@]}"; do
  [[ -f "$j" ]] || { echo "sync-version: manifest missing: $j" >&2; exit 1; }
  # Whitespace-tolerant match (handles a compact `"version":"x"` reformat too),
  # normalized to the canonical spaced form.
  perl -i -pe "s/\"version\"\s*:\s*\"[^\"]*\"/\"version\": \"${ver}\"/g" "$j"
  # Verify the bump actually landed. A missing/renamed version key - or a format
  # this regex somehow still missed - would otherwise no-op SILENTLY and ship a
  # stale plugin version behind a bumped CLI (the exact drift this guards;
  # installers resolve the plugin through these manifests). Fail loud instead.
  grep -q "\"version\": \"${ver}\"" "$j" \
    || { echo "sync-version: no \"version\" field set to ${ver} in $j (missing or unrecognized key?)" >&2; exit 1; }
done

# A release stamp is workspace-only, so crates/ reads dirty at build time and
# the binaries would bake -dirty into the release. Mark the stamp so build.rs
# reads the tree clean: the only working-tree diff is the release identity the
# binary exists to report.
if [[ -n "${GITHUB_ENV:-}" ]]; then
    echo "FNO_RELEASE_STAMP=1" >> "$GITHUB_ENV"
fi

echo "sync-version: all surfaces set to ${ver}"
