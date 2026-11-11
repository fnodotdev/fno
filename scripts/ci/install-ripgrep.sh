#!/usr/bin/env bash
# Install a pinned, checksummed static ripgrep from its GitHub release.
# `apt-get update && apt-get install ripgrep` hung 28 minutes on the hosted
# runner on 2026-10-07 and timed a cargo shard out before any test ran. A
# release download has no package index or mirror to stall on.
set -euo pipefail

version=15.2.0
sha256=33e15bcf1624b25cdd2a55813a47a2f95dbe126268203e76aa6a585d1e7b149c
name="ripgrep-${version}-x86_64-unknown-linux-musl"
tmp=$(mktemp -d)

# An authenticated read rides out GitHub's anonymous-CDN surges (the
# 2026-11-08/09 incident reddened main four times on 504s); a cached
# checksum-verified tarball skips the network entirely. The sha256 gate
# below still runs on every path.
tarball="$tmp/rg.tar.gz"
cache_dir="${RG_TARBALL_CACHE:-}"
if [ -n "$cache_dir" ] && [ -f "$cache_dir/${name}.tar.gz" ]; then
  cp "$cache_dir/${name}.tar.gz" "$tarball"
else
  token="${GITHUB_TOKEN:-${GH_TOKEN:-}}"
  auth=()
  if [ -n "$token" ]; then
    auth=(-H "Authorization: Bearer $token")
  fi
  curl -fsSL --retry 12 --retry-delay 20 --retry-all-errors --connect-timeout 10 --max-time 60 \
    ${auth[@]+"${auth[@]}"} \
    -o "$tarball" \
    "https://github.com/BurntSushi/ripgrep/releases/download/${version}/${name}.tar.gz"
  if [ -n "$cache_dir" ]; then
    mkdir -p "$cache_dir"
    cp "$tarball" "$cache_dir/${name}.tar.gz.part"
    mv "$cache_dir/${name}.tar.gz.part" "$cache_dir/${name}.tar.gz"
  fi
fi
echo "${sha256}  $tarball" | sha256sum -c -
tar -xzf "$tmp/rg.tar.gz" -C "$tmp"
sudo install -m 0755 "$tmp/${name}/rg" /usr/local/bin/rg
rg --version
