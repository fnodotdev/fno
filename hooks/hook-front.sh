#!/usr/bin/env bash
# fno hook: UserPromptSubmit, Stop, SessionStart, PostToolUse - every leg in one process
# `hook-front.sh <Event>`: `fno-agent hook front <Event>` runs every former
# hooks.json leg of the event in process, each on its own thread under its
# registered timeout, and prints one merged answer
# (crates/fno-agent/src/runtime/hook/front.rs). They were up to eleven hook
# processes per prompt, each a bash start plus a binary or jq start.
# `hook-front.sh <Event> --leg <name> [args...]` runs one leg alone: the
# scripts codex, pi and opencode still run by name exec this form.
#
# Hot path: stdin rides the read builtin and PATH resolution never forks.
# Never exec a candidate blindly: an old build answers "unknown entry" at
# exit 2, and exit 2 blocks a Stop or a prompt. Run each, deployed binary
# first; relay only exit 0. If nothing can answer, the event answers `{}`
# (every leg failed open before) and a lone leg answers nothing.
stdin=
IFS= read -r -d '' stdin || true
root="$(cd -- "${0%/*}/.." && pwd)"
bins=()
[[ -n "${FNO_AGENT_BIN:-}" ]] && bins+=("$FNO_AGENT_BIN")
[[ -n "${FNO_AGENT_FRONT:-}" ]] && bins+=("$FNO_AGENT_FRONT")
IFS=: read -ra path_dirs <<<"$PATH"
for dir in "${path_dirs[@]}"; do
    # An empty PATH segment means the cwd, as command -v read it.
    dir="${dir:-.}"
    [[ -x "$dir/fno-agent" ]] && { bins+=("$dir/fno-agent"); break; }
done
bins+=("$PWD"/crates/fno-agent/target/release/fno-agent \
    "$PWD"/crates/fno-agent/target/debug/fno-agent)
for bin in "${bins[@]}"; do
    [[ -x "$bin" ]] || continue
    if out="$(FNO_HOOK_PLUGIN_ROOT="$root" "$bin" hook front "$@" <<<"$stdin")"; then
        [[ -n "$out" ]] && printf '%s\n' "$out"
        exit 0
    fi
done
echo "hook-front: no fno-agent could answer hook front $*" >&2
# No binary at a session start usually means a plugin-only install: the
# front-door hook prints the install notice, as context-run.sh did.
if [[ "$*" == "SessionStart" ]]; then
    notice="$(bash "$root/hooks/frontdoor-nudge-session-start.sh" 2>/dev/null)"
    [[ -n "$notice" ]] && { printf '%s\n' "$notice"; exit 0; }
fi
[[ "${2:-}" == "--leg" ]] || printf '%s\n' '{}'
exit 0
