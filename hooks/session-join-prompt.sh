#!/usr/bin/env bash
# fno hook: UserPromptSubmit and PostToolUse Skill - session join trigger
# A prompt or Skill call that names an fno command joins the session and lands the using-fno preamble. It lives in crates/fno-agent/src/runtime/hook/front/user_prompt.rs.
exec "${BASH_SOURCE[0]%/*}/hook-front.sh" UserPromptSubmit --leg session-join-prompt "$@"
