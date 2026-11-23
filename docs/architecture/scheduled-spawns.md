# Scheduled spawns

A scheduled spawn queues a worker for the daemon to fire at a time, on an interval, or on a condition turning true. The spawn gate, overlays, naming and journal stay one implementation: the fire re-enters the spawn front door as a subprocess, the way the PR watcher fires a review spawn.

## The row

`fno agent spawn --when <time> <name> [prompt]` writes one row into the scheduled_spawns table beside the registry tables in the shared graph store. The row carries the fire time (UTC only on disk), the trigger, the grace, the force bit, the node, the recorded spawn argv, the cwd and the scheduler session. `fno agent spawn --list-scheduled` reads the table, `--cancel-scheduled <id>` drops one row, and both answer before the strict spawn parse. A row survives a daemon restart, because the store is the database the registry already survives one.

## The three triggers

`--when <time>` fires once at the named instant. Time input accepts the machine's local zone by default, a named IANA zone (`12:00 America/Los_Angeles`), a US abbreviation (`12:00 PT`), UTC (`20:00Z`), or a full RFC 3339 timestamp. A bare `HH:MM` already past today rolls to tomorrow, and the echo says so. A DST-ambiguous wall clock refuses naming both UTC candidates; a DST gap refuses naming the next valid local time.

`--every <interval>` fires on an interval: `hourly`, `daily`, `weekly`, or a duration like `30m` or `2h`. The first fire is one interval after the schedule, and the daemon re-queues the row after each fire.

`--on <command>` polls the command each arm pass under sh -c with the row's cwd, bounded at 60s with the process-group kill. Exit 0 fires the row. Its grace counts from creation, so a command that never exits 0 releases the row after a week by default.

## Fire-time gate semantics

At the fire time the daemon re-enters `fno agent spawn` with the recorded argv, so the worker passes through the same gate any manual spawn passes: seat caps, lead share, occupancy and placement all read live. `--force` recorded on the row skips at fire time exactly what `spawn --force` skips now (seat caps, lead share, the CPU instrument refusal) and never lifts the memory or disk axis; the row journals the gate reading it overrode. The fire passes `--spawned-by <scheduler session>` so lead share counts against the scheduler while that session lives; a dead scheduler passes the axis, so a missed scheduler never wedges the fire. A done, superseded or deferred node skips the fire and journals why. Two rows due in one minute fire in live node priority order (p0 before p3), then fire time, then row id.

The re-entry runs with the daemon's environment, not the scheduler's, so a caller-relative anchor names no pane at fire time. `relative_placement` in `crates/fno-agent/src/spawn/spawn_schedule.rs` decides it: `--from current`, or a split with no `--tab`, `--from` or `--portal N`. The front door refuses such a row at schedule time, while the caller can still fix the line. A row already stored with one fires as an unshown thread through `unshown_thread_argv`. The fire writes the reason to `last_gate_reading` and the fired journal row. A refusal at fire time ends an `--every` series, because only a fire requeues the row.

## Grace, retry and miss

A refused fire retries every 5 minutes inside the row's grace (60 minutes for a timed row by default, 7 days for an --on row, counted from creation), then the row reads `missed` with the gate reason and one mail goes to the scheduler. The retry backoff rides the row's last attempt stamp, so a daemon restart does not shorten it.

## Restart and reboot

The store is the shared graph database, so a row survives a daemon restart. After a reboot the PR-watcher launchd tick restarts a down daemon inside one 600s interval while a row is pending. The ensure rides its own idempotent `restart --if-down`, and a pinned lane never forks a daemon. A pending row holds the daemon's idle exit and never the drift retirement: drift outranks it, so a weekly row never pins an old binary for a week.

## The autonomy row and the switch

`fno agent admit autonomy status` lists the scheduled spawn with its gate key `config.schedule.enabled` (default true, under the `config.autonomy.enabled` master switch). With the gate false, a due row journals `scheduled_spawn_skipped` with reason `disabled` and fires nothing. The row stays pending, so re-enabling the gate lets it fire later. Fired workers carry the dispatch-provenance code `sc`.

## Journal kinds

The arm journals four kinds into the agents journal: `scheduled_spawn_fired`, `scheduled_spawn_missed` (with the gate reason and the scheduler session), `scheduled_spawn_skipped` (a closed node, or the disabled gate), and `scheduled_spawn_requeued` (an interval re-queue after a fire). The fleet feed projects them like any agents journal row.
