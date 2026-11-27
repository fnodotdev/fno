#!/usr/bin/env bash
# fno hook: SessionStart - daemon liveness + heal
# With the fno-agent daemon down, fire the gated `fno-agent restart --if-down` detached and say so. The leg lives in crates/fno-agent/src/runtime/hook/front/session_start.rs.
exec "${BASH_SOURCE[0]%/*}/hook-front.sh" SessionStart --leg daemon-liveness-session-start "$@"
