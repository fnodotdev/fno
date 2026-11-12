#!/usr/bin/env bash
# build-app-bundle.sh: layout, plist lint, version stamp, clean rebuild, and
# the missing-binary refusal. macOS only (plutil).
set -euo pipefail

if [ "$(uname -s)" != "Darwin" ]; then
  echo "skip: build-app-bundle needs macOS plutil"
  exit 0
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script="$root/scripts/release/build-app-bundle.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

printf '#!/bin/sh\necho stub\n' > "$tmp/fno-app"
chmod +x "$tmp/fno-app"

bash "$script" "$tmp/fno-app" 0.4.6 "$tmp/out" >/dev/null
plist="$tmp/out/fno-app.app/Contents/Info.plist"
[ -x "$tmp/out/fno-app.app/Contents/MacOS/fno-app" ] || { echo "FAIL: executable missing"; exit 1; }
plutil -lint "$plist" >/dev/null
[ "$(plutil -extract CFBundleShortVersionString raw "$plist")" = "0.4.6" ] || { echo "FAIL: short version"; exit 1; }
[ "$(plutil -extract CFBundleVersion raw "$plist")" = "0.4.6" ] || { echo "FAIL: bundle version"; exit 1; }
[ "$(plutil -extract CFBundleIdentifier raw "$plist")" = "dev.fno.fno-app" ] || { echo "FAIL: identifier"; exit 1; }

# A second run over the same out-dir replaces the bundle cleanly.
touch "$tmp/out/fno-app.app/stale"
bash "$script" "$tmp/fno-app" 0.4.7 "$tmp/out" >/dev/null
[ ! -e "$tmp/out/fno-app.app/stale" ] || { echo "FAIL: stale file survived rebuild"; exit 1; }
[ "$(plutil -extract CFBundleShortVersionString raw "$plist")" = "0.4.7" ] || { echo "FAIL: rebuilt version"; exit 1; }

# A missing binary exits 2 and names the path.
set +e
msg="$(bash "$script" "$tmp/nope" 0.4.6 "$tmp/out2" 2>&1)"
code=$?
set -e
[ "$code" -eq 2 ] || { echo "FAIL: missing binary exit $code, want 2"; exit 1; }
case "$msg" in *"$tmp/nope"*) ;; *) echo "FAIL: message does not name the path: $msg"; exit 1 ;; esac

echo "ok: build-app-bundle"
