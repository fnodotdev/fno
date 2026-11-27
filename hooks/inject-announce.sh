#!/usr/bin/env bash
# fno hook: multi-event - inject announcements
# Fleet announcements print once per session. $1 is the boundary: start, prompt or compact. It lives in crates/fno-agent/src/runtime/hook/front/user_prompt.rs.
exec "${BASH_SOURCE[0]%/*}/hook-front.sh" UserPromptSubmit --leg inject-announce "$@"
