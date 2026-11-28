#!/usr/bin/env bash
# fno hook: PostToolUse - surface a pending friction offer once
# This session's friction offer prints once, then the leg deletes it. It lives in crates/fno-agent/src/runtime/hook/front/user_prompt.rs.
exec "${BASH_SOURCE[0]%/*}/hook-front.sh" UserPromptSubmit --leg friction-offer-inject "$@"
