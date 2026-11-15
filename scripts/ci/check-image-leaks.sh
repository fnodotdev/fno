#!/usr/bin/env bash
# check-image-leaks.sh - CI gate that keeps screenshot images carrying
# live-system data out of the public repo.
#
# A preview.png shot against the live system merged to main with the panels
# PR 42, and an OCR sweep on 2026-11-15 measured 7 tracked images still
# carrying /Users/<user> paths, real session uuids and machine rows. This
# gate OCRs every image CHANGED against the base ref and matches the needle
# shapes a runner can judge without machine facts:
#   uuid-shaped session ids   [0-9a-f]{8}-...-[0-9a-f]{12}
#   home path prefixes        /Users/<name>, /home/<name>
# Machine names stay a capture-time check: `fno mux pane run --shot` scans
# them with scutil, which only the captured machine can read. Existing
# images are grandfathered; only changed files are scanned.
#
# Run: bash scripts/ci/check-image-leaks.sh [--base <ref>] [file...]
#   --base <ref>  diff base; env BASE_REF wins over the origin/main default
#   file...       scan exactly these files (local test mode, no diff)
# Exit: 0 clean, 1 a leak or an unusable scan, 2 misuse.

set -euo pipefail

BASE=""
FILES=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --base)
      [[ $# -ge 2 ]] || { echo "check-image-leaks: --base needs a ref" >&2; exit 2; }
      BASE="$2"; shift 2 ;;
    --base=*)
      BASE="${1#--base=}"; shift ;;
    -*)
      echo "check-image-leaks: unknown flag $1" >&2; exit 2 ;;
    *)
      FILES+=("$1"); shift ;;
  esac
done
# BASE_REF carries a branch name (the workflow's github.base_ref), so it
# resolves through origin/ like the sibling gates; --base takes any ref.
BASE=""
if [[ -z "$BASE" && -n "${BASE_REF:-}" ]]; then
  BASE="origin/${BASE_REF}"
fi
BASE="${BASE:-origin/main}"

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

if [[ ${#FILES[@]} -eq 0 ]]; then
  if ! git -C "$REPO_ROOT" rev-parse --verify --quiet "$BASE" >/dev/null 2>&1; then
    echo "check-image-leaks: base $BASE does not resolve; cannot diff" >&2
    exit 1
  fi
  while IFS= read -r f; do
    FILES+=("$f")
  done < <(git -C "$REPO_ROOT" diff --name-only --diff-filter=ACMR "$BASE" -- '*.png' '*.jpg' '*.jpeg')
fi

if [[ ${#FILES[@]} -eq 0 ]]; then
  echo "check-image-leaks: no changed images vs $BASE"
  exit 0
fi

if ! command -v tesseract >/dev/null 2>&1; then
  echo "check-image-leaks: tesseract is missing; the gate refuses to pass an image it cannot scan" >&2
  echo "  install it locally with 'brew install tesseract', or in CI with" >&2
  echo "  'sudo apt-get install -y tesseract-ocr' ahead of this step" >&2
  exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

leaked=0
for f in ${FILES[@]+"${FILES[@]}"}; do
  orig="$f"
  [[ "$f" = /* ]] || f="$PWD/$f"
  if [[ ! -f "$f" ]]; then
    echo "check-image-leaks: changed image missing from the tree: $orig" >&2
    exit 1
  fi
  stem="$TMP/$(printf '%s' "$orig" | tr '/' '_')"
  if ! tesseract "$f" "$stem" >/dev/null 2>&1; then
    echo "check-image-leaks: tesseract could not read $orig; refusing to pass an unscannable image" >&2
    exit 1
  fi
  if grep -Eqi '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}' "$stem.txt"; then
    echo "leak: $orig: session-id shape:"
    grep -Ei '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}' "$stem.txt"
    leaked=1
  fi
  if grep -Eqi '/(users|home)/[a-z0-9._-]+' "$stem.txt"; then
    echo "leak: $orig: home path:"
    grep -Ei '/(users|home)/[a-z0-9._-]+' "$stem.txt"
    leaked=1
  fi
done

if [[ $leaked -ne 0 ]]; then
  echo "check-image-leaks: leaked image(s) above; re-shoot from the demo rig:" >&2
  echo "  scripts/ops/mux-demo-snapshot.sh (FNO_DEMO_HOST keeps the real host off the frame)" >&2
  exit 1
fi
echo "check-image-leaks: ${#FILES[@]} changed image(s) clean"
