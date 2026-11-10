#!/usr/bin/env python3
"""Bench the plugin's UserPromptSubmit hooks under controlled load.

Complements scripts/release/hookagg.py, which reads what already happened in
transcripts. This script runs the hooks NOW, with a realistic payload, at
chosen synthetic load levels, and answers the two questions the transcript
audit cannot:

  - wall: how long does each hook take from spawn to child exit, per load tier?
  - hold: how long after the child's exit does its stdout pipe stay open (a
    survivor holding the capture, the tail claude waits on after a kill)?

For every hook it reports p50/p95 of wall and hold, plus exit status and
output size, so a fix can be judged against the same numbers before it lands.

Load tiers are counts of CPU-hogging worker processes (one busy loop each),
spawned for the duration of a tier and always reaped in a finally block.
Tier 0 is the idle measurement. The one-minute loadavg is sampled before and
after each tier and printed with it, so a run can be read against `uptime`.

The hooks run against an isolated agents home (FNO_AGENTS_HOME) so a bench
session cannot advance real cursors, bind real nodes, or deliver real mail.
Payload identity fields are bench-prefixed for the same reason.

usage: hook-prompt-bench.py [--reps N] [--tiers 0,12,24] [--timeout S] [--hooks name,name]
"""

import argparse
import json
import os
import pathlib
import signal
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parents[2]
HOOKS_JSON = ROOT / "hooks" / "hooks.json"


def load_hooks(only):
    spec = json.loads(HOOKS_JSON.read_text()).get("hooks", {})
    out = []
    for entry in spec.get("UserPromptSubmit", []):
        for h in entry.get("hooks", []):
            cmd = h["command"]
            if only and not any(name in cmd for name in only):
                continue
            name = cmd
            for token in ("PYTHONDONTWRITEBYTECODE=1", "FNO_HOOK_CONTEXT=1",
                          "bash", '"${CLAUDE_PLUGIN_ROOT}/hooks/', '"',
                          "${CLAUDE_PLUGIN_ROOT}/hooks/"):
                name = name.replace(token, "")
            out.append((name.strip() or cmd, cmd, h.get("timeout")))
    return out


def payload():
    return json.dumps({
        "session_id": "bench-hook-prompt-0001",
        "transcript_path": "/tmp/bench-hook-prompt.jsonl",
        "cwd": str(ROOT),
        "hook_event_name": "UserPromptSubmit",
        "prompt": "bench: measure the prompt-submit hook batch",
        "hookName": "UserPromptSubmit",
    })


def loadavg():
    try:
        return os.getloadavg()[0]
    except OSError:
        return -1.0


class Load:
    """N busy-loop workers, reaped on exit."""

    def __init__(self, n):
        self.n = n
        self.procs = []

    def __enter__(self):
        for _ in range(self.n):
            self.procs.append(subprocess.Popen(
                [sys.executable, "-c", "while True: pass"],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        return self

    def __exit__(self, *exc):
        for p in self.procs:
            p.kill()
        for p in self.procs:
            p.wait()
        self.procs = []


def run_one(cmd, stdin_bytes, cap_ms, env):
    """One hook run under an emulated harness cap.

    The hook runs in its own process group (start_new_session), as claude's
    hook runner bounds its children. At cap_ms the group is SIGKILLed, the
    way a harness discards a timed-out hook, and the clock keeps running
    until the capture pipe hits EOF: that tail is the time the harness stays
    blocked after the kill, the red line's true cost. Returns
    (rc, wall_ms, hold_ms, out_bytes, killed).
    """
    cap_s = cap_ms / 1000.0
    s = time.perf_counter()
    p = subprocess.Popen(cmd, shell=True, env=env, stdin=subprocess.PIPE,
                         stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                         start_new_session=True)
    try:
        p.stdin.write(stdin_bytes)
        p.stdin.close()
    except OSError:
        pass
    killed = False
    try:
        p.wait(timeout=cap_s)
    except subprocess.TimeoutExpired:
        killed = True
        try:
            os.killpg(p.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            p.kill()
            p.wait()
    exit_at = time.perf_counter() - s
    out = b""
    try:
        rest = p.stdout.read()
        out += rest or b""
    except OSError:
        pass
    eof_at = time.perf_counter() - s
    return (p.returncode if p.returncode is not None else 0,
            exit_at * 1000.0, (eof_at - exit_at) * 1000.0, len(out), killed)


def pct(vals, p):
    if not vals:
        return 0
    v = sorted(vals)
    return v[min(len(v) - 1, int(p / 100.0 * len(v)))]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reps", type=int, default=5)
    ap.add_argument("--tiers", type=str, default="0,12,24")
    ap.add_argument("--timeout", type=int, default=10,
                    help="kill bound per hook run, emulating a harness cap")
    ap.add_argument("--hooks", type=str, default="")
    args = ap.parse_args()
    only = [s for s in args.hooks.split(",") if s]

    agents_home = pathlib.Path(os.environ.get(
        "FNO_AGENTS_HOME", str(pathlib.Path.home() / ".fno" / "agents")))
    bench_home = agents_home.parent / "agents-bench-home"
    bench_home.mkdir(parents=True, exist_ok=True)

    base_env = dict(os.environ)
    base_env.update({
        "CLAUDE_PLUGIN_ROOT": str(ROOT),
        "PYTHONDONTWRITEBYTECODE": "1",
        "FNO_HOOK_CONTEXT": "1",
        "FNO_AGENTS_HOME": str(bench_home),
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
    })

    hooks = load_hooks(only)
    stdin_bytes = payload().encode()
    tiers = [int(t) for t in args.tiers.split(",")]

    for tier in tiers:
        rows = []
        la0 = loadavg()
        with Load(tier):
            time.sleep(2 if tier else 0)
            for name, cmd, bound in hooks:
                walls, holds, bad, outs, killed_n = [], [], 0, [], 0
                full = cmd.replace("${CLAUDE_PLUGIN_ROOT}", str(ROOT))
                for _ in range(args.reps):
                    cap = (bound if bound else args.timeout) * 1000
                    rc, wall, hold, out, killed = run_one(full, stdin_bytes,
                                                          cap, base_env)
                    walls.append(wall)
                    holds.append(hold)
                    outs.append(out)
                    if rc != 0:
                        bad += 1
                    if killed:
                        killed_n += 1
                rows.append((name, walls, holds, bad, max(outs), killed_n))
        la1 = loadavg()
        print(f"\n== tier {tier} workers  loadavg {la0:.1f} -> {la1:.1f}  "
              f"reps {args.reps} cap {args.timeout}s ==")
        print(f"{'hook':<42}{'wall p50':>9}{'wall p95':>9}{'hold p95':>9}"
              f"{'maxwall':>9}{'killed':>7}{'maxout':>8}")
        for name, walls, holds, bad, outmax, killed_n in rows:
            print(f"{name[:41]:<42}{pct(walls,50):>9.0f}{pct(walls,95):>9.0f}"
                  f"{pct(holds,95):>9.0f}{max(walls):>9.0f}{killed_n:>7}"
                  f"{outmax:>8}")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
