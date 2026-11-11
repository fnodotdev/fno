#!/usr/bin/env bash
# check-control-sock-writers.sh - one file opens a claude session's control.sock.
#
# Post-attach the control socket is a raw keystroke pipe into a live claude
# session, so every byte typed there must be a checked mail turn or a known
# key. The attach primitives live in claude_attach.rs; the only file allowed to
# use them is the claude control adapter. A new caller anywhere else fails here
# and must route through crates/fno-agents/src/runtime/delivery/claude_control.rs.
#
# Run:  bash scripts/ci/check-control-sock-writers.sh [root]
#       bash scripts/ci/check-control-sock-writers.sh --self-test
# Exit: 0 no stray caller, 1 a stray caller, 2 misuse.

set -euo pipefail

ALLOWED=(
  "crates/fno-agents/src/runtime/claude_attach.rs"
  "crates/fno-agents/src/runtime/delivery/claude_control.rs"
)
TOKENS='UnixControlTransport|perform_attach|attach_for_frames'

scan() {
  local root="$1" hits=""
  [[ -d "$root/crates" ]] || { echo "check-control-sock-writers: no crates/ under $root" >&2; return 2; }
  while IFS= read -r file; do
    local rel="${file#"$root"/}" allowed=0
    for a in "${ALLOWED[@]}"; do [[ "$rel" == "$a" ]] && allowed=1; done
    [[ $allowed -eq 1 ]] && continue
    local lines
    lines="$(grep -nE "$TOKENS" "$file" | grep -vE '^[0-9]+:[[:space:]]*//' || true)"
    [[ -n "$lines" ]] && hits+="$(printf '%s\n' "$lines" | sed "s|^|  $rel:|")"$'\n'
  done < <(find "$root/crates" -name '*.rs' -not -path '*/target/*' | sort)
  if [[ -n "$hits" ]]; then
    echo "check-control-sock-writers: control.sock attach outside the claude control adapter:" >&2
    printf '%s' "$hits" >&2
    echo "  Call crates/fno-agents/src/runtime/delivery/claude_control.rs instead." >&2
    return 1
  fi
  echo "check-control-sock-writers: ok (only ${ALLOWED[*]} attach)"
}

if [[ "${1:-}" == "--self-test" ]]; then
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  mkdir -p "$tmp/crates/x/src"
  printf '// UnixControlTransport is named in a comment only\nfn ok() {}\n' > "$tmp/crates/x/src/a.rs"
  scan "$tmp" >/dev/null || { echo "self-test: a comment-only mention must pass" >&2; exit 1; }
  printf 'fn bad() { let _ = UnixControlTransport::connect(p); }\n' > "$tmp/crates/x/src/b.rs"
  if scan "$tmp" 2>/dev/null; then
    echo "self-test: the canary caller must fail" >&2
    exit 1
  fi
  echo "check-control-sock-writers: self-test ok"
  exit 0
fi

ROOT="${1:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}"
scan "$ROOT"
