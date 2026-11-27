#!/usr/bin/env bash
# install-fnh-runner.sh - run the fnh GitHub Actions runner as a system LaunchDaemon.
#
# A runner started from a user session (a terminal, or `./run.sh` under nohup) dies
# when that session ends, so macOS CI jobs queue forever. This installs the runner
# as a LaunchDaemon under the runner's own user. It starts at boot and survives logout.
#
# Usage:
#   sudo bash scripts/ops/install-fnh-runner.sh            # install and start
#   bash scripts/ops/install-fnh-runner.sh --dry-run       # render and lint only, no root
#
# Overrides (defaults shown):
#   FNH_RUNNER_USER=fnh-runner
#   FNH_RUNNER_HOME=/Users/fnh-runner
#   FNH_RUNNER_DIR=/Users/fnh-runner/actions-runner
#
# Refusals (exit 1, nothing changed):
#   - not root (install mode only)
#   - no runner at $FNH_RUNNER_DIR/run.sh
#   - a Runner.Listener from a manual or user-session start is still running. Stop it
#     first, or two runners register under one name and one of them takes jobs it
#     cannot finish.
set -euo pipefail

LABEL="com.footnote.fnh-actions-runner"
DEST="/Library/LaunchDaemons/${LABEL}.plist"
HERE="$(cd "$(dirname "$0")" && pwd)"
TEMPLATE="${HERE}/launchd/${LABEL}.plist"

RUNNER_USER="${FNH_RUNNER_USER:-fnh-runner}"
RUNNER_HOME="${FNH_RUNNER_HOME:-/Users/${RUNNER_USER}}"
RUNNER_DIR="${FNH_RUNNER_DIR:-${RUNNER_HOME}/actions-runner}"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  "") ;;
  *) echo "unknown argument: $1" >&2; exit 1 ;;
esac

if [[ "${DRY_RUN}" -eq 0 && "$(id -u)" -ne 0 ]]; then
  echo "install needs root: sudo bash $0" >&2
  exit 1
fi

if [[ ! -x "${RUNNER_DIR}/run.sh" ]]; then
  echo "no runner at ${RUNNER_DIR}/run.sh (set FNH_RUNNER_DIR if it lives elsewhere)" >&2
  exit 1
fi

# A re-run finds this daemon's own Listener, so count it as expected, not as a rival.
if [[ "${DRY_RUN}" -eq 0 ]]; then
  EXPECTED=0
  if launchctl print "system/${LABEL}" >/dev/null 2>&1; then EXPECTED=1; fi
  LISTENERS="$(pgrep -f "Runner.Listener" | wc -l | tr -d ' ')"
  if [[ "${LISTENERS}" -gt "${EXPECTED}" ]]; then
    echo "a Runner.Listener outside this daemon is running. Stop it before the daemon takes over:" >&2
    pgrep -fl "Runner.Listener" >&2 || true
    exit 1
  fi
fi

RENDERED="$(mktemp "${TMPDIR:-/tmp}/${LABEL}.XXXXXX")"
trap 'rm -f "${RENDERED}"' EXIT

sed -e "s|__RUNNER_USER__|${RUNNER_USER}|g" \
    -e "s|__RUNNER_HOME__|${RUNNER_HOME}|g" \
    -e "s|__RUNNER_DIR__|${RUNNER_DIR}|g" \
    "${TEMPLATE}" > "${RENDERED}"

plutil -lint "${RENDERED}"

if [[ "${DRY_RUN}" -eq 1 ]]; then
  echo "dry run: rendered plist lints clean. Nothing installed."
  exit 0
fi

install -o root -g wheel -m 644 "${RENDERED}" "${DEST}"
launchctl bootout "system/${LABEL}" 2>/dev/null || true
launchctl bootstrap system "${DEST}"
launchctl enable "system/${LABEL}"
launchctl kickstart -k "system/${LABEL}"

echo "installed ${DEST}"
echo "check: sudo launchctl print system/${LABEL} | grep -E 'state|pid'"
echo "logs:  tail -f /var/log/fnh-actions-runner.log"
