#!/usr/bin/env bash
# fno hook: agy PreToolUse - the pretool host shim (agy wire)
# Runs the full claude PreToolUse block guard chain for one agy tool call via
# the fno-agent pretool host; the binary resolver and fail-open wrapper,
# exactly like the lead guard shim it replaces.
stdin=$(cat)
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
errfile=$(mktemp -t agpt-stderr.XXXXXX) || errfile=/dev/null
trap 'rm -f "$errfile" 2>/dev/null' EXIT
for bin in "$(command -v fno-agent 2>/dev/null)" "${FNO_AGENT_BIN:-}" \
    "$PWD"/crates/fno-agent/target/{release,debug}/fno-agent; do
    [[ -n "$bin" && -x "$bin" ]] || continue
    if out="$(printf '%s' "$stdin" | "$bin" hook pretool --wire agy --plugin-root "$root" 2>"$errfile")"; then
        cat "$errfile" >&2
        printf '%s\n' "$out"
        exit 0
    fi
done
echo "agy-pretool: no fno-agent could answer the hook command; allowing" >&2
printf '{"decision":"allow"}\n'
