---
name: setup
description: Set up a machine for Footnote by launching the interactive setup wizard and reading its report.
---

# Setup

Setup is `fno do wizard`, an interactive terminal program. It detects the machine, measures a concurrency band, probes each harness and model, and writes the config in one grouped apply. This skill launches the wizard and reads the report. It asks no setup question itself.

## Launch inside the mux

When the session sets `FNO_PANE`, open a split beside this pane and run the wizard there:

```bash
fno mux pane run --at current --split right --json -- fno do wizard
```

Keep the pane id from the receipt. Tell the person the wizard is in the split and that they answer there. Wait for the run to end:

```bash
fno mux pane wait <pane> --pattern 'wizard-report: ' --timeout 1800
```

`fno mux pane read <pane>` shows the wizard screens when the person asks what it shows. `fno mux pane send <pane> --text <key> --raw` types one key. Send a key only when the person asks for that exact answer, never to pick for them.

## Launch outside the mux

A session with no pane cannot host a terminal program. Print the command for the person to run in their own terminal:

```
fno do wizard
```

After the wizard finishes, follow the `Read the report` and `Say the result` steps below.

## No person at all

An unattended machine cannot answer the wizard. Run the batch probe and report it:

```bash
fno config setup run --dry-run --json
```

Apply nothing of kind `approve`. An approve needs a person.

The `claude-auto-mode` approve step adds Footnote's narrow state and role rules to the active Claude Code user settings file. If the user sets `CLAUDE_CONFIG_DIR`, setup uses its `settings.json` file. Otherwise, setup uses `~/.claude/settings.json`. A person must approve the step. An unattended run can show the proposal. It does not change Claude Code settings, credentials, or sandbox options.

The `permission-rules` approve step writes fno's must-enable list. The list is fno's own record commands and the common reads, such as `fno backlog`, `git status` and `gh pr view`. It never holds an interpreter, a shell, a runner, or a command that spawns, merges or pushes. The list lives in `crates/fno/src/setup/must_enable.toml`. The step writes only for a harness whose config home exists. Run it with `fno config setup run --only permission-rules`. A second run changes no byte.

Each harness gets one file. Claude Code gets `Bash(fno backlog *)` style entries appended to `permissions.allow` in the user settings file, after the user's own entries. Codex gets `~/.codex/rules/fno.rules`, and codex reads rules only at startup, so restart codex after the step. opencode gets rules at the head of the global `opencode.json` or `opencode.jsonc`. opencode uses the last rule that matches, so a rule of the user's own later in the file wins. A v1 opencode gets `permission.bash` keys, and a v2 opencode gets `permissions` rows. pi and agy have no rule syntax, so they get nothing.

The step refuses a file it cannot merge safely, such as an opencode file with comments. It then names the file and leaves it byte-identical. To undo the rules, delete the entries from `permissions.allow`, delete `fno.rules`, or delete the fno rows from the opencode file. `fno doctor` reports each harness as present, missing or drift. It never adds back a rule that the user removed, and `fno doctor --fix` does not write these rules.

## Read the report

The run writes `<state_dir>/sidecar/wizard-report.json`. It prints the path as `wizard-report: <path>`. Check that `run_id` is newer than your launch. Also check that `status` is not `running`. The run writes a `running` stub before the final report. Then report the red rows and their fixes, the files it wrote, and `next_action`.

## Modes

| Invocation | Effect |
|------------|--------|
| `/setup` | launch `fno do wizard` |
| `/setup join <file>` | launch with `--join <file>`: apply a profile exported on another machine |
| `/setup check` | run `fno config setup run --check --json` and show the drift with each fix |

## The keys the wizard does not ask

For the advanced config tier and the post-merge parking lot, `fno config setup plan --advanced` and `fno config setup post-merge` remain the commands. The `[sandbox]` key is also set by hand: `provider = "devcontainer"` sandboxes each `--as headless` worker in a devcontainer. The guide: [Sandbox a worker](https://fno.sh/docs/guides/sandbox-a-worker).

## Known Limitations and Deferred Work

- The wizard needs a terminal on stdin, and an approve-kind step always waits for a person. See [LIMITATIONS.md](LIMITATIONS.md).
