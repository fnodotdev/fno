# Report freshness: what each arm re-checks before it reports

Every arm that tells a person something owes that person a fresh item. This page binds each arm in the [arms table](background-processes.md) to its source, its bookmark, its re-check step, and its send-confirm, against six rules. Rule source: the article `Building effective agent automations` (2026-10-08), at `~/c3po/raw/articles/2026-10-08-building-effective-agent-automations.md`. The six rules, quoted: read from a bookmark, not a window. A failed read is never a quiet day (`READER FAILED`). Re-check every item just before posting. When a send is confirmed, count it, then advance the ledger. Re-read preferences each run. Read-only creds and a spend cap close the set.

## The four house mechanisms

Four mechanisms carry the rules. A row below names the one its arm uses.

- **Keyed operator notices.** `operator_notice` (crates/fno-agent/src/runtime/operator_notice.rs) keys every page, and a key sends once: the verdict reads `Sent` or `Deduped`. arm_watch (arm_watch.rs:55-88), machine_watch (machine_watch.rs:955), and both slot arms (slot_cutover.rs:146-150) page through it.
- **Fingerprint stores.** notice_route keeps `~/.fno/state/notice-route/sent.json`, fingerprint to timestamp, pruned on write (notice_route.rs:1-8). The lead-wake receipts (lead_wake.rs:1-7) gate each wake: one lapse, one wake.
- **Cursors.** The mail drain advances per-form cursors only after confirmed delivery (crates/fno-agent/src/runtime/mail_cli/drain_self.rs:170-179). A re-ack is a no-op, and a corrupt cursor reseeds freely (mail_notify_self.rs:262-265).
- **Live reads at send time.** A sender that reads its fact and sends in one pass needs no bookmark. The item cannot go stale between read and send. The machine adapters (event_emit.rs:231-240, note_public.rs:397-409) and the settle leg (lead_settle.rs:379-410) hold this shape.

A failed read is never quiet anywhere in the house: the check-in prints `READER FAILED <name>: <reason>` and the beat continues (skills/lead/references/check-in.md:53).

## The arms

One row per arm in the arms table. Column semantics: the bookmark cell names the mechanism or reads `none - live read per tick`. The re-check cell names what holds the item fresh or reads `single fact, fresh at trigger`. The send-confirm cell names what confirms the delivery or `unread in this audit`.

| arm | reports | source | bookmark | re-check | send-confirm |
|---|---|---|---|---|---|
| `lead_wake` (daemon) | wake mail to a lead near its beat | journaled beats | lead-wake receipts (lead_wake.rs:1-7) | one lapse, one wake | unread in this audit |
| `worker_wake` | nudges to ended or dead workers | session state | none - live read per tick | single fact, fresh at trigger | unread in this audit |
| `lead_wake` (launchd spelling) | the pre-rename spelling of the daemon `lead_wake` | journaled beats | same receipts | one lapse, one wake | unread in this audit |
| `watchdog` | lane classifications | lanes | none - live read per tick | single fact, fresh at trigger | unread in this audit |
| `pr_watch_sweep` | fires `/fno:ship pr check` on open-PR nodes | open PRs | none - live read per tick | single fact, fresh at trigger | unread in this audit |
| `pr_watch_merge` | fires the merge queue | merge queue | none - live read per tick | single fact, fresh at trigger | unread in this audit |
| `active_backlog` | the mission drain | backlog nodes | none - live read per tick | single fact, fresh at trigger | unread in this audit |
| `auto_continue` | reconciles web-merged PRs and dispatches the next node | graph + PR state | none - live read per tick | single fact, fresh at trigger | unread in this audit |
| `notify_watch` | state-change signals | state feed | none - live read per tick | change feeds from the read itself | unread in this audit |
| `stop_hook` | one tick row per stop | the stop event | none - event-driven | single fact, fresh at trigger | the row is the receipt |
| `reap` | none: reaps rows and processes | reaper stores | none - live read per tick | not a report | the removal is the confirm |
| `orphan_reap` | none: reaps orphaned test binaries and dead leases | reaper stores | none - live read per tick | not a report | the removal is the confirm |
| `retire` | none: retires finished work | graph rows | none - live read per tick | not a report | the removal is the confirm |
| `machine_watch` | operator page on sustained machine footprint | machine read per tick | none - live read per tick | single fact, fresh at trigger | keyed notice (machine_watch.rs:955) |
| `claude_restart_watch` | revival rows for moved thread workers | boot revival state | revival receipts | single fact, fresh at trigger | keyed mail via lead_mail (boot_revival.rs:625) |
| `arm_watch` | operator page on arms broken past threshold | tick ledger | none - live read per tick | single fact, fresh at trigger | keyed notice, Sent or Deduped (arm_watch.rs:55-88) |
| `lead_settle` | settle mail to the owning lead | live PR read per pass | notice keys (lead_settle.rs:350-351) | single fact, fresh at pass | durable envelope (lead_settle.rs:408) |
| `provider_cap` | none: provider cap accounting | cap ledger | none - live read per tick | not a report | the ledger row is the confirm |
| `slot_cutover` | operator notice on managed-slot cutover | slot state | none - live read per tick | single fact, fresh at trigger | keyed notice (slot_cutover.rs:146) |
| `slot_login_health` | one operator notice per dead shared-slot login | slot login state | none - live read per tick | single fact, fresh at trigger | keyed notice (slot_cutover.rs:150) |
| `merge_close` | none: merge-close sweeps | graph rows | none - live read per tick | not a report | the closure is the confirm |
| `board_render` | renders graph.md and configured boards | graph store | render stamp per moved store version | renders read current state at render time | the render output is the confirm |
| `team_ledger` | renders rundown.html | graph store | render stamp per moved store version | renders read current state at render time | the render output is the confirm |
| `lead_eval` | none: writes missing eval parts for ended leads | eval bank | none - live read per tick | not a report | the written part is the confirm |
| `question_judge` | shadow verdicts on open questions | open questions and the record | verdict ledger keyed by hash (question_judge.rs:1-4) | single fact, fresh at batch | no send: verdicts stay in the ledger |
| `attention` | selects attention rows into the status payload | attention rows | none - live read per tick | single fact, fresh at read | no send: the payload is the read |
| `notice_route` | reconcile warnings to the owning lead | reconcile warnings | sent.json fingerprint store (notice_route.rs:1-8) | single fact, fresh at trigger | fingerprint dedup, pruned on write |
| `otlp_export` | exports tick rows to OTLP | tick ledger | none - live read per tick | not a report | unread in this audit |
| `eval_events` | sends eval attempts to PostHog | eval events | none - live read per tick | not a report | unread in this audit |
| `pane_loop` | types due loop prompts into panes | armed loops | none - live read per tick | single fact, fresh at trigger | the pane send receipt |
| `heal` | the PR auto-heal drive | PR state | none - live read per tick | single fact, fresh at tick | unread in this audit |
| `evals` | checks the evals gate and launches due work | eval bank | none - live read per tick | single fact, fresh at tick | unread in this audit |
| `stranded` | the stranded-session sweep | session rows | none - live read per tick | not a report | unread in this audit |
| `recovery` | the recovery sweep | session rows | none - live read per tick | not a report | unread in this audit |

## The lead check-in

`fno agent hq checkin` reads each of its lines at print time (skills/lead/references/check-in.md:31-53). The repeated-ask count reads this session's replies. The workers line reads `fno agent fleet top --json`. Boot-revival reads the boot pass rows. The visual-log line probes `ensure-reader.sh check` every beat. A failed reader prints its own `READER FAILED <name>: <rule>` line and the beat continues. Verdict: the check-in complies by construction on rules 1 to 4. Every line re-reads at print. A failed read is never quiet. The journal row carries the numbers it printed. Rules 5 and 6 (preferences, creds, spend cap) read from config each run.

## The ask door

The question ask door reads the same facts the sweep closes on, at filing time (crates/fno-agent/src/backlog/needs.rs `ask_subject_resolved`, wired at crates/fno-agent/src/backlog/question_intake.rs in `run_intake`). A terminal rung on every node the ask names refuses the ask. A merged PR on one of them refuses the ask with the merge in the refusal. An amend of an open row rides through. A user-owed ask never refuses. An ask with no node pointer skips the facts read. An unread PR files normally.
