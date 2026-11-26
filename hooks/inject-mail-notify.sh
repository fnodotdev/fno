#!/usr/bin/env bash
# fno hook: UserPromptSubmit - notify about pending mail
# Pending mail renders at the turn boundary, then the cursor moves. It lives in crates/fno-agent/src/runtime/hook/front/user_prompt.rs.
exec "${BASH_SOURCE[0]%/*}/hook-front.sh" UserPromptSubmit --leg inject-mail-notify "$@"
