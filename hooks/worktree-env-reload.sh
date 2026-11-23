#!/usr/bin/env bash
# fno hook: CwdChanged - worktree env reload
# CwdChanged/FileChanged hook: reload env vars for worktree-aware execution
#
# When Claude cd's into a worktree or a .env file changes,
# this hook writes the correct env vars to CLAUDE_ENV_FILE
# so subsequent Bash commands use worktree-specific config.
#
# Supports direnv (preferred) or manual .env parsing (fallback).
#
# The fallback reads untrusted folder data. Keys must be plain shell
# names, each value is written unset-then-exported in single quotes so
# no shell evaluates it, and reserved controls are dropped so a
# checked-out repo cannot steer fno, the harness or the loader. The
# denylist is not a sandbox. Allowed names ship in FNO_PROJECT_ENV_KEYS,
# written last; credential readers refuse a name on that list.
set -uo pipefail

# CLAUDE_ENV_FILE is set by CC - it's the file CC sources before each Bash command
[[ -n "${CLAUDE_ENV_FILE:-}" ]] || exit 0

# If direnv is available and .envrc exists, use it (handles everything)
if command -v direnv >/dev/null 2>&1 && [[ -f ".envrc" ]]; then
    : > "$CLAUDE_ENV_FILE"
    direnv export bash >> "$CLAUDE_ENV_FILE" 2>/dev/null
    exit 0
fi

# Reserved controls a project env file may not set, matched
# case-insensitively against the lower-cased key. The list covers
# integer-eval, startup-exec, toolchain, credential and proxy steering;
# it is not a sandbox.
is_reserved() {
    case "$1" in
    fno_* | claude* | codex_* | otel_* | git_* | xdg_* | bash_func_* | ld_* | dyld_* | curl_* | ssl_* | \
        prompt_command | fpath | cdpath | module_path | manpath | globignore | execignore | nullcmd | readnullcmd | \
        shell | editor | visual | pager | browser | lessopen | lessclose | logname | tmpdir | makeflags | \
        node_options | node_tls_* | node_extra_ca_certs | python* | perl5* | rubyopt | java_tool_options | \
        rustc* | cargo_* | npm_config_* | ssh_* | gh_* | anthropic_* | openai_base_url | *_proxy | *_ca_bundle | \
        home | path | user | env | bash_env | shellopts | bashopts | ps4 | zdotdir | ifs | ci | do_not_track) return 0 ;;
    esac
    return 1
}

# Clear previous env before reloading (prevent unbounded growth)
: > "$CLAUDE_ENV_FILE"

# Fallback: parse .env files manually into CLAUDE_ENV_FILE
# Read from most specific to least specific (later values win)
name_re='^[A-Za-z_][A-Za-z0-9_]*$'
key_names=""
for envfile in .env .env.development .env.development.local .env.local; do
    if [[ -f "$envfile" ]]; then
        while IFS= read -r line || [[ -n "$line" ]]; do
            # Skip comments and empty lines
            [[ -z "$line" || "$line" =~ ^[[:space:]]*# ]] && continue
            # Skip lines without =
            [[ "$line" == *"="* ]] || continue

            key="${line%%=*}"
            value="${line#*=}"

            # Strip leading/trailing whitespace from key
            key=$(printf '%s' "$key" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
            # Accept shell names only
            [[ "$key" =~ $name_re ]] || continue

            # Drop reserved controls (case-insensitive)
            key_lc=$(printf '%s' "$key" | tr '[:upper:]' '[:lower:]')
            is_reserved "$key_lc" && continue

            # Strip one matching pair of surrounding quotes from the value
            case "$value" in
            '"'*'"')
                value="${value#\"}"
                value="${value%\"}"
                ;;
            "'"*"'")
                value="${value#\'}"
                value="${value%\'}"
                ;;
            esac

            # unset -v first: it drops the integer and nameref attributes
            # that make assignment evaluate (HISTCMD, OPTIND, RANDOM,
            # SRANDOM), and it never touches a function. Single quotes
            # keep tilde and zsh equals expansion inert; the C-locale sed
            # doubles any single quote byte-wise. The subshell trial runs
            # the assignment first, so a shell that refuses it (a
            # read-only special in zsh) skips one line instead of
            # aborting the whole source and the name list with it.
            quoted=$(printf '%s' "$value" | LC_ALL=C sed "s/'/'\\\\''/g")
            assign="unset -v $key && export $key='$quoted'"
            printf '( %s ) >/dev/null 2>&1 && %s\n' "$assign" "$assign" >> "$CLAUDE_ENV_FILE"
            key_names="${key_names}${key}
"
        done < "$envfile"
    fi
done

# Hook-owned name list, written last and sorted: credential readers
# refuse a name on this list.
if [[ -n "$key_names" ]]; then
    listed=$(printf '%s' "$key_names" | LC_ALL=C sort -u | tr '\n' ' ')
    printf 'export FNO_PROJECT_ENV_KEYS="%s"\n' "${listed% }" >> "$CLAUDE_ENV_FILE"
fi

exit 0
