#!/usr/bin/env bash
# Packaged-hook acceptance. The generated plugin tree is retired (the
# catalogs pin the release tag directly), so this runs the packaged hooks
# against a raw archive of HEAD. CI-only, like the tree job it replaces.
set -euo pipefail
[[ "${GITHUB_ACTIONS:-}" == true ]] || { echo 'run plugin-hook acceptance in CI' >&2; exit 1; }
repo=$(git rev-parse --show-toplevel)
scratch=$(mktemp -d)
trap 'chmod -R u+w "$scratch"; rm -rf "$scratch"' EXIT
mkdir "$scratch/source"
git archive HEAD | tar -x -C "$scratch/source"
tree="$scratch/source"
sha=$(git rev-parse HEAD)

# A stamped tree carries SOURCE_COMMIT at its root; write it here so the
# session-start comparison has a stamp to read in a raw archive.
printf '%s\n' "$sha" > "$tree/SOURCE_COMMIT"

# The packaged joined-worker hook must relay the native judge's denial and
# start no uv. This job builds no fno-agents, so a stub answers the
# `hook join-partition-guard` arm with the judge's block shape; the judge's
# own decisions are unit-tested beside it in crates/fno-agents.
mkdir -p "$scratch/project/.fno/join-partition" "$scratch/bin" "$scratch/home"
printf '%s\n' '{"band":"one","deny_edit":["peer.rs"]}' > "$scratch/project/.fno/join-partition/ci-one.json"
printf '#!/bin/sh\ntouch "%s/uv-called"\nexit 1\n' "$scratch" > "$scratch/bin/uv"
printf '#!/bin/sh\n[ "$1 $2" = "hook join-partition-guard" ] || exit 2\ncat > "%s/judge-input"\nprintf '\''%%s\\n'\'' '\''{"decision":"block","reason":"peer.rs","hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"peer.rs"}}'\''\n' "$scratch" > "$scratch/bin/fno-agents"
chmod +x "$scratch/bin/uv" "$scratch/bin/fno-agents"
chmod -R a-w "$tree"
decision=$(printf '{"cwd":"%s/project","tool_input":{"file_path":"peer.rs"}}' "$scratch" |
    PATH="$scratch/bin:$PATH" HOME="$scratch/home" FNO_WORKER_NAME=ci-one \
    FNO_AGENTS_BIN="$scratch/bin/fno-agents" \
    bash "$tree/hooks/join-partition-write-guard.sh")
jq -e '.decision == "block"' <<<"$decision"
grep -qF 'peer.rs' "$scratch/judge-input"
[[ ! -e "$scratch/uv-called" ]]

# Both matching and mismatching source versions exercise the shared warning.
printf '#!/bin/sh\nprintf '\''{"git_rev":"%s"}\\n'\''\n' "${sha:0:12}" > "$scratch/bin/fno"
chmod +x "$scratch/bin/fno"
[[ -z "$(PATH="$scratch/bin:$PATH" bash "$tree/hooks/plugin-source-session-start.sh")" ]]
other=000000000000
[[ "$sha" != "$other"* ]] || other=111111111111
printf '#!/bin/sh\nprintf '\''{"git_rev":"%s"}\\n'\''\n' "$other" > "$scratch/bin/fno"
chmod +x "$scratch/bin/fno"
PATH="$scratch/bin:$PATH" bash "$tree/hooks/plugin-source-session-start.sh" > "$scratch/warning"
grep -F 'Footnote source mismatch:' "$scratch/warning"
echo "plugin-hook CI proof: join-partition relay and source-mismatch warning hold on the raw archive"
