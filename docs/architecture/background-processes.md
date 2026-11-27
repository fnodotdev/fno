# Background processes: which fno process or arm is which

## Is this page for you?

You see a process in `ps`, a `sh.fno.*` label in `launchctl list`, or an arm row in `fno agent session status`. You need to know what it is, who starts it, and how to stop it. The `via=` column of `fno agent session status` names the scheduler that owns each row. This page says what each scheduler is.

Not for: which program removed a session and in what order (see [reaping-faq.md](../reaping-faq.md)). And not for: why the stop hook allowed or refused a stop (see [control-plane-loop.md](control-plane-loop.md)).

## Five kinds of thing

A **launchd agent** is macOS's scheduler firing a command on an interval, a calendar time, or at load. It is either a one-shot tick or a `KeepAlive` server. macOS tracks it by label, and killing the tick pid mid-phase never stops the next fire.

A **daemon** is the one lazy-started `fno-agent-daemon` supervisor. Launchd never touches it: a client starts it on first need, and it exits after an idle window.

A **keeper** is a small process that outlives a different parent: the pty keeper outlives the mux server, and the store keeper outlives the daemon. Neither is launchd-managed and neither is a daemon.

An **arm** is a named unit of scheduled work that writes one `control_plane_tick` row per run. It is never a process, and one launchd agent carries several arms. The `pr_watch_merge` confusion comes from exactly this: it is an arm, a phase inside a launchd agent's tick, and not a process at all.

A **hook** runs inside the stopping agent's own process tree, only at Stop. It is not a background process at all.

Beyond the five kinds, two other long-lived fno processes exist: the mux server and the MCP sidecar. Both are listed under Long-running processes below.

## What is pr_watch_merge?

It is the merge phase of the `sh.fno.pr-watcher` tick: `crates/fno-agent/src/gate/pr_watch/tick.rs`, function `run`, calls `run_phase("merge", Some("pr_watch_merge"), 1, 0, phase_merge)`. Launchd fires the tick every 600 seconds, and the same invocation also drives the other launchd arms in the table below.

Read it with `fno agent session status` (the `via=launchd:sh.fno.pr-watcher` column says which scheduler owns the row) and `fno do pr watch status`. See [pr-watch-merge-phase.md](pr-watch-merge-phase.md) for why the merge phase fails and what each failure means.

## Launchd agents

The gated table: every `sh.fno.*` label an installer in this repo writes. Columns: label, installed by, runs, cadence, arms it hosts, health read, safe stop.

| label | installed by | runs | cadence | arms it hosts | health read | safe stop |
|---|---|---|---|---|---|---|
| `sh.fno.pr-watcher` | `fno do pr watch install` (`crates/fno-agent/src/gate/pr_watch/install.rs`, the HOME-derived label in `crates/fno-agent/src/spawn/launchd_labels.rs`) | `fno-agent do pr watch tick` (the installing binary's own path) | `StartInterval` 600 s | every arm stamped `launchd:sh.fno.pr-watcher` | `fno do pr watch status`, `fno agent session status`, `~/.local/state/fno/logs/pr-watcher.out.log` | `fno do pr watch uninstall`; rebind to a new binary with `fno do pr watch refresh` |
| `sh.fno.groom` | `fno backlog groom --install-agent` (`crates/fno-agent/src/backlog/groom.rs`, `install_agent`) | `fno backlog groom` | daily at `--hour` (default 2) | none: it writes no `control_plane_tick` row | `launchctl list sh.fno.groom.<home-suffix>` last exit, `fno doctor`, `~/.local/state/fno/logs/groom.out.log` | `launchctl bootout gui/$(id -u)/sh.fno.groom.<home-suffix>` (no uninstall command exists) |
| `sh.fno.law-upkeep` | `fno inbox decisions upkeep install` (`crates/fno-agent/src/backlog/law_upkeep.rs`, `install_agent`; label and plist const in `crates/fno-agent/src/spawn/launchd_labels.rs`) | `fno inbox decisions upkeep run --apply` (the resolved binary's path) | daily at `--hour` (default 2, minute 30) | none: it writes no `control_plane_tick` row | `fno inbox decisions upkeep status --json`, `launchctl list sh.fno.law-upkeep.<home-suffix>`, `~/.local/state/fno/logs/law-upkeep.out.log` | `fno inbox decisions upkeep uninstall` |
| `sh.fno.web-bridge` | `fno mux web install-agent` and the `fno config setup run` web-bridge step (`crates/fno/src/serve/web_agent.rs`, label in `crates/fno/src/setup/setup_services.rs`) | `fno mux serve --web --attention-api` (the resolved binary's path; ports pinned at install, web 8722) | `RunAtLoad` + `KeepAlive`: a server, not a tick | none: it writes no `control_plane_tick` row | `fno doctor`'s web bridge section, `launchctl list sh.fno.web-bridge.<home-suffix>`, `~/.local/state/fno/logs/web-bridge.out.log` | `fno mux web uninstall-agent` |
| `sh.fno.remote-door` | `fno agent workspace remote serve --install` (`crates/fno-agent/src/remote/keeper.rs`) | `fno-agent remote serve --port <port>` (the installing binary's path; port pinned at install, default 8725) | `RunAtLoad` + `KeepAlive`: a server, not a tick | none: it writes no `control_plane_tick` row | `fno agent workspace remote status`, `~/.fno/agents/remote/door-status.json`, `~/.fno/agents/remote/door.log` | `fno agent workspace remote serve --uninstall` |
| `sh.fno.daemon` | the `fno config setup run` daemon-agent step, installs with `--only daemon-agent` (`crates/fno/src/setup/setup_daemon_agent.rs`, label and plist const in `crates/fno/src/setup/setup_services.rs`) | `fno-agent-daemon --home <agents home>` (the PATH-resolved daemon binary pinned at install, the sibling of the resolved fno-agent as fallback) | `RunAtLoad` + `KeepAlive` on `SuccessfulExit = false`: a login server that launchd restarts on crash only, because the daemon's own 30-minute idle-exit is a clean exit the client lazy-start already covers | none: it writes no `control_plane_tick` row | `launchctl list sh.fno.daemon.<home-suffix>`, `~/.local/state/fno/logs/daemon-agent.out.log`, `~/.local/state/fno/logs/daemon-agent.err.log` | `launchctl bootout gui/$(id -u)/sh.fno.daemon.<home-suffix>`, then remove `~/Library/LaunchAgents/sh.fno.daemon.plist` (no uninstall command exists) |

The `<home-suffix>` is the first 8 hex characters of sha256 over your HOME path (`crates/fno-agent/src/spawn/launchd_labels.rs`). Two fno HOMEs on one macOS account derive different labels, so neither install replaces the other's launchd job; the plist file name keeps no suffix because each HOME has its own `~/Library/LaunchAgents` directory. Installers also retire the pre-suffix label (`sh.fno.pr-watcher`, `sh.fno.groom`) when its registered plist points at this HOME.

Below the table: other `sh.fno.*` labels can appear in `launchctl list` that footnote does not install. Today those are `sh.fno.autocontinue`, `sh.fno.board-server` and `sh.fno.sync-backlog`, an operator's own agents, and `fno agent loop control table` lists every label it folds, including them. The `auto_continue` arm's 1800 s heartbeat needs some scheduler that runs `fno backlog advance` with `FNO_CONTROL_PLANE_SCHEDULER` set (`cli/src/fno/control_plane.py`, `scheduler_from_env`). Without one the arm reads `session`.

## Long-running processes

Not gated by the doc-binding test: these are read from `ps`, not from source. Columns: process as `ps` shows it, started by, lifetime and exit, owns, health read, safe stop.

| process as `ps` shows it | started by | lifetime and exit | owns | health read | safe stop |
|---|---|---|---|---|---|
| `fno-agent-daemon --home <dir>` | `ensure_daemon` in `crates/fno-agent/src/client.rs`, on the first client call | exits after 1800 s idle unless an active-backlog project is live; binary resolves from `FNO_AGENT_DAEMON_BIN` or a sibling of the client | the roster, the registry, and every daemon-scheduled arm | `fno agent session status` (`daemon: serving pid=...`) | `fno agent session restart` (graceful; `--force` is break-glass) |
| `fno --server <sock>` (the mux server) | `spawn_server` in `crates/fno/src/ui/client.rs`, detached with setsid, on the first `fno` or `fno mux` attach | lives until killed | plain panes and the views over keeper panes | `fno mux pane keeper list` | `fno agent session restart --mux` is destructive: it ends plain panes and shells |
| `fno-agent-worker --keeper` (alias `--pane`, the pty keeper) | spawned per pane or thread | lives as long as the hosted child | the child's pty master | `fno mux pane keeper list` | never killed by hand while the child lives; see [pane-keeper.md](pane-keeper.md) |
| `fno-agent-worker --store-keeper --sock <graph>.store.sock` (the store keeper) | spawned by the graph store client when nothing listens on the socket | exits after 600 s idle; `FNO_STORE_KEEPER_IDLE_SECS` overrides, `0` disables | one graph file behind the socket | `lsof <graph>.store.sock` | ends by idle exit, or `fno agent session restart` cycles a stale build and the next read respawns it |
| the MCP sidecar (`crates/fno-agent/src/runtime/mcp/sidecar.rs`, entered as `fno-agent-worker --mcp-exec sidecar`) | lazy-started by the sidecar client on first channel use | idle-exits when no connection, no registered channel and no pending poke outlasts the idle window (default 1800 s) | MCP tool dispatch for its session | see [fno-agent-mcp-channel.md](fno-agent-mcp-channel.md) | ends by idle exit |
| `hooks/target-stop-hook.sh` | the harness, on every Stop | one invocation per stop | the `fno-agent loop-check` decision | `fno agent session status`, arm `stop_hook` | nothing to stop: it is not a background process |

## Two keepers, one word

The pty keeper and the store keeper share a binary (`fno-agent-worker`), a frame shape, and one rule: the keeper keeps, the server views. Nothing else. The other two worker lanes are not keepers. `--stream` streams a child's output. `--store-exec` serves one request and exits, so a client leaves no resident process behind.

## Arms

The gated table: one row per arm the readout can show. The scheduler cell is the exact stamp `KNOWN_ARMS` and the tick rows carry (`crates/fno-agent/src/eval/tick_ledger.rs`). Columns: arm, scheduler, hosted by, what it does, default interval.

| arm | scheduler | hosted by | what it does | default interval |
|---|---|---|---|---|
| `lead_wake` | `daemon` | `fno-agent-daemon` | wakes a lead one minute past its check-in beat; a second missed beat in a row also tells the operator | 300 s |
| `worker_wake` | `daemon` | `fno-agent-daemon` | nudges ended Codex turns or resumes proven-dead sessions on unfinished work, preserving intentional waits | 60 s |
| `lead_wake` | `launchd:sh.fno.pr-watcher` | the pr-watch tick | the python-served pre-rename spelling of `lead_wake` | 900 s |
| `watchdog` | `launchd:sh.fno.pr-watcher` | the pr-watch tick | the fleet watchdog classifier over lanes | 600 s |
| `pr_watch_sweep` | `launchd:sh.fno.pr-watcher` | the pr-watch tick | scans open-PR backlog nodes and fires `/fno:ship pr check` | 600 s |
| `pr_watch_merge` | `launchd:sh.fno.pr-watcher` | the pr-watch tick | the merge phase: fires the merge queue for ready PRs | 600 s |
| `active_backlog` | `daemon` | `fno-agent-daemon` | the daemon's mission drain | 300 s |
| `auto_continue` | `session` | backlog advance + the 1800 s heartbeat | reconciles web-merged PRs and dispatches the next node; upstream `pr_watch_merge` | 1800 s |
| `notify_watch` | `launchd:sh.fno.pr-watcher` | the pr-watch tick | the state-change signals arm | 300 s |
| `stop_hook` | `hook:target-stop-hook` | the target stop hook | one row per stop-hook fire | event-driven |
| `reap` | `daemon` | `fno-agent-daemon` | reaps rows, processes, claims and worktrees of merged PRs | 60 s |
| `orphan_reap` | `daemon` | `fno-agent-daemon` | reaps confirmed orphaned test binaries, mux servers, and any lease-carrying process whose owner process died | 300 s |
| `retire` | `daemon` | `fno-agent-daemon` | retires finished work | 300 s |
| `machine_watch` | `daemon` | `fno-agent-daemon` | watches sustained machine footprint | 300 s |
| `claude_restart_watch` | `daemon` | `fno-agent-daemon` | revives claude thread workers whose job identity a Claude daemon restart moved, through boot revival's revive door | 120 s |
| `arm_watch` | `daemon` | `fno-agent-daemon` | pages the operator when arms stay broken past the threshold | 300 s |
| `lead_settle` | `daemon` | `fno-agent-daemon` | mails the owning lead when a covered PR settles green or its node closes | 300 s |
| `provider_cap` | `daemon` | `fno-agent-daemon` | provider cap accounting | 120 s |
| `slot_cutover` | `daemon` | `fno-agent-daemon` | switches to a globally declared managed Claude account when the proven account is low or exhausted | 120 s |
| `slot_login_health` | `daemon` | `fno-agent-daemon` | health-checks every shared-slot managed Claude login and raises one operator notice when one dies | 120 s |
| `merge_close` | `daemon` | `fno-agent-daemon` | merge-close sweeps | 900 s |
| `board_render` | `daemon` | `fno-agent-daemon` | renders graph.md plus every configured board target once per moved store version, the render-views command bounded at 300 s | 300 s |
| `team_ledger` | `daemon` | `fno-agent-daemon` | renders rundown.html | 300 s |
| `lead_eval` | `daemon` | `fno-agent-daemon` | writes missing eval parts for ended Claude leads | 600 s |
| `question_judge` | `daemon` | `fno-agent-daemon` | reads a batch of open questions against the record with a one-shot reader and writes shadow verdicts when `questions.judge.enabled` is on | 3600 s |
| `attention` | `daemon` | `fno-agent-daemon` | selects attention rows into the status payload | 30 s |
| `notice_route` | `daemon` | `fno-agent-daemon` | routes reconcile warnings to the owning lead and folds repeated failures into one node | 300 s |
| `otlp_export` | `daemon` | `fno-agent-daemon` | exports the tick ledger's control-plane rows to the configured OTLP endpoint when `telemetry.otlp.enabled` is on | 300 s |
| `eval_events` | `daemon` | `fno-agent-daemon` | sends each eval attempt to PostHog as an event when `telemetry.eval_events.enabled` is on | 300 s |
| `pane_loop` | `daemon` | `fno-agent-daemon` | types each due armed loop prompt into its session's pane, skipping a pane the manifest reads working or blocked | 30 s |
| `heal` | `launchd:sh.fno.pr-watcher` | the pr-watch tick | the PR auto-heal drive, gated on `auto_heal.enabled` | 600 s |
| `evals` | `daemon` | `fno-agent-daemon` | checks the evals gate and launches due regression-bank work outside the pr-watch tick | 60 s |
| `stranded` | `launchd:sh.fno.pr-watcher` | the pr-watch tick | the stranded-session sweep | one tick in three |
| `recovery` | `launchd:sh.fno.pr-watcher` | the pr-watch tick | the recovery sweep | one tick in three |

Under the table, four notes. First, `stranded` and `recovery` have no `KNOWN_ARMS` row, so the readout shows them only after they tick and never reads them `UNOBSERVED`. Second, `stranded`, `recovery` and `watchdog` run one tick in three on staggered slots of the 600 s tick. Their effective cadence is one run per 1800 s. Third, the daemon runs work that writes no arm row: the worktree sweep, the merge reaper, orphan sweeps, the terminal-stop sweep, and liveness. [reaping-faq.md](../reaping-faq.md) holds that list. The evals-arm checks each minute for due launches; the separate evals-gate settle grades finished runs on its one-minute sweep. Fourth, [loops.md](../loops.md) is generated from this same arm list and carries each loop's arming key and live state. When a row reads red, run `fno agent loop control table`.

The sweep's per-PR rich read is bounded: one row no more often than every two intervals, default 20 minutes. The cheap listing leg already answered that row's state. The least-recently-read order spreads the pool across the window. A fleet budget hold above 10 s pauses the read pass outright. A refused read ends the pass early and never marks the row stale.

Every arm's report-freshness contract (source, bookmark, re-check, send-confirm, against six rules) is audited in [report-freshness.md](report-freshness.md).

## Stopping things safely

A launchd agent is stopped by its uninstall command or `launchctl bootout`, never by killing the tick pid mid-phase. The next fire is already scheduled. The daemon is restarted, never killed, because workers survive a graceful restart. A keeper is never killed by hand while its child lives. A pty keeper kill ends the session. A store keeper ends by idle exit or `fno agent session restart`.

To keep the daemon down for a window (a data dump, a switchover), run `fno agent restart --hold "<reason>"`. It writes `daemon.hold` in the agents home, then stops the daemon. While that file exists, `ensure_daemon` refuses to start one. The daemon binary also exits clean at boot. So no client command and no launchd relaunch revives it. The `--if-down` and `--if-drifted` gates stay quiet under a hold. `fno agent restart --release` removes the file and starts the daemon. A plain SIGTERM holds nothing: the next client command starts a new daemon in seconds.
