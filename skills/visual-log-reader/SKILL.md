---
name: visual-log-reader
description: Fleet-owned reader for Visual Log taps. One beat per wake, every 15 minutes, in one dedicated session. Load when running the reader session, checking reader liveness, or repairing it.
metadata:
  requires:
    harness:
      - claude
---

# Visual Log tap reader

The Visual Log page carries Approve and Change buttons on every entry. A tap writes a row to the page's database. This skill reads new rows, records each one, and writes the ack back. The reader is ONE fleet-owned session, not a duty of whichever lead happens to be alive. A lead session's cron dies with the lead, and a second reader double-records approvals, so no session ever arms a private reader cron.

## Fleet ownership

Spawn the reader once, on the thread lane:

```bash
fno agents spawn --as thread --name visual-log-reader "/fno:visual-log-reader"
```

The daemon revives a dead reader. `worker_wake` resumes proven-dead sessions. `claude_restart_watch` re-binds a claude job the daemon restart moved. `fno mux workspace restore` brings a reader back through the claude revive plan after a reboot or a killed mux server. A member already live is focused, never spawned twice.

## Liveness

Any session checks the reader mechanically. The probe reads the agent registry's served last-activity age for the reader row, so liveness is measured, never assumed from a stored status:

```bash
SKILL_DIR="$(cat ~/.fno/install/plugin-root 2>/dev/null)/skills/visual-log-reader"
bash "$SKILL_DIR/scripts/ensure-reader.sh" check
```

Exit 0 means present and fresh (last activity inside 20 minutes). Exit 1 means absent or stale, and the output names the repair: the spawn line for an absent reader, `fno agents session resume visual-log-reader` for a stale one. `ensure-reader.sh spawn` runs the same check first and fires the spawn only on an absent row. A stale reader resumes, it never respawns. In a source checkout the script lives at `skills/visual-log-reader/scripts/ensure-reader.sh`.

## The beat, every 15 minutes

Wake off the :00 and :30 marks, where the fleet's other crons fire: the minute marks are 7, 22, 37 and 52. On claude, pace yourself with ScheduleWakeup to the next mark (about 900 seconds). CronCreate with `7,22,37,52 * * * *` is the alternative inside this session. One beat:

1. Read `taps`, `acks` and `config/approvers` (`db_list`, `db_get`).
2. Pick each tap row with no ack, or an ack older than the tap.
3. Check the viewer. The tap doc id is the viewer id. An id not in `config/approvers` gets ack `refused`. Name the viewer to the user once in chat, with the name `db_profiles` gives. When the user says it is their account, add the id to `config/approvers` with `db_write`. The next beat records the tap.
4. Read the PR head: `fno do pr info <pr> --repo <repo>`. A head that differs from the row's head gets ack `stale` with detail `the PR is now at <head 10>`. Record nothing.
5. Record the tap:
   - Approve: run the decide command below. It writes the same record a chat approval makes. The merge gate reads a role row at the PR's node. The rationale must say the user tapped Approve on the Visual Log. The text must name the head.
   - Change: record nothing in the decision store. The look stays unapproved, so the gate keeps holding.

   ```bash
   fno inbox decide <node> "visual approval PR <pr> at <head>: approved" --authority role \
     --rationale "User tapped Approve on the Visual Log (viewer <viewer id>)"
   ```

   A refused decide gets ack `refused`. Its detail is the refusal's first line.
6. Tell the worker and the lead with `fno backlog note <node> "Visual Log: <Approve|Change> at PR <pr> head <head 10>. <note>"`. The note mails the node's live holder and the epic's lead. When it exits 3 (no live holder), mail the PR author's lead by name. Keep the note text verbatim: a lead reads every Change note back with `fno backlog find --notes "Visual Log: Change"` before it rules a look.
7. Write the ack: `db_write` a merge on `acks/<viewer id>` that sets `rows.<key>` to `{state: "recorded", detail: "", at: <now ISO>}`.

A beat with no new tap writes nothing and mails nobody.

## Where the data lives

The page template, the database rules, the taps/acks/config shapes, and how to add an entry are owned by [skills/lead/references/visual-log-taps.md](../lead/references/visual-log-taps.md). That file owns the data contract. This skill owns the reading loop.

## Known Limitations and Deferred Work

- The probe sees only this machine's registry row. See [LIMITATIONS.md](LIMITATIONS.md).
