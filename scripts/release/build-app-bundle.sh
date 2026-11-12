#!/usr/bin/env bash
# Assemble fno-app.app from a built fno-app binary.
# Usage: build-app-bundle.sh <fno-app-binary> <version> <out-dir>
# Exit 2 on a bad argument or a missing binary.
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: build-app-bundle.sh <fno-app-binary> <version> <out-dir>" >&2
  exit 2
fi
bin="$1"
version="$2"
out="$3"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
template="$here/../../crates/fno-app/macos/Info.plist.in"

if [ ! -f "$bin" ]; then
  echo "build-app-bundle: binary not found: $bin" >&2
  exit 2
fi
if [ -z "$version" ]; then
  echo "build-app-bundle: version is empty" >&2
  exit 2
fi

app="$out/fno-app.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "$bin" "$app/Contents/MacOS/fno-app"
chmod 755 "$app/Contents/MacOS/fno-app"
sed "s/@VERSION@/$version/g" "$template" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null
echo "$app"
