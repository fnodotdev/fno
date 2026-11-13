---
name: cancel-target
description: Cancel an active target pipeline
---

# Cancel Target

Cancels a target pipeline. Behavior depends on whether a state file exists:

- **Live session (manifest present):** assert the `.target-cancelled` signal first. The stop gate records the `Interrupted` terminal on the next stop. This is the sanctioned cancel path. The reader is `check_cancel_sentinel` in `crates/fno-agents/src/gate/cancel_sentinel.rs`. It is independent of the transcript-id match and the "latest user turn" check. A human typing the command and an assistant invoking the skill both work. The state file is NOT removed. Removing it re-arms the orphan detector, and the transcript permanently records the `/fno:target` invocation. That re-blocks exit on every stop. Leaving it lets the gate emit the termination event as a durable terminal record. The `Interrupted` finalize carries the postmortem and the ledger entry. Its claim release (`--stamp-do`) closes the do row pinning the node at `in_progress`. The next `fno do target init` archives that terminal state cleanly.

  The `ready` return is the claim's doing. The cancel terminal releases the run's `node:<id>` claim. That release closes the `execute` row that was pinning the status.

- **Orphan (no manifest):** the session was driven off-ceremony (init skipped) or a prior cancel removed the manifest. Clearing the orphan block requires a genuine human-typed `/fno:target cancel`. That human input is the anti-forgery factor an assistant cannot satisfy by invoking this skill itself. The skill writes a session-keyed tombstone so a human's command is honored. The orphan block is bounded and records the bypass. It self-terminates after a few stops, so an unattended loop cannot burn credits indefinitely.

```bash
REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd -P)"
STATE_DIR="$REPO_ROOT/.fno"
# The manifest resolves through the one-owner path command (state_path.rs):
# the live manifest sits in the repo's space dir, not at <repo>/.fno/, and
# only the command's fallback leg still names the checkout path. Guessing
# <repo>/.fno/target-state.md here sent every worktree session's cancel down
# the orphan branch. When fno-agents is absent entirely, the checkout path
# is the honest guess.
MANIFEST="$(fno-agents state path target-state 2>/dev/null || echo "$STATE_DIR/target-state.md")"
SENTINEL="$STATE_DIR/.target-cancelled"
TOMBSTONE="$STATE_DIR/.target-cancelled-final"

if [[ -f "$MANIFEST" ]]; then
  echo "Current target state:"
  grep -E "^(status|input|graph_node_id|target_claim_key|created_at):" "$MANIFEST" || true
  echo ""
  # Gate on a status LINE only when the manifest carries one. Legacy
  # manifests had a status field and only an IN_PROGRESS session was
  # cancellable: touching the sentinel on a finished legacy run would
  # downgrade it, so that branch keeps its refusal. The current manifest has
  # NO status field (the control-plane collapse moved the live/terminal
  # truth into the world: the claim, the ledger, the board), so an absent
  # status reads as LIVE. The safety work the status field once did is done
  # by the sentinel's own guards: the mtime check (a sentinel at or after
  # the manifest's created_at; an older one is ignored), the self-cancel
  # routing in loop-check (a session cancelling itself routes stuck, not
  # stop), and a holder-verified, fill-only claim release in the cancel
  # settle. A finished run's claim is already gone, so its release is a
  # non-fatal no-op and a done node stays done.
  _cs_status=$(grep -E '^status:' "$MANIFEST" | head -1 \
    | sed -e 's/^status:[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/^"//' -e 's/"$//' -e "s/^'//" -e "s/'\$//")
  if [[ -n "$_cs_status" && "$_cs_status" != "IN_PROGRESS" ]]; then
    echo "Target session is already '$_cs_status' (terminal) - nothing to cancel."
  else
    # Assert the sanctioned cancel signal. check_cancel_sentinel honors a
    # .target-cancelled whose mtime is at or after the manifest's created_at.
    # The hook then writes status: BLOCKED itself and exits cleanly on the next
    # stop. We deliberately keep the state file. The payload names the author
    # and reason so the resulting Interrupted line explains the cancel; a bare
    # `touch` stays valid and reads as unattributed.
    # The author names the writing session when the env marks one, so a
    # SELF-cancel (F6) is detectable; the loop check re-derives user vs
    # assistant from the transcript at honor time, which is the truth.
    printf 'author: %s\nreason: cancelled via /fno:cancel-target\n' "${CLAUDE_CODE_SESSION_ID:-${CODEX_THREAD_ID:-operator}}" > "$SENTINEL"
    _EVENT_DATA="$(jq -cn --arg path "$SENTINEL" '{lane: "target", path: $path, reason: "operator"}' 2>/dev/null || true)"
    if [[ -n "$_EVENT_DATA" ]]; then
      fno doctor event emit -t cancel_signal_set -s target -d "$_EVENT_DATA" >/dev/null 2>&1 || \
        echo "warning: cancel signal set but its event was not emitted ($SENTINEL)" >&2
    else
      echo "warning: cancel signal set but its event payload could not be built ($SENTINEL)" >&2
    fi
    echo "✓ Cancel signal asserted ($SENTINEL)."
    echo "  The stop gate reads the sentinel on the next stop, records the"
    echo "  Interrupted terminal, releases the run's claim, and allows the exit."
  fi
else
  # No manifest: orphan / off-ceremony session. Write the session-keyed
  # tombstone so that, when a HUMAN types /fno:target cancel, the orphan
  # detector honors it (the assistant invoking this skill cannot self-clear an
  # orphan - that is the anti-forgery factor). Key the tombstone to
  # CLAUDE_CODE_SESSION_ID, which equals the stop hook's HOOK_INPUT.session_id
  # and is robust even though no state file survives to read
  # claude_transcript_id from.
  TID="${CLAUDE_CODE_SESSION_ID:-}"
  # In an off-ceremony orphan, .fno/ may not exist yet; without this the
  # redirect below fails ("No such file or directory") and the cancel silently
  # writes nothing (Gemini MEDIUM on PR #391).
  mkdir -p "$STATE_DIR"
  {
    echo "claude_transcript_id=${TID}"
    echo "cancelled_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  } > "$TOMBSTONE"
  if [[ -n "$TID" ]]; then
    echo "✓ Cancel tombstone written ($TOMBSTONE), keyed to this session."
    echo "  When a human types /fno:target cancel, the orphan detector"
    echo "  honors it and allows a clean exit. The assistant cannot self-clear"
    echo "  an orphan block; if no human is watching, the orphan block"
    echo "  self-terminates after a few stops and the gate-bypass is recorded."
  else
    echo "⚠ CLAUDE_CODE_SESSION_ID is unset; cannot key the tombstone to this" >&2
    echo "  session. A human can close a genuinely-shipped off-ceremony session" >&2
    echo "  as COMPLETE with /fno:target override <reason>." >&2
  fi
fi
```

On the next stop, for a live session the stop gate reads `.target-cancelled` via `check_cancel_sentinel`. It emits the termination event, and the `Interrupted` finalize writes the postmortem, records the ledger entry, and releases the run's `node:<id>` claim with `--stamp-do`. The release closes the `do` row that was pinning the node at `in_progress`, which returns the node to `ready`. Then it allows a clean exit.

The sentinel can carry a payload of `author:` / `reason:` lines. Loop-check echoes them in the Interrupted line and then deletes the sentinel, so one cancel terminates one run. A bare `touch` stays valid and reads as unattributed.

For an orphan, a human-typed `/fno:target cancel` is honored via the tombstone. `init-target-state.sh` clears both `.target-cancelled` and the tombstone the next time a target session starts in this worktree.
