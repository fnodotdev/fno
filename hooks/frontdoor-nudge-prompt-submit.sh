#!/usr/bin/env bash
# fno hook: UserPromptSubmit - frontdoor nudge prompt submit
# The first prompt after a plugin install relays the install notice once per version. It lives in crates/fno-agent/src/runtime/hook/front/user_prompt.rs.
exec "${BASH_SOURCE[0]%/*}/hook-front.sh" UserPromptSubmit --leg frontdoor-nudge-prompt-submit "$@"
