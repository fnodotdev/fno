#!/usr/bin/env bash
# mux-client-bench.sh - measure the two mux clients against one throwaway server.
#
#   scripts/ops/mux-client-bench.sh [--metrics a,b,c] [--runs 20] [--agents 30]
#       [--idle-secs 30] [--out DIR] [--fno-bin PATH] [--app-bin PATH]
#
# Metrics (v1):
#   launch    cold client start to first painted frame. TUI: fresh pty attach,
#             first output byte. fno-app: `--shot` cold wall time (one offscreen
#             frame incl. png encode; the settle budget stays short in an idle
#             world). 20 runs each.
#   scroll    keystroke to painted settle in a 10,000-line pane (the server's
#             own scrollback ceiling, vt.rs SCROLLBACK_LINES). TUI: Ctrl-B PgUp
#             (keyboard scroll mode, keys.rs DEFAULT_PREFIX = 0x02) then 12
#             PgUp presses, timed to a 2s output-quiet window minus budgets.
#   idle      idle CPU % and RSS of both clients while the world holds 30 agent
#             stand-in panes (idle shells; they cost the client panes and frame
#             streams, not real agent processes). ps sampling at 1s cadence.
#   switch    focused-agent flip to painted change. TUI: `mux pane focus` to the busy
#             pane, detection on its streamed `date +%s` line. fno-app: the
#             control channel drains on a 400ms poll with no ack row, and
#             window-state.json is quit-only, so the paint is observable only
#             through window capture: UNMEASURED in v1.
#   keypaint  keystroke to painted echo in a busy pane. TUI: marker string
#             through the pty until it paints. fno-app: needs synthetic
#             keyboard events (Accessibility grant) plus capture: UNMEASURED
#             in v1.
#
# The world: a temp state root under /tmp (a socket path must stay under 104
# bytes on macOS), FNO_MUX_DIR pointed at it, one server, 30 idle panes, one
# 10,000-line pane, one busy pane that stays silent until the switch metric
# wakes it (`read _` gate). Scroll and idle run in a quiet world; switch and
# keypaint run after the wake; every number can say which world produced it.
#
# Kill discipline: the server, every attach and the app are pid-tracked and
# killed by the EXIT trap; `mux kill-server --end-unkept` ends the panes; a
# final sweep pgreps the unique session name (it embeds this script's pid) so
# only pids this script started can die. The script prints the sweep result.
#
# Provenance: every report records host, kernel, both binary versions, the
# world shape and the date, so a number can always be traced to its run.
#
# Gate: TIMING RUNS NEED THE OPERATOR'S GO. This script ships unrun; the
# commands go to the operator by mail and nothing here executes until the
# go lands. One bench at a time; the machine hosts one shoot or rig run at
# a time by standing rule.
#
# macOS caveats: bash 3.2-safe (no assoc arrays); `date` has no %N, so
# timing uses perl Time::HiRes; TUI and mux server share the process name
# `fno`, so ps matching keys on argv.
#
# State isolation: the whole script pins FNO_CONFIG at a config inside the
# temp root, so every fno-side write (mux sidecars, view prefs, graph)
# lands there and nothing reads the real ~/.fno. The app surface runs
# under a temp HOME (the app has no state-path flag; its state file is
# $HOME/Library/Application Support/fno-app/window-state.json), so the
# bench app can never touch the real window-state.json. The bench asserts
# both: the temp state file must exist after a clean app quit, and the
# real file's mtime+size must be byte-identical before and after. A
# violation prints FAIL in the report and the run's numbers read suspect.
set -euo pipefail

SESSION=""
ROOT=""
OUT_DIR=""
PIDS=()
REAL_HOME="${HOME:?HOME unset}"
REAL_STATE="$REAL_HOME/Library/Application Support/fno-app/window-state.json"
REAL_SIG_BEFORE=""

FNO_BIN="$(command -v fno)"
APP_BIN="$(command -v fno-app)"
RUNS=20
AGENTS=30
IDLE_SECS=30
METRICS="launch,scroll,idle,switch,keypaint"
TUI_TIMEOUT=30
SCROLL_PAGES=12
SCROLL_SETTLE_TICKS=2
PANE_SIZE="120x40"

now_ms() { perl -MTime::HiRes=time -e 'printf("%.0f\n", time()*1000)'; }

die() { echo "mux-client-bench: $*" >&2; exit 2; }

state_sig() { # state_sig <file> : mtime+size fingerprint, "absent" when missing
  if [ -f "$1" ]; then stat -f '%m %z' "$1"; else echo absent; fi
}

# ---- args ----
while [ $# -gt 0 ]; do
  case "$1" in
    --metrics) METRICS="$2"; shift 2;;
    --runs) RUNS="$2"; shift 2;;
    --agents) AGENTS="$2"; shift 2;;
    --idle-secs) IDLE_SECS="$2"; shift 2;;
    --out) OUT_DIR="$2"; shift 2;;
    --fno-bin) FNO_BIN="$2"; shift 2;;
    --app-bin) APP_BIN="$2"; shift 2;;
    *) die "unknown arg: $1 (see the header for usage)";;
  esac
done
[ -x "$FNO_BIN" ] || die "fno binary not runnable: $FNO_BIN"

needs_app=false
case ",$METRICS," in *,launch,*|*,idle,*) needs_app=true;; esac
if $needs_app && [ ! -x "$APP_BIN" ]; then
  die "fno-app binary not runnable: $APP_BIN (launch/idle need it)"
fi

SESSION="bench-$$"
: "${OUT_DIR:=$PWD/mux-client-bench-$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$OUT_DIR"
SAMPLES="$OUT_DIR/samples.csv"
UNMEASURED="$OUT_DIR/unmeasured.txt"
REPORT="$OUT_DIR/report.md"
echo "metric,client,run,value" > "$SAMPLES"
: > "$UNMEASURED"

record() { # record <metric> <client> <run> <ms-or-FAIL-text>
  echo "$1,$2,$3,$4" >> "$SAMPLES"
}

mark_unmeasured() { echo "$1" >> "$UNMEASURED"; }

# ---- teardown: every pid this script started dies here ----
teardown() {
  set +e
  for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null; done
  "$FNO_BIN" mux kill-server "$SESSION" --end-unkept --yes >/dev/null 2>&1
  sleep 1
  # Sweep: the session name embeds this pid, so any match is ours.
  local strays
  strays="$(pgrep -f "$SESSION" 2>/dev/null | tr '\n' ' ')"
  if [ -n "$strays" ]; then
    for p in $strays; do kill "$p" 2>/dev/null; done
    echo "teardown: swept stray pids: $strays"
  else
    echo "teardown: sweep clean (no strays)"
  fi
  # Last isolation check before the temp root disappears: the real
  # window-state.json must still carry the signature taken at world-up.
  if [ -n "$REAL_SIG_BEFORE" ] && [ -n "$ROOT" ]; then
    if [ "$(state_sig "$REAL_STATE")" != "$REAL_SIG_BEFORE" ]; then
      echo "FAIL_real_state_touched" > "$OUT_DIR/isolation.txt" 2>/dev/null || true
      echo "ISOLATION FAIL: the real window-state.json changed during the bench" >&2
    fi
  fi
  [ -n "$ROOT" ] && rm -rf "$ROOT"
  set -e
}
trap teardown EXIT INT TERM

# ---- world ----
PANE_COUNT_EXPECTED=$((AGENTS + 2))
SLEEPER_A_ID=""
BUSY_ID=""
LONG_ID=""

spawn_tracked() { # spawn_tracked <cmd...> : background it, remember the pid
  "$@" &
  PIDS+=("$!")
}

pane_run_id() { # pane_run_id <cmd...> : start a pane, print its id
  "$FNO_BIN" mux pane run --server "$SESSION" --size "$PANE_SIZE" --json "$@" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["pane_id"])'
}

world_up() {
  # /tmp, not TMPDIR: a unix socket path must stay under 104 bytes on macOS.
  ROOT="$(mktemp -d /tmp/fno-bench.XXXXXX)"
  ROOT="$(cd "$ROOT" && pwd -P)"
  export FNO_MUX_DIR="$ROOT/mux"
  # No FNO_SERVER: with it set, `mux attach` refuses ("already inside mux
  # server") and the refusal text reads as a fast first paint.
  unset FNO_SERVER FNO_SESSION
  # Pin the config, the state root and the agent registry inside the temp
  # root. An empty config leaves state_dir at ~/.fno, so selectors would
  # resolve against the real fleet registry.
  mkdir -p "$ROOT/state" "$ROOT/agents"
  printf 'state_dir = "%s"\n' "$ROOT/state" > "$ROOT/config.toml"
  export FNO_CONFIG="$ROOT/config.toml"
  export FNO_AGENT_HOME="$ROOT/agents"
  local reg
  reg="$("$FNO_BIN" config doctor 2>&1 | awk '$1=="OPERATOR" && $2=="registry.json" {print $3; exit}' || true)"
  case "$reg" in
    "$ROOT"/*) ;;
    *) die "registry resolves to '${reg:-none}', outside the bench root $ROOT: refusing to run";;
  esac
  # The app surface's own HOME; window-state and control files land here.
  mkdir -p "$ROOT/home/Library/Application Support"
  REAL_SIG_BEFORE="$(state_sig "$REAL_STATE")"

  spawn_tracked "$FNO_BIN" mux server --server "$SESSION"
  local i=0
  while [ ! -S "$FNO_MUX_DIR/$SESSION.sock" ] && [ "$i" -lt 100 ]; do
    sleep 0.1; i=$((i + 1))
  done
  [ -S "$FNO_MUX_DIR/$SESSION.sock" ] || die "bench server socket never appeared under $FNO_MUX_DIR"

  # Ids are captured from each `pane run --json` receipt, never inferred
  # from position: the server may keep panes of its own.
  LONG_ID="$(pane_run_id sh -c 'seq 1 10000; exec sleep 3600')"
  BUSY_ID="$(pane_run_id sh -c 'read _; while :; do date +%s; sleep 0.2; done')"
  SLEEPER_A_ID="$(pane_run_id sleep 3600)"
  local _k
  for _k in $(seq 2 "$AGENTS"); do
    pane_run_id sleep 3600 >/dev/null
  done

  # Wait until every pane is live before any client attaches.
  local n=0 got=0
  while [ "$n" -lt 100 ]; do
    got="$("$FNO_BIN" mux pane ls --server "$SESSION" --json 2>/dev/null \
      | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))' 2>/dev/null || echo 0)"
    [ "$got" -ge "$PANE_COUNT_EXPECTED" ] && break
    sleep 0.2; n=$((n + 1))
  done
  [ "$got" -ge "$PANE_COUNT_EXPECTED" ] || die "world never reached $PANE_COUNT_EXPECTED panes (got $got)"

  # View moves go through `mux pane focus <pane id>` inside each attach:
  # it moves only attached clients, and `mux view` resolves registry names,
  # which are not this throwaway server's bare-number pane ids.
}

wake_busy() {
  # The newline ends the busy pane's `read _`; the stream starts.
  "$FNO_BIN" mux pane send "$BUSY_ID" --raw $'\n' --server "$SESSION" >/dev/null \
    || die "could not wake the busy pane (pane send refused)"
}

app_env() { # app_env <cmd...> : run an app-surface call under the temp HOME
  HOME="$ROOT/home" "$@"
}

isolation_verdict() { # compare temp-state presence and the real file's signature
  local iso="ok"
  if [ ! -f "$ROOT/home/Library/Application Support/fno-app/window-state.json" ]; then
    iso="FAIL_temp_state_absent"
  fi
  if [ "$(state_sig "$REAL_STATE")" != "$REAL_SIG_BEFORE" ]; then
    iso="FAIL_real_state_touched"
  fi
  echo "$iso"
}

# ---- p50/p90 (nearest-rank) ----
stats_for() { # stats_for <metric> <client> -> "p50 p90 min max n" | "unmeasured"
  awk -F, -v m="$1" -v c="$2" '
    $1==m && $2==c && $4 ~ /^[0-9.]+$/ { v[++n]=$4; if (min==""||$4+0<min+0) min=$4; if ($4+0>max+0) max=$4 }
    END {
      if (n==0) { print "unmeasured"; exit }
      for (i=1;i<=n;i++) for (j=i+1;j<=n;j++) if (v[j]+0 < v[i]+0) { t=v[i]; v[i]=v[j]; v[j]=t }
      p50=v[int(n*0.5 + 0.999)]; p90=v[int(n*0.9 + 0.999)]
      print p50, p90, min, max, n
    }' "$SAMPLES"
}

fmt_stats() { # fmt_stats "p50 p90 min max n" -> "123 / 456 (n=20)"
  case "$1" in
    unmeasured|"") echo "unmeasured";;
    *) awk '{print $1 " / " $2 " (n=" $5 ")"}' <<< "$1";;
  esac
}

# ---- expect driver ----
# One expect run per sample prints exactly one `RESULT <ms>` or
# `RESULT FAIL <reason>` line; everything else is swallowed so the
# sample stays clean.
run_tui_expect() { # run_tui_expect <tcl-file>
  # `|| true`: a tcl error exits expect 1, and under pipefail that would end
  # the whole bench silently instead of recording a FAIL sample.
  expect -f "$1" 2>&1 | sed -n 's/^RESULT //p' | tail -1 || true
}

# ---- metrics ----

metric_launch() {
  # TUI: fresh pty attach; the first output byte is the first painted
  # frame (the compositor diff-writes the whole screen on first draw).
  local tpl="$ROOT/tui_launch.exp"
  cat > "$tpl" <<'EOF'
set timeout $env(BENCH_TIMEOUT)
set t0 [clock milliseconds]
set stty_init "rows 40 cols 120"
spawn -noecho $env(BENCH_FNO) mux attach $env(BENCH_SESSION)
expect {
  -re "." {}
  timeout { puts "RESULT FAIL timeout_first_paint"; exit 0 }
  eof { puts "RESULT FAIL eof_first_paint"; exit 0 }
}
set t1 [clock milliseconds]
catch {exec kill -9 [exp_pid]}
puts "RESULT [expr {$t1 - $t0}]"
EOF
  local i out
  for i in $(seq 1 "$RUNS"); do
    out="$(BENCH_TIMEOUT="$TUI_TIMEOUT" BENCH_FNO="$FNO_BIN" BENCH_SESSION="$SESSION" run_tui_expect "$tpl")"
    case "$out" in
      FAIL*|"") record launch tui "$i" "${out:-FAIL no_result}";;
      *) record launch tui "$i" "$out";;
    esac
  done
  # fno-app: cold offscreen shot; the wall time is start to first painted
  # frame plus one png encode (labeled in the report). The shot is an
  # app-surface call: it runs under the temp HOME like open and close.
  # --shot-own-world: the bench server is this script's own world, so the shot drives it.
  local t0 t1 png
  png="$ROOT/app-launch.png"
  for i in $(seq 1 "$RUNS"); do
    t0="$(now_ms)"
    if app_env "$APP_BIN" --session "$SESSION" --shot --shot-own-world --shot-out "$png" >/dev/null 2>&1; then
      t1="$(now_ms)"
      record launch app "$i" "$((t1 - t0))"
    else
      record launch app "$i" "FAIL shot_refused"
    fi
  done
}

metric_scroll() {
  # Fresh attach per run on the 10k pane. Ctrl-B then PgUp opens keyboard
  # scroll mode; 12 PgUp presses page up; the sample ends at a 2s quiet
  # window. The fixed budgets (key cadence ~510ms + the 2s quiet window)
  # are subtracted so the number is paint time, not harness typing speed.
  local tpl="$ROOT/tui_scroll.exp"
  cat > "$tpl" <<'EOF'
set timeout $env(BENCH_TIMEOUT)
set stty_init "rows 40 cols 120"
spawn -noecho $env(BENCH_FNO) mux attach $env(BENCH_SESSION)
expect {
  -re "." {}
  timeout { puts "RESULT FAIL timeout_first_paint"; exit 0 }
  eof { puts "RESULT FAIL eof_first_paint"; exit 0 }
}
if {[catch {exec $env(BENCH_FNO) mux pane focus $env(BENCH_LONG_ID) --server $env(BENCH_SESSION)} err]} {
  puts "RESULT FAIL focus: [string map [list "\n" " "] $err]"; exit 0
}
sleep 0.5
set t0 [clock milliseconds]
send "\x02"
after 50
send "\x1b\[5~"
after 100
set i 0
while {$i < $env(BENCH_PAGES)} {
  send "\x1b\[5~"
  after 30
  incr i
}
set timeout 1
set quiet 0
while {$quiet < $env(BENCH_SETTLE_TICKS)} {
  expect {
    -re "." { set quiet 0 }
    timeout { incr quiet }
    eof { break }
  }
}
set t1 [clock milliseconds]
catch {exec kill -9 [exp_pid]}
set budget [expr {$env(BENCH_PAGES) * 30 + 150 + $env(BENCH_SETTLE_TICKS) * 1000}]
set ms [expr {$t1 - $t0 - $budget}]
if {$ms < 0} { set ms 0 }
puts "RESULT $ms"
EOF
  local i out
  for i in $(seq 1 "$RUNS"); do
    out="$(BENCH_TIMEOUT="$TUI_TIMEOUT" BENCH_FNO="$FNO_BIN" BENCH_SESSION="$SESSION" BENCH_LONG_ID="$LONG_ID" \
      BENCH_PAGES="$SCROLL_PAGES" BENCH_SETTLE_TICKS="$SCROLL_SETTLE_TICKS" run_tui_expect "$tpl")"
    case "$out" in
      FAIL*|"") record scroll tui "$i" "${out:-FAIL no_result}";;
      *) record scroll tui "$i" "$out";;
    esac
  done
  mark_unmeasured "scroll app: unmeasured in v1 (method: synthetic scroll-wheel events into the GPUI window; needs Accessibility + Screen Recording grants)"
}

metric_idle() {
  # Quiet world: 30 sleepers + 10k pane + still-silent busy pane. One
  # persistent TUI attach, one windowed app; ps both at 1s cadence.
  local tpl="$ROOT/tui_idle.exp"
  cat > "$tpl" <<'EOF'
set timeout $env(BENCH_TIMEOUT)
set stty_init "rows 40 cols 120"
spawn -noecho $env(BENCH_FNO) mux attach $env(BENCH_SESSION)
expect {
  -re "." {}
  timeout { puts "RESULT FAIL timeout_first_paint"; exit 0 }
  eof { puts "RESULT FAIL eof_first_paint"; exit 0 }
}
puts "TUIPID [exp_pid]"
after [expr {$env(BENCH_IDLE_SECS) * 1000}]
catch {exec kill -9 [exp_pid]}
puts "RESULT hold_done"
EOF
  BENCH_TIMEOUT=$((IDLE_SECS + 30)) BENCH_FNO="$FNO_BIN" BENCH_SESSION="$SESSION" \
    BENCH_IDLE_SECS="$IDLE_SECS" expect -f "$tpl" > "$ROOT/tui_idle.log" 2>&1 &
  PIDS+=("$!")
  local n=0 tui_pid=""
  while [ $n -lt 50 ] && [ -z "$tui_pid" ]; do
    tui_pid="$(sed -n 's/^TUIPID //p' "$ROOT/tui_idle.log" 2>/dev/null | head -1)"
    [ -n "$tui_pid" ] && break
    sleep 0.2; n=$((n + 1))
  done
  if [ -z "$tui_pid" ]; then
    mark_unmeasured "idle tui: FAIL attach never reported its pid"
  fi

  local app_pid=""
  if $needs_app; then
    app_pid="$(app_env "$FNO_BIN" mux app open --server "$SESSION" --mode terminal 2>/dev/null | sed -n 's/^[^0-9]*\([0-9][0-9]*\).*$/\1/p' | head -1)"
    if [ -n "$app_pid" ]; then
      PIDS+=("$app_pid")
    else
      mark_unmeasured "idle app: FAIL mux app open printed no pid (headless display, or the print shape changed?)"
    fi
  fi

  local t=0 cpu rss
  while [ "$t" -lt "$IDLE_SECS" ]; do
    if [ -n "$tui_pid" ]; then
      cpu="$(ps -o %cpu= -p "$tui_pid" 2>/dev/null | tr -d ' ')"
      rss="$(ps -o rss= -p "$tui_pid" 2>/dev/null | tr -d ' ')"
      [ -n "$cpu" ] && echo "idle_cpu,tui,$t,$cpu" >> "$SAMPLES"
      [ -n "$rss" ] && echo "idle_rss_kb,tui,$t,$rss" >> "$SAMPLES"
    fi
    if [ -n "$app_pid" ]; then
      cpu="$(ps -o %cpu= -p "$app_pid" 2>/dev/null | tr -d ' ')"
      rss="$(ps -o rss= -p "$app_pid" 2>/dev/null | tr -d ' ')"
      [ -n "$cpu" ] && echo "idle_cpu,app,$t,$cpu" >> "$SAMPLES"
      [ -n "$rss" ] && echo "idle_rss_kb,app,$t,$rss" >> "$SAMPLES"
    fi
    sleep 1; t=$((t + 1))
  done

  # Clean quit through the control channel so the app runs its save-on-quit
  # path into the temp HOME; the isolation verdict needs that file.
  if [ -n "$app_pid" ]; then
    app_env "$FNO_BIN" mux app close --server "$SESSION" >/dev/null 2>&1 || true
    local n=0
    while [ "$n" -lt 50 ] && [ ! -f "$ROOT/home/Library/Application Support/fno-app/window-state.json" ]; do
      sleep 0.2; n=$((n + 1))
    done
  fi
  isolation_verdict > "$OUT_DIR/isolation.txt"
}

metric_switch() {
  # Wake the busy pane first: its streamed `date +%s` lines are the signal
  # that the flip painted. Fresh attach per run on sleeper A, flip to the
  # busy pane, time to the first epoch line.
  wake_busy
  local tpl="$ROOT/tui_switch.exp"
  cat > "$tpl" <<'EOF'
set timeout $env(BENCH_TIMEOUT)
set stty_init "rows 40 cols 120"
spawn -noecho $env(BENCH_FNO) mux attach $env(BENCH_SESSION)
expect {
  -re "." {}
  timeout { puts "RESULT FAIL timeout_first_paint"; exit 0 }
  eof { puts "RESULT FAIL eof_first_paint"; exit 0 }
}
sleep 0.3
# Freshness: capture the newest epoch line visible before the flip (the
# busy pane streams in the background; equal-length epoch strings compare
# correctly as strings). The post-flip match must be strictly newer, so a
# buffered pre-flip line can never read as an instant switch.
set pre ""
set timeout 2
expect {
  -re {[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]} { set pre $expect_out(0,string) }
  timeout {}
}
set timeout $env(BENCH_TIMEOUT)
set t0 [clock milliseconds]
if {[catch {exec $env(BENCH_FNO) mux pane focus $env(BENCH_BUSY_ID) --server $env(BENCH_SESSION)} err]} {
  puts "RESULT FAIL focus: [string map [list "\n" " "] $err]"; exit 0
}
set got ""
expect {
  -re {[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]} { set got $expect_out(0,string) }
  timeout { puts "RESULT FAIL timeout_busy_never_painted"; exit 0 }
  eof { puts "RESULT FAIL eof_after_flip"; exit 0 }
}
while {$pre != "" && $got <= $pre} {
  expect {
    -re {[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]} { set got $expect_out(0,string) }
    timeout { puts "RESULT FAIL timeout_no_fresher_epoch"; exit 0 }
    eof { puts "RESULT FAIL eof_no_fresher_epoch"; exit 0 }
  }
}
set t1 [clock milliseconds]
catch {exec kill -9 [exp_pid]}
puts "RESULT [expr {$t1 - $t0}]"
EOF
  # Each run is a fresh attach, so it starts on the server's default view.
  local i out
  for i in $(seq 1 "$RUNS"); do
    out="$(BENCH_TIMEOUT="$TUI_TIMEOUT" BENCH_FNO="$FNO_BIN" BENCH_SESSION="$SESSION" \
      BENCH_BUSY_ID="$BUSY_ID" run_tui_expect "$tpl")"
    case "$out" in
      FAIL*|"") record switch tui "$i" "${out:-FAIL no_result}";;
      *) record switch tui "$i" "$out";;
    esac
  done
  mark_unmeasured "switch app: unmeasured in v1 (method: window capture diff around \`mux app focus\`; the control channel acks nothing and window-state is quit-only; needs Screen Recording + Accessibility grants)"
}

metric_keypaint() {
  # Busy pane focused and streaming; the keystroke is a unique marker the
  # shell echoes; the sample ends when the marker paints.
  local tpl="$ROOT/tui_keypaint.exp"
  cat > "$tpl" <<'EOF'
set timeout $env(BENCH_TIMEOUT)
set stty_init "rows 40 cols 120"
spawn -noecho $env(BENCH_FNO) mux attach $env(BENCH_SESSION)
expect {
  -re "." {}
  timeout { puts "RESULT FAIL timeout_first_paint"; exit 0 }
  eof { puts "RESULT FAIL eof_first_paint"; exit 0 }
}
if {[catch {exec $env(BENCH_FNO) mux pane focus $env(BENCH_BUSY_ID) --server $env(BENCH_SESSION)} err]} {
  puts "RESULT FAIL focus: [string map [list "\n" " "] $err]"; exit 0
}
sleep 0.5
set marker "kpz[clock clicks]"
set t0 [clock milliseconds]
send -- "$marker\r"
expect {
  -re "$marker" {}
  timeout { puts "RESULT FAIL timeout_marker_never_painted"; exit 0 }
  eof { puts "RESULT FAIL eof_after_marker"; exit 0 }
}
set t1 [clock milliseconds]
catch {exec kill -9 [exp_pid]}
puts "RESULT [expr {$t1 - $t0}]"
EOF
  local i out
  for i in $(seq 1 "$RUNS"); do
    out="$(BENCH_TIMEOUT="$TUI_TIMEOUT" BENCH_FNO="$FNO_BIN" BENCH_SESSION="$SESSION" BENCH_BUSY_ID="$BUSY_ID" run_tui_expect "$tpl")"
    case "$out" in
      FAIL*|"") record keypaint tui "$i" "${out:-FAIL no_result}";;
      *) record keypaint tui "$i" "$out";;
    esac
  done
  mark_unmeasured "keypaint app: unmeasured in v1 (method: synthetic keyboard events (Accessibility) with capture diff; needs both TCC grants)"
}

# ---- report ----
write_report() {
  {
    echo "# mux client bench: TUI vs fno-app"
    echo ""
    echo "All numbers are milliseconds (p50 / p90, nearest-rank) unless the cell says unmeasured. n is the number of clean samples; failed runs are named in samples.csv, never averaged in. Methods and world shape: see the provenance block below. Cells marked unmeasured name the method that would fill them."
    echo ""
    echo "| Metric | TUI (p50/p90 ms) | fno-app (p50/p90 ms) |"
    echo "|---|---|---|"
    local row
    for row in launch scroll switch keypaint; do
      echo "| $row | $(fmt_stats "$(stats_for "$row" tui)") | $(fmt_stats "$(stats_for "$row" app)") |"
    done
    echo ""
    echo "## Idle CPU and RSS (30 agent stand-in panes, ${IDLE_SECS}s sampling)"
    echo ""
    echo "| Client | CPU % p50/p90 | RSS KB p50/p90 |"
    echo "|---|---|---|"
    echo "| TUI | $(fmt_stats "$(stats_for idle_cpu tui)") | $(fmt_stats "$(stats_for idle_rss_kb tui)") |"
    echo "| fno-app | $(fmt_stats "$(stats_for idle_cpu app)") | $(fmt_stats "$(stats_for idle_rss_kb app)") |"
    echo ""
    echo "## App state isolation"
    echo ""
    local iso
    iso="$(cat "$OUT_DIR/isolation.txt" 2>/dev/null || echo "not_run")"
    if [ "$iso" = "ok" ]; then
      echo "ok: the bench app wrote its state under the temp HOME and the real window-state.json kept its world-up signature."
    elif [ "$iso" = "not_run" ]; then
      echo "not run: no app surface in this metric set."
    else
      echo "**FAIL: $iso** - the bench touched state outside the temp root. Treat every number in this report as suspect."
    fi
    echo ""
    echo "## Unmeasured cells"
    echo ""
    if [ -s "$UNMEASURED" ]; then
      sed 's/^/- /' "$UNMEASURED"
    else
      echo "- none"
    fi
    echo ""
    echo "Raw samples: samples.csv (failures carry FAIL-* in the value column)"
    echo ""
    echo "## Provenance"
    echo ""
    echo "- date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "- host: $(uname -srm) | macos $(sw_vers -productVersion 2>/dev/null || echo '?')"
    local fno_ver app_ver
    fno_ver="$("$FNO_BIN" --version 2>/dev/null | head -1)"
    app_ver="$("$APP_BIN" --version 2>/dev/null | head -1)"
    echo "- fno: ${fno_ver:-unavailable} ($FNO_BIN)"
    echo "- fno-app: ${app_ver:-unavailable} ($APP_BIN)"
    echo "- world: $AGENTS idle panes + one 10000-line pane + one busy pane; panes $PANE_SIZE; server $SESSION"
    echo "- runs: $RUNS per timed cell; idle sampled 1s cadence for ${IDLE_SECS}s"
    echo "- methods: launch/tui = first pty output byte; launch/app = cold --shot wall incl. one png encode; scroll = keystroke to a 2s quiet window minus fixed budgets; switch = mux pane focus flip to the busy pane's first streamed epoch line; keypaint = marker keystroke to its painted echo; idle = ps argv-keyed sampling (TUI and server share the name fno)"
    echo "- session was $SESSION; killed at teardown (sweep verdict above in the run log)"
  } > "$REPORT"
  echo "report: $REPORT"
}

# ---- main ----
command -v expect >/dev/null || die "expect not found (/usr/bin/expect ships with macOS)"
command -v python3 >/dev/null || die "python3 not found"
command -v perl >/dev/null || die "perl not found"

world_up
echo "world up: $PANE_COUNT_EXPECTED panes, server $SESSION, root $ROOT"
case ",$METRICS," in *,launch,*) metric_launch;; esac
case ",$METRICS," in *,scroll,*) metric_scroll;; esac
case ",$METRICS," in *,idle,*) metric_idle;; esac
case ",$METRICS," in *,switch,*) metric_switch;; esac
case ",$METRICS," in *,keypaint,*) metric_keypaint;; esac
write_report
