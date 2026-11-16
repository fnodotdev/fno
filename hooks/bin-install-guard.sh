#!/usr/bin/env bash
# fno hook: PreToolUse - bin install guard
# PreToolUse: refuse a Bash call that copies a locally built fno-agent or
# fno-agent-worker onto the deployed copy in the cargo bin dir (policy:
# crates/fno-agent/src/runtime/hook/bin_install_guard.rs). Never
# exec a candidate: an old build answers "unknown entry" and a nonzero
# PreToolUse refuses every tool in the session. Run each, deployed binary
# first; relay only exit 0.
stdin=$(cat)
errfile=$(mktemp -t big-stderr.XXXXXX) || errfile=/dev/null
trap 'rm -f "$errfile"' EXIT
for bin in "$(command -v fno-agent 2>/dev/null)" "${FNO_AGENT_BIN:-}" \
    "$PWD"/crates/fno-agent/target/{release,debug}/fno-agent; do
    [[ -n "$bin" && -x "$bin" ]] || continue
    if out="$(printf '%s' "$stdin" | "$bin" hook bin-install-guard 2>"$errfile")"; then
        cat "$errfile" >&2
        printf '%s\n' "$out"
        exit 0
    fi
done
echo "bin-install-guard: no fno-agent could answer the hook command; allowing" >&2
printf '%s\n' '{}'
