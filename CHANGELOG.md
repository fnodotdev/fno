# Changelog

Notable changes to fno, newest first. From 0.4.8 on, each release also has a page on [GitHub releases](https://github.com/fnodotdev/fno/releases). Run `fno --version` to check your installed version. Run `fno update` to upgrade.

Releases before 0.4.8 were built in a private repository. Their entries here summarize the release notes from that time. The pull request numbers in the notes up to 0.4.8 point at that repository, so this file leaves them out.

## [Unreleased]

### Added

* Added the attention ledger: `fno inbox attention-ledger` reports the week's orchestration tax. The lead checkin shows the numbers as one line. The weekly report lands under the vault reports dir.

### Changed

* Renamed the docs stamp action `fno do docs bind` to `fno do docs link`. The old word is a shell builtin. Claude Code refuses a worktree-isolated session's command whose string names it. The stamp step was refused in every isolated worker that owed the step. The new spelling is the only one the help names. The old spelling stays one release as a hidden alias that dispatches to link. Skills, help lines and the docs step carry the new name. The docs step names the hand-stamp route while an installed binary predates the rename.
* Backlog filing stages the node facts. `fno backlog idea` and `fno backlog add` file with the coarse `--audience` and `--why` alone. At scoping, `--done-means` and a customer node's `customer:<persona>` become required. The move to ready, a blueprint, and a target start refuse an unscoped node. Each refusal names the flag that supplies the missing fact. Help for `idea`, `update`, the outstanding ask, and the decide door states which fields hold at which stage.
* The builtin visual paint list covers the desktop app. An `fno-app` UI PR now carries the same look evidence a mux client PR takes. The CI selector answers paint the same way. The gate list also re-reads `crates/fno/src/ui/band_remap.rs` after the module regroup renamed it. The CI list had tracked the rename and the gate list had not.
* Renamed `fno-agents` to `fno-agent`: the crate, the three binaries (`fno-agent`, `fno-agent-daemon`, `fno-agent-worker`), the release tarballs and the `FNO_AGENT_*` env vars. The `fno agents` command group is now `fno agent`. The old names still work for one release. `fno agents` is a hidden alias. The installer and `fno update` link each `fno-agents*` name to its new binary. In each fno binary, a set `FNO_AGENTS_*` var fills its unset `FNO_AGENT_*` name. Shell hooks read only the new names.

### Fixed

* Fixed the merge door's main-repair exemption. A PR whose own node carries the `main-repair` tag merges through a red main again. The hold's graph slice returned a tags-less projection row for the node. The gate refused the PR while naming that same tagged node as the declared repair lane.

## Preview releases (0.0.x)

The public repository restarts the version line at 0.0.x. The private-repository previews below ran 0.1.0 to 0.4.9. Decision D3437 (2026-11-16) postponed the v0.1.0 public launch, so releases continue on v0.0.x stamps of current code, next at v0.0.17.

## [0.0.16] - 2026-11-16

The rc1 code (`befc6642`) stamped 0.0.16, so the Claude Code plugin and fno.sh install a current build before the v0.1.0 launch. ([release notes](https://github.com/fnodotdev/fno/releases/tag/v0.0.16))

## [0.0.15] - 2026-11-14

The v0.0.15 release body carries the v0.4.9 notes. The [0.4.9] entry below describes its content. ([release notes](https://github.com/fnodotdev/fno/releases/tag/v0.0.15))

## Internal previews (0.1.0 to 0.4.9)

Every entry below this heading came before the public launch.

## [0.4.9] - 2026-11-13

135 pull requests since 0.4.8. The headline is fno-app, a desktop app for macOS.

### Added

* Added fno-app, a desktop app for macOS. It is a thin client over the mux. It has a chat view, an Extensions view for plugins, and the orchestration board. The release ships it signed and notarized, and the app follows the macOS appearance.
* Added `fno agents session move`, which carries a live session to another machine.
* Added oh-my-pi (omp) as a harness beside earendil pi, and the claude hook chains on pi.
* Added opencode 2.x support. Serve runs beside 1.x, thread workers run under your grants, and a portal pane shows the live TUI.
* Added multi-stroke key chords in the mux, and Alt-prefix keys like tmux. The footer reads the live keys and names the prefix once.
* Added the HQ launch screen to the mux. It is always on, and the empty state paints the animated F[no] mark.
* Added mod slots. `fno mux graft` realizes a slot, and mods and layouts install by URL from the gallery.
* Added a scrollback cap of 10,000 lines per pane, with pane memory you can measure.
* Added a Visual Log reader owned by the fleet, with Approve and Change buttons on the log.
* Added one chat event type and a ChatTail stream shared by Claude, Codex and opencode.
* Added a command center in the client. Added an exchange picker that lands a pane on the exchange's row. Added a launchd agent that keeps the web bridge up across reboots.
* Added opt-in telemetry: a daily usage report and a PostHog event journal. The always-on install count is retired.
* Added the Homebrew tap fnodotdev/homebrew-fno.
* Added `fno backlog find --tag` and `--related`, with a tag vocabulary and a backfill command.

### Changed

* Changed the release to ship no PyPI wheels and to need no PyPI token. The GitHub Release keeps the tarballs, their sha256 files, `fno.sh` and `fno-release.json`. Old wheel installs keep their path: `fno update` upgrades the frozen wheel, then moves the install to the binary install.
* Changed the mux grid to run on the Rio terminal engine.
* Changed every fno refusal to name fno, a way out, and an override, and gate refusals land a spawn_gate_refused row.
* Changed the merge gates to refuse a red head everywhere. A head with no completed CI run reads unmeasured, never green. A paint PR with no recorded look holds the merge.
* Changed the plugin catalogs to pin the fno entry to the release tag. The stable branch and the nightly plugin are gone.
* Changed the release to sync crate versions after promotion on its own.
* Changed the setup wizard to open at the end of install and on the first bare `fno`.
* Changed a mail hold to carry an end instead of lasting until cleared.

### Fixed

* Fixed `fno update` failing on a slow install. It warns instead, the heal arm reads the registered checkout, and the cargo route installs the sibling crate.
* Fixed `fno` failing in the same session that installed the plugin. Fixed the dev stage registering under a name `/plugin marketplace add` cannot see.
* Fixed daemon wakeups. A missed lead beat now wakes only the idle lead. Burn-watch reads the worktree the node claim names. The boot revival skips a worker a stop command stopped.
* Fixed a worker name held by an idle spare, which blocked reuse of that name.
* Fixed the script gate refusing read-only git lines and misreading commented-out lines. The bug blocked the plan validator and the review emitter in worktree sessions. The gate now refuses a push pair only.
* Fixed headless one-shots lingering in the registry and the mux after they end.
* Fixed mux rendering: full-block cells paint without seams, and inline attach states truecolor over ssh.
* Fixed spawn builders ignoring the positional boundary. A bad bootstrap is named at session start. Dispatch resolves a node's checkout through the work map.

## [0.4.8] - 2026-11-11

The first release from this public repository. 94 pull requests since 0.4.7.

### Added

* Added named sandboxes: one sandbox holds its own fno root, mux, and claude and codex homes.
* Added a setup wizard that asks about you and writes the config keys.
* Added Muse Code as a target for the fno skills install.
* Added `fno do ci tier`, which prints the last CI tier pass.
* Added `fno do ci tier promote <pr>`, which raises the node of a PR one priority step.
* Added a rule book view in the client that groups standing law by section.
* Added a client view that groups your asks by topic.
* Added completion in the composer for `+flag` and `:value`, and a live line that shows what will run.
* Added saved composer sets that you apply by digit 1-9.
* Added a + on each workspace header that opens that workspace's draft card.
* Added revival of claude thread workers that a daemon restart drops.
* Added stuck-session readers in the mux that name the cause and the command that fixes it.

### Changed

* Changed decisions and questions to share one record and one D number.
* Changed backlog output to print node numbers beside the hash.
* Changed spawn to refuse a model that no lane serves.
* Changed the merge gate to judge plan and project fit before it merges.

### Fixed

* Fixed a leaked owner lease killing the daemon or the children of a pane.
* Fixed the libsqlite3-sys CVE by moving to rusqlite 0.41.1.
* Fixed a cancelled CI run taking a passing run's slot.
* Fixed later stops of a finished target re-running the stop gate.

## [0.4.7] - 2026-11-08

About 140 pull requests since 0.4.6.

### Before you upgrade

* 0.4.7 ships only as a binary. On a wheel (pip or uv) install, `fno update` moves you to the binary install in one step. PyPI keeps a bridge wheel so the move keeps working.
* An install older than 0.4.5 updates to 0.4.6 first. Then run `fno update` again. If `fno update` refuses, it names the fix.

### Experimental features

The three experimental features from 0.4.6 stay off by default. The other machines feature adds a Remote hosts tab that adds a host by name.

### Added

* Added zoom, next pane, last pane, last tab and swap pane commands in the mux.
* Added mux rows that show the cargo build state of each session.
* Added spawn across machines. `--machine` pins the relay host, and a spawn with no pin takes the least-loaded host.
* Added a mid-task consult: a worker asks a stronger model one bounded question.
* Added a model change on a live claude session.
* Added a team-wide model spend ceiling with child session meters folded in.
* Added enforcement of the per-account spend cap. Set `cost_cap_usd_per_session` on an `[[accounts.records]]` entry. A target run then ends with a `Budget` termination at the cap. With the key unset, there is no cap.
* Added a boot pass that revives keeper threads after a reboot. It re-seats leads before the fleet sweep can end them.
* Added one list of your asks and the answer each got. A filed question raises the need-input flag of the row that asked.
* Added a push to the Claude app for each filed question. One line holds the question number, the decision and the recommended option. A tap opens the session to answer.
* Added a family graph view in the mux and the backlog web board.
* Added evals tools: a session label table, a judge calibration gate and the go-N fixer.
* Added bounded local failure traces with user profiles you can edit.
* Added signing for macOS. Release binaries are signed and notarized, and `fno update` signs every Mach-O file it places.
* Added a new workspace form with a name, a folder and a Link a folder action.
* Added workspace cards that put pinned cards first, with a group-by-lead toggle and a named sort.
* Added node and PR columns that fill together and show names, not raw ids.
* Added the claude guard chain on agy and pi. One pretool host runs the same guards for both harnesses.
* Added failure recording with a preview of the public GitHub report.
* Added a Messages view that shows same-machine mail from other machines.
* Added an owner lease for the fleet org board. Fleet workers carry an owner tag, and the sweep reaps any tagged orphan.

### Changed

* Changed fno to one Rust binary. The Python runtime is deleted, and `fno update` moves a wheel install to the binary.
* Changed questions and decisions to print and accept the short number only.
* Changed the mux to add a remote host by name from the Remote hosts tab, with one bounded probe.
* Changed `fno backlog idea` to print a prior-work search before it makes a node.
* Changed the repo rename map to ship as defaults. The doctor names plugin registrations that use an old name.
* Changed codex workers to keep fno hooks across a plugin update.

### Fixed

* Fixed placeholder GitHub endpoints that used up the user token. Repo config now resolves first.
* Fixed grant doors that accepted claims without positive human proof.
* Fixed answers to the permission prompt of a thread worker with no TTY.
* Fixed a mail or resume turn that lost the sandbox of the codex thread.
* Fixed session identity. A re-register binds the restarted pid, and one name per session wins.
* Fixed a lead hand-off. The successor now gets the seat charge and the reports.
* Fixed attention sinks that dropped delivery. Dropped questions retry, and a dead sink raises an alarm.
* Fixed the retired opencodev2 wrapper. Plain opencode runs 2.x.
* Fixed the release channel to follow the install receipt, not the current folder.
* Fixed a plugin update on a wheel install. The move to the binary install now starts even with no `~/.fno/install` folder.
* Fixed the whole-graph publish to compute before it locks.
* Fixed portal viewers that drifted off their session ids. Parked prompts now show as Needs input rows.

## [0.4.6] - 2026-11-06

About 300 pull requests since 0.4.5.

### Before you upgrade

* Upgrade to 0.4.6 before 0.4.7. On a wheel (pip or uv) install, `fno update` now moves you to the binary install. 0.4.7 removes the wheel, so this release is the bridge.
* The state folders moved. Durable and mail state now live under `db/`. Bin, migrations and latches fold under install and state. `fno update` moves them for you.

### Experimental features

Three features ship switched off. Each is a toggle in the mux: open the menu, choose settings, then experimental. The choice persists. Turn a feature off the same way.

* Fleet org board. A command center in the sideline shows the fleet and its machines. It also shows the leads on each machine and their seats. From the Fleet tab you can peek, view and spawn on another machine. Turn on `fleet org board`.
* Pane share. Share the focused pane and copy a join link. Press prefix+X in a pane. Press it again to copy the link again. Turn on `pane share`.
* Other machines. The sideline groups sessions by machine. Enter on a row from another machine opens that session in a portal. A remote row looks and acts like a local one. Turn on `other machines`.

### Added

* Added PR review in the mux. Open a PR diff as a split beside the chat pane, and send line comments as one blocking finding.
* Added drag-to-resize for the review split, the file panel and every client split, plus a resize chord.
* Added key presets, including a tmux preset. `keybindings.json` holds the preset and your key overrides.
* Added a Decisions table to audit, retract, overrule and mark rulings reviewed.
* Added shared epics. An epic can publish one GitHub issue, and fno keeps its checklist. A `/claim` comment adds a line to it.
* Added a shared backlog for teammates.
* Added law strength. A narrower, newer law wins, and a rule that ends soon says how to renew.
* Added `fno law renew`, `law set --until`, and a signed user certificate that proves who records a wide law.
* Added `fno rules explain`, which says why each rule applies.
* Added an evals gate. Skill, hook, Rust and CLI changes are gated on golden tasks.
* Added `evals/` folders in skills. Tests can grade a fix: fail to pass, and pass to pass.
* Added `fno do show`, `fno do diff` and `fno do feedback`, which drafts a redacted upstream issue.
* Added spawn guards. New target seats wait while open PRs exceed `agents.max_open_prs`. Build workers refuse under a free-disk floor.
* Added D and Q numbers on every row. Every id door accepts a number.
* Added answer buttons in the web question drawer.
* Added `fno accounts` usage reads from the logins already on the machine.

### Changed

* Changed `fno mail` to a root command, and retired the mail note lane.
* Changed the PR watch tick, the agent sweeps and the last Bash guards to Rust. The Python packages are deleted.
* Changed GitHub reads to use the fnodotdev GitHub App. With the app set up, machines share one API quota.
* Changed `fno do pr push` to run `cargo fmt --check` on changed crates.
* Changed a PR closed unmerged to move its node off `in_review`.
* Changed `fno backlog` to refuse a `details=` replace that halves the stored text.

### Fixed

* Fixed the mux Messages view freezing, and arrow keys in typed-field overlays.
* Fixed auto-merge being disarmed by a timed-out hold read.
* Fixed CI reads to pin to the pushed head with `--expect-head`. A cancelled GitHub-native run no longer reads red.
* Fixed the reaper keeping stopped rows. A dead mux session row now revives in place.
* Fixed Dependabot alerts, and hardened subprocess capture files.

## [0.4.5] - 2026-10-31

397 pull requests since 0.4.4. The accounts commands and the role resolver move from Python to Rust.

### Added

* Added every `fno accounts` command in Rust: list, pick, use, window, usage, combos, benchmarks, test, doctor, register, remove, and reconcile-slot.
* Added chat channels to the attention lane: Telegram, Discord, and BlueBubbles.
* Added the question ladder: an ask climbs from worker to lead to you. A card carries origin, screenshots, and what was tried.
* Added question sharing across machines on one store.
* Added mail signing with per-machine Ed25519 keys, a DeliveryState vocabulary, a sent view, and withdraw for stuck sends.
* Added the merge queue: green PRs land in batches tested once on main.
* Added CI tiers: open PRs run six at a time by priority. When a slot frees, a held push wakes.
* Added the rules engine: shipped, project, and law sources assemble one rule set, and the doors answer through it.
* Added auto-revert: on a lingering red main, the daemon reverts the culprit merge.
* Added Muse Code as a harness.
* Added the You tab, the reply dock, star pinning, and keyboard scroll mode in the mux.
* Added the web /command page and the /trace page.
* Added codex converge: an old fno@footnote install migrates to fno@fno.
* Added a devcontainer headless spawn that asks the plan and runs in the box.
* Added `fno do pr status main`: main's head verdict across every workflow.

### Changed

* Changed `fno do pr push` to fold main by merge, never by rebase.
* Changed the marketplace name to fno, so the install reads fno@fno.
* Changed plan bodies of open nodes to ride the shared primary.
* Changed the bare spawn, role panes, routed accounts, and account panes to run in the Rust client. The Python spawn seam is deleted.

### Fixed

* Fixed the stop gate reading cancelled CI and a merge freeze as waits, not work.
* Fixed spawn handover claims surviving a name collision and binding the right session.
* Fixed path resolution: one registry resolver for every reader, so spaces resolve the same under env, relative, and nested roots.
* Fixed `fno update` installing an older published build.
* Fixed an unreadable optional-review count holding a merge. It warns instead.

## [0.4.4] - 2026-10-23

236 pull requests since 0.4.3. The mux file viewer renders markdown and shows images through the terminal's image protocol.

### Added

* Added markdown, PDF pages, and png, jpeg, gif, and webp images in the mux file viewer.
* Added node rename: `fno backlog update --to <new-id>` renames a node id in place, and retired ids resolve as aliases.
* Added the by-lane scorecard, and a router that proposes lanes from it.
* Added the fleet view: every machine and its sessions, read from any machine.
* Added intel lenses: the decision lens, the scorecard lens, and the corpus export.
* Added done_probes as a requirement: a code plan with none is refused, not warned.
* Added `--model`, `--effort`, `--permission-mode`, `--route`, and `--account` on the Rust spawn arm.
* Added the take-over door: a superior lead takes over a stalled sub-team.
* Added the merge freeze: a pin only you lift.
* Added the footnote profile over ACP stdio.
* Added claude `--bg` sessions as first-class fleet rows, and every claude launch carrying the Footnote theme.
* Added a session joining fno at its first fno skill.
* Added supersede recording a reason and refusing a narrower successor.
* Added the capacity read: one CPU signal, reserve mode, and the spare classifier.
* Added prompt-wait: a worker parked on a permission prompt reaches its spawner and lead.

### Changed

* Changed roles ls, roles show, and the business-role resolver to answer from Rust.
* Changed mail: the reply, wrapped send, and ordinary mail answer at the tool boundary from Rust.
* Changed pr to the native front door: info, transports, and the wait loop. The Python pr package retires.
* Changed review-hold onto the claim verb, and retract and publish-review onto native verbs.
* Changed the graph store to name the real store in every message and take one lock per spelling.
* Changed every pane harness to spawn on the Rust spine.
* Changed worker rows to carry names, not raw ids.

### Fixed

* Fixed a revive forking a held claude session. A fork stays on the parent row.
* Fixed headless one-shots appearing in the mux without `--mux`.
* Fixed cross-host mail signing the proven session, never a neighbor.
* Fixed the visual gate clearing only on approve.
* Fixed every mux session reader healing a dead server's leftover files.
* Fixed ssh panes reaching file open, obsidian open, and clipboard copy.
* Fixed the machine footprint reading physical footprint by group, not summed RSS.
* Fixed attention false alarms: an empty read closes nothing as lost.

## [0.4.3] - 2026-10-18

32 pull requests since 0.4.2. Adds the `mux.open_with` and `mux.viewer` keys and sharing of one masked pane with a viewer on any network.

### Added

* Added pane share: share the focused pane from the mux and admit a viewer with one key. A pane is watchable over ssh, and the sideline lists who watches.
* Added the `mux.open_with` and `mux.viewer` keys. The file viewer opens as a panel or a modal.
* Added the backlog board as a first-class mux surface, with the org board behind an experimental section.
* Added one secret-masking module for pane text. Outside text never publishes by default.
* Added keep marks on the reaper: `--keep`, `--unkeep`, and `--kept`.
* Added the spawn-gate burst verdict: the gate admits one settled spawn past max_live.
* Added field operators on `fno backlog update`, the freshness batch read, and origin-session backfill.
* Added the demand vote: an outside vote ranks a node and moves nothing.
* Added cross-machine lead succession as one transaction.
* Added the blueprint design step: name routes and unknowns instead of settling the design.
* Added investigate-before-code: target settles the plan's unknowns before the first edit.
* Added `fno backlog calibration`: planned against actual.
* Added `fno agents mail send --raw` in Rust, retiring the Python leaf.
* Added refusal of `pr create` while the bound plan still has an open unknown.

### Fixed

* Fixed review posts anchoring to the real commit, not the head.
* Fixed the merge gate refusing a head whose review never posted.
* Fixed an expired merge-slot lease lapsing while its holder still runs.

## [0.4.2] - 2026-10-17

102 pull requests since 0.4.1. Mux panes re-emit desktop notifications. More commands move to Rust, including claim status and the mail hold commands.

### Added

* Added desktop notifications from mux panes: the wire carries notification shapes, and OSC notices forward to your client. `fno mux notify` sends one from a script.
* Added the setup wizard in Rust: services, projects, verify, dry-run, export, and read-only machine detection.
* Added the spawn gate rules: leads take no slot, and the cap yields to CPU and RAM. A grant lifts it for a window.
* Added the org view: graph, table, and tree modes of the fleet on one pane, opened with prefix+G.
* Added named mux workspaces: a workspace takes its creator's folder, and the sideline header shows each folder.
* Added alt-click link opening in mouse-reporting panes.
* Added native answers for thirteen backlog commands: intake, decompose, batch, pick, discover, requeue, task grain, and epic status. The wheel bodies retire.
* Added the review root in Rust: resolve-level, the finding classifier, and one attestation writer.
* Added the merge verb and pr-watch in Rust: heal, the ledger join, cleanup, and the post-merge ritual.
* Added `--as` for spawn lanes and pi headless one-shots.
* Added the target steps mode, plus `fno do target init` and `fno do target start` in Rust.

### Fixed

* Fixed workspace folders: a named workspace binds its folder, and a new tab never opens in TMPDIR.
* Fixed a click on a live claude row attaching without a plan.
* Fixed memory pressure judging kernel pressure, not swap used.
* Fixed GitHub rate limits reading as a wait, not a failure.
* Fixed `fno mux pane ls` reporting the pane's own cwd.
* Fixed pr-heal capping conflicting refreshes at one per PR per day, and receipting no-op refreshes honestly.

## [0.4.1] - 2026-10-13

616 pull requests since 0.4.0. Two machines can share one backlog. The live cap learns from machine load. The old role names became leads and teams. Agent mail opens with a one-line header.

### Added

* Added the shared store: dispatch claims, the backlog, and cross-host mail run on one libSQL primary. An ssh relay is the fallback.
* Added moving a session to another machine over a pairing code or a file.
* Added the learned spawn cap (AIMD under your ceiling), the machine runaway brake, and the agent-spawn admission gate.
* Added leads and teams as the role words across views, the Rundown page, and check-ins.
* Added the one-line delivered mail header, `fno agents mail show`, `send --subject`, and a JSON receipt on every lane.
* Added the chats store behind the Messages tab: one chat per pair, bubbles, and tap-to-reply.
* Added the notifications bell panel and theme import from files or GitHub.
* Added the backlog note thread: notes append comments, and the boards read the thread.
* Added the backlog search grammar: one parser answers the list, the TUI board, and the web board.
* Added SQL tables for claims and the registry, and one Rust judge for every event, with retention.
* Added the burn watch: on 20 minutes with no edit, the lead gets mail.
* Added the machine runaway brake and the agent-spawn admission gate.
* Added cargo build slots with load-scaled compile slots and opt-in sccache.
* Added the visual-approval gate: a paint PR holds the merge until you look at the shots.
* Added `--until red` on `fno do pr wait`: a red verdict cancels the head's runs and wakes the wait.
* Added duplicate-PR refusal at create, and refusal of net-new tests past the test-delta cap.
* Added draft-PR refusal behind `pr.open_ready`.
* Added updates from CI-built binaries, with a lifecycle journal and `--status`.
* Added the footnote (fnh) and zcode harnesses.
* Added `fno doctor stalls`: one weekly cause table over the event journals.
* Added the composer `!` shell mode, ctrl+b chords, and splits in four directions.
* Added hooks that refuse hand edits of `config.toml`, refuse EnterWorktree from a subagent, and route SendMessage to fno mail.

### Changed

* Changed the state root to one layout. Journals live under history/, logs under logs/, pages under pages/, runtime state under state/, and the graph under db/.
* Changed the backlog verbs (advance, triage, maintain, groom, publish) to answer natively.
* Changed registry rows to mint one fno_id each, addressable at tier 1.

### Fixed

* Fixed a busy server ending a human attach after ten seconds.
* Fixed question holds: a hold blocks once, ends on the second fire, and names the remedy.
* Fixed pr-wait reading a red made only of cancelled checks as pending.
* Fixed merges holding while main's CI is red.

## [0.4.0] - 2026-09-28

1,962 pull requests since 0.3.1. fno runs a fleet of agent workers across several harnesses, with its own terminal multiplexer to watch them.

### Added

* Added the fleet daemon: settle, burn, machine, arms, notify, board, and daily sweep. Fleet revival after a reboot joins it, with the fleet incident breaker.
* Added the lead system: promotion, succession, term limits, the term ledger, and check-in beats. A fleet readout shows every live lead.
* Added harnesses: cursor-agent, grok, kimi, DeepSeek (dsh), and pi, over one capability table with ACP stdio drivers.
* Added the pane keeper: every pane's pty master lives in a keeper that outlives the mux server.
* Added portals: a thread pane becomes an addressable set of seats that persist and restore.
* Added the composer: chips, type-to-filter pickers, an @ node picker, and thread as the default spawn lane.
* Added the backlog store on SQLite: graph.db becomes the only store, with graph.json written on demand.
* Added native claims in graph.db with liveness verdicts, a FIFO wait queue, and task-grain claims.
* Added the event store: one SQLite store beside every durable journal, with judged envelopes and retention.
* Added decisions and law: `fno decide`, the repository law catalog, and operator-only writes.
* Added the attention system: question pages, the mux answer door, and ntfy and webhook sinks.
* Added the operator turn queue: `fno inbox user`, with machine-shaped turns refused.
* Added the intel fold over every session, with `fno doctor intel` and the /fno:intel skill.
* Added `fno do pr heal`: reads a red PR, fixes what it can, pushes once.
* Added `fno doctor footprint` and `fno doctor reclaim`.
* Added the gh budget: one machine-wide GitHub request ledger.
* Added one authorized merge operation with a merge slot for held PRs.
* Added review coverage: the review.posture ladder, diff-pinned attestations, and self-review for code payloads.
* Added territories: one owner per scope and team caps in both spawn gates.
* Added `fno backlog board`, `fno backlog join`, encounter votes, and the demand read.
* Added `fno backlog reopen` and `fno backlog unarchive`.
* Added carveouts: the in-place edit, a severity field, and a close that refuses unharvested rows.
* Added the promise gate: closure refuses a plan that ships less than it promised.
* Added execute waves: ready-task work stealing and task claims through the verb.
* Added blueprint gates: the Consolidation Gate, the Code Index Audit, and surface blocks.
* Added `fno growth-launch` and the four growth-studio roles.
* Added worktree salvage refs and the stranded classifier.
### Changed

* Changed the CLI root to eleven groups: agents, backlog, config, doctor, do, inbox, mail, test, update, whoami, workspace.
* Changed the graph reads and writes to ride the store keeper. Every mutation names itself.
* Changed the Rust mux to own `fno`. The Python console script becomes `fno-py`.
* Changed every spawn to cross one seam with a validated origin and a separate owner.
* Changed the bg substrate name to thread.
* Changed `fno pr status` to the Rust door with a coalescing cache. `fno do pr wait` is now the one sanctioned watcher.
* Changed PR bodies to claim nodes with a Fixes line.

### Fixed

* Fixed feature-worktree builds opening live stores. The fence refuses them.
* Fixed the reaper asking the session, not only the node, before a retire.
* Fixed merge holds reading file overlap, not base distance.
* Fixed stop and rm releasing the stopped session's claims.

## [0.3.1] - 2026-08-04

268 pull requests since 0.3.0. Adds mux workspaces, a stated guarantee model for mail, and accounts that survive a quota limit.

### Added

* Added the mail delivery ladder with a stated guarantee: live injection first and the durable bus as fallback. Receipts say what actually landed.
* Added mail holds on the wall clock: DND with an end time, and a rolling pair budget. Sends queue behind a hold.
* Added managed account slots: a credential registers only after it proves itself, and quota locks carry their reset time.
* Added `fno providers pick` for the account with headroom, and `fno providers doctor`.
* Added mux workspaces and the layout engine: templates, drag to move panes, and seam resize.
* Added the sideline overhaul: density states, the header band, the Backlog section, and tab names that persist.
* Added epics and missions: rollups, contained nodes that never dispatch, and mission drain loops.
* Added done_probes: the loop gates a done promise on the plan's declared probes.
* Added `/target beastmode` for walk-away authority.
* Added the leader-pass skill: one-day delegation that grew into the full orchestrator brief.
* Added the sigma review panel with cite-or-drop evidence and head-pinned attestations.
* Added think receipts: deterministic discovery, sources stamped in frontmatter, and closure classification.
* Added filing-time dedup: similar-node scoring behind the consolidation offer.
* Added claim hardening: machine id on claims, the incarnation fence, and live-tick claim refresh.
* Added `fno pr logs`, the agent-facing CI reader.
* Added the daily groom and the reconcile-findings close for phantom nodes.
* Added worktree overlap detection and the overlaps report.
* Added `fno whoami` answering what model the session actually runs.
* Added the wake ladder that revives an exited session in place.
* Added the three-axis spawn surface: `--harness` is canonical, and spawn takes the prompt.

### Changed

* Changed mux squads to workspaces everywhere.
* Changed the abilities/abi branding to fno across binaries and docs. <!-- fno-rename-keep: historical entry -->
* Changed plan statuses: `shipped` and `archived` retire as values.
* Changed `--provider` to name the model vendor, not the harness.
* Changed `fno pr merge` to close its own node on merge.

### Fixed

* Fixed retired mail aliases migrating and delivering instead of refusing.
* Fixed dir-mutex locks: identify by inode and mtime, and verify before a steal.
* Fixed the ledger writing one cost row per session instead of one per alias.
* Fixed graph deriving node status from the rung table, not plan-path presence.

## [0.3.0] - 2026-07-17

About 417 pull requests since 0.2.1. Republishes the package so the bootstrap install works again. Adds the native terminal multiplexer and the agent worker mesh.

### Added

* Added the mux: panes, tabs, splits, the sideline, leader keys, scrollback search, block capture, and a read-only web bridge.
* Added the agent mesh: `fno agents spawn`, peek, attach, resume, wait, top, and logs, over pane, thread, and headless lanes.
* Added agent mail: the `<fno_mail>` envelope, live injection first, and the durable bus as the fallback.
* Added codex and opencode as spawnable, readable harnesses, and agy panes with its Stop-hook adapter.
* Added the dispatch resolver with per-harness command templates and failover on exhaustion.
* Added provider rotation: managed accounts, quota-aware combos, usage probes, and failover on a quota limit.
* Added parallel lanes with a shared lane cap, and the batch lane that ships one PR per batch.
* Added single-doc plans: folder plans retire, and plan and graph states reconcile both ways.
* Added `fno worktree ensure`, `fno worktree cleanup --merged`, and the per-project isolation policy.
* Added `fno do target start`, bg dispatch, and the boundary-reconcile step.
* Added `fno pr status`, the stale-base guard, the post-merge ritual, and the pr-watch daemon.
* Added `fno scoreboard`: plan fidelity, per-skill and per-provider outcomes.
* Added flat `config.toml` with a one-shot migration from settings.yaml.
* Added epics that close on their last landed child, causal links, and archive read-through.
* Added review peer lanes with the adversarial sub-mode and reviewer attestations.
* Added the keep-going engine: postmortem drain, gate-escape ranking, and retro harvest.
* Added the digest: the while-you-were-gone fold over a node or worktree.
* Added webhook status sinks with per-sink isolation.
* Added the `<watching>` idle: an armed watcher lets a loop wait instead of spinning.
* Added `fno test`, which runs pytest pinned to the worktree.

### Changed

* Changed `fno update` to verify the triad same-build and install the mux front door.
* Changed graph done to mean merged: nodes close on merge evidence.
* Changed the spawn harness default to the invoking harness.

### Fixed

* Fixed the bootstrap republish so a fresh install works again.
* Fixed batch state rooting at the canonical repo, so the batch lane works across worktrees.

## [0.2.1] - 2026-06-24

### Fixed

* Fixed fno creating a `.fno` folder in projects that do not use fno.

## [0.2.0] - 2026-06-24

Adds the backlog dispatcher, cross-project spawn and per-spawn model routing.

### Added

* Added cross-project spawn: `-P/--project` dispatches work to another project, and a session works only in its own project.
* Added role-based model routing per spawn, with the z.ai lane and its config-driven endpoint.
* Added the active-backlog drain: a resident supervisor with wake scheduling, nudges, and a circuit breaker.
* Added `fno research`, the scout executor, and a guard against SSRF on self-fetch.
* Added the `/ship` umbrella: PR and doc deliverables under one verb.
* Added `fno setup wizard`, `fno setup cli-hooks`, and the first-run nudge toward setup.
* Added multi-key atomic `fno config set a.b=1 c.d=2`, `fno config unset`, and block values from JSON or YAML.
* Added born-with-why: filing a node offers a grounded `/think` pass.
* Added tournament triage: pairwise Copeland ordering for the backlog.
* Added node provenance: parent edges stamped at birth and read back by `fno backlog provenance`.
* Added the structured, fail-closed return contract for subagent results.
* Added the Homebrew formula and the Intel-Mac install path.

### Fixed

* Fixed setup exiting cleanly on Ctrl-C and recovering a stale partial skills clone.
* Fixed `fno config` validating a multi-key batch against the final block state.

## [0.1.0] - 2026-06-18

The first release. The fno CLI, the backlog graph, the target pipeline skills, and the Claude Code plugin.
