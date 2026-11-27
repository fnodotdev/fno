#!/usr/bin/env bash
# fno hook: multi-event - surface captured operator turns
# Reports the operator-capture queue depth as prompt Markdown. The leg lives in crates/fno-agent/src/runtime/hook/front/stop.rs; codex runs this script by name at SessionStart.
exec "${BASH_SOURCE[0]%/*}/hook-front.sh" Stop --leg operator-capture-nudge "$@"
