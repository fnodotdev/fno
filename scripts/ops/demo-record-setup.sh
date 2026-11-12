#!/usr/bin/env bash
# Set up one real target run for the founder video, ready to record in one take.
#
#   scripts/ops/demo-record-setup.sh [--sandbox <name>] [--repo <owner/name>]
#
# Nothing here is replayed or invented. The run happens live: a real Claude
# Code session in the command center runs /fno:target on a real node and opens
# a real PR on the demo repo. This script only lays the stage:
#
#   1. a named sandbox (`fno config project sandbox`), reset to empty fno state
#      while its Claude login stays, so the live fleet never shows in the shot;
#   2. the fno plugin installed in the sandbox's own Claude config dir, the
#      same two commands a new user types;
#   3. a fresh clone of the demo repo inside the sandbox home;
#   4. four real backlog nodes for that repo, one of them the take's target.
#
# Run it again before every take: each run resets the sandbox and re-clones.
# The run sheet for the take lives with the growth plan, not in this repo.
set -euo pipefail

SANDBOX=demo
REPO=fnodotdev/fno-demo
while [ $# -gt 0 ]; do
  case "$1" in
    --sandbox|--repo)
      [ $# -ge 2 ] || { echo "demo-record-setup: $1 needs a value" >&2; exit 2; }
      if [ "$1" = --sandbox ]; then SANDBOX="$2"; else REPO="$2"; fi
      shift 2 ;;
    -h|--help) sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "demo-record-setup: unknown argument: $1" >&2; exit 2 ;;
  esac
done

say() { printf 'demo-record-setup: %s\n' "$*"; }

# The login is a person step, done once per sandbox. Create runs it on a TTY.
if ! fno config project sandbox status "$SANDBOX" --json >/dev/null 2>&1; then
  say "no sandbox named $SANDBOX. Create it once and sign in:"
  say "  fno config project sandbox create $SANDBOX"
  exit 2
fi
LOGIN="$(fno config project sandbox status "$SANDBOX" --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["claude"])')"
if [ "$LOGIN" != "logged_in" ]; then
  say "sandbox $SANDBOX reads claude: $LOGIN. Sign in once, then run this again:"
  say "  fno config project sandbox shell $SANDBOX   # then: claude auth login"
  exit 2
fi

# Reset refuses while a sandbox process is live, and names it. That is the
# leftover server or session of the last take: end it first.
fno config project sandbox reset "$SANDBOX"
eval "$(fno config project sandbox env "$SANDBOX")"

# The plugin goes in the sandbox's own Claude config dir. Both commands are
# idempotent, so a second take only refreshes the catalog.
claude plugin marketplace add fnodotdev/fno >/dev/null 2>&1 || claude plugin marketplace update fno >/dev/null
claude plugin install fno@fno >/dev/null
claude plugin list 2>/dev/null | grep -q 'fno@fno' || { say "fno plugin did not install in $CLAUDE_CONFIG_DIR"; exit 1; }

CLONE="$HOME/code/$(basename "$REPO")"
rm -rf -- "$CLONE"
mkdir -p "$HOME/code"
gh repo clone "$REPO" "$CLONE" -- -q
cd "$CLONE"

# Real, small work on the demo repo. The first node is the take: one visible
# bug with a before and after a viewer can read in two seconds. The other
# three fill the board and stay untouched.
node() { # node <priority> <title> : file one node, print its id
  fno backlog idea "$2" -p "$1" --difficulty low | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])'
}
TARGET="$(node p1 "tally: strip punctuation, so end. and end count as one word")"
node p2 "tally: read stdin when the path is -" >/dev/null
node p2 "tally: print a clear error when the file does not exist" >/dev/null
node p3 "tally: --min-count N hides words seen fewer than N times" >/dev/null
fno backlog rank "$TARGET" --top --operator >/dev/null

say "ready. The take's node is $TARGET in $CLONE."
say "next, in a clean terminal:"
say "  fno config project sandbox shell $SANDBOX"
say "  cd ~/code/$(basename "$REPO") && fno mux attach $SANDBOX"
say "then in the pane: claude, and type /fno:target $TARGET"
