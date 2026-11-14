---
name: decide
description: Record a standing decision from a conversation ruling.
argument-hint: "<plain-language ruling>"
metadata:
  internal: false
  requires:
    binaries:
      - "fno >= 0.3.1"
---

# Decide

`/fno:decide` records a durable project decision in one step. The user types the ruling. A `d-` id comes back in the same turn. There is no staged proposal, no hash, no approval receipt, and no resume path.

A recording made from chat carries `authority_source: chat_attested`. It reads in the standing lane. It is never recorded as `operator`. So a reader can always tell a chat recording from a person at a terminal. `fno inbox decisions set` is the same destination for a user already at a terminal. It is the same one call.

## Beginner example

Type `/fno:decide Merges belong to the user` in chat. A `d-` id comes back.

## Workflow

1. Read the argument as a proposed durable rule. When the text names an area, query it and use the returned `canonical_subject`. A subject matches exactly. A near-synonym does not. When the subject or the rationale is genuinely unclear, ask one focused question. Do not ask for permission you already have. The user typing the line IS the invocation.
2. Classify the statement. A rule limited to this PR, node, target, change, or merge is coordination. Refuse to label it law. Offer `/fno:mail` or an ordinary agent ruling instead. `fno inbox decisions set` refuses those markers itself, so a slip is caught rather than recorded.
3. Read current decision with `fno inbox decisions <subject> --lane law --state live --json`. Treat an unreadable result as unknown. Continue only from `current_law.status`. `single` names the decision this ruling can supersede. `conflict` requires every id and a resolution first. `none` proves only that this canonical subject has no current law.
4. Record it:

```bash
fno inbox decisions set "<subject>" "<decision>" --rationale "<why>" --quote "<the user's words>" [--supersedes d-xxxxxxxx] [--keep d-xxxxxxxx]... [--sweep <word>]...
```

The door compares the new decision with every live ruling in the same scope, subject and text together. A ruling that shares six or more topic words, and half of the shorter text, is near. The door refuses until you name each near ruling. `--supersedes <id>` retires it. `--keep <id>` keeps it beside the new decision. Read each near ruling before you choose. Never pass `--keep` to get past the refusal.

Pass `--sweep <word>` for each word that names the changed thing in other words. A decision on `fno-app` versions sweeps `--sweep desktop`. After the write, the door finds the open nodes the decision touches: the best word matches, and every node that carries a `--sweep` word. It mails each node's owner session.

Pass the words the user typed as `--quote`. Under `autonomy.mode` `balanced` (the default) and `ask`, a session's decision write with no quote refuses with exit 3. If the user did not ask for this rule, ask them first. The quote records the ask. It never changes the row's authority.

The command prints the `d-` id on stdout. Report that id and the subject.

5. Read stderr. Each stderr line that starts `decision: d-...` names an open question the decision can answer. Read the question and the decision together, and judge whether the decision answers it. For each question it answers, run the mail command the line prints. Report which askers you told. Do not close a question yourself: an agent session cannot answer it, and a withdrawal notifies nobody.
6. When the recorded ruling names a command form, add a row to `scripts/ci/law-command-forms.txt` in the same PR. The row names the skill file that must carry the form. Then run `bash scripts/ci/check-law-command-forms.sh --live`. It confirms that every live form is registered.
7. Each `law: <id> touches node ... No owner session to mail.` line names open work with nobody to tell. Note the decision on that node: `fno backlog note <node> "<law id>: <what changes>"`.

## What this store does and does not reach

This store is machine-local. A stranger who clones the repository must obey some rules. Those rules do not reach that person from here. Land them in the code, a doc, or a gate, in a PR. When the user wants the rule recalled by subject, record it here as well.

Recording decision from chat works, and so does retracting a chat-recorded law. When the user says to drop a `chat_attested` decision, run `fno inbox decisions retract <id> --reason <why> --quote "<the user's words>"` from the session. The quote is required. The retract prints its undo: `fno inbox decisions restore <id>` re-records the decision under a new id that supersedes the old one. Restore refuses a live id, an id already restored, and a subject that gained a new live decision since. An `operator` decision stays the user's to retract: the session is refused, and the user answers on the question board. Supersession follows the same rule. A chat recording can supersede another `chat_attested` row. It cannot supersede an `operator` row.

## Refusals

`fno inbox decisions set` refuses, records nothing, and exits **3** in eight cases:

- the statement is coordination rather than durable decision,
- the rationale is missing,
- `--supersedes` is not a `d-` decision id, or names no recoverable decision,
- `--supersedes` names an `operator` decision row while this recording is `chat_attested`,
- `--graduation` is not a valid kind,
- a live ruling near the new decision is named by neither `--supersedes` nor `--keep`,
- `autonomy.mode` asks and a session passed no `--quote`,
- no harness session resolves and no terminal is attached, so nothing marks a decider.

Exit **1** means something else entirely. It means the row IS recorded and the recall index write failed. Run `fno backlog decide-reindex`. Never re-run the command.

Report the refusal verbatim. Say that no decision was recorded. Never infer a recording from a green exit. Read the printed `d-` id.

## Safety boundary

Invocation is the authority. The door is the standing LANE, not the `operator` value. Any process descended from a harness session records as `chat_attested`. That includes an agent's own Bash call. A mail-injected slash command is a special case of it. It is the case that is impossible to detect. An `operator` row stays refused from any session. That trade is deliberate. [LIMITATIONS.md](LIMITATIONS.md) states it with the measurement behind it. What the skill still guarantees is honest attribution. A chat recording says `chat_attested` and never claims the `operator` lane. Environment scrubbing stays forbidden.

## Known Limitations and Deferred Work

- See [LIMITATIONS.md](LIMITATIONS.md).
