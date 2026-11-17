#!/usr/bin/env bash
# The class homes (x-c52e): rebuildable caches and out-of-config-home state
# live outside the state root. One helper per class; the precedence mirrors
# paths_cli::class_home: the env carrier, then the config paths key, then the
# platform default. Sourced, never executed. The full helpers read
# `fno config get` best-effort (a machine without fno keeps the platform
# default); the *_fast variants skip that fork for hot paths (D1839 holds
# PreToolUse under 150ms) and answer env-then-platform only.
#
# Mirrors crates/fno-agent/src/paths_cli.rs class_home / cache_home /
# state_home. Change one, check the other.

# Full resolution: env carrier, config override, platform default.
fno_cache_home() {
    if [[ -n "${FNO_CACHE_HOME:-}" ]]; then
        printf '%s\n' "${FNO_CACHE_HOME/#\~/$HOME}"
        return 0
    fi
    local raw=""
    if command -v fno >/dev/null 2>&1; then
        raw="$(fno config get config.paths.cache_dir 2>/dev/null || true)"
    fi
    if [[ "$raw" != "null" && -n "$raw" ]]; then
        printf '%s\n' "${raw/#\~/$HOME}"
        return 0
    fi
    fno_cache_home_fast
}

# Env carrier then platform default; no fno fork.
fno_cache_home_fast() {
    if [[ -n "${FNO_CACHE_HOME:-}" ]]; then
        printf '%s\n' "${FNO_CACHE_HOME/#\~/$HOME}"
        return 0
    fi
    if [[ "$(uname -s)" == "Darwin" ]]; then
        printf '%s\n' "$HOME/Library/Caches/fno"
    else
        printf '%s\n' "${XDG_CACHE_HOME:-$HOME/.cache}/fno"
    fi
}

# Full resolution for the state home (logs).
fno_state_home() {
    if [[ -n "${FNO_STATE_HOME:-}" ]]; then
        printf '%s\n' "${FNO_STATE_HOME/#\~/$HOME}"
        return 0
    fi
    local raw=""
    if command -v fno >/dev/null 2>&1; then
        raw="$(fno config get config.paths.state_dir_local 2>/dev/null || true)"
    fi
    if [[ "$raw" != "null" && -n "$raw" ]] && [[ "$raw" != "-"* ]]; then
        printf '%s\n' "${raw/#\~/$HOME}"
        return 0
    fi
    printf '%s\n' "${XDG_STATE_HOME:-$HOME/.local/state}/fno"
}
