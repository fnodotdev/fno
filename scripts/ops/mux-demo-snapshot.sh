#!/usr/bin/env bash
# Shoot the mux for a public page without one byte of a real session in it.
#
#   scripts/ops/mux-demo-snapshot.sh --out shot.svg [--theme light] [--format png] [--size 200x60]
#       [--window-size smallest|latest|largest] [--peer 120x40] [--no-fit]
#       [--hold] [--group <axis>] [--remote-pane] [--web [--web-port <port>]]
#       [--hold] [--group <axis>] [--remote-pane] [--flat-pins] [--sort <col:dir>] [--layout list] [--lead-nesting] [--holds]
#       [--install-card data|code] [--mod-pane review-ledger] [--app [--app-theme <theme>]]
#       [--app-mode chat|terminal]
#
# Builds a throwaway server in a temp state root: its own config, graph,
# agent registry, view prefs and socket dir. It seeds invented backlog nodes,
# leads, mail, feed questions and harness panes, then shoots it with
# `fno mux serve --snapshot --server`, kills the server and deletes the root.
# Every view a paint PR changes renders populated: the command itself refuses an
# empty view render, and the refusal dies here.
set -euo pipefail

FNO="${FNO_BIN:-fno}"
# Resolved before any cd: the shoot runs from inside the throwaway root.
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd -P)"
# Backlog commands exec the fno-agent binary, not this front door. A branch
# build drives its own tree's agents sibling, so the seeded mint (which reads
# FNO_DEMO_ID_SEED there) is the one under test, never the deployed binary.
if [ -n "${FNO_BIN:-}" ] && [ -z "${FNO_AGENT_BIN:-}" ]; then
  SIBLING_DIR="$(cd "$(dirname "$FNO_BIN")/../../.." && pwd)/fno-agent/target"
  SIBLING=""
  for profile in release debug; do
    if [ -x "$SIBLING_DIR/$profile/fno-agent" ]; then SIBLING="$SIBLING_DIR/$profile/fno-agent"; break; fi
  done
  if [ -n "$SIBLING" ]; then
    export FNO_AGENT_BIN="$SIBLING"
  else
    SIBLING="$SIBLING_DIR/release/fno-agent"
    # A branch build without its agents sibling would silently test the
    # deployed mint, and the seeded ids would re-roll. Say so instead.
    echo "mux-demo-snapshot: no fno-agent sibling beside FNO_BIN ($SIBLING missing); backlog ids come from the deployed binary" >&2
  fi
fi
SERVER=demo
# /tmp, not TMPDIR: a socket path must stay under 104 bytes on macOS.
# pwd -P: macOS resolves /tmp to /private/tmp, and HOME must match the real path.
ROOT="$(cd "$(mktemp -d /tmp/fno-demo.XXXXXX)" && pwd -P)"
PANES=()

# The seeded board's volatile strings are pinned so the frames compare sees
# paint, never run noise: one fixed epoch freezes the seed stamps and
# the render clock (digest_overlay::now_secs reads it), a fixed host stand-in
# keeps the live machine's name off the shot, and each idea call carries its
# own id-seed token so the board mints the same ids on every shoot. The epoch
# is a constant, not date-derived: both shoots of a compare must read the same
# stamps, and with the render clock frozen to the same constant the ages never
# age out.
export FNO_DEMO_EPOCH=1794096000
export FNO_DEMO_HOST=demo-host
export FNO_DEMO_ID_SEED=""

# The shot runs from $ROOT/code/checkout, so a relative --out would land in
# the throwaway root and die with it. Absolutize against the caller's cwd.
# --window-size and --no-fit are this script's own: the first sets the demo
# server's mux.window_size policy by env (the server inherits it), the second
# shoots as a passive observer so the shot crops instead of resizing the tab.
# --hold arms a daemon reserve on the demo's breaker record, so the status row
# shows the fleet hold. --group <axis> sets the sideline's group-by axis
# (workspace, team, cwd, status, harness). --remote-pane adds a fourth pane to
# the first tab that shows a session on another machine, so its title wears the
# remote glyph.
ARGS=()
FIT=(--fit)
PREV=""
HOLD=""
GROUP=""
REMOTE_PANE=""
WEB=""
WEB_PORT=8722
APP=""
APP_THEME=""
# --app-mode chat|terminal: which view the app opens on. Default chat.
APP_MODE="chat"
FNO_DEMO_FLATPINS=""
FNO_DEMO_HOLDS=""
FNO_DEMO_DND=""
THEME_ARG=""
FNO_DEMO_SORT=""
# --feed-view <json> seeds the activity feed's saved table view (sort, group,
# filters, folded groups), the shape `view_store::FeedViewPref` reads.
FNO_DEMO_FEED_VIEW=""
FNO_DEMO_LAYOUT=""
FNO_DEMO_LEAD_NESTING=""
# --second-workspace adds `notes` (open), `archive` (folded by hand) and `ledger`
# (mostly exited, so it paints the live-only caret) beside the active `checkout`.
FNO_DEMO_SECOND_WS=""
# --install-card data|code runs a real `fno config plugins install` of a
# throwaway local pack in the first pane, so the shot shows its consent card.
INSTALL_CARD=""
# --mod-pane review-ledger activates the first-party pack and opens that feed
# mod in the first pane over invented review_attestation rows.
MOD_PANE=""
for a in "$@"; do
  if [ "$PREV" = "--out" ]; then
    mkdir -p -- "$(dirname -- "$a")"
    ARGS+=("$(cd "$(dirname -- "$a")" && pwd -P)/$(basename -- "$a")")
  elif [ "$PREV" = "--window-size" ]; then
    export FNO_MUX_WINDOW_SIZE="$a"
  elif [ "$PREV" = "--group" ]; then
    GROUP="$a"
  elif [ "$PREV" = "--web-port" ]; then
    WEB_PORT="$a"
  elif [ "$PREV" = "--sort" ]; then
    FNO_DEMO_SORT="$a"
  elif [ "$PREV" = "--feed-view" ]; then
    FNO_DEMO_FEED_VIEW="$a"
  elif [ "$PREV" = "--layout" ]; then
    FNO_DEMO_LAYOUT="$a"
  elif [ "$PREV" = "--install-card" ]; then
    INSTALL_CARD="$a"
  elif [ "$PREV" = "--app-theme" ]; then
    APP_THEME="$a"
  elif [ "$PREV" = "--app-mode" ]; then
    case "$a" in chat|terminal) APP_MODE="$a" ;; *) echo "mux-demo-snapshot: --app-mode takes chat or terminal, got '$a'" >&2; exit 2 ;; esac
  elif [ "$PREV" = "--mod-pane" ]; then
    MOD_PANE="$a"
  elif [ "$PREV" = "--theme" ]; then
    # Captured AND forwarded: the serve lens takes dark|light|macchiato,
    # while the demo config the client renders through needs the theme's
    # config twin - `light` is no config theme, so the cells would stay
    # dark and only the selection bar would invert.
    THEME_ARG="$a"
    ARGS+=("$a")
  elif [ "$a" = "--window-size" ] || [ "$a" = "--group" ] || [ "$a" = "--web-port" ] || [ "$a" = "--sort" ] || [ "$a" = "--feed-view" ] || [ "$a" = "--layout" ] || [ "$a" = "--install-card" ] || [ "$a" = "--app-theme" ] || [ "$a" = "--app-mode" ] || [ "$a" = "--mod-pane" ]; then
    :
  elif [ "$a" = "--second-workspace" ]; then
    export FNO_DEMO_SECOND_WS=1
  elif [ "$a" = "--lead-nesting" ]; then
    FNO_DEMO_LEAD_NESTING=1
  elif [ "$a" = "--flat-pins" ]; then
    FNO_DEMO_FLATPINS=1
  elif [ "$a" = "--holds" ]; then
    FNO_DEMO_HOLDS=1
  elif [ "$a" = "--dnd" ]; then
    FNO_DEMO_DND=1
  elif [ "$a" = "--hold" ]; then
    HOLD=1
  elif [ "$a" = "--remote-pane" ]; then
    REMOTE_PANE=1
  elif [ "$a" = "--web" ]; then
    WEB=1
  elif [ "$a" = "--app" ]; then
    APP=1
    export FNO_DEMO_APP=1
  elif [ "$a" = "--no-fit" ]; then
    FIT=()
  else
    ARGS+=("$a")
  fi
  PREV="$a"
done
case "$MOD_PANE" in
  ""|review-ledger) ;;
  *) echo "mux-demo-snapshot: --mod-pane supports: review-ledger" >&2; exit 2 ;;
esac
export FNO_DEMO_FLATPINS FNO_DEMO_SORT FNO_DEMO_LAYOUT FNO_DEMO_LEAD_NESTING FNO_DEMO_HOLDS FNO_DEMO_DND
# bash 3.2 (stock macOS) reads an empty array under set -u as unbound.
set -- ${ARGS[@]+"${ARGS[@]}"}

# --app is a thin alias for the shot door now: the door builds its own
# throwaway world, orders the window below the desktop and captures by
# window id, so the hand-rolled visible launch is retired. FNO_APP_BIN
# and --app-theme pass straight through.
if [ -n "$APP" ]; then
  APP_THEME="${APP_THEME:-dark}"
  exec "$FNO" mux app shot --out "${OUT%.*}-app.png" --theme "$APP_THEME"
fi

cleanup() {
  # The serve sites lead their own process groups (see set -m below), so the
  # signal that killed this script never reached them. End them by tracked
  # pid before anything else; TERM reaches the whole serve subtree through
  # the tool calls below, and a serve has no children of its own to orphan.
  for sp in "${WARM_PID:-}" "${SERVE_PID:-}" "${WEB_PID:-}"; do
    [ -n "$sp" ] && kill -TERM "$sp" 2>/dev/null || true
  done
  for p in "${PANES[@]+"${PANES[@]}"}"; do
    "$FNO" mux pane kill "$p" --server "$SERVER" >/dev/null 2>&1 || true
  done
  "$FNO" mux kill-server "$SERVER" --end-unkept --yes >/dev/null 2>&1 || true
  # The app is the only process this rig starts that can exec a harness:
  # the launch's spawn client and, through it, the harness shim. Job
  # control puts the app at the head of its own process group, so one
  # group signal ends the whole launch subtree. Tracked pids only: a name
  # or cwd sweep kills processes another session started (it happened
  # twice on 2026-11-18), so no sweep runs here.
  # kill -0 passes for a wrapped pid too: the recorded pid must still be
  # the app binary, or the group signal would land on a stranger.
  if [ -n "${APP_PID:-}" ] && kill -0 "$APP_PID" 2>/dev/null \
    && [ "$(ps -o comm= -p "$APP_PID" 2>/dev/null)" = "${APP_BIN##*/}" ]; then
    kill -TERM -- "-$APP_PID" 2>/dev/null || true
    local i
    for i in 1 2 3 4 5 6; do
      kill -0 "$APP_PID" 2>/dev/null || break
      sleep 0.5
    done
    kill -KILL -- "-$APP_PID" 2>/dev/null || true
  fi
  if [ -n "${STUB_PID:-}" ]; then
    kill "$STUB_PID" 2>/dev/null || true
  fi
  # Read-only stray proof: anything still naming ROOT in its argv outlived
  # every pid this rig tracks (a launched harness child never does: the app
  # group signal killed it, SIGKILL, uncatchable). The signal may still be
  # landing as the proof runs, so give the dying members a short grace
  # before reading the table; a member mid-death is not a stray. Killing a
  # real survivor by pattern could hit another session's process, so the
  # root stays and the survivors are named instead.
  local gi
  for gi in 1 2 3 4 5 6 7 8; do
    pgrep -f -- "$ROOT/" >/dev/null 2>&1 || break
    sleep 0.25
  done
  if pgrep -f -- "$ROOT/" >/dev/null 2>&1; then
    echo "mux-demo-snapshot: processes this rig did not track still name $ROOT; root kept:" >&2
    pgrep -fl -- "$ROOT/" >&2
    exit 1
  fi
  if [ -n "${FNO_DEMO_KEEP:-}" ]; then echo "kept $ROOT" >&2; else rm -rf "$ROOT"; fi
}
trap cleanup EXIT
# bash runs the EXIT trap only on a normal exit. A Ctrl-C, a closed terminal
# or a cancelled CI job kills the shell outright, and every draw loop keeps
# forking sleep for hours. Route the kill signals through exit, so the
# teardown above always runs.
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

# A run killed with SIGKILL, or lost to a dead battery, never ran cleanup:
# its root stays behind. This rig never reaps another root's processes: a
# name or cwd sweep kills pids another session started (it happened twice
# on 2026-11-18). Name the stale roots for the operator instead.
report_stale_demos() {
  local d m age
  for d in /tmp/fno-demo.*; do
    [ -d "$d" ] || continue
    [ "$d" = "$ROOT" ] && continue
    # python3, not stat: a BSD-style `stat -f %m` on GNU coreutils exits 1
    # while still printing the filesystem stanza of the later operand, and
    # that garbage in the captured value kills the age math.
    m=$(python3 -c 'import os,sys; print(int(os.stat(sys.argv[1]).st_mtime))' "$d" 2>/dev/null) || continue
    age=$(( $(date +%s) - m ))
    [ "$age" -gt 3600 ] || continue
    echo "mux-demo-snapshot: stale demo root older than 1 h, not touched: $d" >&2
  done
}
report_stale_demos

mkdir -p "$ROOT/mux" "$ROOT/agents" "$ROOT/code/checkout" "$ROOT/text" "$ROOT/.fno/claims"
# State under $ROOT/.fno: the mux board reads the graph from $HOME/.fno.
{
  printf 'schema_version = 1\nstate_dir = "%s"\n' "$ROOT/.fno"
  # The two seeded teams lead configured projects: a live team's scope
  # compiles only against work.workspaces projects or graph epics, so the
  # payments and search leads name them here.
  printf '%s\n' \
    '[work.workspaces.checkout]' \
    '[[work.workspaces.checkout.projects]]' \
    'name = "payments"' \
    'path = "~/code/checkout"' \
    '[[work.workspaces.checkout.projects]]' \
    'name = "search"' \
    'path = "~/code/checkout"'
  # Invented accounts for the connections view. No credential exists under
  # these paths, so nothing here can log in or bill.
  printf '%s\n' \
    '[accounts]' \
    'active = "studio"' \
    '[[accounts.records]]' \
    'id = "studio"' \
    'name = "Studio"' \
    'harness = "claude"' \
    'auth = "managed"' \
    'priority = 10' \
    '[[accounts.records]]' \
    'id = "night-shift"' \
    'name = "Night shift"' \
    'harness = "claude"' \
    'auth = "managed"' \
    'priority = 20' \
    '[[accounts.records]]' \
    'id = "codex-lab"' \
    'name = "Codex lab"' \
    'harness = "codex"' \
    'auth = "managed"' \
    'plan = "Pro"' \
    'priority = 30' \
    '[[accounts.records]]' \
    'id = "glm-key"' \
    'name = "GLM key"' \
    'harness = "claude"' \
    'auth = "api_key"' \
    'env = { ANTHROPIC_BASE_URL = "https://api.z.ai/api/anthropic", ANTHROPIC_API_KEY = "demo-placeholder" }' \
    'plan = "GLM Coding Plan Max"' \
    'priority = 40'
  # FNO_DEMO_PREFIX=alt+shift+a shoots the keys list and footer with that prefix.
  # --app-theme pins the palette the desktop app paints, light or dark. Both
  # keys live in one [mux] table: TOML refuses a second header.
  if [ -n "${FNO_DEMO_PREFIX:-}" ] || [ -n "$APP_THEME" ] || [ -n "$THEME_ARG" ]; then
    printf '[mux]\n'
    [ -z "${FNO_DEMO_PREFIX:-}" ] || printf 'prefix = "%s"\n' "$FNO_DEMO_PREFIX"
    # The app knows palette names, not light/dark: footnote-paper paints
    # light, footnote-superscript dark. Anything else falls back dark. The
    # serve lens (--theme dark|light|macchiato) maps to the same twin: the
    # cells are painted through the [mux] theme config key (theme_resolved
    # reads mux.theme), so a lens without its twin leaves dark cells under
    # a light frame and the shot mixes two palettes.
    case "${APP_THEME:-$THEME_ARG}" in
      light) theme=footnote-paper ;;
      dark) theme=footnote-superscript ;;
      *) theme="" ;;
    esac
    [ -z "$theme" ] || printf 'theme = "%s"\n' "$theme"
  fi
  # FNO_DEMO_LAYOUT=list shoots the one-line row instead of the card.
  if [ -n "${FNO_DEMO_LAYOUT:-}" ]; then
    printf '[sideline]\nlayout = "%s"\n' "$FNO_DEMO_LAYOUT"
  fi
} >"$ROOT/config.toml"
# Readers that resolve HOME/.fno/config.toml before FNO_CONFIG see the
# same config at the stock HOME path.
mkdir -p "$ROOT/.fno"
cp "$ROOT/config.toml" "$ROOT/.fno/config.toml"
export FNO_CONFIG="$ROOT/config.toml" FNO_MUX_DIR="$ROOT/mux" FNO_AGENT_HOME="$ROOT/.fno/agents"
# The tab holds three agent panes and the persona column; the drill-past-three
# shot appends record panes, which the default four-pane tab ceiling refuses.
export FNO_MUX_PANE_GROUP_MAX="${FNO_MUX_PANE_GROUP_MAX:-12}"
# The socket pin every daemon client honors: with it set, a client talks to
# THIS socket and never lazy-starts or finds an incumbent daemon on the real
# home. The world's own answerer binds it; nothing here may reach a daemon
# outside the root.
export FNO_SUPERVISOR_SOCKET="$FNO_AGENT_HOME/supervisor.sock"
# The org gather shells `fno-agent` from PATH (org-fold), and the fresh
# root provisions the python leg's executables under .local/bin. Without
# this every fold dies with "could not run" and the org views shoot empty.
export PATH="$ROOT/.local/bin:$PATH"
# The org view reads `fno-agent census --workers --machines --json`. The
# demo root has no fleet primary, and fleet top keeps only seats with a live
# pid, so the org views would shoot empty. This shim answers that one call
# with the invented fleet map seeded below, and hands every other call to the
# real binary. The views resolve fno-agent through FNO_AGENT_BIN first, so
# the shim takes that name and forwards to the binary it named.
REAL_AGENTS="${FNO_AGENT_BIN:-fno-agent}"
mkdir -p "$ROOT/fleetbin" "$ROOT/harnessbin"
cat >"$ROOT/fleetbin/fno-agent" <<SH
#!/bin/sh
if [ "\$*" = "census --workers --machines --json" ]; then exec cat "$ROOT/fleet-top.json"; fi
# Drop this directory wherever it sits in PATH: a process that prepends its
# own directory would otherwise send the call back here forever. Only THIS
# dir is dropped; harnessbin stays on PATH, so every fno-agent child (a
# spawn among them) still resolves the world's harness shims and never a
# real harness binary.
PATH="\$(printf '%s' "\$PATH" | tr ':' '\\n' | grep -vxF "$ROOT/fleetbin" | paste -sd: -)"
export PATH
exec "$REAL_AGENTS" "\$@"
SH
chmod +x "$ROOT/fleetbin/fno-agent"
# The managed demo records would read the macOS Keychain, and its unscoped
# item is the real login on this machine. This shim answers every Keychain
# read with not-found, so the shot never touches a real credential. It sits
# in BOTH dirs: fleetbin is stripped inside fno-agent children, harnessbin
# is not, and either surface may reach it.
printf '#!/bin/sh\nexit 44\n' >"$ROOT/fleetbin/security"
printf '#!/bin/sh\nexit 44\n' >"$ROOT/harnessbin/security"
chmod +x "$ROOT/fleetbin/security" "$ROOT/harnessbin/security"
export PATH="$ROOT/fleetbin:$ROOT/harnessbin:$PATH"
export FNO_AGENT_BIN="$ROOT/fleetbin/fno-agent"
# The claims legacy lock takes a FILESYSTEM root; a fresh root carries the
# graph.db migration tombstone at .fno/claims, which is not a directory and
# crashes the lock. Give the demo's claims their own directory.
export FNO_CLAIMS_ROOT="$ROOT/claimroot"
# The lock nests .fno/claims under the claims root and needs the full path
# to exist before its tempfile lands there.
mkdir -p "$ROOT/claimroot/.fno/claims"
# HOME too, for every process: the server reads harness rosters under it,
# and a real home would list real sessions in the sideline.
export HOME="$ROOT"
# Chat transcripts the server reads live here, never under a real home.
export FNO_CLAUDE_PROJECTS_DIR="$ROOT/transcripts"
# The app launch's trust preflight reads projects[cwd].hasTrustDialogAccepted
# out of the .claude.json the spawn resolves (world HOME, or an account
# record's own config_dir). Seed the world's checkout as trusted under every
# store a launch could read, both spellings of the temp path.
python3 - "$ROOT" <<'PY'
import json, os, sys
root = sys.argv[1]
key = root + "/code/checkout"
canon = os.path.realpath(key)
entry = {"hasTrustDialogAccepted": True}
stores = [root + "/.claude.json"] + [
    root + "/.fno/providers/" + r + "/.claude.json" for r in ("studio", "night-shift")
]
for store in stores:
    os.makedirs(os.path.dirname(store), exist_ok=True)
    try:
        with open(store) as f:
            data = json.load(f)
    except Exception:
        data = {}
    data.setdefault("projects", {}).update({p: dict(entry) for p in (key, canon)})
    with open(store, "w") as f:
        json.dump(data, f)
PY
# The launch execs the harness command from PATH. A real claude would run on
# the demo's invented tokens and reach the real Claude Code spare pool; this
# shim plays one. Two forms matter: a viewer attach paints the invented
# transcript and holds the pane; a --bg create (what a composer launch runs)
# mints the session, answers the `backgrounded` line the spawn client parses,
# writes the transcript the truth probe reads, and holds so the row reads
# alive. It lives in harnessbin, the one shim dir an fno-agent child still
# sees, so no launch can resolve past it.
cat >"$ROOT/harnessbin/claude" <<SH
#!/bin/sh
{
  echo "=== invocation"
  echo "PWD=\$PWD"
  echo "ARGV=\$@"
} >> "\$HOME/shim-dump.txt" 2>/dev/null
SID=""
NAME=""
prev=""
for a in "\$@"; do
  if [ "\$prev" = "--session-id" ]; then SID="\$a"; fi
  if [ "\$prev" = "--name" ]; then NAME="\$a"; fi
  prev="\$a"
done
case "\$*" in
  *--bg*)
    # The composer launch: a bg create. Mint the session, answer the launch
    # line the spawn client parses, write the transcript the truth probe
    # reads, and exit. The real bg create exits after backgrounding too;
    # a held-open stdio pipe would keep every read-to-EOF capture in the
    # spawn client (the mail leg drains a child's stderr) waiting forever,
    # and the row reads alive from the transcript mtime, not a live process.
    [ -n "\$SID" ] || SID="\$(uuidgen | tr 'A-Z' 'a-z')"
    SHORT="\$(printf '%s' "\$SID" | cut -c1-8)"
    NOW="\$(date -u -r "\${FNO_DEMO_EPOCH:-\$(date +%s)}" '+%Y-%m-%dT%H:%M:%S.000Z')"
    if [ -n "\$FNO_CLAUDE_PROJECTS_DIR" ]; then
      DIR="\$FNO_CLAUDE_PROJECTS_DIR/\$(printf '%s' "\$PWD" | tr '/' '-')"
      mkdir -p "\$DIR"
      printf '%s\n' '{"type":"user","uuid":"u-new","sessionId":"'"\$SID"'","timestamp":"'"\$NOW"'","message":{"role":"user","content":"launch the demo task"}}' > "\$DIR/\$SID.jsonl"
      printf '%s\n' '{"type":"assistant","uuid":"a-new","sessionId":"'"\$SID"'","timestamp":"'"\$NOW"'","message":{"role":"assistant","model":"claude-sonnet-5-5","content":[{"type":"text","text":"On it. Starting with the handler and its tests."}],"usage":{"input_tokens":2100,"output_tokens":340}},"stop_reason":"end_turn"}' >> "\$DIR/\$SID.jsonl"
    fi
    # The spawn client verifies a bg create landed before it writes the row:
    # it polls <home>/.claude/sessions for a bg record naming the short id.
    # The real daemon writes that record; the shim plays the daemon here too.
    SESSDIR="\$HOME/.claude/sessions"
    mkdir -p "\$SESSDIR"
    printf '{"jobId":"%s","kind":"bg","sessionId":"%s","pid":%s,"messagingSocketPath":""}\n' "\$SHORT" "\$SID" "\$\$" > "\$SESSDIR/\$SHORT-session.json"
    echo "backgrounded · \$SHORT · \$NAME started"
    exit 0
    ;;
  *attach*|*--resume*|*--teleport*)
    # A viewer attach: paint the invented transcript and hold the pane.
    cat "$ROOT/text/claude.txt"
    while :; do sleep 1; done
    ;;
  *)
    # A probe (daemon status, --version): answer nothing and exit, so the
    # caller's stdout read reaches EOF instead of holding the spawn forever.
    exit 0
    ;;
esac
SH
chmod +x "$ROOT/harnessbin/claude"
# The keys-tags view shows a preset row, a user row and a two-stroke chord,
# so it needs a file. Only that view gets one: a preset would move key hints
# in every other shot. grab-work leaves `g` so the chord can start on it.
case " $* " in
  *" --view keys-tags "*)
    printf '{\n  "preset": "zellij",\n  "split-up": "}",\n  "split-left": "g s",\n  "grab-work": "^"\n}\n' >"$ROOT/.fno/keybindings.json"
    ;;
esac
# The persona overlay reads the persona files the doors validate, resolved
# at the D3440 layout HOME/.fno/profile/customers/<project>/<persona>/PERSONA.md.
# Only the persona views read them; the demo writes clean fixtures, never
# the operator's real profiles.
case " $* " in
  *" --view persona"*|*" --view bell-personas"*)
    mkdir -p "$ROOT/.fno/profile/customers/fno/solo-founder"
    cat >"$ROOT/.fno/profile/customers/fno/solo-founder/PERSONA.md" <<'PSEED'
---
persona: solo-founder
project: fno
status: approved 2026-11-07
confidence: inferred from the launch plan; no outside user interviewed yet
---

# Solo founder

## Who they are
One person building a product with no engineering team behind them.
Time is the scarce resource; they watch the bill.

## Jobs

### ship-first-feature
Turn a product idea into shipped features while doing founder work, without
losing the evening to it. The first paying user decides what ships next, so
each run ends with one visible feature and a note about what it served.

### run-agents-in-parallel
Run several agents so one person ships like a small team.
PSEED
    mkdir -p "$ROOT/.fno/profile/customers/fno/vibe-coder"
    cat >"$ROOT/.fno/profile/customers/fno/vibe-coder/PERSONA.md" <<'PSEED'
---
persona: vibe-coder
project: fno
status: hypothesis, draft awaiting user approval
confidence: inferred from the launch plan; no outside user interviewed yet
---

# Vibe coder

## Who they are
A builder who steers agents in natural language and reads the diff rarely.
Ships from the terminal and judges work by what lands.

## Jobs

### describe-what-to-build
Say what to build in one line and trust the agent on the rest.

### see-what-changed
See what changed and what it costs before it merges.
PSEED
    ;;
esac
# FNO_BOARD_SCOPE is latched per client pane: left in, it filters the invented
# backlog to the caller's own project and the board shoots empty.
unset FNO_SERVER FNO_SESSION FNO_PANE FNO_PANE_EPOCH FNO_OWNER_BIRTH FNO_BOARD_SCOPE
# A harness identity in the caller's env would join the demo's fresh store as
# that real session: every seed call stamps it onto the demo nodes and the
# shot leaks the id (the look flag on the backlog graph paint). Strip the
# identity keys, so the demo store carries only what the seed writes.
unset CLAUDE_CODE_SESSION_ID CLAUDE_SESSION_ID CLAUDECODE CLAUDECODE_SESSION_ID \
  CODEX_THREAD_ID CODEX_SESSION_ID CODEX_COMPANION_SESSION_ID \
  CODEX_COMPANION_TRANSCRIPT_PATH GEMINI_SESSION_ID OPENCODE_SESSION_ID \
  PI_SESSION_ID PI_SESSION_FILE HERMES_SESSION_ID TARGET_SESSION_ID \
  FNO_HARNESS_NAME FNO_HARNESS_SESSION_ID REX_SERVER REX_SESSION REX_BLOCK
# Under an owner session the server lives as long as its owner. A one-shot
# `pane run` never names itself the owner, so this script declares itself,
# and the server dies with us.
export FNO_OWNER_PID=$$

# The usage cache the connections view reads first: invented readings, probed
# now, with reset times ahead.
mkdir -p "$ROOT/.fno/state" "$FNO_AGENT_HOME"
python3 - "$ROOT" "$HOLD" "$GROUP" <<'PY'
import json, os, sys, time
root, hold, group = sys.argv[1:4]
now = int(os.environ["FNO_DEMO_EPOCH"])
def win(label, pct, resets_in):
    return {"label": label, "used_pct": pct, "resets_at": now + resets_in}
usage = {
    "studio": {"probed_at": now, "source": "oauth-endpoint",
               "windows": [win("5h", 38.0, 7900), win("weekly", 61.0, 302400)]},
    "night-shift": {"probed_at": now - 240, "source": "oauth-endpoint",
                    "windows": [win("5h", 4.0, 15800), win("weekly", 22.0, 475200)]},
    "codex-lab": {"probed_at": now - 120, "source": "session-log",
                  "windows": [win("5h", 12.0, 9600), win("weekly", 9.0, 518400)]},
    "glm-key": {"probed_at": now - 180, "source": "api-quota",
                "windows": [win("5h", 71.0, 3600)]},
}
with open(f"{root}/.fno/state/provider-runtime-state.json", "w") as f:
    json.dump({"schema_version": 2, "usage": usage}, f)
# Saved logins for the managed records: invented principals and tiers, and
# tokens that sign in nowhere. The row reads its plan and email from these.
import os
logins = {
    "studio": ("claude", "ada@studio.example", {"subscriptionType": "max", "rateLimitTier": "default_claude_max_20x"}),
    "night-shift": ("claude", "night@ops.example", {"subscriptionType": "pro", "rateLimitTier": "default_claude_ai"}),
    "codex-lab": ("codex", "lab@codex.example", None),
}
for rid, (harness, email, tier) in logins.items():
    d = f"{root}/.fno/providers/{rid}"
    os.makedirs(d, exist_ok=True)
    meta = {"harness": harness, "account_id": rid, "captured_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now - 2 * 86400)), "kind": "keychain",
            "principal": {"account_uuid": f"acc-{rid}", "organization_uuid": "org-demo", "email": email}}
    with open(f"{d}/meta.json", "w") as f:
        json.dump(meta, f)
    blob = {"claudeAiOauth": dict({"accessToken": f"demo-{rid}", "refreshToken": f"demo-{rid}"}, **tier)} if tier else {"tokens": {}}
    with open(f"{d}/blob", "w") as f:
        json.dump(blob, f)
# A managed record reads as active from the slot stamp, not the pointer.
with open(f"{root}/.fno/providers/.active-claude", "w") as f:
    f.write("studio")
stamp = lambda t: time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(t))
if hold:
    with open(f"{root}/.fno/agents/fleet-stop.json", "w") as f:
        json.dump({"version": 1, "state": "stopped", "generation": 1,
                   "changed_at": stamp(now), "changed_by": "machine_watch",
                   "reason": "OBS is running, so the fleet keeps the machine free for stream",
                   "holds": ["spawns", "tests"], "origin": "reserve_auto",
                   "expires_at": stamp(now + 1800)}, f)
view = {}
if group:
    view["agent_group"] = group
if os.environ.get("FNO_DEMO_SECOND_WS"):
    view["sections"] = {f"squad:{root}/code/archive": "collapsed"}
if view:
    with open(f"{root}/.fno/agents/mux-view.json", "w") as f:
        json.dump(view, f)
PY

cd "$ROOT/code/checkout"
# A repo, so every attach resolves the panes' workspace instead of
# minting a second one, and the status row names a branch.
git init -q -b main
if [ -n "$FNO_DEMO_SECOND_WS" ]; then
  for extra in notes archive ledger; do
    git init -q -b main "$ROOT/code/$extra"
  done
fi
# Invented work. The board's lanes come from priority, and its scope from
# the server's project, so these stay unscoped.
IDS=()
file() { # file <priority> <title> : record the id, show the demo's home path
  local id
  # A per-call seed token keeps each idea's mint deterministic and distinct.
  id="$(FNO_DEMO_ID_SEED="node-$(( ${#IDS[@]} + 1 ))" "$FNO" backlog idea "$2" -p "$1" --audience internal --why "demo snapshot seeds its fleet" --done-means "demo node exists" --difficulty medium | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
  "$FNO" backlog update "$id" --set cwd='~/code/checkout' >/dev/null
  IDS+=("$id")
}
# The persona files the customer seeds validate against: one `## Jobs`
# section, one `### <job-slug>` heading per job the demo names. The seeds
# resolve their project as payments before the update sets payments or
# search, so both project dirs carry the file.
for project in payments search; do
  mkdir -p "$ROOT/.fno/profile/customers/$project/solo-founder"
  cat > "$ROOT/.fno/profile/customers/$project/solo-founder/PERSONA.md" <<PEOF
---
persona: solo-founder
project: $project
status: approved 2026-11-07
confidence: inferred from the launch plan; no outside user interviewed yet
---

# Solo founder

## Who they are
A founder shipping alone: they cut scope, not corners, and they read the
board to decide what today's one shipped thing is.

## Jobs

### see-the-demo

See the demo: judge the product in one look, with nothing to install. The
whole demo has to land inside one screen, so anything that needs a second
look waits for the follow-up with the customer's own data.

### ship-the-demo

Ship the demo: get the running product in front of a customer today.
PEOF
done
file_customer() { # file_customer <priority> <title> : one customer-facing seed
  local id
  id="$(FNO_DEMO_ID_SEED="node-$(( ${#IDS[@]} + 1 ))" "$FNO" backlog idea "$2" -p "$1" --audience customer:solo-founder --job solo-founder/see-the-demo --why "the solo founder judges the demo in one look" --done-means "the demo shows the customer audience and its job" --difficulty medium | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
  "$FNO" backlog update "$id" --set cwd='~/code/checkout' >/dev/null
  IDS+=("$id")
}
file p1 "Rate limit the checkout api per key"
file p1 "Retry webhooks with backoff and a dead-letter queue"
file p1 "Cache product search for 60 seconds"
file p1 "Page the on-call when p95 latency passes 800ms"
file_customer p1 "Show order status on the receipt page"
file_customer p1 "Retry a failed webhook delivery with visible backoff for the founder"
file p2 "Add an audit log for refunds"
file p2 "Split the payments module out of the monolith"
file p2 "Remove the legacy coupon service"
file p3 "Move image resizing to a queue worker"
file p3 "Support Apple Pay on the checkout page"
file p2 "Unify the three checkout config surfaces into one documented schema with a migration guide and a deprecation window"
file p3 "Split the merchant onboarding wizard into three steps with save-and-resume and a progress indicator"

# Real teams: every node carries a scope, and the two seeded leads hold
# those scopes.
for i in "${!IDS[@]}"; do
  if [ $((i % 2)) -eq 0 ]; then s=payments; else s=search; fi
  "$FNO" backlog update "${IDS[$i]}" --set project="$s" >/dev/null
done

# The graph shot needs a real family, a cross-project blocker and live nodes;
# every other snapshot keeps the compact, unrelated work fixture above.
GRAPH_VIEW=""
case " $* " in
  *" --view backlog-graph "*)
    GRAPH_VIEW=1
    "$FNO" backlog update "${IDS[0]}" type=epic >/dev/null
    "$FNO" backlog update "${IDS[1]}" parent="${IDS[0]}" "blocked_by+=${IDS[5]}" >/dev/null
    "$FNO" backlog update "${IDS[2]}" parent="${IDS[0]}" >/dev/null
    "$FNO" backlog update "${IDS[3]}" parent="${IDS[1]}" >/dev/null
    "$FNO" backlog update "${IDS[4]}" parent="${IDS[0]}" >/dev/null
    "$FNO" backlog done "${IDS[4]}" --force --reason "demo fixture includes one completed graph child" --note "demo fixture completed child" --skip-stamp >/dev/null
    "$FNO" backlog update "${IDS[5]}" type=epic >/dev/null
    ;;
esac

# The three panes work the first three nodes: a claim moves each node to In
# Progress, and a session row names the harness session its pane runs.
SIDS=(7d1e2f3a-4b5c-4d6e-8f70-81a2b3c4d5e6 0199a1b2-c3d4-7e5f-8a6b-7c8d9e0f1a2b ses_3f9a1c2b7e4dA1b2C3d4E5f6g7)
HARNESSES=(claude codex opencode)
if [ -n "$GRAPH_VIEW" ]; then
  SIDS=(0199a1b2-c3d4-7e5f-8a6b-7c8d9e0f1a2b 0199a1b2-c3d4-7e5f-8a6b-7c8d9e0f1a2c 0199a1b2-c3d4-7e5f-8a6b-7c8d9e0f1a2d)
  HARNESSES=(codex codex codex)
fi
# The first backlog command migrates the store and publishes its tombstone AS
# the claims path: a marker file where the lock needs a directory. The
# marker's backup imported at publish, so retiring the marker and
# recreating the directory hands the path back to the legacy lock writer.
claims="$FNO_CLAIMS_ROOT/.fno/claims"
if [ -f "$claims" ]; then
  rm -f "$claims"
  mkdir -p "$claims"
fi
for i in 0 1 2; do
  # The session row lands first: the acquire resolves the holder's
  # identity from the registry, and an unseeded row reads "no owned
  # identity", skips the claim, and the board's sweep surfaces the gap
  # as a live error in the shot.
  "$FNO" backlog session add "${IDS[$i]}" --phase execute --harness "${HARNESSES[$i]}" --session-id "${SIDS[$i]}" >/dev/null
  "$FNO" agents claim acquire "node:${IDS[$i]}" --holder "target-session:${SIDS[$i]}" --ttl 2h --pid-unavailable >/dev/null
done
# The selected card carries a plan and a rank. --operator: this script is
# the operator's own tool, writing a graph that dies with the shot.
mkdir -p "$ROOT/code/checkout/plans"
printf -- '---\nstatus: ready\n---\n# Rate limit the checkout api per key\n' >"$ROOT/code/checkout/plans/rate-limit.md"
"$FNO" backlog update "${IDS[0]}" 'plan_path=~/code/checkout/plans/rate-limit.md' >/dev/null
"$FNO" backlog rank "${IDS[0]}" --top --operator >/dev/null
# A PR on the first claimed node, so its pane's row carries one and the
# navigator-pr view has a number to find.
"$FNO" backlog update "${IDS[0]}" pr_url=https://github.com/demo/checkout/pull/412 pr_number=412 >/dev/null

# Pane text: an invented transcript per harness, shown by a process that
# never prints a prompt. The cursor is hidden, so no block reads as a glyph.
# Lines stay under 56 columns: the desktop app's narrowest pane fits that.
E=$'\033'
# Each harness's screen: a transcript on top, its own footer at the bottom.
cat >"$ROOT/text/claude.txt" <<EOF
${E}[2m> rate limit the checkout api per key${E}[0m

${E}[97m⏺${E}[0m Read ${E}[1msrc/checkout/handler.ts${E}[0m and
  ${E}[1msrc/middleware/limits.ts${E}[0m

${E}[97m⏺${E}[0m The handler has no limit today. I will add a token
  bucket per api key: 60 requests a minute, and a 429
  with Retry-After when it empties.

${E}[97m⏺${E}[0m ${E}[1mUpdate${E}[0m(src/middleware/limits.ts)
  ⎿  Added ${E}[32m42${E}[0m lines, removed ${E}[31m3${E}[0m lines

${E}[97m⏺${E}[0m ${E}[1mBash${E}[0m(npm test -- limits)
  ⎿  ${E}[32m14 passed${E}[0m

${E}[97m⏺${E}[0m Opened PR 118: rate limit the checkout api per key.
EOF
cat >"$ROOT/text/claude.foot" <<EOF
${E}[2m────────────────────────────────────────────────────────────${E}[0m
${E}[1m❯${E}[0m
${E}[2m────────────────────────────────────────────────────────────${E}[0m
${E}[2m  Opus 5.5 on main · ~/code/checkout${E}[0m
EOF
cat >"$ROOT/text/codex.txt" <<EOF
${E}[1m›${E}[0m retry webhooks with backoff and a dead-letter queue

${E}[2m•${E}[0m ${E}[1mRan${E}[0m ${E}[36mrg -n "deliver\(" src/webhooks${E}[0m
  ${E}[2m└${E}[0m src/webhooks/send.ts:41: await deliver(event)

${E}[2m•${E}[0m Delivery is one attempt today. A failed call is dropped.

${E}[2m•${E}[0m Plan:
  1. Retry 5 times, doubling from 2s.
  2. Move the event to a dead-letter queue after the last try.
  3. Add a replay command for the queue.

${E}[2m•${E}[0m ${E}[1mEdited${E}[0m src/webhooks/send.ts ${E}[32m(+31${E}[0m ${E}[31m-4)${E}[0m

${E}[2m•${E}[0m Working (2m 14s • ${E}[1mesc${E}[0m to interrupt)
EOF
cat >"$ROOT/text/codex.foot" <<EOF
${E}[1m›${E}[0m ${E}[2mAsk Codex to do anything${E}[0m

${E}[2m  GPT-6-Sol medium · ~/code/checkout${E}[0m
EOF
cat >"$ROOT/text/opencode.txt" <<EOF
${E}[1m┃${E}[0m cache product search for 60 seconds

  Search runs the full query on every keypress.
  I added a 60s cache keyed by the normalized query.

  ${E}[32m✓${E}[0m Edit src/search/index.ts ${E}[32m+18${E}[0m ${E}[31m-2${E}[0m

  Running npm test -- search
EOF
cat >"$ROOT/text/opencode.foot" <<EOF
${E}[1m┃${E}[0m ${E}[2mType a message${E}[0m

${E}[2m  zen · opencode · ~/code/checkout${E}[0m
EOF
cat >"$ROOT/text/pi.txt" <<EOF
${E}[1m> page the on-call when p95 passes 800ms${E}[0m

  Added an alert rule and a runbook link.
${E}[32m  3 files changed${E}[0m
EOF
cat >"$ROOT/text/docs.txt" <<EOF
${E}[1m> document the refund audit log${E}[0m

  Drafting docs/refunds.md from the new schema.
EOF
cat >"$ROOT/text/remote.txt" <<EOF
${E}[1m> add a refund endpoint behind the payments flag${E}[0m

  Edited src/payments/refund.ts (+24 -3).
${E}[32m  1 file changed${E}[0m
EOF
: >"$ROOT/text/pi.foot"
: >"$ROOT/text/docs.foot"
: >"$ROOT/text/remote.foot"

# Each pane prints its text, hides the cursor, and prints it again on every
# resize, so the sizing attach below lands on a full screen.
show() { # show <name> : the transcript, then the footer on the last rows
  local t="$ROOT/text/$1"
  printf "draw() { printf '\\\\033[?25l\\\\033[2J\\\\033[H'; cat '%s.txt'; n=\$(wc -l < '%s.foot'); r=\$(stty size | cut -d' ' -f1); printf '\\\\033[%%d;1H' \$((r - n)); cat '%s.foot'; }; trap draw WINCH; draw; while :; do sleep 1; done" "$t" "$t" "$t"
}
run() { # run <args...> : start a pane, record its id
  PANES+=("$("$FNO" mux pane run --server "$SERVER" --json "$@" | python3 -c 'import json,sys; print(json.load(sys.stdin)["pane_id"])')")
}

W=(--workspace checkout)
# The seat a remote session opens in: no registry row, and the registered name
# is the session's name@host address. With --remote-pane it spawns first, so
# the shot's focus lands on the seat and the chosen band lands on the remote
# row the seat folds into.
if [ -n "$INSTALL_CARD" ]; then
  PACK="$ROOT/packsrc/demo-$INSTALL_CARD"
  PANELS="$REPO_ROOT/plugins/fno-panels/mods"
  mkdir -p "$PACK/mods/one"
  cp "$PANELS/review-ledger/preview.png" "$PACK/mods/one/preview.png"
  printf '# Recipe\n\nOpen it from Settings, Mods.\n' > "$PACK/mods/one/RECIPE.md"
  if [ "$INSTALL_CARD" = code ]; then
    RENDER='    run: [sh, -c, "while :; do date; sleep 1; done"]'
    TITLE="Clock"
  else
    RENDER='    feed:
      argv: [fno, backlog, list, --json]
      columns:
        - {key: /id, title: id}
        - {key: /title, title: title}'
    TITLE="Open work"
  fi
  cat > "$PACK/plugin.yaml" <<YAML
id: demo-$INSTALL_CARD
version: 0.1.0
footnote_compat:
  minimum: "0.0.16"
mods:
  - id: one
    title: $TITLE
    summary: A demo mod for the consent card shot.
    preview: mods/one/preview.png
    recipe: mods/one/RECIPE.md
$RENDER
YAML
  git -C "$PACK" init --quiet
  git -C "$PACK" add -A
  git -C "$PACK" -c user.name=demo -c user.email=demo@example.invalid -c commit.gpgsign=false \
    commit --quiet -m "demo pack"
  run --cwd "$ROOT/code/checkout" -- \
    sh -c "'$FNO' config plugins install 'file://$PACK'; while :; do sleep 1; done"
  run --cwd "$ROOT/code/checkout" --tab 1 --at "${PANES[0]}" --split right -- \
    sh -c "$(show codex)"
  RUN_AT="${PANES[1]}"
elif [ -n "$MOD_PANE" ]; then
  # Invented attestations on the frozen clock: mixed verdicts and rounds so the
  # verdict colours show. The feed reads the state root's events journal.
  python3 - "$ROOT" <<'PY'
import json, os, sys, datetime
root = sys.argv[1]
now = datetime.datetime.fromtimestamp(int(os.environ["FNO_DEMO_EPOCH"]), datetime.timezone.utc)
rows = [
    (12, "feature/tide-source", "3c9a1e7", "pass", 2, 0),
    (95, "feature/meter-units", "81d04b2", "fail", 1, 3),
    (240, "fix/rate-limit-retry", "e5f7a60", "pass", 1, 1),
    (610, "feature/queue-drain", "0b72c9d", "fail", 2, 2),
    (1500, "docs/onboarding", "7aa31f4", "pass", 1, 0),
    (2900, "fix/sync-backoff", "d41e8c5", "pass", 2, 0),
]
os.makedirs(f"{root}/.fno", exist_ok=True)
with open(f"{root}/.fno/events.jsonl", "w") as f:
    for mins, branch, head, verdict, rnd, blocking in rows:
        ts = (now - datetime.timedelta(minutes=mins)).strftime("%Y-%m-%dT%H:%M:%SZ")
        f.write(json.dumps({"type": "review_attestation", "ts": ts, "data": {
            "reviewer": "code-review", "branch": branch, "head_sha": head,
            "verdict": verdict, "review_round": rnd,
            "findings_blocking": blocking, "findings_nonblocking": 0}}) + "\n")
PY
  "$FNO" config plugins activate "$REPO_ROOT/plugins/fno-panels" >/dev/null
  run --cwd "$ROOT/code/checkout" -- \
    sh -c "'$FNO' config plugins open fno-panels/review-ledger; while :; do sleep 1; done"
  run --cwd "$ROOT/code/checkout" --tab 1 --at "${PANES[0]}" --split right -- \
    sh -c "$(show codex)"
  RUN_AT="${PANES[1]}"
elif [ -n "$REMOTE_PANE" ]; then
  run --cwd "$ROOT/code/checkout" -- \
    env FNO_AGENT_SELF=kestrel@mac2 sh -c "$(show remote)"
  run --cwd "$ROOT/code/checkout" --tab 1 --at "${PANES[0]}" --split right -- \
    sh -c "$(show codex)"
  RUN_AT="${PANES[1]}"
else
  run --cwd "$ROOT/code/checkout" -- sh -c "$(show codex)"
  RUN_AT="${PANES[0]}"
fi
# The first tab stays the active one, so the three-pane tab comes first.
run --cwd "$ROOT/code/checkout" --tab 1 --at "$RUN_AT" --split right -- sh -c "$(show claude)"
run --cwd "$ROOT/code/checkout" --tab 1 --at "$RUN_AT" --split down -- sh -c "$(show opencode)"
run --cwd "$ROOT/code/checkout" -- sh -c "$(show pi)"
run --cwd "$ROOT/code/checkout" -- sh -c "$(show docs)"
if [ -n "$FNO_DEMO_SECOND_WS" ]; then
  run --cwd "$ROOT/code/notes" -- sh -c "$(show claude)"
  run --cwd "$ROOT/code/archive" -- sh -c "$(show opencode)"
  run --cwd "$ROOT/code/ledger" -- sh -c "$(show claude)"
fi
# The registry seed skips the seat pane: its row count shifts by one.
SEED_OFF=0
if [ -n "$REMOTE_PANE" ] || [ -n "$INSTALL_CARD" ]; then
  SEED_OFF=1
fi
"$FNO" mux tab rename --server "$SERVER" "${W[@]}" --tab 1 --name agents >/dev/null
"$FNO" mux tab rename --server "$SERVER" "${W[@]}" --tab 2 --name alerts >/dev/null
"$FNO" mux tab rename --server "$SERVER" "${W[@]}" --tab 3 --name docs >/dev/null

# The registry rows that name each pane's harness, model and state. The
# fence directory at agents/registry.json marks the table as the owner: the
# rows live in the graph store's registry table, and the document envelope
# they fold into rides beside them in registry_meta. A seed that writes rows
# without the envelope reads as InvalidData, i.e. no registry at all.
# The mux server reads each agent's running cost and tokens from the daemon's
# agent.list RPC. This stub answers that one call: invented readings for the
# seeded rows, and for any row the world GAINED since seeding (a composer
# launch) facts computed from the row's own transcript, so the launched row
# shows model, cost, tokens and age like its neighbors. The server finds it
# at supervisor.sock under FNO_AGENT_HOME; cleanup kills it with the root's
# other processes, because its argv names the root.
cat >"$ROOT/fleetbin/agents-stub.py" <<'PY'
import json, os, socket, struct, sys, time
path, sids = sys.argv[1], sys.argv[2:5]
# The last column is seconds since the agent last acted, so no two read alike.
rows = [
    ("archer", sids[1], 8130, 402000, 64),
    ("scout", sids[0], 4497, 184000, 380),
    ("reviewer", sids[2], 312, 21000, 175),
    ("pager", None, 126, 52000, 840),
    ("scribe", None, 88, 9000, 2290),
]
static_names = {r[0] for r in rows}
agents = []
for name, sid, cost, tokens, age in rows:
    row = {"name": name, "session_cost_cents": cost, "session_tokens": tokens,
           "last_activity_age_s": age}
    if sid:
        row["harness_session_id"] = sid
    agents.append(row)

def launched_rows():
    # Rows the world gained after seeding: a composer launch writes one into
    # the graph store. Its facts come from the transcript the shim wrote.
    out = []
    db = os.environ.get("FNO_DEMO_GRAPH", "")
    projects = os.environ.get("FNO_CLAUDE_PROJECTS_DIR", "")
    epoch = int(os.environ.get("FNO_DEMO_EPOCH", "0") or 0)
    if not db or not projects:
        return out
    import sqlite3
    try:
        conn = sqlite3.connect(db)
        for ident, payload in conn.execute("SELECT identity, payload FROM registry"):
            try:
                src = json.loads(payload)
            except ValueError:
                continue
            name = src.get("name")
            if not name or name in static_names:
                continue
            row = {"name": name}
            sid = src.get("harness_session_id") or ""
            if sid:
                row["harness_session_id"] = sid
            transcript = os.path.join(
                projects, (src.get("cwd") or "").replace("/", "-"), sid + ".jsonl"
            ) if sid and src.get("cwd") else ""
            if transcript and os.path.exists(transcript):
                row["last_activity_age_s"] = max(0, epoch - int(os.stat(transcript).st_mtime))
                inp = outp = 0
                model = None
                with open(transcript) as f:
                    for line in f:
                        try:
                            j = json.loads(line)
                        except ValueError:
                            continue
                        msg = j.get("message") or {}
                        usage = msg.get("usage") or {}
                        inp += usage.get("input_tokens") or 0
                        outp += usage.get("output_tokens") or 0
                        model = msg.get("model") or model
                row["session_tokens"] = inp + outp
                row["session_cost_cents"] = round(inp * 3e-4 + outp * 15e-4, 2)
                if model:
                    row["observed_model"] = {"kind": "observed", "model": model}
            out.append(row)
        conn.close()
    except Exception:
        pass
    return out

def answer():
    return {"id": 1, "result": {"agents": agents + launched_rows()}}

err = json.dumps({"id": 1, "error": "demo stub answers agent.list only"}).encode()
if os.path.exists(path):
    os.unlink(path)
srv = socket.socket(socket.AF_UNIX)
srv.bind(path)
srv.listen(8)
while True:
    conn, _ = srv.accept()
    head = conn.recv(4)
    if len(head) == 4:
        want = struct.unpack("<I", head)[0]
        req = b""
        while len(req) < want:
            chunk = conn.recv(want - len(req))
            if not chunk:
                break
            req += chunk
        # Only agent.list is answered; every other call is refused so the
        # server falls back to its own file reads for them.
        try:
            method = json.loads(req).get("method")
        except ValueError:
            method = None
        if method == "agent.list":
            out = json.dumps(answer()).encode()
        else:
            out = err
        conn.sendall(struct.pack("<I", len(out)) + out)
    conn.close()
PY
# Only the desktop app shots get it: the mux shots keep their cards as they were.
if [ -n "$APP" ]; then
  FNO_DEMO_GRAPH="$ROOT/.fno/db/graph.db" \
    python3 "$ROOT/fleetbin/agents-stub.py" "$FNO_AGENT_HOME/supervisor.sock" "${SIDS[@]}" >/dev/null 2>&1 &
  STUB_PID=$!
  # The launch's post-create preflight reads Claude's daemon roster and
  # proves its supervisor pid is a live process holding a worker whose
  # ptySock walks up to a control.sock. No real daemon exists here, so the
  # seed plays one: this script's pid (alive for the whole shot window) and
  # scout's session behind a control.sock the seed creates.
  DAEMOND="$ROOT/.claude/daemon"
  mkdir -p "$DAEMOND/fake-pty/spare"
  : > "$DAEMOND/fake-pty/control.sock"
  python3 - "$DAEMOND/roster.json" "$$" "$ROOT/code/checkout" <<'PY'
import json, os, sys
path, pid, cwd = sys.argv[1], int(sys.argv[2]), sys.argv[3]
base = path.rsplit("/", 1)[0]
worker = {
    "sessionId": "7d1e2f3a-4b5c-4d6e-8f70-81a2b3c4d5e6",
    "pid": pid, "cwd": cwd,
    "ptySock": base + "/fake-pty/spare/7d1e2f3a-4b5c-4d6e-8f70-81a2b3c4d5e6.sock",
}
roster = {
    "proto": 1,
    "supervisorPid": pid,
    "updatedAt": int(os.environ.get("FNO_DEMO_EPOCH", "0") or 0),
    "workers": {"7d1e2f3a": worker},
}
with open(path, "w") as f:
    json.dump(roster, f)
PY
fi
python3 - "$ROOT/.fno/db/graph.db" "$SERVER" "$ROOT/code/checkout" "$SEED_OFF" "${SIDS[@]}" "${PANES[@]}" <<'PY'
import json, os, sys, datetime
db, server, cwd, seed_off = sys.argv[1:5]
sids = sys.argv[5:8]
ids = [int(p) for p in sys.argv[8:]]
off = int(seed_off)
now = datetime.datetime.fromtimestamp(
    int(os.environ["FNO_DEMO_EPOCH"]), datetime.timezone.utc
).strftime("%Y-%m-%dT%H:%M:%SZ")
def row(name, harness, model, pane, state, sid=None):
    return {"harness_session_id": sid} | {
        "name": name, "cwd": cwd, "harness": harness, "model": model,
        "status": "live", "liveness": "alive", "liveness_measured_at": now,
        "created_at": now, "last_message_at": now, "substrate": "pane",
        "mux": {"session": server, "pane_id": pane},
        "inside_leg": {"state": state, "seq": 1, "received_at": now},
    }
def thread(name, harness, how, attach=None):
    # idle: a live thread with no report; unmeasured: exited with no proof;
    # exited: a confirmed exit. `attach` lands as the row's short jobId: the
    # claude attach form reads `short_id` off the row, and that is what makes
    # it an attachable bg thread - the row-menu's Split Direction branch
    # renders for exactly that shape.
    exited = how != "idle"
    return {
        "name": name, "cwd": cwd, "harness": harness, "substrate": "thread",
        "status": "exited" if exited else "live",
        "liveness": {"idle": "alive", "unmeasured": "unmeasured", "exited": "dead"}[how],
        "liveness_measured_at": now, "created_at": now, "last_message_at": now,
        "mux": None,
    } | ({} if attach is None else {"short_id": attach})
agents = [
    # archer holds submitted mail mid-turn, so the sideline paints its held
    # badge; the oldest wait is 12 minutes before the demo epoch.
    row("archer", "codex", "gpt-6-sol", ids[0 + off], "working", sids[1])
    | {
        "context_used_pct": 62,
        "mail_held": 8,
        "mail_held_since": datetime.datetime.fromtimestamp(
            int(os.environ["FNO_DEMO_EPOCH"]) - 12 * 60, datetime.timezone.utc
        ).strftime("%Y-%m-%dT%H:%M:%SZ"),
    },
    row("scout", "claude", "opus", ids[1 + off], "done", sids[0]) | {"context_used_pct": 41},
    row("reviewer", "opencode", "zen", ids[2 + off], "working", sids[2]) | {"context_used_pct": 23},
    # Blocked, so the strip's need-input count and the Input word render.
    # The literal sid gives the first row a session, so its row menu shows
    # the pin and the do-not-disturb arm every session-carrying row offers.
    row("pager", "pi", "glm-5", ids[3 + off], "blocked",
        "6b65e7e1-0000-4000-8000-00000000d5b3") | {"context_used_pct": 88},
    row("scribe", "claude", "sonnet", ids[4 + off], "done") | {"context_used_pct": 15},
    # Paneless threads, so the sideline shows the other states too.
    # Leads dispatch and never host a pane: attachable thread rows.
    thread("rowan", "claude", "idle", attach="demo-lead-1") | {"role_level": 1, "role_scope": "payments"},
    thread("wren", "codex", "idle", attach="demo-lead-2") | {"role_level": 1, "role_scope": "search"},
    thread("planner", "codex", "idle", attach="demo-attach-1"),
    thread("indexer", "claude", "unmeasured"),
    thread("migrator", "opencode", "exited"),
]
# --flat-pins adds the pin-order fixture: two pinned leads with
# workers and one unpinned lead. The names sort atlas, orion, vega, so a
# name sort that ignored pins would open on the unpinned row.
# The app shots age each agent: minutes since its last message and since it
# started, so no two rows read the same and none reads 0s.
if os.environ.get("FNO_DEMO_APP"):
    epoch = int(os.environ["FNO_DEMO_EPOCH"])
    def ago(mins):
        return datetime.datetime.fromtimestamp(
            epoch - mins * 60, datetime.timezone.utc
        ).strftime("%Y-%m-%dT%H:%M:%SZ")
    ages = {"archer": (1, 47), "scout": (6, 132), "reviewer": (3, 21),
            "pager": (14, 95), "scribe": (38, 260), "rowan": (9, 410),
            "wren": (22, 305), "planner": (64, 180), "indexer": (120, 600),
            "migrator": (240, 900)}
    for a in agents:
        if a["name"] in ages:
            last, up = ages[a["name"]]
            a["last_message_at"], a["created_at"] = ago(last), ago(up)
if os.environ.get("FNO_DEMO_FLATPINS"):
    # Working, so no idle fold hides a row the shot exists to show.
    live = {"inside_leg": {"state": "working", "seq": 1, "received_at": now}}
    def lead(name, sid):
        return thread(name, "claude", "idle", attach="demo-" + name) | live | {
            "role_level": 1, "role_scope": "payments", "harness_session_id": sid}
    def worker(name, parent_sid, sid):
        return thread(name, "codex", "idle", attach="demo-" + name) | live | {
            "lineage_kind": "child", "spawned_by_session": parent_sid,
            "harness_session_id": sid}
    agents += [
        lead("atlas", "demo-sid-atlas"),
        worker("atlas-w1", "demo-sid-atlas", "demo-sid-atlas-w1"),
        lead("orion", "demo-sid-orion"),
        worker("orion-w1", "demo-sid-orion", "demo-sid-orion-w1"),
        worker("orion-w2", "demo-sid-orion", "demo-sid-orion-w2"),
        lead("vega", "demo-sid-vega"),
        worker("vega-w1", "demo-sid-vega", "demo-sid-vega-w1"),
    ]
# --holds adds the hold fixture: keeper carries a live conversation hold
# (the fno-mail hold record), sentinel carries the user-set do-not-disturb
# hold the row menu's arm writes, sentry takes mail only (the register
# door's bus-only capability), and eval-once is a headless one-shot that
# paints no row.
# The board's reply session: a registry name the requests fold resolves,
# headless so no sideline row shifts.
agents += [
    thread("t-board-worker", "claude", "idle")
    | {"inside_leg": {"state": "working", "seq": 1, "received_at": now},
       "substrate": "headless",
       "harness_session_id": "2222bbbb-2222-4bbb-9bbb-bbbbbbbbbb2"},
]
if os.environ.get("FNO_DEMO_HOLDS"):
    live = {"inside_leg": {"state": "working", "seq": 1, "received_at": now}}
    hold_dir = os.path.join(os.path.dirname(db), "mail", "mail-hold")
    os.makedirs(hold_dir, exist_ok=True)
    # The badge overlay reads the real wall clock, not the frozen demo
    # epoch, so the records run a month out or every shoot renders them
    # expired and the marks vanish.
    until = datetime.datetime.fromtimestamp(
        int(os.environ["FNO_DEMO_EPOCH"]) + 30 * 86400, datetime.timezone.utc
    ).strftime("%Y-%m-%dT%H:%M:%SZ")
    ceiling = until
    keeper_sid = "6b65e7e1-0000-4000-8000-00000000d3e0"
    with open(os.path.join(hold_dir, keeper_sid + ".json"), "w") as f:
        json.dump({"session": keeper_sid, "until": until, "set_by": "conversation",
                   "reason": "conversation", "clock": {"kind": "wall"}}, f)
    sentinel_sid = "6b65e7e1-0000-4000-8000-00000000d4a7"
    with open(os.path.join(hold_dir, sentinel_sid + ".json"), "w") as f:
        json.dump({"session": sentinel_sid, "until": ceiling, "set_by": "user",
                   "reason": "sideline_menu", "clock": {"kind": "wall"}}, f)
    agents += [
        thread("keeper", "claude", "idle", attach="demo-keeper") | live | {
            "harness_session_id": keeper_sid},
        thread("sentinel", "claude", "idle", attach="demo-sentinel") | live | {
            "harness_session_id": sentinel_sid},
        thread("sentry", "codex", "idle", attach="demo-sentry") | live | {
            "delivery_policy": "bus-only", "harness_session_id": "demo-sid-sentry"},
        thread("eval-once", "claude", "idle") | live | {"substrate": "headless"},
    ]
# --dnd seeds only the pager user hold: the off/armed pair shoots the
# SAME sidebar with one row toggled, never two different fleets.
if os.environ.get("FNO_DEMO_DND"):
    pager_sid = "6b65e7e1-0000-4000-8000-00000000d5b3"
    hold_dir = os.path.join(os.path.dirname(db), "mail", "mail-hold")
    os.makedirs(hold_dir, exist_ok=True)
    ceiling = datetime.datetime.fromtimestamp(
        int(os.environ["FNO_DEMO_EPOCH"]) + 30 * 86400, datetime.timezone.utc
    ).strftime("%Y-%m-%dT%H:%M:%SZ")
    with open(os.path.join(hold_dir, pager_sid + ".json"), "w") as f:
        json.dump({"session": pager_sid, "until": ceiling, "set_by": "user",
                   "reason": "sideline_menu", "clock": {"kind": "wall"}}, f)
if os.environ.get("FNO_DEMO_SECOND_WS"):
    ledger = cwd.rsplit("/", 1)[0] + "/ledger"
    agents += [
        thread("sweeper", "claude", "exited") | {"cwd": ledger},
        thread("rotator", "opencode", "exited") | {"cwd": ledger},
        thread("courier", "codex", "exited") | {"cwd": ledger},
    ]
import sqlite3
conn = sqlite3.connect(db)
# The registry rides the graph store the commands migrated to .fno/db. Its
# tables are created lazily by the first Rust registry writer, so the seed
# matches that DDL instead of assuming they exist.
if not conn.execute(
    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='nodes')"
).fetchone()[0]:
    sys.exit(f"registry seed: no nodes table in {db}; the live store is missing")
conn.executescript(
    """
    CREATE TABLE IF NOT EXISTS registry (
        identity TEXT PRIMARY KEY,
        ordinal INTEGER NOT NULL,
        payload TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS registry_meta (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    """
)
records = []
for i, agent in enumerate(agents):
    sid = agent.get("harness_session_id")
    if sid:
        identity = f"session:{agent['harness']}:{sid}"
    else:
        fid = agent.get("fno_id") or f"fno-demo-{agent['name']}"
        agent["fno_id"] = fid
        identity = f"fno:{fid}"
    records.append((identity, i, json.dumps(agent)))
conn.execute("DELETE FROM registry")
conn.executemany(
    "INSERT INTO registry(identity,ordinal,payload) VALUES (?,?,?)", records
)
# The document envelope the reader folds the rows into, plus the imported
# marker the ownership probe checks: without them the read falls back to
# the legacy file and the client serves live panes only.
conn.executemany(
    "INSERT INTO registry_meta(key,value) VALUES (?,?) "
    "ON CONFLICT(key) DO UPDATE SET value=excluded.value",
    [
        ("document", json.dumps({"schema_version": 1, "agents": []})),
        ("imported", "1"),
    ],
)
row = conn.execute(
    "SELECT value FROM registry_meta WHERE key='revision'"
).fetchone()
rev = int(row[0]) + 1 if row and str(row[0]).isdigit() else 1
conn.execute(
    "INSERT INTO registry_meta(key,value) VALUES ('revision',?) "
    "ON CONFLICT(key) DO UPDATE SET value=excluded.value",
    (str(rev),),
)
conn.commit()
conn.close()
PY

# The fleet map the org view reads: this machine and a second one. Three
# local seats carry the pane sessions above, so they join their registry
# rows. Each seat's parent is its spawner's session id prefix: rowan (the mux
# lead) and wren (the fleet lead) spawned the rest, and reviewer was spawned
# by archer, so it walks up to wren. One remote seat comes from an old daemon,
# publishes no seat facts and reaches no lead.
python3 - "$ROOT/fleet-top.json" "${SIDS[@]}" "${IDS[@]}" <<'PY'
import json, os, sys, datetime
out = sys.argv[1]
sids = sys.argv[2:5]
ids = sys.argv[5:]
now = datetime.datetime.fromtimestamp(
    int(os.environ["FNO_DEMO_EPOCH"]), datetime.timezone.utc
)
def ts(mins_ago):
    return (now - datetime.timedelta(minutes=mins_ago)).strftime("%Y-%m-%dT%H:%M:%SZ")
MUX_LEAD = "a0d1e2f3-demo-mux-lead"
FLEET_LEAD = "b1e2f3a4-demo-fleet-lead"
def seat(name, harness, model, project, status, mins, sid=None, node=None, role=None, short=None, parent=None, workspace=None, needs=None, pinned=None, cost=None, tokens=None, compactions=None, tail=None):
    return {
        "workspace": workspace, "needs_input": needs, "pinned": pinned,
        "cost_cents": cost, "tokens": tokens, "compactions": compactions, "last_message": tail,
        "name": name, "session": short or (sid or name)[:8], "harness": harness, "node": node,
        "status": status, "age_s": mins * 60, "rss_mb": 410.0,
        "parent": parent[:8] if parent else None,
        "session_id": sid, "model": model, "model_basis": "registry",
        "started_at": ts(mins + 95), "last_message_at": ts(mins), "cwd": "~/code/checkout",
        "project": project, "role": role, "context_pct": 38, "parent_at": None,
    }
def machine(mid, host, local, age, sessions):
    return {
        "machine_id": mid, "host": host, "owner": "demo", "role": "worker",
        "beat_at": int(now.timestamp()) - age, "cpu": None, "mem_used": 0.62,
        "mem_pressure": 1, "watts": None, "gpu": None, "verdict": None, "cap": 16,
        "ceiling": 24, "takes_work": True, "fresh": True, "age_s": age, "local": local,
        "sessions": sessions,
    }
local = [
    seat("rowan", "claude", "opus", "payments", "quiet", 12, sid=MUX_LEAD, role="lead", short="demo-ld1"),
    seat("sentry", "claude", "haiku", "payments", "blocked", 7, short="demo-bk1", parent=MUX_LEAD),
    seat("scout", "claude", "opus", "payments", "writing", 1, sid=sids[0], node=ids[0], parent=MUX_LEAD),
    seat("pager", "pi", "glm-5", "payments", "parked", 150, node=ids[4], short="demo-pg1", parent=MUX_LEAD),
    seat("wren", "codex", "gpt-6-sol", "search", "quiet", 30, sid=FLEET_LEAD, role="lead", short="demo-ld2"),
    seat("archer", "codex", "gpt-6-sol", "search", "writing", 2, sid=sids[1], node=ids[1], parent=FLEET_LEAD),
    seat("reviewer", "opencode", "zen", "search", "parked", 9, sid=sids[2], node=ids[2], parent=sids[1]),
]
remote = [
    seat("kestrel", "claude", "sonnet", "payments", "writing", 3, sid="4a1b2c3d-remote-0001", node=ids[6], parent=MUX_LEAD, workspace="payments-api", needs=False, pinned=True, cost=412, tokens=184000, compactions=1, tail="Edited src/payments/refund.ts +24 -3"),
    seat("heron", "codex", "gpt-6-sol", "payments", "quiet", 44, sid="5b2c3d4e-remote-0002", node=ids[8], parent=FLEET_LEAD, workspace="payments-api", needs=True, pinned=False, cost=126, tokens=52000, compactions=3, tail="Keep the legacy webhook, or drop it with the refund rewrite?"),
    seat("ibis", "opencode", "zen", "search", "writing", 5, sid="6c3d4e5f-remote-0003", node=ids[3], parent=MUX_LEAD, workspace="search-index", needs=False, pinned=False, cost=88, tokens=41000, tail="Reindexing products: 12,408 of 30,000"),
    {"name": "plover", "session": "7d4e5f60", "harness": "claude", "node": None,
     "status": "unknown", "age_s": None, "rss_mb": None, "parent": None},
]
fleet = {"fresh_s": 300, "machines": [
    machine("demo-u", "demo-host", True, 4, local),
    machine("demo-mac2", "mac2", False, 21, remote),
]}
open(out, "w").write(json.dumps(fleet))
PY

# Mail and feed events. The bell, the Messages tab and the activity feed read
# stores the board fixture never wrote, so those views shot empty. Both are
# append-only JSONL with a documented row shape, so the seed writes them
# directly. The projections resolve the stores under the config state dir:
# <state>/chats/<chat>/messages.jsonl and <state>/questions.jsonl.
python3 - "$ROOT" "${SIDS[0]}" "${IDS[0]}" <<'PY'
import json, os, sys, datetime
root, scout_sid, rate_limit_node = sys.argv[1], sys.argv[2], sys.argv[3]
now = datetime.datetime.fromtimestamp(
    int(os.environ["FNO_DEMO_EPOCH"]), datetime.timezone.utc
)

def ts(mins_ago):
    return (now - datetime.timedelta(minutes=mins_ago)).strftime("%Y-%m-%dT%H:%M:%SZ")

# Chats store: from/to join the registry rows above, so threads resolve to
# seeded agents. The fleet: broadcast lands on the bell's Announcements tab.
chats = f"{root}/.fno/db/mail/chats"

def msg(i, mins_ago, frm, to, body):
    return {"type": "message", "kind": "send", "v": 1, "id": f"fmail-demo-{i}",
            "ts": ts(mins_ago), "thread": f"chat-demo-{i}", "from": frm, "to": to,
            "body": body}

def write(chat, rows):
    os.makedirs(f"{chats}/{chat}", exist_ok=True)
    with open(f"{chats}/{chat}/messages.jsonl", "w") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")

def state(i, mins_ago, word, reason=None):
    # A delivery-state line: what the recipient's harness did with a send.
    return {"type": "delivery", "id": f"fmail-demo-{i}", "how": word, "reason": reason,
            "ts": ts(mins_ago)}

# Every delivery state a sent row shows. A chat whose last row is parked or
# failed waits on the sender, so the chat list marks it with `!`.
write("chat-demo-review", [
    msg(1, 9, "rowan", "scout", "Rate-limit review pass today? PR 118 is up."),
    state(1, 9, "accepted-new-turn"),
    msg(2, 6, "scout", "rowan", "Landed: 60 req/min per key, the 429 carries Retry-After."),
    # A failed send the user retried with r, and it then delivered.
    msg(11, 5, "rowan", "scout", "Pin the limit in the README too."),
    state(11, 5, "failed", "not-confirmed"),
    state(11, 4, "accepted-new-turn"),
    msg(7, 2, "rowan", "scout", "Good. Add the 429 case to the journey test before merge."),
    state(7, 2, "parked"),
    msg(9, 1, "rowan", "scout", "And tag the release once CI is green."),
    state(9, 1, "failed", "not-confirmed"),
    # The r retry failed too, so the line offers the copy rung.
    state(9, 0, "failed", "waited-3s;no-pane-ref"),
    # A first failure: its cause and r retry, no copy rung yet.
    msg(10, 0, "rowan", "scout", "Then post the 429 shot on the visual page."),
    state(10, 0, "failed", "not-confirmed"),
    # Inline markdown the reading pane renders: **bold** and `code` paint
    # their spans, never the markers.
    msg(12, 0, "scout", "rowan", "**Settings:** run `fno doctor test` locally, then push."),
    state(12, 0, "accepted-new-turn"),
])
write("chat-demo-keys", [
    msg(8, 4, "rowan", "archer", "Rotate the staging key once the deploy is green."),
    state(8, 4, "failed", "bus-only"),
])
write("chat-demo-scope", [
    msg(3, 21, "wren", "archer", "Webhook retry landed behind a flag; the dead-letter queue drains nightly."),
])
# Broadcasts: an announce row lands on the bell (fleet-incident) or the #fno
# channel (a lead's team row). A plain send to fleet: never does, and a pair
# row it is not, so the demo seeds real announce rows.
write("chat-demo-broadcast", [
    {"type": "message", "kind": "announce", "v": 1, "id": "fmail-demo-5",
     "ts": ts(2), "from": "fno/fleet-incident", "to": "fleet:fno",
     "meta": {"scope": "fno"},
     "body": "Test hold lifted at noon; CI covers the fleet."},
    {"type": "message", "kind": "announce", "v": 1, "id": "fmail-demo-6",
     "ts": ts(2), "from": "rowan", "to": "fleet:fno",
     "meta": {"scope": "fno"},
     "body": "Demo lanes reseeded; workers start on the top three cards after the shot."},
])

# Questions journal: the feed's questions leg (operator_question,
# operator_decision envelopes). The first question is the full card: asker,
# node, options and every context field, so the page shows zero missing.
# The second is the bare fallback card.
import struct, zlib
def png(path, w, h, rgb):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    raw = b"".join(b"\x00" + bytes(rgb) * w for _ in range(h))
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))
os.makedirs(f"{root}/screens", exist_ok=True)
shot = f"{root}/screens/q-demo-a1-1-bell.png"
png(shot, 160, 90, (40, 120, 200))
qs = [
    {"ts": ts(4), "type": "operator_question", "source": "target",
     "data": {"question_id": "q-demo-a1", "question": "Hold the 18:00 demo until the rate-limit shot lands?",
              "asker": "scout", "node": rate_limit_node,
              "audience": "customer:solo-founder", "why": "the demo is the solo founder's onboarding story",
              "job": ["solo-founder/see-the-demo"],
              "options": [
                  {"n": 1, "text": "Hold the demo for the shot",
                   "next": "reschedule the slot once the shot lands",
                   "pros": ["the demo shows the real card"], "cons": ["the slot slips about an hour"]},
                  {"n": 2, "text": "Run the demo on the old card",
                   "next": "review the provenance card on the next paint PR",
                   "pros": ["the slot holds"], "cons": ["the provenance fields stay unreviewed"]}],
              "origin": {"handle": "scout", "session_id": scout_sid, "name": "scout",
                         "harness": "claude", "host": "demo-host"},
              "rung": {"level": "user"},
              "via": [{"level": "lead", "holder_name": "rowan", "evidence": "the demo script reads no machine state"}],
              "context": {"blocked_because": "the paint gate holds the run until the look clears",
                          "options_rationale": "holding shows the real pixels; running early demos unreviewed paint",
                          "recommendation": {"option": 1, "why": "the demo is the only shared audience this week",
                                             "downside": "the slot slips if the shot overruns"},
                          "unknowns": "whether the projector renders the dark theme",
                          "reversible": "costly",
                          "cost_if_wrong": "the 18:00 slot is lost for the week",
                          "meanwhile": "stops",
                          "tried": "Two lead-rung reviews passed; the paint gate still holds the run.",
                          "evidence": "The 18:00 slot is the only shared window this week.",
                          "deadline": ts(-3600), "on_silence": "proceed"},
              "class": "irreversible"}},
    {"ts": ts(14), "type": "operator_question", "source": "target",
     "data": {"question_id": "q-demo-b2", "question": "Keep the demo root's mail store after the shot run?",
              "audience": "internal", "why": "the demo root is this project's own operation"}},
    {"ts": ts(26), "type": "operator_decision", "source": "target",
     "data": {"decision_id": "d-demo-c3", "decision": "Ship the seeded shots behind the paint gate",
              "audience": "internal", "why": "the shots serve this project's own review"}},
]
with open(f"{root}/.fno/questions.jsonl", "w") as f:
    for q in qs:
        f.write(json.dumps(q) + "\n")
# Seed the short numbers, so the shots show them on the bell and the card.
import sqlite3
numbers = sqlite3.connect(f"{root}/.fno/db/graph.db")
numbers.execute("CREATE TABLE IF NOT EXISTS fno_numbers (kind TEXT NOT NULL, num INTEGER NOT NULL, "
                "key TEXT NOT NULL, store TEXT NOT NULL, host TEXT NOT NULL, created_at INTEGER NOT NULL, "
                "PRIMARY KEY (store, kind, num), UNIQUE (store, key))")
for num, key in ((87, "q-demo-a1"), (88, "q-demo-b2")):
    numbers.execute("INSERT OR IGNORE INTO fno_numbers VALUES ('q', ?, ?, 'local', 'u', 0)", (num, key))
# Node numbers too, so the board cards show the number beside the hash the
# way the primary mints them in production. Plain rowid order is arrival
# order on the real store, and the stubbed reap-test table carries no
# created_at column to order by.
for num, (nid,) in enumerate(numbers.execute("SELECT id FROM nodes"), 1):
    kind = "n:" + nid.split("-", 1)[0]
    numbers.execute("INSERT OR IGNORE INTO fno_numbers VALUES (?, ?, ?, 'local', 'u', 0)", (kind, num, nid))
numbers.commit()
numbers.close()
PY

# The cargo build marks: the state-root table folds db/build-marks under the
# state root, so the seed writes the daemon arm's sidecar shape straight
# into the demo root: scout compiling, archer waiting at position 2, the
# rest idle. The server's read loop stamps the rows from these exactly as
# it does in production.
mkdir -p "$ROOT/.fno/db/build-marks"
python3 - "$ROOT" "${SIDS[1]}" "${SIDS[2]}" <<'PY'
import json, os, sys, time
root, archer_sid, reviewer_sid = sys.argv[1:4]
now = time.strftime(
    "%Y-%m-%dT%H:%M:%SZ", time.gmtime(int(os.environ["FNO_DEMO_EPOCH"]))
)
marks = {
    archer_sid: {"state": "compiling"},
    reviewer_sid: {"state": "waiting", "position": 2},
}
d = f"{root}/.fno/db/build-marks"
os.makedirs(d, exist_ok=True)
for sid, body in marks.items():
    with open(f"{d}/{sid.lower()}.json", "w") as f:
        json.dump({"state": body["state"], "position": body.get("position"),
                   "at": now, "ttl_ms": 30000}, f)
PY

# FNO_DEMO_PIN=1 pins the scout row: a keep marker on its session id, the
# file `fno agent reap --keep` writes.
if [ -n "${FNO_DEMO_PIN:-}" ]; then
  mkdir -p "$FNO_AGENT_HOME/keep"
  printf '{"session_id":"%s","name":"scout","set_at":"2026-10-28T12:00:00Z","set_by":"human"}' \
    "${SIDS[0]}" >"$FNO_AGENT_HOME/keep/${SIDS[0]}.json"
fi

# --flat-pins pins the orion and vega leads (see the registry seed).
if [ -n "${FNO_DEMO_FLATPINS:-}" ]; then
  mkdir -p "$FNO_AGENT_HOME/keep"
  for n in orion vega; do
    printf '{"session_id":"demo-sid-%s","name":"%s","set_at":"2026-10-28T12:00:00Z","set_by":"human"}' \
      "$n" "$n" >"$FNO_AGENT_HOME/keep/demo-sid-$n.json"
  done
fi

# Feed rows across a day and a half, so the activity feed's time column
# shows order and its harness, node and area columns carry real values:
# spawns land in the agents journal, stalls in the main store journal. Both
# ride the frozen clock, so a reshoot paints the same stamps.
python3 - "$ROOT" "$FNO_AGENT_HOME" "${IDS[@]}" <<'PY'
import json, os, sys, datetime
root, agents_home, ids = sys.argv[1], sys.argv[2], sys.argv[3:]
now = datetime.datetime.fromtimestamp(int(os.environ["FNO_DEMO_EPOCH"]), datetime.timezone.utc)
def ts(mins):
    return (now - datetime.timedelta(minutes=mins)).strftime("%Y-%m-%dT%H:%M:%SZ")
names = ["archer", "reviewer", "scout", "scribe", "planner", "wren"]
harnesses = ["claude", "codex", "opencode"]
os.makedirs(agents_home, exist_ok=True)
with open(f"{agents_home}/events.jsonl", "a") as f:
    for i in range(18):
        f.write(json.dumps({"type": "agent_spawned", "ts": ts(30 + i * 97), "data": {
            "name": names[i % len(names)], "harness": harnesses[i % 3],
            "node": ids[i % len(ids)] if ids else None, "substrate": "pane"}}) + "\n")
with open(f"{root}/.fno/events.jsonl", "a") as f:
    for i in range(6):
        f.write(json.dumps({"type": "worker_silent", "ts": ts(55 + i * 241), "data": {
            "handle": names[(i + 2) % len(names)], "age_s": 600 + i * 120,
            "harness": harnesses[(i + 1) % 3],
            "node": ids[(i + 3) % len(ids)] if ids else None}}) + "\n")
PY

# --sort <column>:<direction> (for example age:descending) sets the
# sideline's sort column, the way the sort control persists it.
# --lead-nesting turns on the group-by-lead toggle the same way.
if [ -n "${FNO_DEMO_SORT:-}" ] || [ -n "${FNO_DEMO_LEAD_NESTING:-}" ] || [ -n "${FNO_DEMO_FEED_VIEW:-}" ]; then
  python3 - "$FNO_AGENT_HOME/mux-view.json" "${FNO_DEMO_SORT:-}" "${FNO_DEMO_LEAD_NESTING:-}" "${FNO_DEMO_FEED_VIEW:-}" <<'PY'
import json, os, sys
path, spec, nesting, feed_view = sys.argv[1:5]
data = json.load(open(path)) if os.path.exists(path) else {}
if spec:
    column, direction = spec.split(":")
    data["sort"] = {"column": column, "direction": direction}
if nesting:
    data["group_by_lead"] = True
if feed_view:
    data["feed_view"] = json.loads(feed_view)
json.dump(data, open(path, "w"))
PY
fi

# The You and asks views read typed turns from the operator capture dir:
# each <session>.scan.json names a transcript and the projection walks the
# transcript's typed rows. Two invented sessions share one mention, so the
# asks topic list paints one cluster: the pending ask never got a reply,
# the answered one carries a claude end_turn row.
CAPTURE="$ROOT/code/checkout/.fno/operator-capture"
export FNO_OPERATOR_CAPTURE_DIR="$CAPTURE"
mkdir -p "$CAPTURE"
# Timestamps ride the frozen demo clock: with now_secs frozen to the same
# epoch, the asks recency window reads the seeded transcripts as fresh on
# every shoot, and a hardcoded date never ages out of the read.
TS_BASE=$(python3 -c 'import os, datetime; print(datetime.datetime.fromtimestamp(int(os.environ["FNO_DEMO_EPOCH"]), datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M"))')
TR_A="$ROOT/transcript-pending.jsonl"
printf '%s\n' "{\"type\":\"user\",\"timestamp\":\"${TS_BASE}:00.000Z\",\"message\":{\"role\":\"user\",\"content\":\"merge x-1a2b0 now\"}}" > "$TR_A"
TR_B="$ROOT/transcript-answered.jsonl"
{
  printf '%s\n' "{\"type\":\"user\",\"timestamp\":\"${TS_BASE}:00.000Z\",\"message\":{\"role\":\"user\",\"content\":\"did x-1a2b0 land?\"}}"
  printf '%s\n' "{\"type\":\"assistant\",\"timestamp\":\"${TS_BASE}:01.000Z\",\"message\":{\"role\":\"assistant\",\"content\":\"landed in #4621; both asks share the mention\"},\"stop_reason\":\"end_turn\"}"
} > "$TR_B"
printf '{"transcript": "%s"}\n' "$TR_A" > "$CAPTURE/aaa11111-1111-4aaa-9aaa-aaaaaaaaaaa1.scan.json"
# The board's fixture asks: seven rows so the leaderboard reads as a board,
# one of each judged outcome - one linked ask typed in four sessions
# (the rank leader, acknowledged but forgotten), one re-ask of a done node
# (regression), one solved ask on a second done node, one lost bare ask,
# one bare ask with a reply, and a pair sharing a PR mention for the
# by-topic toggle (one ask pasted under a markdown heading). The repeats
# carry their own date: the open-days the row shows is now minus the
# newest ask.
BOARD_A="$ROOT/transcript-board-ask.jsonl"
printf '%s\n' "{\"type\":\"user\",\"timestamp\":\"${TS_BASE}:00.000Z\",\"message\":{\"role\":\"user\",\"content\":\"ship the requests board on the hq page and the mux this week\"}}" > "$BOARD_A"
BOARD_B1="$ROOT/transcript-board-rep1.jsonl"
BOARD_B2="$ROOT/transcript-board-rep2.jsonl"
BOARD_B3="$ROOT/transcript-board-rep3.jsonl"
BOARD_B4="$ROOT/transcript-board-rep4.jsonl"
REP_TEXT="what blocks ${IDS[0]} on the requests board"
printf '%s\n' "{\"type\":\"user\",\"timestamp\":\"2026-11-12T09:00:00.000Z\",\"message\":{\"role\":\"user\",\"content\":\"$REP_TEXT\"}}" > "$BOARD_B1"
{
  printf '%s\n' "{\"type\":\"user\",\"timestamp\":\"2026-11-12T09:00:00.000Z\",\"message\":{\"role\":\"user\",\"content\":\"$REP_TEXT\"}}"
  printf '%s\n' "{\"type\":\"assistant\",\"timestamp\":\"2026-11-12T09:01:00.000Z\",\"message\":{\"role\":\"assistant\",\"content\":\"the fold landed in #483; the board paints this week\"},\"stop_reason\":\"end_turn\"}"
} > "$BOARD_B2"
printf '%s\n' "{\"type\":\"user\",\"timestamp\":\"2026-11-12T10:30:00.000Z\",\"message\":{\"role\":\"user\",\"content\":\"$REP_TEXT\"}}" > "$BOARD_B3"
printf '%s\n' "{\"type\":\"user\",\"timestamp\":\"2026-11-12T14:00:00.000Z\",\"message\":{\"role\":\"user\",\"content\":\"$REP_TEXT\"}}" > "$BOARD_B4"
"$FNO" backlog done "${IDS[11]}" --force --reason "demo fixture done node for the regression row" --note "demo fixture completed node" --skip-stamp >/dev/null
"$FNO" backlog done "${IDS[10]}" --force --reason "demo fixture solved node for the outcome rows" --note "demo fixture completed node" --skip-stamp >/dev/null
BOARD_R="$ROOT/transcript-board-regress.jsonl"
RNOW="$(python3 -c 'import datetime; print((datetime.datetime.now(datetime.timezone.utc)+datetime.timedelta(minutes=2)).strftime("%Y-%m-%dT%H:%M:00.000Z"))')"
printf '%s\n' "{\"type\":\"user\",\"timestamp\":\"$RNOW\",\"message\":{\"role\":\"user\",\"content\":\"did ${IDS[11]} land already\"}}" > "$BOARD_R"
BOARD_T1="$ROOT/transcript-board-rel1.jsonl"
BOARD_T2="$ROOT/transcript-board-rel2.jsonl"
printf '%s\n' "{\"type\":\"user\",\"timestamp\":\"${TS_BASE}:00.000Z\",\"message\":{\"role\":\"user\",\"content\":\"## merge #483 and cut the release notes today\"}}" > "$BOARD_T1"
printf '%s\n' "{\"type\":\"user\",\"timestamp\":\"${TS_BASE}:30.000Z\",\"message\":{\"role\":\"user\",\"content\":\"the release notes for #483 are still missing\"}}" > "$BOARD_T2"
printf '{"transcript": "%s"}\n' "$TR_B" > "$CAPTURE/bbb22222-2222-4bbb-9bbb-bbbbbbbbbb2.scan.json"
BOARD_SOLVED="$ROOT/transcript-board-solved.jsonl"
printf '%s\n' "{\"type\":\"user\",\"timestamp\":\"2026-11-15T09:00:00.000Z\",\"message\":{\"role\":\"user\",\"content\":\"close out the ${IDS[10]} doc gaps\"}}" > "$BOARD_SOLVED"
BOARD_LOST="$ROOT/transcript-board-loss.jsonl"
printf '%s\n' "{\"type\":\"user\",\"timestamp\":\"2026-11-15T10:00:00.000Z\",\"message\":{\"role\":\"user\",\"content\":\"write the onboarding guide for new machines\"}}" > "$BOARD_LOST"
BOARD_SIDS=(ccc33333-3333-4ccc-9ccc-ccccccccccc3 1111aaaa-1111-4aaa-9aaa-aaaaaaaaaaa1 2222bbbb-2222-4bbb-9bbb-bbbbbbbbbb2 3333cccc-3333-4ccc-9ccc-ccccccccccc3 4444dddd-4444-4ddd-9ddd-ddddddddddd4 eeee5555-5555-4eee-9eee-eeeeeeeeeee5 ffff6666-6666-4fff-9fff-fffffffffff6 abab7777-7777-4aba-9aba-abababababa7 cccc8888-8888-4ccc-9ccc-ccccccccccc8 dddd9999-9999-4ddd-9ddd-dddddddddd9)
BOARD_FILES=("$BOARD_A" "$BOARD_B1" "$BOARD_B2" "$BOARD_B3" "$BOARD_B4" "$BOARD_R" "$BOARD_T1" "$BOARD_T2" "$BOARD_SOLVED" "$BOARD_LOST")
# The windowed requests fold (ask_turns_since) enumerates transcripts under
# FNO_CLAUDE_PROJECTS_DIR, not the capture-dir scan files (those feed only
# the unwindowed asks walk), so every board ask lands in both places.
BOARD_SLUG="demo"
mkdir -p "$FNO_CLAUDE_PROJECTS_DIR/$BOARD_SLUG"
for i in "${!BOARD_SIDS[@]}"; do
  cp "${BOARD_FILES[$i]}" "$FNO_CLAUDE_PROJECTS_DIR/$BOARD_SLUG/${BOARD_SIDS[$i]}.jsonl"
  printf '{"transcript": "%s"}\n' "${BOARD_FILES[$i]}" > "$CAPTURE/${BOARD_SIDS[$i]}.scan.json"
done

sleep 6

# The server reads the registry only while a viewer is attached, and the
# first attach ends before the rows join. A warm-up shot starts that read.
# A slow server can miss the 10s layout window under load; the warm svg is
# decoration, so a miss skips it instead of killing the run.
# wait keeps the trap load-bearing: a trapped signal fires as soon as the
# serve child exits, and cleanup ends the serve children by the tracked
# pids above. The serve shares this script's group, so a run-wide signal
# kills it too and the pending trap runs at once.
"$FNO" mux serve --snapshot --server "$SERVER" --size 200x56 --fit --squad checkout --out "$ROOT/warm.svg" >/dev/null &
WARM_PID=$!
wait "$WARM_PID" || true
sleep 3

# The flags after ours win. HOME makes the status row read ~/code/checkout.
if [ -n "$WEB" ]; then
  "$FNO" mux serve --web --server "$SERVER" --port "$WEB_PORT" &
  WEB_PID=$!
  wait "$WEB_PID"
elif ! { "$FNO" mux serve --snapshot --server "$SERVER" --size 200x56 ${FIT[@]+"${FIT[@]}"} --squad checkout "$@" & SERVE_PID=$!; wait "$SERVE_PID"; }; then
  # A first store fold can answer empty and refuse the shot (measured
  # 2026-11-10 on three shoots; the same roots rendered minutes later). One
  # warm retry, the same shape as the warm-up attach above: a second attach
  # whose read lands. The deterministic form, a fresh server re-adopting a
  # prior shoot's kept panes, is tracked in the backlog, not cured here.
  sleep 2
  "$FNO" mux serve --snapshot --server "$SERVER" --size 200x56 ${FIT[@]+"${FIT[@]}"} --squad checkout "$@" &
  SERVE_PID=$!
  wait "$SERVE_PID"
fi
