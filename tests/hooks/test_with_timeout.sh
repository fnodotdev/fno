#!/usr/bin/env bash
# Unit tests for scripts/lib/with-timeout.sh.
#
# The two injection-hook suites cover the cap end-to-end through a hook. This
# covers the helper directly, because three of its properties are invisible from
# a hook's output and each one was a live defect in a previous implementation:
# the fast path must not pay the bound, no orphan `sleep` may survive a call, and
# nothing may reach the caller's stderr.
#
# Runs under /bin/bash explicitly. bash 3.2 is the floor this has to hold on
# (stock macOS), and it is also the shell whose job-control notices leak.
#
# Exit codes: 0 all passed, 1 assertion failed, 77 skipped.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
LIB="$REPO_ROOT/scripts/lib/with-timeout.sh"

PASS=0
FAIL=0
pass() { echo "  PASS: $*"; PASS=$((PASS + 1)); }
fail() { echo "  FAIL: $*"; FAIL=$((FAIL + 1)); }

[[ -f "$LIB" ]] || { echo "FAIL: helper not found at $LIB" >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo "SKIP: python3 not on PATH" >&2; exit 77; }

bash -n "$LIB" || fail "bash -n rejected $LIB"

TMP="$(mktemp -d -t with-timeout-XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

# A hung command with a distinctive bound, and a fast one. `hang` sleeps well
# past its bound; `wrapped` puts a shell between the helper and the sleep, which
# is the case a single-pid kill fails to cap.
cat > "$TMP/hang" <<'EOF'
#!/usr/bin/env bash
sleep 30
EOF
cat > "$TMP/wrapped" <<'EOF'
#!/usr/bin/env bash
bash -c 'sleep 30; echo late'
EOF
cat > "$TMP/fast" <<'EOF'
#!/usr/bin/env bash
echo "ok-payload"
exit 3
EOF
# Ignores SIGTERM and keeps running, so only the KILL escalation can end it.
cat > "$TMP/stubborn" <<'EOF'
#!/usr/bin/env bash
trap '' TERM
while :; do sleep 0.2; done
EOF
# The direct child dies on TERM, but leaves behind a TERM-immune group member
# still holding the caller's capture pipe. `wait` therefore returns promptly
# while the command substitution stays open, so only a group KILL issued AFTER
# `wait` can release the caller.
cat > "$TMP/pipeholder" <<'EOF'
#!/usr/bin/env bash
bash -c 'trap "" TERM; sleep 30' &
sleep 30
EOF
chmod +x "$TMP/hang" "$TMP/wrapped" "$TMP/fast" "$TMP/stubborn" "$TMP/pipeholder"

# Each case runs in its own /bin/bash so `set -m` state cannot leak between them.
# stdout carries "elapsed|rc|output"; stderr is kept separate so an assertion can
# prove it stayed empty.
probe() {
    local secs="$1" cmd="$2"
    /bin/bash -c '
        set -uo pipefail
        source "$1"
        export PATH="$2:$PATH"
        s=$(python3 -c "import time; print(int(time.time()*1000))")
        out=$(with_timeout "$3" "$4" 2>/dev/null || true)
        e=$(python3 -c "import time; print(int(time.time()*1000))")
        printf "%s|%s\n" "$((e - s))" "$out"
    ' _ "$LIB" "$TMP" "$secs" "$cmd"
}

# 1. A hung command is capped near its bound, not run to completion.
res="$(probe 2 hang 2>/dev/null)"
elapsed="${res%%|*}"
if [[ "$elapsed" -ge 1500 && "$elapsed" -le 5000 ]]; then
    pass "hung command capped at the bound (${elapsed}ms, not 30000ms)"
else
    fail "hung command not capped: ${elapsed}ms (expected 1500-5000ms)"
fi

# 2. Same, with a shell wrapper between helper and sleep. A single-pid kill
#    leaves the grandchild holding the capture pipe and the caller blocks anyway.
res="$(probe 2 wrapped 2>/dev/null)"
elapsed="${res%%|*}"
if [[ "$elapsed" -ge 1500 && "$elapsed" -le 5000 ]]; then
    pass "wrapped hung command capped too (${elapsed}ms) - the whole group dies"
else
    fail "wrapped hung command not capped: ${elapsed}ms (grandchild survived the signal)"
fi

# 3. The fast path pays neither the bound nor a truncated payload, and the
#    command's own exit status survives.
fast_bound=30
res="$(probe "$fast_bound" fast 2>/dev/null)"
elapsed="${res%%|*}"
out="${res#*|}"
[[ "$elapsed" -lt $((fast_bound * 1000 / 2)) ]] \
    && pass "fast command returned before the watchdog bound (${elapsed}ms, bound ${fast_bound}s)" \
    || fail "fast command took ${elapsed}ms; expected less than half the ${fast_bound}s watchdog bound"
[[ "$out" == "ok-payload" ]] \
    && pass "fast path output captured intact" \
    || fail "fast path output was [$out], expected [ok-payload]"

/bin/bash -c 'set -uo pipefail; source "$1"; export PATH="$2:$PATH"; with_timeout 30 fast >/dev/null 2>&1' _ "$LIB" "$TMP"
rc=$?
[[ $rc -eq 3 ]] \
    && pass "command's own exit status propagates (rc=3)" \
    || fail "expected rc=3 from the command, got rc=$rc"

# A regression in the bound HANGS rather than fails, and a hanging test in CI is
# its own outage, so the cases below need a deadline of their own. probe() writes
# its result to a sentinel file and we poll for the FILE, never for the pid:
# `kill -0` on an unreaped background job succeeds indefinitely, and signalling
# the job to stop waiting makes bash announce "Terminated" on this script's
# stderr - noise in a suite whose whole subject is a helper that keeps stderr
# clean. Sets `probe_rc` and `probe_ms`, or returns 1 if it never finished.
probe_bounded() {
    local bound="$1" cmd="$2" out="$TMP/probe-result" i=0 pid
    rm -f "$out"
    # Runs under a command substitution, because every real caller does and it is
    # the shape that exposes a pipe-holder: the capture stays open until every
    # process holding it exits, so a surviving group member hangs the caller even
    # after the direct child is reaped.
    /bin/bash -c '
        set -uo pipefail
        source "$1"; export PATH="$2:$PATH"
        s=$(python3 -c "import time; print(int(time.time()*1000))")
        out=$(with_timeout "$3" "$4" 2>/dev/null)
        rc=$?
        e=$(python3 -c "import time; print(int(time.time()*1000))")
        printf "%s %s\n" "$rc" "$((e - s))" > "$5"
    ' _ "$LIB" "$TMP" "$bound" "$cmd" "$out" >/dev/null 2>&1 &
    pid=$!
    while (( i < 30 )); do
        [[ -f "$out" ]] && break
        sleep 0.5
        i=$(( i + 1 ))
    done
    if [[ ! -f "$out" ]]; then
        kill -KILL "$pid" 2>/dev/null
        wait "$pid" 2>/dev/null
        pkill -KILL -f "$TMP/stubborn" >/dev/null 2>&1
        return 1
    fi
    wait "$pid" 2>/dev/null
    read -r probe_rc probe_ms < "$out"
}

# 2b. A child that IGNORES SIGTERM is still bounded. TERM alone is not a cap:
#     `wait` has no remaining deadline after it, so such a child leaves the
#     caller stuck forever - worse than no cap, because the caller is stuck
#     rather than merely slow.
if ! probe_bounded 2 stubborn; then
    fail "a SIGTERM-ignoring child was never bounded: with_timeout did not return and the external deadline had to kill it"
    kill_rc=""
else
    kill_rc="$probe_rc"
    if [[ "$probe_ms" -ge 1500 && "$probe_ms" -le 8000 ]]; then
        pass "SIGTERM-ignoring child escalated to KILL and bounded (${probe_ms}ms)"
    else
        fail "SIGTERM-ignoring child returned in ${probe_ms}ms, outside the 2s bound plus grace"
    fi
fi
pkill -KILL -f "$TMP/stubborn" >/dev/null 2>&1

# 2c. A fired bound reports ONE status regardless of whether TERM or the KILL
#     escalation ended it. A caller forced to distinguish 143 from 137 is a
#     caller coupled to this file's internals, which is how the frontdoor hook
#     came to nag users about a front door they already had.
if probe_bounded 2 hang; then
    term_rc="$probe_rc"
else
    fail "the plain hang case never returned"
    term_rc=""
fi
[[ "$term_rc" == "124" && "$kill_rc" == "124" ]] \
    && pass "a fired bound reports 124 on both the TERM and the KILL path" \
    || fail "fired bound must report 124 on both paths, got TERM=${term_rc:-none} KILL=${kill_rc:-none}"

# 2d. A group member that outlives the TERM while holding the capture pipe must
#     not hang the caller. `wait` returns as soon as the DIRECT child dies, which
#     cancels the watchdog and the escalation still pending inside it, so the
#     release depends entirely on the group KILL issued after `wait`. Without
#     that one line the caller blocks forever while every other case in this file
#     still passes, which is the untested-load-bearing-guard shape this whole
#     change exists to remove. It also pins the escalation's GROUP-ness: a
#     regression to a single-pid kill leaves the immune member alive.
if ! probe_bounded 2 pipeholder; then
    fail "a TERM-immune group member held the capture pipe open and the caller never returned; the post-wait group KILL is not working"
elif [[ "$probe_ms" -ge 1500 && "$probe_ms" -le 8000 ]]; then
    pass "pipe-holding group member reaped after wait, caller released (${probe_ms}ms)"
else
    fail "pipe-holder case returned in ${probe_ms}ms, outside the 2s bound plus grace"
fi
pkill -KILL -f "$TMP/pipeholder" >/dev/null 2>&1
pkill -KILL -f 'trap "" TERM; sleep 30' >/dev/null 2>&1

# 3b. A non-integer bound is refused, not silently turned into a kill at t=0.
#     GNU timeout accepted `30m`; `sleep` on stock macOS does not, and the one
#     operator-settable bound in the tree (EVAL_SWEEP_STAGE_TIMEOUT) sits two
#     lines from a sibling knob that defaults to `30m`.
badout=$(/bin/bash -c 'set -uo pipefail; source "$1"; export PATH="$2:$PATH"; with_timeout 30m fast 2>/dev/null' _ "$LIB" "$TMP" 2>/dev/null)
badrc=$?
[[ $badrc -eq 2 ]] \
    && pass "non-integer bound refused (rc=2), not treated as an expired timer" \
    || fail "expected rc=2 for a non-integer bound, got rc=$badrc"
[[ -z "$badout" ]] \
    && pass "refused bound runs nothing" \
    || fail "a refused bound still ran the command: $badout"

# 3c. The caller's stdin reaches the command. Bash hands an async job /dev/null
#     for stdin when job control is off, so before the explicit `<&0` a piped
#     caller got silence - the defect that kept law-stage-inject printing no law
#     at all (measured 2026-09-22).
stdinout="$(/bin/bash -c 'set -uo pipefail; source "$1"; printf through | with_timeout 5 cat' _ "$LIB" 2>/dev/null)"
[[ "$stdinout" == "through" ]] \
    && pass "caller's stdin passes through to the command" \
    || fail "piped stdin was dropped: got [$stdinout], expected [through]"

# 4. Nothing reaches the caller's stderr. An unwaited job killed by a signal
#    makes bash announce "Terminated" on whoever sourced the helper.
errfile="$TMP/err"
/bin/bash -c '
    set -uo pipefail
    source "$1"
    export PATH="$2:$PATH"
    out=$(with_timeout 2 hang 2>/dev/null || true)
    # A second call, so a notice deferred to the next command boundary has one.
    out=$(with_timeout 2 fast 2>/dev/null || true)
' _ "$LIB" "$TMP" 2>"$errfile"
if [[ -s "$errfile" ]]; then
    fail "helper wrote to the caller's stderr: $(tr '\n' ' ' < "$errfile")"
else
    pass "no job-control noise on the caller's stderr"
fi

# 5. A fast call releases its watchdog lock. The positive control observes
# the same lock held by a running sleeper, without a process-table permission.
if python3 - "$LIB" "$TMP" <<'PY'
import fcntl, os, pathlib, shlex, subprocess, sys, time
lib, root = sys.argv[1:]
case = pathlib.Path(root) / "watchdog-lock"
case.mkdir()
lock, ready = case / "lock", case / "ready"
worker = "import fcntl,pathlib,sys,time; f=open(sys.argv[1], 'w'); fcntl.flock(f, fcntl.LOCK_EX); pathlib.Path(sys.argv[2]).touch(); time.sleep(8); sys.exit(1)"
def started():
    until = time.monotonic()+5
    while not ready.exists() and time.monotonic() < until: time.sleep(.01)
    assert ready.exists(), "watchdog fixture did not start"
def locked():
    with open(lock, "a") as f:
        try: fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError: return True
    return False
control = subprocess.Popen([sys.executable, "-c", worker, str(lock), str(ready)])
try:
    started()
    assert locked(), "positive control must detect a running watchdog"
finally:
    control.terminate(); control.wait(timeout=3)
ready.unlink()
sleeper = case / "sleep"
sleeper.write_text("#!/bin/sh\nexec "+shlex.quote(sys.executable)+" -c "+shlex.quote(worker)+" "+shlex.quote(str(lock))+" "+shlex.quote(str(ready))+"\n")
sleeper.chmod(0o755)
command = "import pathlib,sys,time\np=pathlib.Path(sys.argv[1])\nend=time.monotonic()+5\nwhile not p.exists() and time.monotonic()<end: time.sleep(.01)\nsys.exit(3 if p.exists() else 2)"
launcher = 'source "$1"; trap : HUP; trap "" INT; trap "" TERM; before=$(trap -p HUP INT TERM); modes=$-; with_timeout 47 python3 -c "$2" "$3"; status=$?; [[ "$before" == "$(trap -p HUP INT TERM)" && "$modes" == "$-" ]] || exit 21; exit "$status"'
result = subprocess.run(["/bin/bash", "-c", launcher, "_", lib, command, str(ready)], env=dict(os.environ, PATH=str(case)+os.pathsep+os.environ["PATH"]), capture_output=True, text=True, timeout=7)
assert result.returncode == 3, f"ordinary status or caller traps changed: {result}"
assert not locked(), "watchdog survived a completed command"
PY
then
    pass "watchdog reaped; caller traps, shell modes and command status preserved"
else
    fail "watchdog survived a fast call or caller state changed"
fi

# A signal to the launcher must release its command lock and watchdog group.
if python3 - "$LIB" "$TMP" <<'PY'
import fcntl, os, pathlib, shlex, signal, subprocess, sys, time
lib, root = sys.argv[1:]
errors = []
for sig in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
    case = pathlib.Path(root) / str(sig)
    case.mkdir()
    groups = case / "groups"
    sleeper = case / "sleep"
    watchdog = case / "watchdog.py"
    watchdog.write_text("import fcntl,os,sys,time\nf=open(os.environ['WT_WATCHDOG_LOCK'], 'w')\nfcntl.flock(f, fcntl.LOCK_EX)\ngroup=os.environ['WT_GROUP_LOG']\nwith open(group+'.pending', 'w') as log: log.write(str(os.getpgrp())+'\\n')\nos.replace(group+'.pending', group)\ntime.sleep(float(sys.argv[1])); sys.exit(1 if float(sys.argv[1]) == 15 else 0)\n")
    sleeper.write_text("#!/bin/sh\nexec "+shlex.quote(sys.executable)+" "+shlex.quote(str(watchdog))+' "$@" 2>>'+shlex.quote(str(case / "watchdog.stderr"))+"\n")
    sleeper.chmod(0o755)
    child = "import fcntl,os,pathlib,signal,sys,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); f=open(sys.argv[1], 'w'); fcntl.flock(f, fcntl.LOCK_EX); pathlib.Path(sys.argv[2]).write_text(str(os.getpgrp())); time.sleep(15)"
    launcher = 'source "$1"; trap "printf HUP > \"$5\"; exit 129" HUP; trap "printf INT > \"$5\"; exit 130" INT; trap "printf TERM > \"$5\"; exit 143" TERM; with_timeout 15 python3 -c "$2" "$3" "$4"'
    env = dict(os.environ, PATH=str(case)+os.pathsep+os.environ["PATH"], WT_GROUP_LOG=str(groups), WT_WATCHDOG_LOCK=str(case / "watchdog.lock"))
    proc = subprocess.Popen(["/bin/bash", "-c", launcher, "_", lib, child, str(case / "lock"), str(case / "ready"), str(case / "trap")], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    try:
        until = time.monotonic()+5
        while not (case / "ready").exists() and time.monotonic() < until: time.sleep(.01)
        assert (case / "ready").exists(), f"{sig.name}: command not ready; launcher pid={proc.pid} rc={proc.poll()}"
        until = time.monotonic()+5
        while not groups.exists() and time.monotonic() < until: time.sleep(.01)
        child_group = (case / "ready").read_text()
        context = f"{sig.name}: launcher pid={proc.pid} rc={proc.poll()}, command pgid={child_group}"
        stderr = (case / "watchdog.stderr").read_text() if (case / "watchdog.stderr").exists() else ""
        assert groups.exists(), f"{context}: watchdog not ready: {stderr}"
        context += f", watchdog pgid={groups.read_text().strip()}"
        os.kill(proc.pid, sig)
        try: proc.communicate(timeout=5)
        except subprocess.TimeoutExpired: errors.append(f"{context}: interrupted launcher left its command or pipe alive")
        with open(case / "lock", "a") as lock:
            try: fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError: errors.append(f"{context}: command still owns its private lock")
        with open(case / "watchdog.lock", "a") as lock:
            try: fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError: errors.append(f"{context}: watchdog still owns its private lock")
        observed = (case / "trap").read_text() if (case / "trap").exists() else ""
        if observed != sig.name[3:]: errors.append(f"{context}: caller trap was not preserved")
        if proc.poll() != 128+sig: errors.append(f"{context}: launcher status {proc.poll()} lost signal status")
    finally:
        pgids = [proc.pid]
        if (case / "ready").exists(): pgids.append(int((case / "ready").read_text()))
        if groups.exists(): pgids += [int(p) for p in groups.read_text().splitlines()]
        for pgid in set(pgids):
            try: os.killpg(pgid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError): pass
        proc.communicate(timeout=20)
if errors:
    print("\n".join(errors)); sys.exit(1)
PY
then
    pass "launcher signals release the command lock and watchdog"
else
    fail "launcher interruption leaves a command or watchdog alive"
fi

# 6. Exactly one implementation of the bound exists. No behavioral test on one
#    caller can cover this: the defect being fixed was six implementations of one
#    operation, five of which capped nothing, and two of those were found only by
#    running this grep by hand. The positive control matters as much as the
#    absence - assert a positive marker, never an absence
#    (scripts/lib/assert-absent.sh).
defs="$(grep -rlE '^[[:space:]]*(_?with_timeout|_timeout)\(\)' "$REPO_ROOT/hooks" "$REPO_ROOT/scripts" --include='*.sh' 2>/dev/null | sort)"
if [[ "$defs" == "$REPO_ROOT/scripts/lib/with-timeout.sh" ]]; then
    # Scope named honestly: this proves uniqueness across hooks/ and scripts/,
    # NOT the whole repo. Self-contained skill scripts under skills/ carry their
    # own bounds by design and cannot source scripts/lib/; routing them through
    # skill-bundles.yaml is separate work.
    pass "exactly one timeout implementation in hooks/ and scripts/, and it is the shared lib"
else
    fail "expected only scripts/lib/with-timeout.sh to define the bound, found: ${defs:-<nothing, so this grep is broken>}"
fi

# A coreutils preference chain in front of the helper reintroduces the two-paths
# defect: the bash path then goes untested on every host that has the binary.
# Scoped to all of scripts/, not just scripts/lib/: check-event-schema-parity.sh
# is a direct caller and lives in neither hooks/ nor scripts/lib/, so a narrower
# sweep passes with a chain reintroduced in the one caller that has no
# behavioral coverage of its bound.
chain="$(grep -rlE 'command -v g?timeout' "$REPO_ROOT/hooks" "$REPO_ROOT/scripts" --include='*.sh' 2>/dev/null | sort)"
if [[ -z "$chain" ]]; then
    pass "no timeout/gtimeout preference chain remains in hooks/ or scripts/"
else
    fail "timeout preference chain reintroduced in: $chain"
fi

# 7. POSITIVE per-hook guard for the per-turn 30s budget (x-989d). #6 above
#    proves only that no SECOND bound implementation exists - a negative
#    invariant, structurally blind to a hook with NO bound at all, which is how
#    inside-leg-report.sh shipped unbounded right under this passing test. The
#    positive invariant: every UserPromptSubmit hook (the only hooks bound by
#    Claude Code's per-turn timeout) that shells to a potentially-blocking
#    daemon must source the shared helper AND wrap every such call. Scoped to
#    UserPromptSubmit hooks read LIVE from hooks.json - SessionStart / Stop /
#    helper hooks run under other budgets and are out of scope, and reading the
#    wiring instead of a hardcoded list is what catches a FOURTH unbounded hook
#    the day it is added (the exact hole x-1386's negative guard left open).
hooks_dir="$REPO_ROOT/hooks"
hooks_json="$hooks_dir/hooks.json"
ups_hooks="$(python3 - "$hooks_json" <<'PY'
import json, os, re, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(0)
seen = []
for entry in d.get("hooks", {}).get("UserPromptSubmit", []):
    for h in entry.get("hooks", []):
        m = re.search(r"([\w./-]+\.sh)", h.get("command", ""))
        if m:
            seen.append(os.path.basename(m.group(1)))
print("\n".join(dict.fromkeys(seen)))
PY
)"
# Scope control: the known UserPromptSubmit hooks must all parse out, else the
# json read broke and every check below is vacuous (AGENTS.md: an empty result is
# a claim, not a success).
for _known in session-state.sh born-with-why-offer-inject.sh inject-mail-notify.sh; do
    grep -qx "$_known" <<<"$ups_hooks" \
        || fail "scope control: $_known missing from parsed UserPromptSubmit hooks; guard cannot run"
done
[[ -n "$ups_hooks" ]] && pass "parsed UserPromptSubmit hooks: ${ups_hooks//$'\n'/ }"

guard_fail=0
for name in $ups_hooks; do
    f="$hooks_dir/$name"
    [[ -f "$f" ]] || { fail "UserPromptSubmit hook $name not found at $f"; guard_fail=1; continue; }

    # (a) If it touches a daemon at all - a bare fno/fno-agents token, or a
    #     resolved "$VAR" report RPC over a socket - it must source the bound.
    #     scripts/lib/hook-budget.sh counts: it sources with-timeout.sh and
    #     bounds through it, so the bound is present either way.
    if grep -qE '\b(fno|fno-agents)\b' "$f" \
       || grep -qE '"\$[A-Z_][A-Z_]*"[[:space:]]+report' "$f"; then
        grep -qE '(^|[[:space:]])source[[:space:]].*(with-timeout|hook-budget)\.sh' "$f" \
            || { fail "$name shells to a daemon but does not source scripts/lib/with-timeout.sh"; guard_fail=1; }
    fi

    # (b) No UNWRAPPED direct fno/fno-agents call. A wrapped call has
    #     `with_timeout N ` between the statement boundary and the binary, so a
    #     direct command-position hit IS the defect. Comments (which quote `fno`
    #     in backticks) are stripped; `command -v` presence checks never match
    #     because the `-v` sits between the boundary and the binary.
    bad="$(grep -nE '(^|[|&;]|\$\(|`)[[:space:]]*(fno|fno-agents)([[:space:]]|$)' "$f" 2>/dev/null \
           | grep -vE '^[0-9]+:[[:space:]]*#' || true)"
    [[ -z "$bad" ]] \
        || { fail "$name has an unwrapped daemon call (wrap in with_timeout): $bad"; guard_fail=1; }

    # (c) A resolved-binary report RPC (e.g. `"$BIN" report`) is invisible to
    #     the fno-token grep in (b), so check it on its own: every such line
    #     must carry with_timeout.
    while IFS= read -r ln; do
        [[ -n "$ln" ]] || continue
        printf '%s' "$ln" | grep -q with_timeout \
            || { fail "$name has an unwrapped report RPC: $ln"; guard_fail=1; }
    done < <(grep -nE '"\$[A-Z_][A-Z_]*"[[:space:]]+report' "$f" 2>/dev/null)
done
[[ $guard_fail -eq 0 ]] \
    && pass "every UserPromptSubmit hook sources the bound and wraps every daemon call"

# Positive control for (b): a hook with a known-unwrapped call MUST trip the
# same grep, else the empty result above proves nothing.
mkdir -p "$TMP/ctl"
printf '#!/usr/bin/env bash\nx=$(fno backlog get foo)\n' > "$TMP/ctl/bad.sh"
ctl_hit="$(grep -nE '(^|[|&;]|\$\(|`)[[:space:]]*(fno|fno-agents)([[:space:]]|$)' "$TMP/ctl/bad.sh" 2>/dev/null)"
[[ -n "$ctl_hit" ]] \
    && pass "positive control: an unwrapped fno call is detected" \
    || fail "positive control failed: grep missed a known-unwrapped fno call"

# Positive control for (c): a resolved-binary report RPC MUST trip the feed-grep,
# else the empty loop above proves nothing - the same vacuous-pass hole this
# guard exists to close.
printf '#!/usr/bin/env bash\n"$BIN" report --session-id x\n' > "$TMP/ctl/rpc.sh"
rpc_hit="$(grep -nE '"\$[A-Z_][A-Z_]*"[[:space:]]+report' "$TMP/ctl/rpc.sh" 2>/dev/null)"
[[ -n "$rpc_hit" ]] \
    && pass "positive control: an unwrapped report RPC is detected" \
    || fail "positive control failed: grep missed a known-unwrapped report RPC"

echo ""
echo "with-timeout: $PASS passed, $FAIL failed"
[[ $FAIL -eq 0 ]]
