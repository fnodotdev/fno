#!/usr/bin/env bash
# fno hook: SessionStart - session start using fno
# The using-fno stub as context for a session that joined fno. Args: [EVENT] [join] [trigger]; the answer is {} when the preamble is not due. The leg lives in crates/fno-agent/src/runtime/hook/front/session_start.rs.
exec "${BASH_SOURCE[0]%/*}/hook-front.sh" SessionStart --leg session-start-using-fno "$@"
