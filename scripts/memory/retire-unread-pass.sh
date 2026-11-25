#!/usr/bin/env bash
# fno pass: SessionStart - retire unread auto_generated memory entries by move.
# For each top-level *.md entry in the memory dir:
#   skip unless frontmatter carries auto_generated: true (whitespace-tolerant);
#   skip when created_at is missing or unparsable (never archive unknown age);
#   skip when now - created_at < --probation-days (default 30);
#   skip when the sidecar (.reads.jsonl) carries any line naming the entry
#     whose ts is not older than the entry's own created_at (reads and
#     writer update lines both count as use; lines predating the entry are
#     stale ghosts of a prior life of that name);
#   otherwise MOVE to .archived/ (timestamp-suffixed when the name is taken;
#     never delete or overwrite) and prune the entry's line from MEMORY.md -
#     the writer-owned index only; operator-curated CLAUDE.md and
#     ARCHIVE-INDEX.md are never touched.
#
# Args:
#   --memory-dir DIR     The memory dir. Without it, the hook payload's
#                        (.cwd) is derived exactly as the Read counter does.
#   --probation-days N   Minimum age before a zero-read entry is eligible.
#                        Default 30. 0 makes every past entry eligible.
#   --dry-run            Name the would-move entries; move and prune nothing.
#
# Posture: optional hook, fail open. Exit 0 on every routine path. Prints
# one stdout line only when an entry moved (SessionStart stdout is context);
# silent otherwise. Bash 3.2 portable: no mapfile, no associative arrays.
set -uo pipefail

MEM_DIR=""
PROBATION_DAYS=30
DRY_RUN=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --memory-dir)     MEM_DIR="$2"; shift 2 ;;
        --probation-days) PROBATION_DAYS="$2"; shift 2 ;;
        --dry-run)        DRY_RUN=1; shift ;;
        -h|--help)        sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "retire-unread-pass: unknown arg: $1" >&2; exit 1 ;;
    esac
done
[[ "$PROBATION_DAYS" =~ ^[0-9]+$ ]] || {
    echo "retire-unread-pass: --probation-days needs a number" >&2; exit 1; }

if [[ -z "$MEM_DIR" ]]; then
    # Hook form: derive the dir from the SessionStart payload. A tty has no
    # payload; fail open.
    [[ -t 0 ]] && exit 0
    IFS= read -r -d '' INPUT || true
    CWD="$(printf '%s' "${INPUT:-}" | jq -r '.cwd // empty' 2>/dev/null)"
    [[ -n "${CWD:-}" ]] || exit 0
    ROOT="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
    SLUG="$(printf '%s' "$CWD" | LC_ALL=C tr -c '[:alnum:]' '-')"
    MEM_DIR="$ROOT/projects/$SLUG/memory"
fi

[[ -d "$MEM_DIR" ]] || exit 0
SIDECAR="$MEM_DIR/.reads.jsonl"

TMPD="$(mktemp -d -t retire-unread-XXXXXX)" || exit 0
trap 'rm -rf "$TMPD"' EXIT

now_epoch="$(date +%s)"
moved=0
moved_names=""
would_move=""
prune_patterns="$TMPD/prune.txt"
: > "$prune_patterns"

for ENTRY in "$MEM_DIR"/*.md; do
    [[ -e "$ENTRY" ]] || break   # unmatched glob: no entries at all
    FNAME="${ENTRY##*/}"
    case "$FNAME" in
        MEMORY.md|CLAUDE.md|ARCHIVE-INDEX.md) continue ;;
    esac

    # Frontmatter parse, trimmed: one live entry carries two-space indented
    # keys, so a strict `^auto_generated:` read would misfile it as
    # hand-written and keep it forever.
    AUTO=""
    CREATED=""
    in_fm=0
    fm_seen=0
    while IFS= read -r line || [[ -n "$line" ]]; do
        if [[ "$line" == "---" ]]; then
            if [[ "$fm_seen" == "0" ]]; then
                in_fm=1; fm_seen=1; continue
            elif [[ "$in_fm" == "1" ]]; then
                in_fm=0; break
            fi
        fi
        if [[ "$in_fm" == "1" ]]; then
            t="${line#"${line%%[![:space:]]*}"}"
            case "$t" in
                auto_generated:*) AUTO="${t#auto_generated:}"; AUTO="${AUTO# }" ;;
                created_at:*)     CREATED="${t#created_at:}"; CREATED="${CREATED# }" ;;
            esac
        fi
    done < "$ENTRY"

    [[ "$AUTO" == "true" ]] || continue
    [[ -n "$CREATED" ]] || continue
    case "$(uname -s)" in
        Darwin) EP="$(date -j -f '%Y-%m-%dT%H:%M:%S' "${CREATED%Z}" +%s 2>/dev/null)" ;;
        *)      EP="$(date -u -d "$CREATED" +%s 2>/dev/null)" ;;
    esac
    [[ -n "${EP:-}" && "$EP" =~ ^[0-9]+$ ]] || continue
    AGE_DAYS=$(( (now_epoch - EP) / 86400 ))
    [[ "$AGE_DAYS" -ge "$PROBATION_DAYS" ]] || continue
    # Zero recorded use, read against the live sidecar (no start-of-pass
    # snapshot: a read landing while the pass runs still protects the
    # entry). ISO-8601 UTC timestamps compare correctly as strings, so a
    # line counts only when its ts is at least the entry's created_at;
    # lines predating the entry are stale ghosts of a recreated name.
    if [[ -f "$SIDECAR" ]] && awk -v pat="\"file\":\"$FNAME\"" -v since="$CREATED" '
        !found && index($0, pat) {
            if (match($0, /"ts":"[^"]*"/)) {
                ts = substr($0, RSTART + 6, RLENGTH - 7)
                if (ts >= since) found = 1
            }
        }
        END { if (!found) exit 1 }' "$SIDECAR"; then
        continue
    fi

    if [[ "$DRY_RUN" == "1" ]]; then
        would_move+="$FNAME"$'\n'
        continue
    fi

    ARCH="$MEM_DIR/.archived"
    mkdir -p "$ARCH" 2>/dev/null || {
        echo "retire-unread-pass: cannot create $ARCH" >&2
        exit 0
    }
    TARGET="$ARCH/$FNAME"
    # A move never overwrites: a prior retire of the same name is history.
    [[ -e "$TARGET" ]] && TARGET="$ARCH/$FNAME.$(date +%s)"
    if ! mv "$ENTRY" "$TARGET" 2>/dev/null; then
        echo "retire-unread-pass: move failed for $FNAME" >&2
        continue
    fi
    moved=$((moved + 1))
    moved_names+="$FNAME "
    # The writer's index line carries the filename in link form: (name.md)
    printf '(%s)\n' "$FNAME" >> "$prune_patterns"
done

if [[ "$DRY_RUN" == "1" ]]; then
    if [[ -n "$would_move" ]]; then
        printf 'memory-retire (dry run): would archive:\n'
        printf '%s' "$would_move" | sed 's/^/  /'
    fi
    exit 0
fi

if [[ "$moved" -gt 0 ]]; then
    # Prune the writer-owned index once, surgically: drop lines whose link
    # form names a moved entry, keep every other line byte-for-byte.
    if [[ "$moved" -eq 1 ]]; then
        WORD="entry"
    else
        WORD="entries"
    fi
    INDEX="$MEM_DIR/MEMORY.md"
    if [[ -f "$INDEX" && -s "$prune_patterns" ]]; then
        tmp="$(mktemp "$MEM_DIR/.tmp.retire.XXXXXX")" || tmp=""
        if [[ -n "$tmp" ]]; then
            grep -v -F -f "$prune_patterns" "$INDEX" > "$tmp" 2>/dev/null || true
            if ! cmp -s "$tmp" "$INDEX"; then
                mv "$tmp" "$INDEX"
            else
                rm -f "$tmp"
            fi
        fi
    fi
    printf 'memory-retire: archived %s unread auto %s: %s\n' \
        "$moved" "$WORD" "${moved_names% }"
fi
exit 0
