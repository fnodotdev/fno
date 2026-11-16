#!/usr/bin/env bash
# fno hook: PreCompact and SessionEnd - save a readable session transcript.
# Policy lives in crates/fno-agent/src/runtime/hook/save_session.rs. A machine with no
# fno-agent saves nothing; a hook here never blocks.
# Claude Code gives all SessionEnd hooks one shared 1.5 s budget, so no work
# runs inline: stdin goes to a temp file, the save starts detached, and this
# hook exits at once. The child leads its own session (setsid binary when
# present, perl POSIX::setsid otherwise, plain nohup as the last resort) so a
# runner's post-hook group cleanup cannot reap the save. Removing the temp
# file here is safe - the child holds the open fd and reads it to EOF. The
# child's stderr lands truncated in db/saved-sessions/save-session.err, so a
# save that silently stops working still leaves its last error behind.
command -v fno-agent >/dev/null 2>&1 || exit 0
stdin_file=$(mktemp "${TMPDIR:-/tmp}/fno-save-session.XXXXXX") || exit 0
cat > "$stdin_file"
err_dir="${FNO_HOME:-$HOME/.fno}/db/saved-sessions"
err_log="$err_dir/save-session.err"
mkdir -p "$err_dir" 2>/dev/null || err_log=/dev/null
if command -v setsid >/dev/null 2>&1; then
    setsid fno-agent hook save-session < "$stdin_file" > "$err_log" 2>&1 &
elif command -v perl >/dev/null 2>&1; then
    perl -e 'use POSIX; setsid(); exec @ARGV' \
        fno-agent hook save-session < "$stdin_file" > "$err_log" 2>&1 &
else
    nohup fno-agent hook save-session < "$stdin_file" > "$err_log" 2>&1 &
fi
disown 2>/dev/null || true
rm -f "$stdin_file"
exit 0
