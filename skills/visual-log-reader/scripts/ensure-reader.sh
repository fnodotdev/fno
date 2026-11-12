#!/usr/bin/env bash
# Liveness probe and repair for the fleet-owned Visual Log tap reader.
#
# The reader is one spawned thread (name: visual-log-reader) that runs one
# beat every 15 minutes. No session arms a private reader cron: two readers
# double-record approvals, and a lead's cron dies with the lead. This script
# is how any session checks the reader and repairs it:
#
#   ensure-reader.sh check [--status-file <blob.json>] [--fresh-seconds N]
#   ensure-reader.sh spawn [--status-file <blob.json>]
#
# check reads the agent registry (fno agents session list) and looks for the
# reader row's served last_activity_age_s. Exit 0 = present and fresh.
# Exit 1 = absent, stale, or an unreadable activity age; the output names
# the repair. --status-file feeds the same registry JSON from a file so the
# verdicts are testable without a roster. spawn runs check first and fires
# the thread spawn only on an absent row.

set -euo pipefail

READER_NAME="visual-log-reader"
SPAWN_LINE="fno agents spawn --as thread --name ${READER_NAME} \"/fno:visual-log-reader\""
RESUME_LINE="fno agents session resume ${READER_NAME}"
FRESH_SECONDS=1200

usage() {
    sed -n '2,17p' "$0" >&2
    exit 2
}

MODE="check"
case "${1:-}" in
    check|spawn) MODE="$1"; shift ;;
    "") ;;
    *) usage ;;
esac

STATUS_FILE=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --status-file) STATUS_FILE="${2:?--status-file needs a path}"; shift 2 ;;
        --fresh-seconds) FRESH_SECONDS="${2:?--fresh-seconds needs a number}"; shift 2 ;;
        *) usage ;;
    esac
done

if [[ -n "$STATUS_FILE" ]]; then
    BLOB="$(cat "$STATUS_FILE")"
else
    BLOB="$(fno agents session list 2>/dev/null || true)"
fi

# One jq read answers presence and the served age together: an absent row
# prints "absent", a present row prints its age or "unknown". Prints the
# verdict line on stdout and returns the check exit code.
do_check() {
    local age_read
    # Count first, then read: binding syntax (`as $r`) skips its body when
    # the row is absent, and an empty read would compare as 0 and read fresh.
    age_read="$(printf '%s' "$BLOB" | jq -r --arg n "$READER_NAME" \
        'if ([.agents[]? | select(.name == $n)] | length) == 0 then "absent"
         else (.agents[] | select(.name == $n) | .last_activity_age_s
              | if type == "number" then (floor | tostring) else "unknown" end)
         end' | head -1)"
    # A failed registry read leaves this empty, and the registry's served
    # age is f64 (a float kills bash arithmetic). A non-numeric read other
    # than "absent" lands in the unknown branch: an unreadable state never
    # reads fresh.
    [[ "$age_read" == "absent" || "$age_read" =~ ^[0-9]+$ ]] || age_read="unknown"
    case "$age_read" in
        absent)
            echo "reader absent from the registry; repair: ${SPAWN_LINE}"
            return 1
            ;;
        unknown)
            echo "reader present but its last activity is unreadable; check with: fno agents session peek ${READER_NAME} --json --lines 5"
            return 1
            ;;
        *)
            if (( age_read <= FRESH_SECONDS )); then
                echo "reader fresh: last activity ${age_read}s ago (bound ${FRESH_SECONDS}s)"
                return 0
            fi
            echo "reader stale: last activity ${age_read}s ago (bound ${FRESH_SECONDS}s); repair: ${RESUME_LINE}"
            return 1
            ;;
    esac
}

if [[ "$MODE" == "check" ]]; then
    do_check
    exit $?
fi

# spawn mode: check first, fire the thread spawn only on an absent row.
OUT="$(do_check)" && { echo "$OUT"; echo "already present; nothing to spawn"; exit 0; }
if grep -q "absent" <<< "$OUT"; then
    fno agents spawn --as thread --name "$READER_NAME" "/fno:visual-log-reader"
    echo "spawned visual-log-reader; re-check in a few minutes for the first beat"
    exit 0
fi
echo "$OUT"
echo "stale readers resume, they do not respawn (a second reader double-records approvals)"
exit 1
