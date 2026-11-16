# One resolver for the fno-agent binary, most-local first. This is the shared
# copy of the resolver that hooks/footnote-agy-target-stop-hook.sh and
# hooks/session-state.sh source; target-stop-hook.sh keeps its own copy
# under separate ownership.
fno_agent_bin() {
    local root="${1:-.}"
    if [[ -n "${FNO_AGENT_BIN:-}" ]] && [[ -x "${FNO_AGENT_BIN}" ]]; then
        printf '%s' "$FNO_AGENT_BIN"
    elif [[ -x "$root/crates/fno-agent/target/release/fno-agent" ]]; then
        printf '%s' "$root/crates/fno-agent/target/release/fno-agent"
    elif [[ -x "$root/crates/fno-agent/target/debug/fno-agent" ]]; then
        printf '%s' "$root/crates/fno-agent/target/debug/fno-agent"
    else
        command -v fno-agent || printf ''
    fi
}
