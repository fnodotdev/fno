#!/usr/bin/env bash
# fno hook: UserPromptSubmit - first-turn node bind
# A first turn that names one open node binds the session to it. It lives in crates/fno-agent/src/runtime/hook/front/user_prompt.rs.
exec "${BASH_SOURCE[0]%/*}/hook-front.sh" UserPromptSubmit --leg node-bind-prompt-submit "$@"
