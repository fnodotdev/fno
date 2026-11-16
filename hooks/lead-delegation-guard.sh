#!/usr/bin/env bash
# fno hook: PreToolUse - lead delegation guard
# PreToolUse: a teamed org session does not implement (policy:
# crates/fno-agent/src/runtime/hook/lead_guard.rs). Never exec a candidate: an old
# build answers "unknown command: hook", and a nonzero PreToolUse refuses every
# tool in the session. Run each, deployed binary first; relay only exit 0.
stdin=$(cat)
errfile=$(mktemp -t kgd-stderr.XXXXXX) || errfile=/dev/null
trap 'rm -f "$errfile"' EXIT
for bin in "$(command -v fno-agent 2>/dev/null)" "${FNO_AGENT_BIN:-}" \
    "$PWD"/crates/fno-agent/target/{release,debug}/fno-agent; do
    [[ -n "$bin" && -x "$bin" ]] || continue
    # Both command spellings for one release: the installed binary and the
    # repo hook update at different times.
    for command in lead-guard; do
        if out="$(printf '%s' "$stdin" | "$bin" "hook" "$command" 2>"$errfile")"; then
            cat "$errfile" >&2
            printf '%s\n' "$out"
            exit 0
        fi
    done
done
echo "lead-delegation-guard: no fno-agent could answer the hook command; allowing" >&2
printf '%s\n' '{}'
