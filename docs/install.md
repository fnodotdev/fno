# Install fno

## Is this page for you?

You decided to try fno. This page installs it on your harness, proves the install worked, and sets up your first project. When `fno --version` prints a version and `/fno:` autocompletes, go to the [quickstart](quickstart.md).

Not for: a first PR. That is the [quickstart](quickstart.md).

## Install

### Claude Code

In any Claude Code session:

```
/plugin marketplace add fnodotdev/fno
/plugin install fno@fno
```

The catalog pins the fno entry to the newest release tag with its commit sha, and the release job moves that pin. Turn on auto-update so releases reach you: in `/plugin`, open **Marketplaces**, select **fno**, and choose **Enable auto-update** (it is off by default). Without it, run `/plugin marketplace update fno` after a release.

The add clones the repository, so a slow connection can hit the 120-second "Add marketplace" timeout. Retrying usually lands it.

The add can refuse with `Cannot add marketplace "fno": its source doesn't match its extraKnownMarketplaces entry`. That means an older dev install registered your local stage under the name `fno`. Run `fno doctor update` or `fno config plugin install claude` once. Each moves the stage to its own name, `fno-dev`. Then the add works.

The previous stage path, `~/.fno/plugin-stage`, remains readable during migration. `fno doctor update` rebuilds the new stage, repoints configured harnesses, and keeps the old stage until earlier sessions exit and no config names that old path.

`fno config plugin install claude` is the dev channel: it installs your local checkout for development. It registers the stage at `~/.fno/install/plugin-stage/fno` as the marketplace `fno-dev`, and installs `fno@fno-dev`. The public `fno` name stays free for the GitHub catalog, so both installs can sit side by side. Only one copy loads. If `fno@fno` is installed, the dev install turns it off. Its receipt names the command that turns it back on. For Codex, both channels share the one id `fno@fno`, so a Codex dev install refuses with exit 3 over a release install. Pass `--dev` to switch.

An install made before the marketplace rename reads `fno@footnote`. It keeps working, and `fno doctor` reads it and names the move. To move a GitHub install to `fno@fno`, run `/plugin uninstall fno@footnote` and `/plugin marketplace remove footnote`, then the two commands above. The dev install, `fno config plugin install claude`, moves a local `footnote` stage by itself. For Codex, `fno config setup run --only harness-wiring` moves a release install and `fno config plugin install codex` moves a dev install.

On the first session after install, footnote prints the install plan and starts nothing: a person's yes is what runs it. Install the plugin mid-session and the same notice arrives on the session's next prompt. On a yes, run `bash <plugin root>/.claude-plugin/postinstall.sh`. Its output ends with `installer exit 0` on success. The installer edits no shell profile. The installing session can use `fno` right away: call it as `~/.local/bin/fno`. When your PATH already carries `~/.local/bin`, bare `fno` works too. No new session is needed. The `curl -fsSL fno.sh | sh` route is the one route that edits a shell profile. There, new terminals get bare `fno`. When your PATH lacked `~/.local/bin`, the current shell needs the export the installer output prints.

### Codex, opencode, agy and other harnesses

Codex CLI:

```
codex plugin marketplace add fnodotdev/fno
codex plugin add fno@fno
```

Antigravity (`agy`): run `bash scripts/install/agy-plugin.sh` from a clone. It installs a staged payload and checks that `agy plugin list` names Footnote. For a slow install, set `AGY_PLUGIN_INSTALL_TIMEOUT_S=120`.

Every other harness has its own page under [harnesses/](harnesses/). [HARNESSES.md](HARNESSES.md) says what works on each one.

### The CLI alone

`curl -fsSL fno.sh | sh` installs the release tarball into `~/.fno/install/bin`. The tarball holds the four Rust binaries: the `fno` front door and the three `fno-agents` binaries. At the end of an install from a real terminal, the setup wizard opens. The installer hands it the terminal through `/dev/tty`, because the script's own stdin is the install pipe. Ctrl-C skips the wizard, and the install still succeeds. The next bare `fno` then offers the wizard again. An agent install (`sh -s -- --yes`, or `FNO_YES`/`FNO_NO_WIRE` set) and CI keep the non-interactive setup. A wizard that already finished or was skipped never runs twice. Its marker at `~/.fno/sidecar/wizard-done` proves it. The first bare `fno` after a Homebrew or `--yes` install is where a person answers the questions. After that, bare `fno` opens the mux straight away. On macOS, `brew install fnodotdev/fno/fno` installs the same tarball. The formula refuses Linux. The PyPI wheel (`uv tool install fno` or `pip install fno`) is a bridge for old installs. It is frozen at 0.4.8: PyPI no longer takes new releases. An update moves the wheel install to the tarball install. `cargo install fno fno-agents` is the source route. It builds the front door and the three `fno-agents` binaries into the cargo bin dir with your Rust toolchain.

### Local clone

Clone, run setup, get the CLI:

```bash
git clone https://github.com/fnodotdev/fno.git footnote
cd footnote
bash scripts/setup.sh
```

The setup checks the prerequisites (`bash`, `git`, `gh` and `jq`). It registers the global `~/.fno` state dir and scaffolds `.fno/config.toml` in the clone. When no `fno` answers, it installs the CLI through the same `fno.sh` the curl one-liner uses. Afterwards `fno`, `fno-agents`, and the mux all answer. Per-path overrides live in the `config.paths.*` keys: see [path configuration](path-config.md).

### Windows (WSL2)

fno runs under [WSL2](https://learn.microsoft.com/windows/wsl/install), not native Windows. The loop leans on POSIX file locking, Unix sockets, and signals that Windows handles differently. WSL2 is real Linux, so everything here works unchanged inside it.

One-time setup, from PowerShell as Administrator:

```powershell
wsl --install        # installs WSL2 + Ubuntu; reboot if prompted
```

Then open the Ubuntu shell and work from there. Install `gh` and `jq`. Run Claude Code inside WSL2, and follow the install steps above. Keep your repos on the Linux filesystem (under `~/`), not `/mnt/c/...`. On the Windows mount, file locking and file watches are slow and unreliable.

## Verify it worked

```bash
fno --version          # prints a version
```

If `fno` is "command not found", your install predates the complete payload or is a source/editable dev build (those are development artifacts and carry no binaries). Every supported channel ships the Rust **`fno` front door**: the plugin's first-session installer, `fno.sh`, and Homebrew. Repair: run `curl -fsSL fno.sh | sh`, or install the front door directly with `cargo install fno` (needs a **Rust toolchain**, `rustup`). When the front door is missing, a Claude Code session also reminds you. To see what is missing, run the zero-binary doctor from the installed plugin tree: `bash ~/.claude/plugins/cache/footnote/fno/*/scripts/install/bootstrap.sh`. It prints every prerequisite's state and exact fix as JSON. `--repair` runs the plugin's own installer and never edits shell rc files. `--from-source <checkout>` builds and installs from a clone.

Inside Claude Code, type `/fno:` and skill autocomplete appears (`target`, `think`, `blueprint`, ...).

```bash
claude /status         # shows your Claude account; if not, run: claude login
gh auth status         # authenticated; if not, run: gh auth login
```

`gh auth status` is the most common cold-start failure. Credentials for each provider are in [the auth guide](auth.md).

## Configure your project

Configuration lives in `.fno/config.toml` (project-local) layered over `~/.fno/config.toml` (global). The global file holds shared defaults. The project file holds only the per-repo deltas. There are two ways to set it up.

**In a Claude Code session (agent-driven):**

```
/fno:setup
```

**In the terminal, no agent (CLI-native):**

```bash
fno config setup run --yes         # takes every recommended default, never prompts, prints the JSON report
fno config setup run --check       # names each missing piece (agent CLI, gh login, jq, git, fno-agents) and config drift, each with its fix
fno config setup run --list --json # the step table (global, project, contributor layers)
fno config setup plan --advanced   # the advanced config tier, as JSON
```

Both walk the same schema-derived question plan and write through the validated config writer, so a typo or an out-of-range value is rejected, not silently stored.

To wire fno's SessionStart context into Codex and Gemini user config, run `fno config setup cli-hooks`. The wizard does not do this step.

### Reading and editing config directly

```bash
fno config get config.review.github_apps            # read one value
fno config set config.auto_merge.enabled true       # set one key (atomic, schema-checked)
fno config set a.b=1 c.d=2                           # set several keys in one atomic call
fno config unset config.auto_merge.enabled          # remove a key (reverts to its default)
fno config doctor                                    # what resolved, and any suspicious values
```

`fno config set` also takes a whole block as JSON: `fno config set config.review '{"github_apps":["chatgpt-codex-connector"]}'`.

### The settings you'll touch first

These are real keys in `config.toml`. Run `fno config get <key>` to read any of them, or `fno config schema --markdown` for the complete reference:

| Key | What it does | Default |
|-----|--------------|---------|
| `config.review.github_apps` | External review bots that must approve before `target` calls a PR done; none set means no external gate | none |
| `config.review.posture` | How much review a code PR needs before it can merge; unset floors at `self_review` (nine-rung ladder) | none |
| `config.review.max_rounds` | Review rounds per PR, counted across its whole life | `2` |
| `config.review.external_reviewers` | Which reviewer(s) `pr check` waits on (e.g. `gemini`, `codex`) | `[]` |
| `config.auto_merge.enabled` | Let `target` merge a PR itself once review passes | `false` |
| `config.target.defaults.max_iterations` | How many times `target` retries before stopping | `40` |
| `config.backlog.id_prefix` | The prefix for minted backlog node ids (e.g. `fno-a3f9`); unset falls back to `ab-` | none |
| `config.obsidian.enabled` + `.vault` | Store plans and design docs in an Obsidian vault | `false` |
| `config.project.vision` | One line: what this codebase is and why (project-scoped) | none |

Budget and skip behavior are not config keys. They are flags you pass to a run, for example `/fno:target --budget 25 "..."` or `/fno:target --no-external "..."`. See [the target guide](guides/target.md) for the full flag list.

## Set up a second machine

A second machine does not re-answer every question. Export the setup from a machine that already works, copy the profile over, and join it on the new one.

On the set-up machine:

```bash
fno config setup run --export --out fno-profile.toml
```

On the new machine, pick the join that fits who is present:

| Who runs it | Command |
|---|---|
| A person, in a terminal | `fno do wizard --join fno-profile.toml` |
| A person, in a session | `/fno:setup join fno-profile.toml` |
| A script, no person | `fno config setup run --yes --only join --join fno-profile.toml` |

The join prints the diff first, then applies. The previous global config is backed up to `~/.fno/sidecar/setup-backup/`. Later, `fno config setup run --check --json` shows how the machine has drifted, each row with its fix.

What travels is decided per key, once, by `classify` in `crates/fno/src/setup/setup_profile.rs`:

| Class | Decided by | On a join |
|---|---|---|
| Secret | the leaf name carries `token`, `secret` or `password`, or ends `api_key` | never exported |
| Local | machine paths and names: the `paths.`, `work.`, `user.`, `build.`, `accounts.`, `agents.provider_limits.`, `store.` families; the exact keys `state_dir`, `plans_dir`, `obsidian.vault`, `research.output_dir`, `lead.enabled`, `agents.max_live`, `agents.max_live_per_territory`; or any string value starting `/` or `~` | stays behind, listed in the diff |
| Portable | everything else | travels |

The machine-local rows still need answering here. The wizard handles the big one: tick its projects screen and it appends this checkout to `work.workspaces.main.projects` through the validated writer.

When a person runs the wizard, a preset sets the pace. The preset screen opens with `balanced` highlighted. `--preset aggressive|balanced|user-reviewed` pre-answers it.

| Preset | Decides | Harnesses | Models | Services | Auto-merge |
|---|---|---|---|---|---|
| aggressive | everything with safe answers | every detected one | the first catalog lane per harness | all three | on after review and CI |
| balanced | the safe middle | the detected ones with credentials | each harness keeps its current model | the PR watcher and the groom | off |
| user-reviewed | nothing without you | none ticked | each harness keeps its current model | none | off |

## Keeping fno on the machine: codesign and updates

On macOS, `fno update` signs each binary it places with the Developer ID identity in your keychain. The identifiers are fixed (dev.fno.fno, dev.fno.fno-agents, and so on), so your macOS permission grants survive every update. Login Items then stops reporting fno as an item from an unidentified developer. Without a Developer ID certificate, fno skips the signing and says so. The real identity checks are `codesign --verify --strict` and the Authority chain in `codesign -dv`. An `spctl -a` rejection on a bare CLI binary comments on the file shape, never on the identity. The Python `fno` console script cannot carry this signature: it is a script, not a Mach-O binary. If the repository holds the two certificate secrets below, CI signs the macOS release binaries. If it also holds the three App Store Connect secrets, CI notarizes them. Without the certificate secrets, CI prints one notice and ships binaries without a Developer ID signature.

| Secret | Holds |
|---|---|
| `MACOS_CERT_P12_BASE64` | The Developer ID Application certificate (team GQHC8CPYGZ) as a `.p12` file, base64 encoded |
| `MACOS_CERT_PASSWORD` | The password of that `.p12` file |
| `APPLE_NOTARY_KEY_P8` | The text of the App Store Connect API key (`.p8`) |
| `APPLE_NOTARY_KEY_ID` | The key ID of that API key |
| `APPLE_NOTARY_ISSUER_ID` | The issuer ID of the App Store Connect team |

The same secrets sign and notarize `fno-app.app` in the desktop app release job. The two certificate secrets alone give a Developer ID signature with the hardened runtime. macOS keys privacy grants to the team, so they stay put across releases even before notarization. Notarization and stapling need the three App Store Connect secrets as well. If those are missing, CI signs, skips notarization, and prints one notice that names what it skipped. A bare binary cannot carry a stapled notarization ticket, so Gatekeeper checks the notarization online.

## Keeping fno up to date

On a release install, run `fno update`. On a legacy wheel install (`uv tool install fno`, frozen at 0.4.8), `fno update` runs twice. The first run upgrades the wheel. The next run performs the move to the binary install in `~/.fno/install/bin`. On 0.4.4 and older, the first update stops with "Could not locate the fno CLI source", so run `uv tool upgrade fno` yourself first. After the move, `fno update` installs the newest GitHub release. A failed move names `fno update` and waits an hour before trying again. The plugin's session start runs the same move for a wheel whose plugin tree is newer. `fno update --adopt <dir>` is the staged-release door postinstall uses. Each release also ships a `min_supported` floor in `fno-release.json`. Below it, `fno` refuses every command except `fno update` and `fno doctor` with one line naming the fix. Plugin updates are separate: in Claude Code, run `/plugin update`.

From a clone, the full refresh has three steps. `fno doctor update` refreshes the binaries. The long-running mux server and agents daemon keep old code until restarted:

```bash
git pull
fno doctor update          # reinstall the Python CLI + cargo binaries (fno mux + fno-agents) from source
fno agents session restart --mux   # restart the daemon AND the mux server onto the fresh binaries
```

`fno agents session restart` on its own restarts only the agents daemon (PTY workers survive). The `--mux` flag also restarts the mux server, which is **destructive**: it ends live mux sessions, so it is opt-in. Reattach afterward. `fno doctor` flags a running mux server that predates the installed binary and reminds you to run it. In a running Claude Code session, bump the plugin (or relaunch) to pick up new skills and hooks after a pull.

## Uninstall

Run it from a plain terminal, not from inside an fno mux pane:

```bash
fno uninstall --dry-run    # list every item found; change nothing
fno uninstall              # ask once, then remove them all
fno uninstall --yes        # no question; needed when stdin is not a terminal
```

It removes each item through its own uninstall command, in this order:

1. The launchd agents (`~/Library/LaunchAgents/sh.fno.*.plist`), plus any autocorrect agents or crontab block an old install left.
2. The Claude Code and Codex plugins, their `fno` or older `footnote` marketplaces, and the OpenCode plugin files.
3. The hooks fno added to `~/.claude/settings.json`, `~/.gemini/settings.json` and `~/.codex/config.toml`. Your own hooks stay.
4. The `uv` tool, the Homebrew formula and the bootstrap cache. When the install script installed `uv` itself, `uv` is listed too.
5. The daemon and the mux sessions, which it stops.
6. The `cargo` binaries, `fno` last.

`~/.fno` stays. `--purge` deletes it too, after you type `purge`. It holds your backlog and any worktrees under `~/.fno/worktrees`, so commit or push that work first. The run ends with a list of anything named fno that is still on `PATH`, in `LaunchAgents`, in a plugin cache or running. Exit 0 means that list is empty. It also names each `~/.local/bin` PATH line in your shell profiles by file and line. Other `uv` tools share that line, so uninstall never removes it, and it does not change the exit.

## Next

- [Quickstart](quickstart.md): your first PR with fno.
- [Troubleshooting](troubleshooting.md): the failures first runs hit.
