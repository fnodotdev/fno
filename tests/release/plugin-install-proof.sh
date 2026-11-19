#!/usr/bin/env bash
# The container proof. On a clean ubuntu:24.04 with no fno, the candidate
# plugin tree's postinstall fetches the candidate's GitHub Release tarball,
# verifies it and adopts the binary install. The proof asks for that primary
# path: the scripts/install/fno.sh fallback would hide a broken tarball door.
# Then the installed binary reads a backlog and the daemon survives start.
#
# Usage: bash tests/release/plugin-install-proof.sh --candidate 0.0.17rc1 \
#          [--repo fnodotdev/fno]
# Writes .fno/tmp/plugin-install-proof/<run>/result.json with on_binary.
set -euo pipefail
cd "$(git rev-parse --show-toplevel 2>/dev/null || pwd -P)"

CANDIDATE=""
REPO="${FNO_RELEASE_REPO:-fnodotdev/fno}"
OUT_DIR=".fno/tmp/plugin-install-proof/$(date +%s)"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --candidate) CANDIDATE="${2:?--candidate needs a version}"; shift 2 ;;
    --repo) REPO="${2:?--repo needs owner/name}"; shift 2 ;;
    --out) OUT_DIR="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[[ -n "$CANDIDATE" ]] || { echo "usage: $0 --candidate <version> [--repo owner/name]" >&2; exit 2; }
# docker -v refuses a relative host path, and the runner cwd is one.
[[ "$OUT_DIR" == /* ]] || OUT_DIR="$(pwd -P)/$OUT_DIR"
command -v docker >/dev/null 2>&1 || { echo "docker is required"; exit 1; }
mkdir -p "$OUT_DIR"

cat >"$OUT_DIR/proof.sh" <<'PROOF'
set -euo pipefail
apt-get update -qq && apt-get install -y -qq curl ca-certificates jq >/dev/null
export PATH="$HOME/.local/bin:$PATH"
CLAUDE_PLUGIN_ROOT=/repo
export CLAUDE_PLUGIN_ROOT
LOG=/host/postinstall.log
bash /repo/.claude-plugin/postinstall.sh >"$LOG" 2>&1 || { echo "proof: postinstall failed"; cat "$LOG"; exit 1; }
grep -q 'binary install adopted' "$LOG" || { echo "proof: postinstall did not take the tarball path"; cat "$LOG"; exit 1; }
[[ "$(cat "$HOME/.fno/install/channel" 2>/dev/null)" == "binary" ]] || { echo "proof: no binary channel marker"; cat "$LOG"; exit 1; }
TGT="$(readlink -f "$HOME/.local/bin/fno")"
[[ "$TGT" == "$HOME/.fno/install/bin/"* ]] || { echo "proof: $TGT is not under the binary install"; exit 1; }
VER="$("$HOME/.local/bin/fno" --version | grep -oE '[0-9]+\.[0-9]+\.[0-9]+[^ ]*' | head -1)"
VER="${VER/-rc./rc}"
[[ "$VER" == "$CANDIDATE" ]] || { echo "proof: fno --version printed $VER, want $CANDIDATE"; exit 1; }
"$HOME/.fno/install/bin/fno" backlog list --count >/dev/null
"$HOME/.fno/install/bin/fno-agent-daemon" >/dev/null 2>&1 &
DPID=$!
sleep 6
kill -0 "$DPID" || { echo "proof: the daemon did not survive start"; exit 1; }
# The daemon can exit between the kill -0 probe and the kill; a failed
# cleanup kill is not a proof failure under set -e.
kill "$DPID" 2>/dev/null || true
# wait returns the SIGTERM status (143) under set -e; the survival contract
# was already proven by kill -0, so the cleanup wait cannot fail the proof.
wait "$DPID" 2>/dev/null || true
echo '{"on_binary": 1}' > /host/proof.result
PROOF

# The candidate's plugin tree carries the release stamp (release.yml's stamp
# step); the tagged commit does not, so overlay a stamped manifest.
semver="$(printf '%s' "$CANDIDATE" | sed -E 's/\.dev([0-9]+)$/-dev.\1/; s/rc([0-9]+)$/-rc.\1/')"
perl -pe "s/\"version\": \"[^\"]*\"/\"version\": \"${semver}\"/" \
  .claude-plugin/plugin.json >"$OUT_DIR/plugin.json"

docker run --rm -e "CANDIDATE=$CANDIDATE" -e "FNO_RELEASE_REPO=$REPO" \
  -v "$OUT_DIR:/host" -v "$(pwd -P):/repo:ro" \
  -v "$OUT_DIR/plugin.json:/repo/.claude-plugin/plugin.json:ro" \
  ubuntu:24.04 bash /host/proof.sh || echo "plugin-install-proof: the container exited $?"

# The container writes its verdict file only when every check passed, so the
# file alone decides result.json.
ON="$([[ -f "$OUT_DIR/proof.result" ]] && echo 1 || echo 0)"
printf '{"on_binary": %d}\n' "$ON" > "$OUT_DIR/result.json"
cat "$OUT_DIR/result.json"
[[ "$ON" -eq 1 ]] || { echo "plugin-install-proof: FAILED"; exit 1; }
echo "plugin-install-proof: the plugin install landed the candidate binary"
