#!/usr/bin/env bash
# Bump the Homebrew tap (fnodotdev/homebrew-fno) to a published stable release.
#
#   TAP_TOKEN=<pat> bump-homebrew-tap.sh <version>
#
# The stable publish job runs it, and so does homebrew-tap.yml, the tap-only
# retry, so a failed tap leg never re-runs crates.io or the release upload.
# TAP_TOKEN is a fine-grained PAT with Contents read+write on the tap repo:
# the default GITHUB_TOKEN cannot push cross-repo.
#
# The tap repo is public, so a clone succeeds with any token or none. Only
# the push proves write access. The probe below fails first on a token that
# GitHub rejects outright, so the error names the real cause.
set -euo pipefail

TAP_REPO="fnodotdev/homebrew-fno"
version="${1:?usage: bump-homebrew-tap.sh <version>}"
root="$(cd "$(dirname "$0")/../.." && pwd)"

if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "::error::tap takes stable versions only; '${version}' is not X.Y.Z"
  exit 1
fi
if [ -z "${TAP_TOKEN:-}" ]; then
  echo "::error::HOMEBREW_TAP_TOKEN is not set - the tap (${TAP_REPO}) was NOT bumped to ${version}. Add the secret to the release environment, then run the homebrew-tap workflow."
  exit 1
fi

code="$(curl -s -o /dev/null -w '%{http_code}' \
  -H "Authorization: Bearer ${TAP_TOKEN}" "https://api.github.com/repos/${TAP_REPO}")"
if [ "$code" = "401" ]; then
  echo "::error::GitHub rejects HOMEBREW_TAP_TOKEN (HTTP 401): it is expired, revoked or not a token. Replace it in the release environment with a fine-grained PAT (Contents read+write on ${TAP_REPO}), then run the homebrew-tap workflow."
  exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
git clone --depth 1 "https://x-access-token:${TAP_TOKEN}@github.com/${TAP_REPO}.git" "$work/tap" \
  || { echo "::error::clone of ${TAP_REPO} failed; the release already shipped, so run the homebrew-tap workflow to retry."; exit 1; }
# The seed is the structure source: copy it over the tap formula before the
# patch, so the tap's url shape and install block move with this repo's seed.
cp "$root/scripts/install/homebrew/fno.rb" "$work/tap/Formula/fno.rb"
python3 "$root/scripts/release/update-homebrew-formula.py" "$version" "$work/tap/Formula/fno.rb"
cd "$work/tap"
grep -q "version \"${version}\"" Formula/fno.rb \
  || { echo "::error::tap formula does not read version ${version} after the rewrite"; exit 1; }
if git diff --quiet; then
  echo "tap already at ${version} - nothing to push."
  exit 0
fi
git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
git commit -qam "fno ${version}: bump url + sha256 (auto)"
git push origin HEAD \
  || { echo "::error::push to ${TAP_REPO} refused: HOMEBREW_TAP_TOKEN lacks Contents write on it. Widen the token, then run the homebrew-tap workflow."; exit 1; }
echo "tap bumped to ${version}"
