#!/usr/bin/env bash
# infer-has-ui.sh - derive has_ui from a changeset (file list on stdin).
#
# Echoes "true" if any path matches the locked frontend surface inference
# list, else "false". Delegates to the native executor-route door
# (crates/fno-agent/src/spawn/executor_route.rs), the single source of truth since
# the worker-family port deleted the Python leg, so has_ui inference and
# executor routing share the SAME locked patterns and can never drift (the
# bug this fixes: target init defaulted has_ui:false on the M profile even
# for obvious UI surfaces, so the browser/frontend-craft gates were silently
# skipped).
#
# Usage:
#   git diff --name-only main...HEAD | infer-has-ui.sh   # -> true | false
#   printf '%s\n' src/components/Foo.tsx | infer-has-ui.sh   # -> true

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# The fno-agent binary resolver, shared with the hooks. The canonical copy is
# hooks/lib/agents-bin.sh; this same-shape inline fallback covers a partial
# deploy whose hooks tree is not beside the scripts tree.
if [[ -f "${BASH_SOURCE[0]%/*}/../../hooks/lib/agents-bin.sh" ]]; then
    # shellcheck source=/dev/null
    source "${BASH_SOURCE[0]%/*}/../../hooks/lib/agents-bin.sh"
else
    fno_agent_bin() {
        local root="${1:-.}"
        if [[ -n "${FNO_AGENT_BIN:-}" ]] && [[ -x "${FNO_AGENT_BIN}" ]]; then
            printf '%s' "${FNO_AGENT_BIN}"
        elif [[ -x "$root/crates/fno-agent/target/release/fno-agent" ]]; then
            printf '%s' "$root/crates/fno-agent/target/release/fno-agent"
        elif [[ -x "$root/crates/fno-agent/target/debug/fno-agent" ]]; then
            printf '%s' "$root/crates/fno-agent/target/debug/fno-agent"
        else
            command -v fno-agent || printf ''
        fi
    }
fi

bin="$(fno_agent_bin "$SCRIPT_DIR/../..")"
# A missing binary keeps the old unimportable-module fail shape: nothing on
# stdout, one stderr note, nonzero exit. No Python fallback shell.
if [[ -z "$bin" ]]; then
    echo "infer-has-ui: fno-agent binary not found" >&2
    exit 1
fi
# --has-ui mode reads the same newline file list on stdin and echoes
# true/false, keeping this a stdin -> true/false filter with identical output.
exec "$bin" executor-route surface --has-ui
