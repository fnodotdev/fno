#!/usr/bin/env bash
# fno hook: UserPromptSubmit - wake resume protocol.
# A bare wake word on a session with an open target run injects the resume protocol. It lives in crates/fno-agent/src/runtime/hook/front/user_prompt.rs.
exec "${BASH_SOURCE[0]%/*}/hook-front.sh" UserPromptSubmit --leg wake-resume-inject "$@"
