#!/usr/bin/env bash
# fno hook: SessionStart - claude supervisor bootstrap
# Name a claude supervisor born outside the Aqua GUI session, or one that lost DNS, in at most one line. The leg lives in crates/fno-agent/src/runtime/hook/front/session_start.rs.
exec "${BASH_SOURCE[0]%/*}/hook-front.sh" SessionStart --leg bootstrap-session-start "$@"
