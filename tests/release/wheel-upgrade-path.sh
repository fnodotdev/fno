#!/usr/bin/env bash
# The container proof. From a clean 0.4.5 wheel install in
# ubuntu:24.04, prove both upgrade paths land the candidate binary.
# PyPI publishes nothing past the frozen wheel, so both legs reach the
# candidate through the GitHub Release tarball.
#
# Leg A (the wheel ladder): 0.4.5's `fno update` runs `uv tool upgrade fno`
#   and the frozen latest wheel wins that resolution; the next `fno update`
#   (now the 0.4.7+ front door) performs the move to the binary install and
#   lands the newest stable tarball. The candidate rc is then fetched from
#   the GitHub Release (tag + .sha256, verified) and adopted with the
#   staged front door's `update --adopt`, the postinstall door.
# Leg B (one plugin update): the candidate plugin tree's wheel-move
#   session-start producer starts postinstall detached; the detached run
#   fetches the rc tarball and adopts the binary.
#
# Usage: bash tests/release/wheel-upgrade-path.sh --candidate 0.4.6rc1 \
#          [--repo fnodotdev/fno]
# Writes .fno/tmp/wheel-upgrade-path/<run>/result.json with legs_on_binary.
set -euo pipefail
cd "$(git rev-parse --show-toplevel 2>/dev/null || pwd -P)"

CANDIDATE=""
REPO="${FNO_RELEASE_REPO:-fnodotdev/fno}"
OUT_DIR=".fno/tmp/wheel-upgrade-path/$(date +%s)"

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

# One leg per container, same setup: uv + a clean 0.4.5 wheel install, and
# for leg B the candidate plugin tree mounted at /repo.
run_leg() { # run_leg <letter> <extra docker args...>
  local letter="$1"; shift
  local setup leg
  setup="$(mktemp "${TMPDIR:-/tmp}/fno-leg-setup.XXXXXX")"
  cat >"$setup" <<'SETUP'
set -euo pipefail
apt-get update -qq && apt-get install -y -qq curl ca-certificates >/dev/null
curl -fsSL https://astral.sh/uv/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
uv tool install --compile-bytecode fno==0.4.5
# A == pin refuses `uv tool upgrade`; the real 0.4.5 stable install predates
# the pin (postinstall ran the by-name install), so strip only the specifier
# from the receipt to reproduce that unpinned receipt. The requirements key
# is one line, so deleting the line would empty the receipt and the upgrade
# would remove the tool instead of resolving a candidate.
sed -i -E 's/, specifier = "[^"]*"//' "$(uv tool dir)/fno/uv-receipt.toml"
SETUP

  leg="$setup.leg"
  if [[ "$letter" == "A" ]]; then
    cat >"$leg" <<'LEGA'
set -euo pipefail
apt-get update -qq && apt-get install -y -qq jq >/dev/null
export PATH="$HOME/.local/bin:$PATH"
# Leg A: the wheel ladder with PyPI frozen. 0.4.5's own fno update runs
# uv tool upgrade fno; the frozen latest wheel wins that resolution. The
# next fno update (now the 0.4.7+ front door) performs the move to the
# binary install and lands the newest stable tarball.
fno update
"$HOME/.local/bin/fno" --version
VER="$("$HOME/.local/bin/fno" --version | grep -oE '[0-9]+\.[0-9]+\.[0-9]+[^ ]*' | head -1)"
VER="${VER/-rc./rc}"
[[ "$VER" != "0.4.5" ]] || { echo "leg A: the upgrade left the seed wheel in place ($VER)"; exit 1; }
fno update
TGT="$(readlink -f "$HOME/.local/bin/fno")"
[[ "$TGT" == "$HOME/.fno/install/bin/"* ]] || { echo "leg A: $TGT is not under the binary install"; exit 1; }
[[ "$(cat "$HOME/.fno/install/channel")" == "binary" ]] || { echo "leg A: channel marker is not binary"; exit 1; }
# The candidate never rides an index any more: fetch the rc tarball from
# the GitHub Release with its .sha256 sidecar, verify the sum, unpack, and
# adopt through the staged front door - the same door the plugin
# postinstall drives.
PLAT="$(case "$(uname -m)" in x86_64) echo linux-x64 ;; aarch64) echo linux-arm64 ;; *) echo unknown ;; esac)"
[[ "$PLAT" != unknown ]] || { echo "leg A: no release tarball is built for $(uname -m)"; exit 1; }
TAG="v${CANDIDATE}"
URL="https://github.com/${FNO_RELEASE_REPO}/releases/download/${TAG}/fno-agents-${PLAT}.tar.gz"
STAGE="$HOME/.fno/install/adopt-proof"
rm -rf "$STAGE" && mkdir -p "$STAGE"
curl -fsSL --max-time 300 -o "$STAGE/tarball" "$URL"
curl -fsSL --max-time 60 -o "$STAGE/tarball.sha256" "$URL.sha256"
mv "$STAGE/tarball" "$STAGE/fno-agents-$PLAT.tar.gz"
want="$(cut -d' ' -f1 "$STAGE/tarball.sha256")"
got="$(sha256sum "$STAGE/fno-agents-$PLAT.tar.gz" | cut -d' ' -f1)"
[[ "$got" == "$want" && -n "$want" ]] || { echo "leg A: sha256 mismatch for the candidate tarball"; exit 1; }
tar -xzf "$STAGE/fno-agents-$PLAT.tar.gz" -C "$STAGE"
[[ -x "$STAGE/fno" ]] || { echo "leg A: the candidate tarball carries no executable front door"; exit 1; }
"$STAGE/fno" update --adopt "$STAGE"
"$HOME/.local/bin/fno" --version
VER="$("$HOME/.local/bin/fno" --version | grep -oE '[0-9]+\.[0-9]+\.[0-9]+[^ ]*' | head -1)"
VER="${VER/-rc./rc}"
[[ "$VER" == "$CANDIDATE" ]] || { echo "leg A: fno --version printed $VER, want $CANDIDATE"; exit 1; }
# The state root born under the seed wheel still reads and runs on the
# candidate: a backlog read answers and the daemon survives its start pass
# (no stepping stone; the layout folds self-heal at daemon start).
"$HOME/.fno/install/bin/fno" backlog list --count >/dev/null
"$HOME/.fno/install/bin/fno-agents-daemon" >/dev/null 2>&1 &
DPID=$!
sleep 6
kill -0 "$DPID" || { echo "leg A: the daemon did not survive start on the upgraded state root"; exit 1; }
# The daemon can exit between the kill -0 probe and the kill; a failed
# cleanup kill is not a leg failure under set -e.
kill "$DPID" 2>/dev/null || true
# wait returns the SIGTERM status (143) under set -e; the survival contract
# was already proven by kill -0, so the cleanup wait cannot fail the leg.
wait "$DPID" 2>/dev/null || true
echo '{"leg_a": 1}' > /host/A.result
LEGA
  fi

  if [[ "$letter" == "B" ]]; then
    cat >"$leg" <<'LEGB'
set -euo pipefail
apt-get update -qq && apt-get install -y -qq jq >/dev/null
export PATH="$HOME/.local/bin:$PATH"
# Leg B: the candidate plugin tree's wheel-move producer runs as a plugin
# update's session start would. The detached postinstall fetches the rc
# tarball and adopts; the loop waits for the marker.
CLAUDE_PLUGIN_ROOT=/repo
export CLAUDE_PLUGIN_ROOT
bash /repo/hooks/wheel-move-session-start.sh
# The marker lands at adopt, before the shim sweep relinks; wait for the
# detached postinstall itself to exit.
for i in $(seq 1 180); do
  grep -qs postinstall.sh /proc/[0-9]*/cmdline || break
  sleep 1
done
MOVELOG="$HOME/.fno/install/wheel-move.log"
[[ "$(cat "$HOME/.fno/install/channel" 2>/dev/null)" == "binary" ]] || { echo "leg B: the detached move never landed the marker"; cat "$MOVELOG" 2>/dev/null; exit 1; }
TGT="$(readlink -f "$HOME/.local/bin/fno")"
[[ "$TGT" == "$HOME/.fno/install/bin/"* ]] || { echo "leg B: $TGT is not under the binary install"; cat "$MOVELOG" 2>/dev/null; exit 1; }
"$HOME/.local/bin/fno" --version >/dev/null
VER="$("$HOME/.local/bin/fno" --version | grep -oE '[0-9]+\.[0-9]+\.[0-9]+[^ ]*' | head -1)"
VER="${VER/-rc./rc}"
[[ "$VER" == "$CANDIDATE" ]] || { echo "leg B: fno --version printed $VER, want $CANDIDATE"; exit 1; }
"$HOME/.fno/install/bin/fno" backlog list --count >/dev/null
"$HOME/.fno/install/bin/fno-agents-daemon" >/dev/null 2>&1 &
DPID=$!
sleep 6
kill -0 "$DPID" || { echo "leg B: the daemon did not survive start on the 0.4.5 state root"; exit 1; }
# The daemon can exit between the kill -0 probe and the kill; a failed
# cleanup kill is not a leg failure under set -e.
kill "$DPID" 2>/dev/null || true
# wait returns the SIGTERM status (143) under set -e; the survival contract
# was already proven by kill -0, so the cleanup wait cannot fail the leg.
wait "$DPID" 2>/dev/null || true
echo '{"leg_b": 1}' > /host/B.result
LEGB
  fi

  # Mount a scratch dir at /host for the leg result, plus the repo for leg B.
  local -a mounts=("-v" "$OUT_DIR:/host")
  local -a envs=("-e" "CANDIDATE=$CANDIDATE" "-e" "FNO_RELEASE_REPO=$REPO")
  if [[ "$letter" == "B" ]]; then
    # The candidate's plugin tree carries the release stamp (release.yml's
    # stamp step); the tagged commit does not, so overlay a stamped manifest.
    local semver
    semver="$(printf '%s' "$CANDIDATE" | sed -E 's/\.dev([0-9]+)$/-dev.\1/; s/rc([0-9]+)$/-rc.\1/')"
    perl -pe "s/\"version\": \"[^\"]*\"/\"version\": \"${semver}\"/" \
      .claude-plugin/plugin.json >"$OUT_DIR/plugin.json"
    mounts+=("-v" "$(pwd -P):/repo:ro" "-v" "$OUT_DIR/plugin.json:/repo/.claude-plugin/plugin.json:ro")
  fi
  cp "$setup" "$OUT_DIR/$letter.setup"
  cp "$leg" "$OUT_DIR/$letter.leg"
  docker run --rm "${envs[@]}" "${mounts[@]}" ubuntu:24.04 \
    bash -c "bash /host/$letter.setup && bash /host/$letter.leg"
  rm -f "$setup" "$leg"
}

run_leg A
run_leg B

# Each container starts with an empty /host and writes its own verdict file,
# so the run folds the two into result.json.
A="$([[ -f "$OUT_DIR/A.result" ]] && echo 1 || echo 0)"
B="$([[ -f "$OUT_DIR/B.result" ]] && echo 1 || echo 0)"
printf '{"legs_on_binary": %d}\n' "$((A + B))" > "$OUT_DIR/result.json"
cat "$OUT_DIR/result.json"
[[ "$((A + B))" -eq 2 ]] || { echo "wheel-upgrade-path: FAILED (legs_on_binary=$((A+B)))"; exit 1; }
echo "wheel-upgrade-path: both legs landed on the binary"
