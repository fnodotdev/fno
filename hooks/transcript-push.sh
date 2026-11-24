#!/usr/bin/env bash
# fno hook: Stop - transcript push (tombstone)
# Retired: the sandbox host relay now writes the sandbox transcript itself.
# This stub exits 0, so a session that cached the old registration keeps a
# working shell. Delete it one release after the registration left hooks.json
# (the shim tombstone rule, docs/architecture/multi-cli-hooks.md).
exit 0
