#!/usr/bin/env bash
# tests/memory/test_read_count_retire.sh
#
# Reads of auto_generated memory entries are counted in a sidecar,
# and a session-start pass retires a never-read entry past probation by
# MOVE, never delete. Both arms of every guard live here: the counter
# appends in-dir and stays silent out-of-dir and for index files; the pass
# archives a zero-read past-probation auto entry and keeps entries that are
# on probation, read, writer-updated, hand-written, or unparsable.
#
# Run: bash tests/memory/test_read_count_retire.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
COUNTER="$REPO_ROOT/scripts/memory/read-count-hook.sh"
RETIREE="$REPO_ROOT/scripts/memory/retire-unread-pass.sh"
WRITER="$REPO_ROOT/scripts/memory/write-memory-entry.sh"
HOOKS_JSON="$REPO_ROOT/hooks/hooks.json"

log()  { printf '[read-count-retire] %s\n' "$*"; }
fail() { printf '[read-count-retire] FAIL: %s\n' "$*" >&2; exit 1; }
pass() { printf '[read-count-retire] ok: %s\n' "$*"; }

[[ -x "$COUNTER" ]] || fail "counter not executable at $COUNTER (expected failure in RED)"
[[ -x "$RETIREE" ]] || fail "retire pass not executable at $RETIREE (expected failure in RED)"
[[ -x "$WRITER" ]] || fail "writer not executable at $WRITER"

WORK=$(mktemp -d -t read-count-retire-XXXXXX)
trap 'rm -rf "$WORK"' EXIT

# The counter and the pass derive the projects root the way Claude Code
# does: $CLAUDE_CONFIG_DIR/projects when set, else $HOME/.claude/projects.
export CLAUDE_CONFIG_DIR="$WORK/claude-config"
PROJ_ROOT="$WORK/proj/fnodotdev/fno"
# Same documented transform the scripts implement: every non-alphanumeric
# byte becomes '-', so the leading slash becomes a leading dash.
SLUG="$(printf '%s' "$PROJ_ROOT" | LC_ALL=C tr -c '[:alnum:]' '-')"
MEM="$CLAUDE_CONFIG_DIR/projects/$SLUG/memory"
mkdir -p "$MEM"

iso_old="2026-01-01T00:00:00Z"
if date -u -v-1d "+%Y-%m-%dT%H:%M:%SZ" >/dev/null 2>&1; then
    iso_new="$(date -u -v-1d "+%Y-%m-%dT%H:%M:%SZ")"   # BSD/macOS
else
    iso_new="$(date -u -d "1 day ago" "+%Y-%m-%dT%H:%M:%SZ")"   # GNU
fi

entry() { # entry FILENAME CREATED_AT [INDENT_AUTO]
    local f="$MEM/$1" created="$2" indent="${3:-}"
    {
        printf -- '---\n'
        printf 'name: %s\n' "${1%.md}"
        printf 'description: fixture entry\n'
        printf 'type: project\n'
        if [[ "$indent" == "indented" ]]; then
            printf '  auto_generated: true\n'
        else
            printf 'auto_generated: true\n'
        fi
        printf 'source_session: sid-fixture\n'
        printf 'created_at: %s\n' "$created"
        printf -- '---\nBody of %s.\n' "${1%.md}"
    } > "$f"
}

# Fixtures. Distinct basename per case so one pass run judges all of them.
entry project_old_zero.md        "$iso_old"          # AC4: archived (must stay zero-read)
entry project_indented_auto.md   "$iso_old" indented # archived only if the parser trims
entry project_old_read.md        "$iso_old"          # AC6: has a read line, kept
entry project_old_updated.md     "$iso_old"          # AC7: writer update line, kept
entry project_new_auto.md        "$iso_new"          # AC5: on probation, kept
entry project_handwritten.md     "$iso_old"          # AC8: no auto tag, kept
entry project_unparsable.md      "not-a-timestamp"   # AC9: unparsable created_at, kept
entry project_read_probe.md      "$iso_old"          # counter-test target only; its reads keep it
# The AC7 fixture's only use is a writer update event, the shape the writer
# appends on its update path. The AC6 fixture's only use is one read line.
# Both are seeded before the pass so the single destructive run judges them.
printf '{"file":"project_old_updated.md","ts":"2026-01-01T00:00:00Z","kind":"update","session":"sid-x"}\n' \
    >> "$MEM/.reads.jsonl"
printf '{"file":"project_old_read.md","ts":"2026-01-01T00:00:00Z","kind":"read","session":"sid-x"}\n' \
    >> "$MEM/.reads.jsonl"
sed -i '' '/auto_generated: true/d' "$MEM/project_handwritten.md" 2>/dev/null \
    || sed -i '/auto_generated: true/d' "$MEM/project_handwritten.md"
mkdir -p "$MEM/.archived"
printf 'ARCHIVED-FIRST (a prior retire of the same name)\n' \
    > "$MEM/.archived/project_old_zero.md"              # AC10: collision seed
cat > "$MEM/MEMORY.md" <<'EOF'
# Memory Index

- [old zero](project_old_zero.md) - fixture entry
- [old read](project_old_read.md) - fixture entry
EOF

sidecar_lines() { [[ -f "$MEM/.reads.jsonl" ]] && wc -l < "$MEM/.reads.jsonl" | tr -d ' ' || printf 0; }

read_feed() { # read_feed FILEPATH -> one counter invocation on stdin
    printf '{"tool_name":"Read","tool_input":{"file_path":"%s"},"cwd":"%s","session_id":"sid-counter"}' \
        "$1" "$PROJ_ROOT" | bash "$COUNTER"
    [[ $? -eq 0 ]] || fail "counter exited nonzero for $1"
}

# ── AC1-HP: one append per in-dir read ────────────────────────────────
log "AC1: counter appends one line per in-dir read"
# The AC7 fixture seeded one update line already; counts are relative to it.
BASE="$(sidecar_lines)"
read_feed "$MEM/project_read_probe.md"
[[ "$(sidecar_lines)" == "$((BASE + 1))" ]] || fail "expected one line after first read, $BASE -> $(sidecar_lines)"
grep -q '"file":"project_read_probe.md"' "$MEM/.reads.jsonl" || fail "line does not name the entry"
read_feed "$MEM/project_read_probe.md"
[[ "$(sidecar_lines)" == "$((BASE + 2))" ]] || fail "expected append-per-read, $((BASE + 1)) -> $(sidecar_lines)"
pass "AC1: append per read"

# ── AC2-EDGE: out-of-dir reads append nothing ─────────────────────────
log "AC2: out-of-dir read stays silent"
read_feed "$WORK/elsewhere/not-memory.md"
[[ "$(sidecar_lines)" == "$((BASE + 2))" ]] || fail "out-of-dir read appended a line"
pass "AC2: out-of-dir silent"

# ── AC3-EDGE: index files append nothing ──────────────────────────────
log "AC3: index-file reads stay silent"
read_feed "$MEM/MEMORY.md"
read_feed "$MEM/CLAUDE.md"
read_feed "$MEM/ARCHIVE-INDEX.md"
[[ "$(sidecar_lines)" == "$((BASE + 2))" ]] || fail "an index read appended a line"
pass "AC3: index reads silent"

# ── AC11b: a recreated name does not inherit stale use ────────────────
log "AC11b: sidecar lines predating created_at are not use"
GHOST="$CLAUDE_CONFIG_DIR/projects/-ghost-test/memory"
mkdir -p "$GHOST"
{
    printf -- '---\nname: ghosted\ndescription: fixture\ntype: project\nauto_generated: true\nsource_session: s\ncreated_at: %s\n---\nBody of ghosted.\n' "$iso_new"
} > "$GHOST/project_ghosted.md"
# Two ghost lines from a prior life of this name, predating the recreated
# entry's created_at. Stale lines must not protect the new entry.
printf '{"file":"project_ghosted.md","ts":"2025-01-01T00:00:00Z","kind":"read","session":"prior"}\n' >> "$GHOST/.reads.jsonl"
printf '{"file":"project_ghosted.md","ts":"2025-06-01T00:00:00Z","kind":"update","session":"prior"}\n' >> "$GHOST/.reads.jsonl"
bash "$RETIREE" --memory-dir "$GHOST" --probation-days 0 >/dev/null 2>&1 \
    || fail "ghost-filter pass run failed"
[[ -f "$GHOST/.archived/project_ghosted.md" ]] \
    || fail "stale ghost lines protected a recreated zero-read entry"
grep -q "Body of ghosted" "$GHOST/.archived/project_ghosted.md" \
    || fail "ghosted copy lost its body"
pass "AC11b: stale lines do not protect a recreated name"

# ── AC11a: writer update path appends one update line ─────────────────
log "AC11a: writer update appends one update line"
bash "$WRITER" --memory-dir "$MEM" --session-id sid-w1 \
    --evidence "sid-w1:1" \
    --candidate '{"type":"project","name":"writer_test","description":"sidecar bookkeeping fixture","body":"Body one."}' \
    >/dev/null 2>&1 || fail "writer new-file call failed"
SHA=$(sha256sum "$MEM/project_writer_test.md" 2>/dev/null | awk '{print $1}' \
    || shasum -a 256 "$MEM/project_writer_test.md" | awk '{print $1}')
BEFORE="$(sidecar_lines)"
bash "$WRITER" --memory-dir "$MEM" --session-id sid-w2 \
    --source-sha256 "$SHA" \
    --evidence "sid-w2:2" \
    --candidate '{"type":"project","name":"writer_test","description":"sidecar bookkeeping fixture","body":"Body two, revised."}' \
    >/dev/null 2>&1 || fail "writer update call failed"
AFTER="$(sidecar_lines)"
[[ "$AFTER" == "$((BEFORE + 1))" ]] || fail "expected exactly one new sidecar line, $BEFORE -> $AFTER"
tail -1 "$MEM/.reads.jsonl" | grep -q '"kind":"update"' || fail "last line is not an update event"
tail -1 "$MEM/.reads.jsonl" | grep -q 'project_writer_test.md' || fail "update line does not name the entry"
pass "AC11a: update line appended"

# ── AC dry-run: prints, moves nothing ─────────────────────────────────
log "dry-run: names would-move entries, moves nothing"
DRY_OUT=$(bash "$RETIREE" --memory-dir "$MEM" --probation-days 30 --dry-run)
printf '%s' "$DRY_OUT" | grep -q "project_old_zero.md" || fail "dry-run does not name the archive candidate"
[[ -f "$MEM/project_old_zero.md" ]] || fail "dry-run moved the entry"
pass "dry-run: names without moving"

# ── AC4/AC5-AC9: the single destructive pass run judges every fixture ─
log "pass: archives zero-read past-probation auto entries, keeps the rest"
PASS_OUT=$(bash "$RETIREE" --memory-dir "$MEM" --probation-days 30)
printf '%s' "$PASS_OUT" | grep -qi "archiv" || fail "move run printed no archive line"

# AC4: moved, not deleted; the body lives on the SUFFIXED copy because the
# bare .archived name is the collision seed; index pruned, keeper intact
[[ -f "$MEM/project_old_zero.md" ]] && fail "archived entry still sits at top level"
SUF="$(ls "$MEM/.archived/" 2>/dev/null | grep '^project_old_zero\.md\.[0-9]' | head -1)"
[[ -n "$SUF" ]] || fail "no suffixed archived copy for the moved entry"
grep -q "Body of project_old_zero" "$MEM/.archived/$SUF" || fail "archived copy lost its body"
grep -q "project_old_zero.md" "$MEM/MEMORY.md" && fail "MEMORY.md line for the archived entry survived"
grep -q "project_old_read.md" "$MEM/MEMORY.md" || fail "MEMORY.md prune removed a keeper's line"
pass "AC4: move not delete; content carried; MEMORY.md pruned surgically"

# AC10: the collision seed survived intact at the bare name
grep -q "ARCHIVED-FIRST" "$MEM/.archived/project_old_zero.md" || fail "archived copy was overwritten"
pass "AC10: collision suffixes instead of overwriting"

# AC5: probation keeper
[[ -f "$MEM/project_new_auto.md" ]] || fail "pass archived an entry on probation"
pass "AC5: on-probation entry kept"

# AC6: read keeper
[[ -f "$MEM/project_old_read.md" ]] || fail "pass archived an entry with a recorded read"
pass "AC6: read entry kept"

# AC7: writer-update keeper: its only sidecar line is a writer update event
[[ -f "$MEM/project_old_updated.md" ]] || fail "pass archived an entry whose only use is a writer update"
pass "AC7: writer-update-only entry kept"

# AC8: hand-written keeper
[[ -f "$MEM/project_handwritten.md" ]] || fail "pass archived a hand-written entry"
pass "AC8: hand-written entry kept"

# AC9: unparsable created_at keeper
[[ -f "$MEM/project_unparsable.md" ]] || fail "pass archived an entry with unparsable created_at"
pass "AC9: unparsable entry kept"

# The indented-auto entry: archived ONLY if the parser trims frontmatter keys.
[[ -f "$MEM/.archived/project_indented_auto.md" ]] || fail "indented auto_generated frontmatter was not recognized (parse must trim)"
[[ -f "$MEM/project_indented_auto.md" ]] && fail "indented entry stayed top level"
pass "frontmatter parse trims leading spaces"

# ── silence when nothing to do ────────────────────────────────────────
QUIET_OUT=$(bash "$RETIREE" --memory-dir "$MEM" --probation-days 30)
[[ -z "$QUIET_OUT" ]] || fail "keep-only run printed output: $QUIET_OUT"
pass "pass is silent when nothing moved"

# ── stdin-derived memory dir (no --memory-dir) ────────────────────────
log "stdin path: pass derives the dir from the hook payload"
PROJ2="$WORK/proj/second/site"
SLUG2="$(printf '%s' "$PROJ2" | LC_ALL=C tr -c '[:alnum:]' '-')"
MEM2="$CLAUDE_CONFIG_DIR/projects/$SLUG2/memory"
mkdir -p "$MEM2"
entry2="$MEM2/project_second_old.md"
{
    printf -- '---\nname: second_old\ndescription: fixture\ntype: project\nauto_generated: true\nsource_session: s\ncreated_at: %s\n---\nBody.\n' "$iso_old"
} > "$entry2"
printf '{"cwd":"%s","session_id":"sid-hook"}' "$PROJ2" | bash "$RETIREE" --probation-days 30 >/dev/null \
    || fail "stdin-derived pass run failed"
[[ -f "$MEM2/.archived/project_second_old.md" ]] || fail "stdin-derived run did not archive the zero-read entry"
pass "stdin-derived memory dir works"

# ── AC12: registration is valid JSON and names both scripts ───────────
log "AC12: hooks.json carries both entries"
jq -e '.hooks.PreToolUse[] | select(.matcher == "Read")' "$HOOKS_JSON" >/dev/null \
    || fail "no PreToolUse Read matcher entry"
jq -e '.hooks.PreToolUse[] | select(.matcher == "Read") | .hooks[0].command' "$HOOKS_JSON" \
    | grep -q "read-count-hook.sh" || fail "Read matcher does not call the counter"
jq -r '.hooks.SessionStart[].hooks[].command' "$HOOKS_JSON" \
    | grep -q "retire-unread-pass.sh" || fail "no SessionStart entry calls the retire pass"
jq -e . "$HOOKS_JSON" >/dev/null || fail "hooks.json does not parse"
pass "AC12: registration valid and complete"

echo "[read-count-retire] all read-count and retirement tests passed"
exit 0
