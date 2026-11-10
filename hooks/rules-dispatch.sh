#!/usr/bin/env bash
# fno hook: multi-event - the rule engine's hook decision points
# UserPromptSubmit and PreToolUse: `fno-agents hook rules --event <event>`
# decides the event against the rule set (fno::rules::decide) and prints the
# deny, the context, or `{}`. $1 is prompt or pre-tool; a PreToolUse Skill
# call answers as the skill event inside the binary.
#
# Fail open: a missing binary or a failing run allows the call.
stdin=
IFS= read -r -d '' stdin || true
event="${1:-pre-tool}"
root="$(cd -- "${0%/*}/.." && pwd)"
# shellcheck source=../scripts/lib/with-timeout.sh
source "$root/scripts/lib/with-timeout.sh" 2>/dev/null || { printf '%s\n' '{}'; exit 0; }
bins=()
[[ -n "${FNO_AGENTS_BIN:-}" ]] && bins+=("$FNO_AGENTS_BIN")
[[ -n "${FNO_AGENTS_FRONT:-}" ]] && bins+=("$FNO_AGENTS_FRONT")
IFS=: read -ra path_dirs <<<"$PATH"
for dir in "${path_dirs[@]}"; do
    dir="${dir:-.}"
    [[ -x "$dir/fno-agents" ]] && { bins+=("$dir/fno-agents"); break; }
done
for bin in "${bins[@]}"; do
    [[ -x "$bin" ]] || continue
    # 2s, not 3s: the harness cap for this hook is 4s and the preamble (bash
    # startup, the source chain, the with_timeout watchdog) already costs a
    # fraction of a second under load, so a bound at 3s left the pair one
    # hiccup from the cap (measured 2026-11-08: this hook's UserPromptSubmit
    # leg recorded zero successes over three days, every run killed at cap).
    if out="$(with_timeout 2 env FNO_REPO_ROOT="$root" "$bin" hook rules --event "$event" <<<"$stdin")"; then
        printf '%s\n' "$out"
        exit 0
    fi
done
printf '%s\n' '{}'
