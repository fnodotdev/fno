#!/usr/bin/env bash
# Move the plugin catalog pins to a release tag. Pure file edit: no git, no
# network. The release workflow checks out a pin branch, runs this, commits,
# opens the pin PR and arms auto-merge.
#
# Both plugin.json versions are bumped to the version JUST released. The
# version a user's update detection compares lives in the PINNED tree (the
# tag), so this bump lands one release behind: each new tag's tree carries
# the previous release's number, which still differs from the prior tag's
# tree, so every release updates users.
set -euo pipefail
tag=${1:?release tag, e.g. v0.4.9}
sha=${2:?full sha of the tag commit}
version=${tag#v}
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || { echo "sha must be a full 40-hex commit sha" >&2; exit 2; }

for f in .claude-plugin/marketplace.json .agents/plugins/marketplace.json; do
  tmp=$(mktemp)
  jq --arg tag "$tag" --arg sha "$sha" '
    .plugins |= map(if .name == "fno" then .source.ref = $tag | .source.sha = $sha else . end)
  ' "$f" > "$tmp" && mv "$tmp" "$f"
  got_ref=$(jq -r '.plugins[] | select(.name == "fno") | .source.ref' "$f")
  got_sha=$(jq -r '.plugins[] | select(.name == "fno") | .source.sha' "$f")
  if [[ "$got_ref" != "$tag" || "$got_sha" != "$sha" ]]; then
    echo "pin edit did not land in $f (ref=$got_ref sha=$got_sha)" >&2
    exit 1
  fi
done

for f in .claude-plugin/plugin.json .codex-plugin/plugin.json; do
  tmp=$(mktemp)
  jq --arg version "$version" '.version = $version' "$f" > "$tmp" && mv "$tmp" "$f"
  got=$(jq -r .version "$f")
  if [[ "$got" != "$version" ]]; then echo "version edit did not land in $f" >&2; exit 1
  fi
done
