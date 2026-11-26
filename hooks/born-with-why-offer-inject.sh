#!/usr/bin/env bash
# fno hook: UserPromptSubmit - born with why offer inject
# An unconsumed born-with-why offer surfaces once as a reminder. It lives in crates/fno-agent/src/runtime/hook/front/user_prompt.rs.
exec "${BASH_SOURCE[0]%/*}/hook-front.sh" UserPromptSubmit --leg born-with-why-offer-inject "$@"
