# Changelog

Notable changes to fno, newest first. Each release also has a page on [GitHub releases](https://github.com/fnodotdev/fno/releases). Run `fno --version` to check your installed version. Run `fno update` to upgrade.

Releases before 0.4.8 were built in a private repository. Their entries here summarize the release notes from that time. Pull request numbers before 0.4.8 point at that repository, so this file leaves them out.

## [0.4.8] - 2026-11-11

The first release from this public repository. 94 pull requests since 0.4.7.

### Added

* Added named sandboxes: one sandbox holds its own fno root, mux, and claude and codex homes
* Added a setup wizard that asks about you and writes the config keys
* Added Muse Code as a target for the fno skills install
* Added `fno do ci tier`, which prints the last CI tier pass, and `fno do ci tier promote <pr>`, which raises the PR's node one priority step
* Added a rule book view in the client that groups standing law by section, and a view that groups your asks by topic
* Added completion in the composer for `+flag` and `:value`, a live line that shows what will run, and saved sets you apply by digit 1-9
* Added a + on each workspace header that opens that workspace's draft card
* Added revival of claude thread workers that a daemon restart drops
* Added stuck-session readers in the mux that name the cause and the command that fixes it

### Changed

* Changed decisions and questions to share one record and one D number
* Changed backlog output to print node numbers beside the hash
* Changed spawn to refuse a model that no lane serves
* Changed the merge gate to judge plan and project fit before it merges

### Fixed

* Fixed a leaked owner lease killing the daemon or the children of a pane
* Fixed the libsqlite3-sys CVE by moving to rusqlite 0.41.1
* Fixed a cancelled CI run taking a passing run's slot
* Fixed later stops of a finished target re-running the stop gate

## [0.4.7] - 2026-11-08

About 140 pull requests since 0.4.6.

### Before you upgrade

* 0.4.7 ships only as a binary. On a wheel (pip or uv) install, `fno update` moves you to the binary install in one step; PyPI keeps a bridge wheel so the move keeps working. Installs older than 0.4.5 update to 0.4.6 first, then run `fno update` again; `fno update` names the fix when it refuses

### Experimental features

Three features ship switched off. Each is a toggle in the mux: open the menu, choose settings, then experimental. The choice persists. Turn a feature off the same way.

* Fleet org board. A command center in the sideline with the fleet, its machines, the leads on each machine and each lead's seats. From the Fleet tab you can peek, view and spawn on another machine. Turn on `fleet org board`
* Pane share. Share the focused pane and copy a join link. Press prefix+X in a pane; press it again to copy the link again. Turn on `pane share`
* Other machines. The sideline groups sessions by machine, and Enter on another machine's row opens that session in a portal. A remote row looks and acts like a local one, and a new Remote hosts tab adds a host by name. Turn on `other machines`

### Added

* Added zoom, next pane, last pane, last tab and swap pane commands in the mux, and rows that show each session's cargo build state
* Added spawn across machines: `--machine` pins the relay host, and unpinned spawns take the least-loaded host
* Added a mid-task consult: a worker asks a stronger model one bounded question, and a claude row can re-route onto a new model on the same session
* Added a team-wide model spend ceiling with child session meters folded in
* Added enforcement of the per-account spend cap: `cost_cap_usd_per_session` on an `[[accounts.records]]` entry ends a target run with a `Budget` termination when the session meter crosses the cap. The key parsed before but reached no reader; unset still means no cap
* Added a boot pass that revives keeper threads after a reboot, and re-seats leads before the fleet sweep can end them
* Added one list of the user's asks and the answer each got, and a filed question raises the asking row's need-input flag
* Added a filed question pushing one notification to the Claude app: the question number, the decision and the recommended option in one line, and the tap opens the session to answer
* Added a family graph view in the mux and the backlog web board
* Added evals machinery: a session label table, a judge calibration gate, the go-N fixer, and bounded local failure traces with editable user profiles
* Added signing for macOS: release binaries are signed and notarized, and `fno update` signs every Mach-O it places
* Added a new workspace form (name and folder, plus Link a folder), flat pinned-first cards with a group-by-lead toggle and a named sort, and node and PR columns that fill together with names over raw ids
* Added the claude guard chain on agy and pi: one pretool host runs the same guards for both harnesses
* Added failure recording with a preview of the public GitHub report
* Added a Messages view that shows same-machine mail from other machines
* Added the fleet org board's owner lease: fleet workers carry an owner tag, and the sweep reaps any tagged orphan

### Changed

* Changed fno to one Rust binary: the Python runtime is deleted, and wheel installs move to the binary through `fno update`
* Changed questions and decisions to print and accept the short number only
* Changed the mux to add a remote host by name from a Remote hosts tab with a + host footer button and one bounded probe
* Changed `fno backlog idea` to print a prior-work search before it mints a node
* Changed the repo rename map to ship as defaults, with the doctor naming old-name plugin registrations
* Changed codex workers to keep fno hooks across a plugin update

### Fixed

* Fixed placeholder GitHub endpoints burning the user token; repo config resolves first
* Fixed grant doors accepting claims without positive human proof
* Fixed answering a thread worker's permission prompt without a TTY
* Fixed a mail or resume turn losing the codex thread's sandbox
* Fixed session identity: a re-register binds the restarted pid, one name per session wins, and a lead hand-off moves seat charge and reports to the successor
* Fixed attention sinks dropping delivery, with dropped questions retried and a dead sink alarming
* Fixed the retired opencodev2 wrapper; plain opencode runs 2.x
* Fixed the release channel to follow the install receipt instead of the cwd
* Fixed a plugin update on a wheel install: the move to the binary install now starts even when `~/.fno/install` does not exist yet
* Fixed the whole-graph publish to compute before it locks
* Fixed portal viewers drifting off their session ids, and parked prompts now read as Needs input rows


## [0.4.6] - 2026-11-06

About 300 pull requests since 0.4.5.

### Before you upgrade

* Upgrade to 0.4.6 before 0.4.7. On a wheel (pip or uv) install, `fno update` now moves you to the binary install. 0.4.7 removes the wheel, so this release is the bridge
* The state folders moved: durable and mail state now live under `db/`, and bin, migrations and latches fold under install and state. `fno update` moves them for you

### Experimental features

Three features ship switched off. Each is a toggle in the mux: open the menu, choose settings, then experimental. The choice persists. Turn a feature off the same way.

* Fleet org board. A command center in the sideline with the fleet, its machines, the leads on each machine and each lead's seats. From the Fleet tab you can peek, view and spawn on another machine. Turn on `fleet org board`
* Pane share. Share the focused pane and copy a join link. Press prefix+X in a pane; press it again to copy the link again. Turn on `pane share`
* Other machines. The sideline groups sessions by machine, and Enter on another machine's row opens that session in a portal. A remote row looks and acts like a local one. Turn on `other machines`

### Added

* Added PR review in the mux: open a PR diff as a split beside the chat pane and send line comments as one blocking finding
* Added drag-to-resize for the review split, the file panel and every client split, plus a resize chord
* Added key presets, including a tmux preset. `keybindings.json` holds the preset and your key overrides
* Added a Decisions table to audit, retract, overrule and mark rulings reviewed
* Added shared epics: an epic can publish one GitHub issue whose checklist fno keeps, and a `/claim` comment adds a line to it
* Added a shared backlog for teammates
* Added law strength: a narrower, newer law wins, and a rule that ends soon says how to renew. Added `fno law renew`, `law set --until`, and a signed user certificate that proves who records a wide law
* Added `fno rules explain`, which says why each rule applies
* Added an evals gate: skill, hook, Rust and CLI changes are gated on golden tasks. Skills can carry their own `evals/`, and a fix can be graded by its tests (fail to pass and pass to pass)
* Added `fno do show`, `fno do diff` and `fno do feedback`, which drafts a redacted upstream issue
* Added spawn guards: new target seats wait while open PRs exceed `agents.max_open_prs`, and build workers refuse under a free-disk floor
* Added D and Q numbers on every row. Every id door accepts a number
* Added answer buttons in the web question drawer
* Added `fno accounts` usage reads from the logins already on the machine

### Changed

* Changed `fno mail` to a root command, and retired the mail note lane
* Changed the PR watch tick, the agent sweeps and the last Bash guards to Rust. The Python packages are deleted
* Changed GitHub reads to go through the fnodotdev GitHub App when it is set up, so machines share one API quota
* Changed `fno do pr push` to run `cargo fmt --check` on changed crates
* Changed a PR closed unmerged to move its node off `in_review`
* Changed `fno backlog` to refuse a `details=` replace that halves the stored text

### Fixed

* Fixed the mux Messages view freezing, and arrow keys in typed-field overlays
* Fixed auto-merge being disarmed by a timed-out hold read
* Fixed CI reads to pin to the pushed head with `--expect-head`, and a cancelled GitHub-native run no longer reads red
* Fixed the reaper keeping stopped rows, and a dead mux session row now revives in place
* Fixed Dependabot alerts, and hardened subprocess capture files


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
