#!/usr/bin/env bash
# fno pass: SessionStart - retire unread auto_generated memory entries by move.
# Move unread auto_generated entries past probation to .archived/ and prune MEMORY.md. Args: --memory-dir DIR, --probation-days N (default 30), --dry-run. The leg lives in crates/fno-agent/src/runtime/hook/front/session_start.rs.
exec "${BASH_SOURCE[0]%/*}/../../hooks/hook-front.sh" SessionStart --leg retire-unread-pass "$@"
