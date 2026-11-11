#!/usr/bin/env bash
# install-journey.sh - prove the public install path end to end, in a fresh HOME.
#
# Runs the README install line (`curl -fsSL fno.sh | sh`) against the published
# release in a throwaway HOME, then walks the steps a new user takes: binaries
# on PATH, versions, plugin wiring, the two sign-ins, config setup, doctor, a
# backlog round trip, a demo clone plus one worker, a mail round trip, a first
# PR, teardown, and uninstall. Every step prints PASS[n]/FAIL[n] and appends
# one JSON line to <run-dir>/journey.jsonl and one markdown section to
# <run-dir>/journey.md.
#
# Refusals (exit 2, nothing written):
#   --run-dir would be the invoking user's real home (dscl NFSHomeDirectory),
#   --run-dir is not under /Users/*/fno-clean/,
#   <run-dir>/home exists and is not empty,
#   --repo-dir sits under a user home (a repo under a home loads that home's
#     CLAUDE.md into every session run in it), or
#   launchctl managername is not Aqua (the teardown guard needs the gui domain).
#
# Isolation: every step runs as
#   env -i HOME=<run-dir>/home CLAUDE_CONFIG_DIR=<run-dir>/home/.claude
#          PATH=<fresh-path> USER LOGNAME SHELL=/bin/zsh TERM LANG
# so no FNO_* export of the operator's own fleet (a supervisor socket, a
# state-dir override) can leak in. Between steps a guard snapshots
# `launchctl list`; a label new since step 0 whose plist sits under the fresh
# HOME gets `launchctl bootout` and a WARN, because a shared-uid machine keeps
# its real fleet loaded in the same gui domain.
#
# Step 18 (`fno uninstall --yes`) runs only when step 0 saw no
# fno-agents-daemon of this uid: today uninstall signals every daemon of the
# uid, so a machine running a real fleet skips it (defect key
# `uninstall-daemon-shared-uid` in the node map) and the dry run in step 17
# still proves the list.
#
# FAIL and SKIP lines carry a symbolic `defect` key. The script never carries
# node ids; the attended run supplies `--node-map <file>` with
# `defect<TAB>node-id` lines and the log's `node` field picks the id up from
# there.
#
# Usage:
#   install-journey.sh --run-dir /Users/you/fno-clean/run1
#       [--repo-dir /Users/Shared/fno-clean/run1/fno-demo] [--version 0.4.8]
#       [--from-step N] [--only N] [--list [--json]] [--dry-run]
#       [--node-map <file>]
#
# Exit: 0 all steps PASS/HUMAN, 1 any FAIL, 2 a refusal fired.
set -uo pipefail

# --- config -----------------------------------------------------------------
VERSION="0.4.8"
DEMO_REPO="fnodotdev/fno-demo"
WORKER_SEED="You are an install-journey probe worker. Check your mail now and then: when a message asks for a nonce reply with exactly that nonce, then keep waiting. When a message carries a build instruction, follow it."
# The first build: a stdlib-only newsletter page beside the demo tally app.
TARGET_SEED="Run /fno:target with this feature for this repo: a stdlib-only Python package newsletter/ beside tally/. python -m newsletter.issue <photos-dir> --family \"Reyes\" --month 2026-11 --out issue.html reads captions.json in that dir (file name, date, caption) and writes one HTML page: title \"The Reyes Family, November 2026\" and one figure per photo in date order, each with its caption; a photo with no caption gets its date as the caption; an empty dir exits 2 with \"no photos in <dir>\". Add tests/test_newsletter.py with three tests (order by date, missing caption, empty dir) and newsletter/sample/ with three placeholder photos plus a captions.json. Ship it as a PR on this repo."

RUN_DIR="" REPO_DIR="" FROM_STEP="" ONLY="" LIST_PLAIN=0 LIST_JSON=0 DRY_RUN=0 NODE_MAP="" REAL_HOME=""
JHOME="" BASE="" LOG_JSON="" LOG_MD="" STATE_FILE=""
NONCE="" WORKER="" PR_NUM="" BASE_LABELS="" HAD_DAEMON=0
FAILS=0 PASSES=0 HUMANS=0 SKIPS=0

# --- output helpers ---------------------------------------------------------
pass() { printf 'PASS[%s] %s\n' "$1" "$2"; }
fail() { printf 'FAIL[%s] %s\n' "$1" "$2"; }
refuse() { printf 'refuse: %s\n' "$1" >&2; exit 2; }

# one display-safe line: drop control chars and backslashes, cap at 200 chars.
clean() { printf '%s' "$1" | tr -d "\000-\010\013\014\016-\037\\" | tr "\t" " " | tr "\n" " " | cut -c1-200; }

# map_node <defect-key>: the node id the node map assigns, or empty.
map_node() {
    [ -n "$NODE_MAP" ] && [ -f "$NODE_MAP" ] || return 0
    awk -F'\t' -v k="$1" '$1 == k { print $2; exit }' "$NODE_MAP" 2>/dev/null
}

# fresh <cmd...>: the step env. env -i clears the operator's exports, so the
# journey cannot reach its own fleet by accident.
fresh() {
    env -i HOME="$JHOME" CLAUDE_CONFIG_DIR="$JHOME/.claude" \
        PATH="$JHOME/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin" \
        USER="$USER" LOGNAME="$USER" SHELL=/bin/zsh \
        TERM="${TERM:-xterm-256color}" LANG="${LANG:-en_US.UTF-8}" "$@"
}

# state_get / state_set: cross-step handoff (worker name, PR number) kept in
# the run dir, never inside the throwaway home.
state_get() { awk -F= -v k="$1" '$1 == k { print substr($0, index($0,"=")+1); exit }' "$STATE_FILE" 2>/dev/null; }
state_set() { grep -v "^$1=" "$STATE_FILE" 2>/dev/null > "$STATE_FILE.tmp"; printf '%s=%s\n' "$1" "$2" >> "$STATE_FILE.tmp"; mv "$STATE_FILE.tmp" "$STATE_FILE"; }

# --- step table -------------------------------------------------------------
# meta: name|kind|budget_s ; dry: the step's commands behind the env prefix.
step_meta() {
    case $1 in
        0) echo 'Preflight baseline|auto|10' ;;
        1) echo 'Download and sha256|auto|60' ;;
        2) echo 'Install via the README line|auto|180' ;;
        3) echo 'Binaries on PATH|auto|10' ;;
        4) echo 'Version matches the release|auto|10' ;;
        5) echo 'Claude Code plugin|auto|30' ;;
        6) echo 'Codex and opencode wiring|auto|30' ;;
        7) echo 'Sign in to claude.ai|human|0' ;;
        8) echo 'gh sign-in and git credentials|human|0' ;;
        9) echo 'Config setup|auto|120' ;;
        10) echo 'Doctor blockers|auto|30' ;;
        11) echo 'Backlog round trip|auto|15' ;;
        12) echo 'Clone demo, first worker|auto|120' ;;
        13) echo 'Mail round trip|auto|120' ;;
        14) echo 'First PR: newsletter slice|auto|900' ;;
        15) echo 'Read-outside prompts|auto|5' ;;
        16) echo 'Close PR, stop worker|auto|30' ;;
        17) echo 'Uninstall dry run|auto|30' ;;
        18) echo 'Uninstall and residue|auto|60' ;;
        *) return 1 ;;
    esac
}
step_dry() {
    case $1 in
        0) echo 'pgrep -f -U <uid> fno-agents-daemon; launchctl list | grep -i fno; ls -la <home>; env -i ... command -v curl shasum git gh claude codex opencode' ;;
        1) echo 'curl -fsSL https://fno.sh -o <run>/fno.sh; shasum -a 256; gh api repos/fnodotdev/fno/contents/scripts/install/fno.sh?ref=v<ver> | base64 -d | shasum -a 256; curl the release tarball + .sha256 and compare' ;;
        2) echo 'sh -c '"'"'curl -fsSL fno.sh | sh'"'"'   (the exact README line)' ;;
        3) echo 'zsh -ic '\''command -v fno fno-agents'\''; grep "added by fno.sh" <home>/.zshrc' ;;
        4) echo 'fno version --json; fno-agents --version' ;;
        5) echo 'claude plugin list --json; fallback: test -d <home>/.claude/skills/fno' ;;
        6) echo 'grep -qi fno <home>/.codex/config.toml; fno-agents plugin-install --check --json (opencode row)' ;;
        7) echo 'poll: claude auth status -> "loggedIn": true   (you sign in: env CLAUDE_CONFIG_DIR=<home>/.claude claude, then /login)' ;;
        8) echo 'poll: gh auth status; then gh auth setup-git; then git ls-remote https://github.com/fnodotdev/fno-demo.git HEAD' ;;
        9) echo 'fno config setup run --check --json; fno config setup run --yes --json; re-check for needs_human rows' ;;
        10) echo 'fno doctor --blockers (bound 30s)' ;;
        11) echo 'fno backlog idea "install journey probe <nonce>"; fno backlog get <id>' ;;
        12) echo 'gh repo clone fnodotdev/fno-demo <repo-dir>; fno agents spawn journey-<nonce> "<seed>" --cwd <repo-dir> --no-wait' ;;
        13) echo 'fno agents mail send <worker> "reply with nonce <n>"; poll fno agents mail unread' ;;
        14) echo 'fno agents mail send <worker> --body-file -  (the newsletter-slice target seed); poll gh pr list + gh pr checks on a 30s bound' ;;
        15) echo 'grep -rl "outside the working directories" <home>/.claude/projects' ;;
        16) echo 'gh pr close <n> --delete-branch -R fnodotdev/fno-demo; fno agents session stop <worker>' ;;
        17) echo 'fno uninstall --dry-run; every listed path must sit under <home>' ;;
        18) echo 'fno uninstall --yes (only with no fleet daemon of this uid at step 0); ls diff, plugin-cache check, launchd check' ;;
    esac
}

# --- the launchd guard, run after every step --------------------------------
guard_launchd() {
    local cur new label
    cur=$(launchctl list 2>/dev/null | grep -i fno | awk '{print $3}' | sort || true)
    new=$(comm -13 <(printf '%s\n' "$BASE_LABELS" | sort) <(printf '%s\n' "$cur" | sort) 2>/dev/null || true)
    for label in $new; do
        [ -n "$label" ] || continue
        if [ -f "$JHOME/Library/LaunchAgents/$label.plist" ] \
            || launchctl print "gui/$(id -u)/$label" 2>/dev/null | grep -qF "$JHOME"; then
            if launchctl bootout "gui/$(id -u)/$label" 2>/dev/null; then
                EV="${EV}WARN bootout $label (plist under the fresh home)
"
            else
                EV="${EV}WARN bootout failed for $label (plist under the fresh home)
"
            fi
        else
            EV="${EV}WARN launchd label $label appeared (plist not under the fresh home; left alone)
"
        fi
    done
}

# --- steps ------------------------------------------------------------------
step_0() {
    local pids tools t
    pids=$(pgrep -f -U "$(id -u)" fno-agents-daemon 2>/dev/null | sort || true)
    printf '%s\n' "$pids" > "$BASE/daemon-pids.txt"
    [ -n "$pids" ] && HAD_DAEMON=1
    BASE_LABELS=$(launchctl list 2>/dev/null | grep -i fno | awk '{print $3}' | sort || true)
    printf '%s\n' "$BASE_LABELS" > "$BASE/launchctl-labels.txt"
    ls -la "$JHOME" > "$BASE/home-listing.txt" 2>&1
    : > "$BASE/fresh-tools.txt"
    for t in curl shasum git gh claude codex opencode zsh jq; do
        tools=$(fresh command -v "$t" 2>/dev/null || true)
        printf '%-8s %s\n' "$t" "${tools:-MISSING}" >> "$BASE/fresh-tools.txt"
        [ -n "$tools" ] || EV="${EV}WARN fresh PATH has no $t
"
    done
    EV="${EV}daemon pids of this uid at start: $(printf '%s' "$pids" | grep -c . ) (file: baseline/daemon-pids.txt)
fresh-tool probe: baseline/fresh-tools.txt
launchd baseline: $(printf '%s' "$BASE_LABELS" | grep -c . ) fno labels"
    if [ -f "$BASE/daemon-pids.txt" ]; then ST=pass; else ST=fail; DF="baseline-incomplete"; fi
}

step_1() {
    local served tagged want plat url got
    fresh curl -fsSL --max-time 45 "https://fno.sh" -o "$RUN_DIR/fno.sh.downloaded" 2>"$RUN_DIR/step1-curl.err" || true
    [ -f "$RUN_DIR/fno.sh.downloaded" ] || { ST=fail; DF="download-failed"; EV="${EV}curl could not fetch https://fno.sh
"; return; }
    served=$(shasum -a 256 "$RUN_DIR/fno.sh.downloaded" 2>/dev/null | awk '{print $1}')
    tagged=$(gh api "repos/fnodotdev/fno/contents/scripts/install/fno.sh?ref=v$VERSION" --jq .content 2>/dev/null | base64 -d 2>/dev/null | shasum -a 256 2>/dev/null | awk '{print $1}')
    [ -n "$tagged" ] || tagged=$(curl -fsSL --max-time 45 "https://raw.githubusercontent.com/fnodotdev/fno/v$VERSION/scripts/install/fno.sh" 2>/dev/null | shasum -a 256 | awk '{print $1}')
    EV="${EV}served fno.sh sha256: $served
v$VERSION scripts/install/fno.sh sha256: $tagged"
    case "$(uname -m)" in x86_64) plat=macos-x64 ;; arm64) plat=macos-arm64 ;; *) plat=macos-x64 ;; esac
    url="https://github.com/fnodotdev/fno/releases/download/v$VERSION/fno-agents-$plat.tar.gz"
    fresh curl -fsSL --max-time 45 "$url" -o "$RUN_DIR/tarball.gz" 2>/dev/null || true
    fresh curl -fsSL --max-time 45 "$url.sha256" -o "$RUN_DIR/tarball.gz.sha256" 2>/dev/null || true
    want=$(cut -d' ' -f1 "$RUN_DIR/tarball.gz.sha256" 2>/dev/null)
    got=$(shasum -a 256 "$RUN_DIR/tarball.gz" 2>/dev/null | awk '{print $1}')
    EV="${EV}tarball fno-agents-$plat.tar.gz sha256: $got (want $want)"
    if [ -n "$served" ] && [ "$served" = "$tagged" ] && [ -n "$got" ] && [ "$got" = "$want" ]; then
        ST=pass
    else
        ST=fail
        if [ "$served" != "$tagged" ]; then DF="served-script-drift"; else DF="tarball-digest-mismatch"; fi
    fi
}

step_2() {
    local out rc last
    out=$(fresh sh -c 'curl -fsSL fno.sh | sh' 2>&1); rc=$?
    last=$(printf '%s' "$out" | tail -1)
    EV="${EV}README line: curl -fsSL fno.sh | sh
exit code: $rc
last installer line: $(clean "$last")"
    if [ $rc -eq 0 ]; then
        ST=pass
    else
        ST=fail; DF="readme-install-failed"
        EV="${EV}installer tail: $(clean "$(printf '%s' "$out" | tail -5)")"
    fi
}

step_3() {
    local got line
    got=$(fresh zsh -ic 'command -v fno; command -v fno-agents' 2>/dev/null | grep -v '^$' || true)
    line=$(grep -F 'added by fno.sh' "$JHOME/.zshrc" 2>/dev/null | head -1)
    EV="${EV}resolved: $(clean "$(printf '%s' "$got" | tr '\n' ' ')")
profile line: $(clean "${line:-none}")"
    if [ "$(printf '%s\n' "$got" | grep -c . )" -ge 2 ]; then ST=pass; else ST=fail; DF="fno-not-on-fresh-path"; fi
}

step_4() {
    local vjson pkg frc
    vjson=$(fresh fno version --json 2>/dev/null) || true
    pkg=$(printf '%s' "$vjson" | sed -n -E 's/.*"package"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' | head -1)
    fresh fno-agents --version >/dev/null 2>&1; frc=$?
    EV="${EV}fno version --json package: ${pkg:-unreadable} (want $VERSION)
fno-agents --version exit: $frc"
    if [ "$pkg" = "$VERSION" ] && [ $frc -eq 0 ]; then ST=pass; else ST=fail; DF="version-mismatch"; fi
}

step_5() {
    local out row
    out=$(fresh claude plugin list --json 2>/dev/null) || true
    row=$(printf '%s' "$out" | grep -E '"id": *"fno@' | head -1)
    if [ -n "$row" ]; then
        EV="${EV}$(clean "$row")"
        ST=pass
    elif [ -d "$JHOME/.claude/skills/fno" ]; then
        EV="${EV}WARN marketplace plugin row missing; the skills-dir fallback drop exists (marketplace add failed)
defect: no-stable-branch"
        ST=pass
    else
        EV="${EV}no fno@ row and no skills-dir drop"
        ST=fail; DF="claude-plugin-missing"
    fi
}

step_6() {
    local oc
    if [ -f "$JHOME/.codex/config.toml" ] && grep -qi fno "$JHOME/.codex/config.toml"; then
        EV="${EV}codex: fno wired in .codex/config.toml"
    else
        EV="${EV}WARN codex: no fno wiring found in .codex/config.toml
defect: codex-wire-unverified"
    fi
    oc=$(fresh fno-agents plugin-install --check --json 2>/dev/null) || true
    # The status field, not the bare word: JSON elsewhere in the output names
    # installed_plugins.json, which a bare 'installed' grep would count.
    if printf '%s' "$oc" | grep -F '"opencode"' >/dev/null && printf '%s' "$oc" | grep -Eq '"status": *"installed"'; then
        EV="${EV}opencode: plugin-install --check reads installed"
    else
        EV="${EV}WARN opencode: not installed on first wire (expected before the first Claude session; re-probe below)
defect: opencode-first-wire"
    fi
    ST=pass   # WARN-only step
}

step_7() {
    local waited=0
    printf '  HUMAN step: sign in to claude.ai for the fresh config dir.\n'
    printf '  In a terminal run:\n    env CLAUDE_CONFIG_DIR=%s/.claude claude\n  then run /login inside it. Waiting for the probe (15s cadence, Ctrl-C aborts).\n' "$JHOME"
    until fresh sh -c 'claude auth status' 2>/dev/null | grep '"loggedIn": true' >/dev/null; do
        sleep 15; waited=$((waited + 15)); printf '.'
    done
    printf '\n'
    EV="${EV}claude auth status reads loggedIn true after $((waited / 60))m $((waited % 60))s"
    ST=human; SECS=$waited
}

step_8() {
    local waited=0 src rc
    printf '  HUMAN step: install gh if missing, then sign in to GitHub for the fresh HOME.\n'
    printf '  gh must answer under the fresh PATH (/usr/local/bin/gh works); then:\n    env HOME=%s gh auth login\n  Waiting for gh auth status (15s cadence, Ctrl-C aborts).\n' "$JHOME"
    until fresh sh -c 'gh auth status' >/dev/null 2>&1; do
        sleep 15; waited=$((waited + 15)); printf '.'
    done
    printf '\n'
    fresh gh auth setup-git >/dev/null 2>&1; src=$?
    fresh git ls-remote "https://github.com/$DEMO_REPO.git" HEAD >/dev/null 2>&1; rc=$?
    EV="${EV}gh auth status passed after $((waited / 60))m; gh auth setup-git exit: $src
git ls-remote on the demo repo exit: $rc (0 means the credential helper carries push rights)"
    if [ $rc -eq 0 ]; then ST=human; SECS=$waited; else ST=fail; DF="git-credentials-missing"; SECS=$waited; fi
}

step_9() {
    local before after afterrc
    before=$(fresh fno config setup run --check --json 2>/dev/null || true)
    EV="${EV}check rows before: $(clean "$(printf '%s' "$before" | grep -o '"code": "[a-z_]*"' | sort -u | tr '\n' ' ')")"
    fresh fno config setup run --yes --json >/dev/null 2>"$RUN_DIR/step9.err"; ST=$?
    after=$(fresh fno config setup run --check --json 2>/dev/null || true); afterrc=$?
    EV="${EV}setup run --yes exit: $ST
check rows after: $(clean "$(printf '%s' "$after" | grep -o '"code": "[a-z_]*"' | sort -u | tr '\n' ' ')")"
    if [ "$ST" -eq 0 ] && [ "$afterrc" -eq 0 ] && ! printf '%s' "$after" | grep -q 'needs_human'; then
        ST=pass
    else
        ST=fail; DF="setup-needs-human"
    fi
}

step_10() {
    local out rc t0 t1
    t0=$(date +%s)
    out=$(fresh fno doctor --blockers 2>&1); rc=$?
    t1=$(date +%s)
    EV="${EV}exit: $rc, wall $((t1 - t0))s (budget 30s)
output tail: $(clean "$(printf '%s' "$out" | tail -3)")"
    if [ $rc -eq 0 ]; then ST=pass; else ST=fail; DF="doctor-blockers"; fi
}

step_11() {
    local out id title nonce2 oc
    nonce2="journey-$(date +%s)"
    out=$(fresh fno backlog idea "install journey probe $nonce2" 2>/dev/null || true)
    # Whole-token match only: an unanchored grep once extracted the nonce's
    # own tail ("rney-17313264") ahead of the real node id.
    id=$(printf '%s' "$out" | awk '{ for (i = 1; i <= NF; i++) if ($i ~ /^[a-z]{2,4}-[0-9a-f]{4,8}$/) { print $i; exit } }')
    title=$(fresh fno backlog get "$id" 2>/dev/null || true)
    EV="${EV}minted: ${id:-none}; title read back: $(clean "$(printf '%s' "$title" | head -1)")"
    oc=$(fresh fno-agents plugin-install --check --json 2>/dev/null || true)
    if printf '%s' "$oc" | grep -F '"opencode"' >/dev/null && printf '%s' "$oc" | grep -Eq '"status": *"installed"'; then
        EV="${EV}opencode re-probe: installed"
    else
        EV="${EV}WARN opencode re-probe: still not installed"
    fi
    if [ -n "$id" ] && printf '%s' "$title" | grep -q "$nonce2"; then ST=pass; else ST=fail; DF="backlog-roundtrip"; fi
}

step_12() {
    local alive=0 src deadline
    fresh gh repo clone "$DEMO_REPO" "$REPO_DIR" >/dev/null 2>"$RUN_DIR/step12-clone.err" || true
    [ -d "$REPO_DIR/.git" ] || { ST=fail; DF="demo-clone-failed"; EV="${EV}clone failed: $(clean "$(tail -2 "$RUN_DIR/step12-clone.err" 2>/dev/null)")"; return; }
    EV="${EV}cloned $DEMO_REPO into $REPO_DIR"
    WORKER="journey-w${NONCE}"
    fresh fno agents spawn "$WORKER" "$WORKER_SEED" --cwd "$REPO_DIR" --no-wait > "$RUN_DIR/step12-spawn.out" 2>&1
    src=$?
    state_set WORKER "$WORKER"
    EV="${EV}spawn exit: $src, name: $WORKER (output tail: $(clean "$(tail -1 "$RUN_DIR/step12-spawn.out" 2>/dev/null)"))"
    deadline=$(( $(date +%s) + 120 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        # grep without -q: -q can close the pipe early and SIGPIPE the writer,
        # which pipefail reads as a failed poll on a match.
        if fresh fno agents session list 2>/dev/null | grep -F "$WORKER" >/dev/null; then alive=1; break; fi
        sleep 10
    done
    if [ "$alive" -eq 1 ]; then ST=pass; else ST=fail; DF="worker-not-listed"; fi
}

step_13() {
    local n got=0 deadline sent
    n="nonce-$(date +%s)"
    sent=$(fresh fno agents mail send "$WORKER" "install journey probe: reply with exactly $n" 2>&1) || true
    EV="${EV}sent to $WORKER: $(clean "$(printf '%s' "$sent" | tail -1)")"
    deadline=$(( $(date +%s) + 120 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if fresh fno agents mail unread 2>/dev/null | grep -F "$n" >/dev/null; then got=1; break; fi
        sleep 10
    done
    EV="${EV}reply carrying $n received: $([ $got -eq 1 ] && echo yes || echo no)"
    if [ $got -eq 1 ]; then ST=pass; else ST=fail; DF="mail-roundtrip"; fi
}

step_14() {
    local max_before now pr="" crc=8 t0 elapsed deadline
    max_before=$(fresh gh pr list -R "$DEMO_REPO" --state open --json number 2>/dev/null | grep -o '"number": *[0-9]*' | grep -o '[0-9]*' | sort -n | tail -1)
    [ -n "$max_before" ] || max_before=0
    fresh fno agents mail send "$WORKER" --body-file - >/dev/null 2>&1 <<EOF2
$TARGET_SEED
EOF2
    t0=$(date +%s)
    deadline=$(( t0 + 420 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        now=$(fresh gh pr list -R "$DEMO_REPO" --state open --json number 2>/dev/null | grep -o '"number": *[0-9]*' | grep -o '[0-9]*' | sort -n | tail -1)
        if [ -n "$now" ] && [ "$now" -gt "$max_before" ]; then pr="$now"; break; fi
        sleep 30
    done
    if [ -z "$pr" ]; then
        elapsed=$(( $(date +%s) - t0 ))
        ST=fail; DF="pr-not-open"
        EV="${EV}no new PR after ${elapsed}s"
        state_set PR ""
        return
    fi
    PR_NUM="$pr"; state_set PR "$pr"
    deadline=$(( deadline + 480 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        fresh gh pr checks "$pr" -R "$DEMO_REPO" >/dev/null 2>&1; crc=$?
        [ $crc -eq 8 ] || break
        sleep 30
    done
    elapsed=$(( $(date +%s) - t0 ))
    EV="${EV}PR #$pr opened in ${elapsed}s (was $max_before open before)
gh pr checks exit: $crc (0 green, 1 red, 8 pending at bound)"
    if [ $crc -eq 0 ]; then
        ST=pass
    else
        ST=fail
        if [ $crc -eq 8 ]; then DF="pr-checks-timeout"; else DF="pr-checks-red"; fi
    fi
}

step_15() {
    local hits
    hits=$(grep -rl "outside the working directories" "$JHOME/.claude/projects" 2>/dev/null | grep -c . || true)
    EV="${EV}transcripts carrying a read-outside permission request: $hits"
    if [ "$hits" -eq 0 ]; then ST=pass; else ST=fail; DF="read-outside-prompt"; fi
}

step_16() {
    local crc src
    [ -n "$PR_NUM" ] || PR_NUM=$(state_get PR)
    crc=0; src=0
    if [ -n "$PR_NUM" ]; then
        fresh gh pr close "$PR_NUM" --delete-branch -R "$DEMO_REPO" >/dev/null 2>&1 || crc=$?
    fi
    [ -n "$WORKER" ] || WORKER=$(state_get WORKER)
    fresh fno agents session stop "$WORKER" >/dev/null 2>&1 || src=$?
    EV="${EV}gh pr close $PR_NUM exit: $crc; session stop $WORKER exit: $src"
    if [ $crc -eq 0 ] && [ $src -eq 0 ]; then ST=pass; else ST=fail; DF="cleanup-failed"; fi
}

step_17() {
    local out outside danger p
    out=$(fresh fno uninstall --dry-run 2>&1); ST=$?
    # Path tokens without spaces, so prose paths are tokens too; the FAIL
    # class is the dangerous one (the operator's real home), not any outside
    # path the report happens to mention.
    outside=$(printf '%s' "$out" | grep -oE '/[[:alnum:]./-]+' | grep -v "^$JHOME" | sort -u | head -5)
    danger=""
    for p in $outside; do
        case "$p" in "$REAL_HOME"|"$REAL_HOME"/*) danger="$danger $p" ;; esac
    done
    EV="${EV}dry-run exit: $ST
paths named outside $JHOME: $(clean "$(printf '%s' "$outside" | tr '\n' ' ')")
paths under the real home: $(clean "${danger:-none}")"
    if [ "$ST" -eq 0 ] && [ -z "$danger" ]; then
        ST=pass
    else
        ST=fail; DF="uninstall-reaches-real-home"
    fi
}

step_18() {
    local pids rc now_cache now_labels left lost bp
    if [ "$HAD_DAEMON" -eq 1 ]; then
        pids=$(pgrep -f -U "$(id -u)" fno-agents-daemon 2>/dev/null | sort || true)
        lost=""
        while IFS= read -r bp; do
            [ -n "$bp" ] || continue
            kill -0 "$bp" 2>/dev/null || lost="$lost $bp"
        done < "$BASE/daemon-pids.txt"
        EV="${EV}SKIP: daemon pids of this uid existed at step 0; uninstall signals every daemon of the uid, so the live run is held until the scoped-signal fix lands (map defect key uninstall-daemon-shared-uid)
fleet daemons now: $(printf '%s' "$pids" | tr '\n' ' ')
baseline daemons no longer alive:${lost:- none}"
        if [ -n "$lost" ]; then
            ST=fail; DF="fleet-daemon-lost"
        else
            ST=skip; DF="uninstall-daemon-shared-uid"
        fi
        return
    fi
    fresh fno uninstall --yes >/dev/null 2>"$RUN_DIR/step18.err"; rc=$?
    now_cache=""; [ -e "$JHOME/.claude/plugins/cache/fno" ] && now_cache=present
    now_labels=$(launchctl list 2>/dev/null | grep -i fno | awk '{print $3}' | sort || true)
    left=""
    for label in $now_labels; do
        [ -f "$JHOME/Library/LaunchAgents/$label.plist" ] && left="$left $label"
    done
    EV="${EV}uninstall --yes exit: $rc (stderr tail: $(clean "$(tail -1 "$RUN_DIR/step18.err" 2>/dev/null)"))
plugin cache fno dir: ${now_cache:-absent}
launchd labels still pointing into the fresh home:${left:- none}
home leftovers vs step 0: $(diff <(grep -v total "$BASE/home-listing.txt" 2>/dev/null) <(ls -la "$JHOME" 2>/dev/null) 2>/dev/null | grep -c '^>')"
    if [ $rc -eq 0 ] && [ -z "$now_cache" ] && [ -z "$(printf '%s' "$left" | tr -d ' ')" ]; then
        ST=pass
    else
        ST=fail; DF="uninstall-residue"
    fi
}

# --- record -----------------------------------------------------------------
# Reads ST (status), EV (evidence lines), DF (defect key), SECS (override).
record() {
    local n="$1" meta name kind budget status node defect json_ev first
    meta=$(step_meta "$n") || return 0
    name=${meta%%|*}; rest=${meta#*|}; kind=${rest%%|*}; budget=${rest##*|}
    status=${ST:-fail}
    case "$status" in pass) PASSES=$((PASSES + 1)) ;; human) HUMANS=$((HUMANS + 1)) ;; skip) SKIPS=$((SKIPS + 1)) ;; *) FAILS=$((FAILS + 1)) ;; esac
    [ -n "${SECS:-}" ] || SECS=$(( $(date +%s) - STEP_T0 ))
    defect=${DF:-}
    node=$(map_node "$defect")
    first=$(printf '%s' "$EV" | head -1)
    case "$status" in
        pass) pass "$n" "$name: $(clean "$first")" ;;
        human) pass "$n" "$name: done in $((SECS / 60))m ($(clean "$first"))" ;;
        skip) fail "$n" "$name: skipped - $(clean "$first")" ;;
        *) fail "$n" "$name: $(clean "$first")${defect:+ (defect: $defect)}" ;;
    esac
    # journey.jsonl: one object per step, stable field order. Evidence goes
    # through clean() per line, then awk swaps any double quotes left.
    json_ev=$(printf '%s' "$EV" | while IFS= read -r l; do clean "$l"; done \
        | awk '{ gsub(/\\/, " "); gsub(/"/, "'\''"); printf (NR > 1 ? ", " : "") "\"%s\"", $0 }')
    node_json=null; [ -n "$node" ] && node_json="\"$node\""
    defect_json=null; [ -n "$defect" ] && defect_json="\"$defect\""
    printf '{"step": %s, "name": "%s", "kind": "%s", "status": "%s", "seconds": %s, "budget_s": %s, "evidence": [%s], "defect": %s, "node": %s}\n' \
        "$n" "$(clean "$name")" "$kind" "$status" "$SECS" "$budget" "$json_ev" "$defect_json" "$node_json" >> "$LOG_JSON"
    # journey.md
    {
        printf '\n## Step %s: %s\n\n' "$n" "$name"
        printf -- '- status: %s (%ss, budget %ss)%s%s\n' "$status" "$SECS" "$budget" "${defect:+ - defect: }" "${defect:-}"
        printf '%s' "$EV" | while IFS= read -r l; do [ -n "$l" ] && printf -- '- %s\n' "$(clean "$l")"; done
    } >> "$LOG_MD"
    ST=""; EV=""; DF=""; SECS=""; STEP_T0=$(date +%s)
}

# --- args, refusals, main ---------------------------------------------------
usage() { awk 'NR > 1 && /^# /{ sub(/^# ?/, ""); print } /^set /{ exit }' "$0" >&2; }

while [ $# -gt 0 ]; do
    case $1 in
        --run-dir) RUN_DIR="${2:-}"; shift 2 ;;
        --repo-dir) REPO_DIR="${2:-}"; shift 2 ;;
        --version) VERSION="${2:-}"; shift 2 ;;
        --from-step) FROM_STEP="${2:-}"; shift 2 ;;
        --only) ONLY="${2:-}"; shift 2 ;;
        --list) LIST_PLAIN=1; shift ;;
        --json) LIST_JSON=1; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        --node-map) NODE_MAP="${2:-}"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'unknown argument: %s\n' "$1" >&2; usage; exit 2 ;;
    esac
done

if { [ "$LIST_PLAIN" -eq 1 ] || [ "$LIST_JSON" -eq 1 ]; }; then
    if [ "$LIST_JSON" -eq 1 ]; then
        printf '['
        for n in $(seq 0 18); do
            meta=$(step_meta "$n"); name=${meta%%|*}; rest=${meta#*|}; kind=${rest%%|*}; budget=${rest##*|}
            printf '%s{"step": %s, "name": "%s", "kind": "%s", "budget_s": %s}' \
                "$([ "$n" -gt 0 ] && printf ', ')" "$n" "$name" "$kind" "$budget"
        done
        printf ']\n'
    else
        for n in $(seq 0 18); do
            meta=$(step_meta "$n"); printf '%-3s %s\n' "$n" "$(printf '%s' "$meta" | tr '|' ' ')"
        done
    fi
    exit 0
fi

[ -n "$RUN_DIR" ] || { printf -- '--run-dir is required\n' >&2; usage; exit 2; }
case "$RUN_DIR" in
    /*) : ;;
    *) RUN_DIR="$(pwd -P)/$RUN_DIR" ;;
esac
JHOME="$RUN_DIR/home"
REPO_DIR="${REPO_DIR:-/Users/Shared/fno-clean/$(basename "$RUN_DIR")/fno-demo}"

# refusals - nothing is written before all of them pass
real_home=$(dscl . -read "/Users/$USER" NFSHomeDirectory 2>/dev/null | awk '{print $2}')
[ -n "$real_home" ] || real_home="$HOME"
REAL_HOME="$real_home"
# exact matches only: the required /Users/<you>/fno-clean/ location sits
# under the real home by design; only BEING the real home is dangerous.
case "$RUN_DIR" in "$real_home") refuse "--run-dir $RUN_DIR is the real home" ;; esac
case "$JHOME" in "$real_home") refuse "the fresh HOME $JHOME would be the real home $real_home" ;; esac
case "$RUN_DIR" in /Users/*/fno-clean|/Users/*/fno-clean/*) : ;; *) refuse "--run-dir must sit under /Users/<you>/fno-clean/" ;; esac
if [ -e "$JHOME" ] && [ -n "$(ls -A "$JHOME" 2>/dev/null)" ]; then
    refuse "$JHOME exists and is not empty; point --run-dir at a fresh run dir"
fi
case "$REPO_DIR" in /Users/Shared/*|/Users/Shared) : ;; /Users/*) refuse "--repo-dir $REPO_DIR sits under a user home; sessions there load that home's CLAUDE.md (use /Users/Shared/...)" ;; esac
mgr=$(launchctl managername 2>/dev/null || true)
[ "$mgr" = "Aqua" ] || refuse "not a GUI session (launchctl managername: ${mgr:-unknown}); run inside the fno-clean tmux session in Terminal"
[ -n "$NODE_MAP" ] && [ ! -f "$NODE_MAP" ] && refuse "--node-map $NODE_MAP is not a file"

# setup (refusals passed; from here on the run dir is ours)
BASE="$RUN_DIR/baseline"
mkdir -p "$RUN_DIR" "$BASE" "$JHOME" "$(dirname "$REPO_DIR")"
LOG_JSON="$RUN_DIR/journey.jsonl"; LOG_MD="$RUN_DIR/journey.md"; STATE_FILE="$RUN_DIR/journey-state"
NONCE="$(date +%Y%m%d-%H%M%S)"
if [ -z "$FROM_STEP" ] && [ -z "$ONLY" ]; then : > "$LOG_JSON"; : > "$LOG_MD"; : > "$STATE_FILE"; fi
[ -f "$LOG_JSON" ] || : > "$LOG_JSON"
[ -f "$LOG_MD" ] || : > "$LOG_MD"
[ -f "$STATE_FILE" ] || : > "$STATE_FILE"
STEP_T0=$(date +%s)

from=${FROM_STEP:-0}; to=18
[ -n "$ONLY" ] && { from="$ONLY"; to="$ONLY"; }
printf '# install journey %s: run dir %s, version %s\n\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$RUN_DIR" "$VERSION" >> "$LOG_MD"

for n in $(seq "$from" "$to"); do
    step_meta "$n" >/dev/null || continue
    meta=$(step_meta "$n"); name=${meta%%|*}; rest=${meta#*|}; kind=${rest%%|*}; budget=${rest##*|}
    if [ "$DRY_RUN" -eq 1 ]; then
        printf 'DRY[%s] %s\n  env -i HOME=%s CLAUDE_CONFIG_DIR=%s/.claude PATH=%s/... USER LOGNAME SHELL=/bin/zsh TERM LANG \\\n  %s\n' \
            "$n" "$name" "$JHOME" "$JHOME" "$JHOME/.local/bin" "$(step_dry "$n")"
        continue
    fi
    STEP_T0=$(date +%s); SECS=""; DF=""
    case $n in 0) step_0 ;; 1) step_1 ;; 2) step_2 ;; 3) step_3 ;; 4) step_4 ;; 5) step_5 ;; 6) step_6 ;; 7) step_7 ;; 8) step_8 ;; 9) step_9 ;; 10) step_10 ;; 11) step_11 ;; 12) step_12 ;; 13) step_13 ;; 14) step_14 ;; 15) step_15 ;; 16) step_16 ;; 17) step_17 ;; 18) step_18 ;; esac
    [ "$n" -eq 0 ] || guard_launchd
    record "$n"
    WORKER=$(state_get WORKER); PR_NUM=$(state_get PR)
done

printf '\ninstall-journey: pass=%s human=%s skip=%s fail=%s\n' "$PASSES" "$HUMANS" "$SKIPS" "$FAILS"
printf 'log: %s\nsummary: %s\n' "$LOG_JSON" "$LOG_MD"
[ "$FAILS" -eq 0 ] && exit 0
exit 1
