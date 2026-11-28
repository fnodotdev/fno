#!/usr/bin/env bash
# fno hook: PostToolUse - launch the spend + model-drift monitor
# Warns once when an interactive session passes its spend cap or runs a model other than the attested one. The leg lives in crates/fno-agent/src/runtime/hook/front/post_tool.rs.
exec "${BASH_SOURCE[0]%/*}/hook-front.sh" PostToolUse --leg spend-drift-monitor "$@"
