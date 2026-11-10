# Move a live session to another machine

`fno agents session move <name> --to <host>` carries one live claude session to another machine over ssh. The session keeps its name and its session id. The source stops and its registry row goes away, so nothing runs twice.

To take a machine out of service, use it on each session. Use `--all --to <host>` to move every claude worker on this host. Add `--dry-run` to print the steps and move nothing.

## What a move carries

A move carries five things.

1. The commits. The source pushes the branch straight to the target's git directory over ssh. The move needs no GitHub.
2. The worktree. The target adds a worktree at the same path. A dirty tree is copied with rsync after the source stops.
3. The transcript. The source copies the transcript, its `<session-id>/` sidecar directory and the origin record to the same paths on the target.
4. The lead canon doc. A row with a `role_scope` carries its handoff file. A lead is not resumed on the target. The move prints the hand-off command for a successor.
5. The claims. The target takes over the `node:` claims that the session held.

A move needs the same absolute worktree and transcript paths on both hosts. If they differ, the move refuses.

## The step order

The order is the contract. The source stops before it copies any transcript byte, so two writers never share one transcript.

1. Preflight.
2. Push commits.
3. Stop source.
4. Worktree sync. This step runs only for a dirty tree.
5. Transcript sync.
6. Canon doc sync.
7. Target row. The target registers the row under the same name.
8. Claims handover.
9. Resume. The target resumes the session and sends it a first message.
10. Verify. The move polls the target for 120 seconds.
11. Retire. The source runs `fno agents session rm <name> --force`, which removes its row and its stopped session. The claims already moved at step 8, so retire releases none.

Three facts must hold for verify to pass. The target lists one live row under the same name and session id. The target transcript is longer than the copied length. The move then prints `moved: <name> <session-id> -> <host>`.

## Refusals

A refusal exits 2 with one line that names the cause. It happens before any step touches either machine.

- The harness is not claude. Codex and opencode moves are not built. Use the [manual recipe](#manual-recipe).
- `--to` names this host, or is not a host name.
- The name has no row in this host's registry.
- The row has no session id, or no transcript is found.
- The target does not answer `ssh -o BatchMode=yes` with `fno --version`.
- The target claude daemon is not gui-born. A daemon born in an ssh session or a launchd job loses DNS, and a resume lands under it. Start the daemon from a login session on the target, then retry.
- The target path already holds a different worktree, or a newer transcript or canon doc.

## Recover from a failed verify

When verify fails, the source stays stopped and keeps its row. The move exits 1 and prints one command that resumes the session on the source:

```
fno agents session resume <name>
```

Run it. Then find the cause on the target and move again.

## Manual recipe

Use this for a codex or opencode session. Run each step once, in this order.

1. Push the branch. Run `git push` from the worktree.
2. Stop the session. Run `fno agents session stop <name>`.
3. Copy the worktree to the same path on the target.
4. Copy the transcript and its sidecar. `fno agents registry transcript send` and `receive` carry one transcript over a pairing code.
5. On the target, run `fno agents registry adopt <session-id>`. Then run `fno agents registry rename cc-<short-id> <name>`.
6. Run `fno agents session resume <name>` on the target.
