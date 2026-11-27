#!/usr/bin/env bash
# fno hook: Stop - context nudge
# The context, orphan and flush nudges at turn end. The leg lives in crates/fno-agent/src/runtime/hook/front/stop.rs; codex runs this script by name.
exec "${BASH_SOURCE[0]%/*}/hook-front.sh" Stop --leg context-nudge "$@"
