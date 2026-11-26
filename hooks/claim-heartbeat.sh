#!/usr/bin/env bash
# fno hook: PostToolUse - renew claim heartbeat
# Renews this session's node claim TTL while it works, stamps worktree activity, and binds a raw `gh pr create` URL. The leg lives in crates/fno-agent/src/runtime/hook/front/post_tool.rs.
exec "${BASH_SOURCE[0]%/*}/hook-front.sh" PostToolUse --leg claim-heartbeat "$@"
