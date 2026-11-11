# Changelog

Notable changes to fno, newest first. From 0.4.8 on, each release also has a page on [GitHub releases](https://github.com/fnodotdev/fno/releases). Run `fno --version` to check your installed version. Run `fno update` to upgrade.

Releases before 0.4.8 were built in a private repository. Their entries here summarize the release notes from that time. The pull request numbers in the notes up to 0.4.8 point at that repository, so this file leaves them out.

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

## [0.4.4] - 2026-10-23

236 pull requests since 0.4.3. The mux file viewer renders markdown and shows images through the terminal's image protocol.

## [0.4.3] - 2026-10-18

32 pull requests since 0.4.2. Adds the `mux.open_with` and `mux.viewer` keys and sharing of one masked pane with a viewer on any network.

## [0.4.2] - 2026-10-17

102 pull requests since 0.4.1. Mux panes re-emit desktop notifications. More commands move to Rust, including claim status and the mail hold commands.

## [0.4.1] - 2026-10-13

616 pull requests since 0.4.0. Two machines can share one backlog. The live cap learns from machine load. Kings and crowns became leads and teams. Agent mail opens with a one-line header.

## [0.4.0] - 2026-09-28

1,962 pull requests since 0.3.1. fno runs a fleet of agent workers across several harnesses, with its own terminal multiplexer to watch them.

## [0.3.1] - 2026-08-04

268 pull requests since 0.3.0. Adds mux workspaces, a stated guarantee model for mail, and accounts that survive a quota limit.

## [0.3.0] - 2026-07-17

About 417 pull requests since 0.2.1. Republishes the package so the bootstrap install works again. Adds the native terminal multiplexer and the agent worker mesh.

## [0.2.1] - 2026-06-24

Stops creating `.fno` in folders that do not use fno.

## [0.2.0] - 2026-06-24

Adds the backlog dispatcher, cross-project spawn and per-spawn model routing.

## [0.1.0] - 2026-06-18

The first release.
