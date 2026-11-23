#!/usr/bin/env bash
# Print the release notes for one fno-app version: the body of its
# `## <version>` section in crates/fno-app/CHANGELOG.md. The app shows this
# text once after an update, so it reads as plain prose, not as a diff.
# A version with no section prints the one-line default, never nothing.
set -euo pipefail

version="${1:?usage: app-release-notes.sh <version>}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
changelog="$root/crates/fno-app/CHANGELOG.md"

notes=""
if [ -f "$changelog" ]; then
  notes="$(awk -v v="$version" '
    /^## / { if (on) exit; on = ($2 == v) ; next }
    on { print }
  ' "$changelog" | sed -e :a -e '/^[[:space:]]*$/{$d;N;ba' -e '}')"
fi
if [ -z "$notes" ]; then
  notes="Desktop app for macOS (Apple silicon and Intel)."
fi
printf '%s\n' "$notes"
