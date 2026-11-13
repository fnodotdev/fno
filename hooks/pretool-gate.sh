#!/usr/bin/env bash
# fno hook: PreToolUse - every native guard in one process
# PreToolUse: `fno-agents hook gate` runs the write gate, the lead guard, the
# rules, the Bash guard chain, the effect, send-message and subagent-worktree
# guards, the session-state report and the control-mail drain in one
# process, and prints one merged answer (crates/fno-agents/src/runtime/hook/gate.rs).
# They were six hook processes per Bash call, each a bash start plus a
# binary start.
#
# Hot path: stdin rides the read builtin and PATH resolution never forks.
# Never exec a candidate blindly: an old build answers "unknown command:
# hook", and a nonzero PreToolUse refuses every tool in the session. Run
# each, deployed binary first; relay only exit 0. If nothing can answer,
# the write gate's posture holds: a payload naming a protected state file
# fails CLOSED, everything else fails open.
stdin=
IFS= read -r -d '' stdin || true
root="$(cd -- "${0%/*}/.." && pwd)"
bins=()
[[ -n "${FNO_AGENTS_BIN:-}" ]] && bins+=("$FNO_AGENTS_BIN")
[[ -n "${FNO_AGENTS_FRONT:-}" ]] && bins+=("$FNO_AGENTS_FRONT")
IFS=: read -ra path_dirs <<<"$PATH"
for dir in "${path_dirs[@]}"; do
    # An empty PATH segment means the cwd, as command -v read it.
    dir="${dir:-.}"
    [[ -x "$dir/fno-agents" ]] && { bins+=("$dir/fno-agents"); break; }
done
bins+=("$PWD"/crates/fno-agents/target/release/fno-agents \
    "$PWD"/crates/fno-agents/target/debug/fno-agents)
for bin in "${bins[@]}"; do
    [[ -x "$bin" ]] || continue
    if out="$(FNO_REPO_ROOT="$root" "$bin" hook gate <<<"$stdin")"; then
        printf '%s\n' "$out"
        exit 0
    fi
done
echo "pretool-gate: no fno-agents could answer the hook entry" >&2
case "$stdin" in
    *graph.json*|*target-state.md*|*graph.db*|*.fno/artifacts/*)
        printf '%s\n' '{"decision":"block","reason":"graph-write-protect: no fno-agents binary available to parse a payload referencing a protected state file; blocking fail-closed. Install fno (it ships fno-agents).","hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"graph-write-protect: no fno-agents binary available to parse a payload referencing a protected state file; blocking fail-closed. Install fno (it ships fno-agents)."}}'
        exit 0
        ;;
esac
printf '%s\n' '{}'
