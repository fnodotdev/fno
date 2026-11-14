#!/usr/bin/env bash
# fno hook: PreCompact and SessionEnd - save a readable session transcript.
# Policy lives in crates/fno-agents/src/runtime/hook/save_session.rs. A machine with no
# fno-agents saves nothing; a hook here never blocks.
# Claude Code gives all SessionEnd hooks one shared 1.5 s budget, and a plugin
# hook's own timeout does not raise it, so no work runs inline: stdin goes to a
# temp file, the save starts detached (nohup + disown survives session
# teardown), and this hook exits at once. Removing the file here is safe - the
# child holds the open fd and reads it to EOF.
command -v fno-agents >/dev/null 2>&1 || exit 0
stdin_file=$(mktemp "${TMPDIR:-/tmp}/fno-save-session.XXXXXX") || exit 0
cat > "$stdin_file"
nohup fno-agents hook save-session < "$stdin_file" >/dev/null 2>&1 &
disown 2>/dev/null || true
rm -f "$stdin_file"
exit 0
