---
name: using-fno
description: Explain when to use Footnote workflow skills versus atomic fno CLI commands.
allowed-tools: Read
---

# You are in a footnote-enabled project

The `footnote` plugin is installed. Two surfaces compose: workflow skills and atomic `fno` CLI commands. Never hand-edit the state files the CLI manages.

**Workflow commands** run plugin-qualified: `/fno:target`, `/fno:think`, `/fno:review`, `/fno:ship pr`, `/fno:fix`. Codex spells `$fno:`. **CLI primitives** run exact state transitions: `fno backlog done <id>`, `fno do pr status <n>`. A state transition runs the CLI primitive, never a skill.

**Missing `fno`?** Nothing installs `fno` unasked: the hooks only print the install plan. If absent, tell the user the installer fetches the release tarball, checks its sha256, and links binaries into `~/.local/bin`, then ask. On a yes run `bash "${CLAUDE_PLUGIN_ROOT}/.claude-plugin/postinstall.sh"`: `installer exit 0` means done, any other exit means failed. The binary lands at `~/.local/bin/fno`. Bare `fno` works once PATH carries that folder. No new session is needed. Only a no blocks: say what is blocked and stop cleanly. Then run the user's command right here.

**Full guide:** the version-matched guide (mail receipts, panes, mesh, fno-app) prints on demand: `fno whoami guide` whole, `fno whoami guide <topic>` for one.

## Relay compression contract

Agent-authored `fno agent mail send`, `fno agent mail reply`, `fno mux pane send` are handoffs: 80 words or fewer. Keep technical terms, commands, errors, numbers, negation exact. Send outcome, reason, next action. A stop, resume or scope change starts the body with `control:`.

## Verified facts only

State facts you verified this turn, each with its footnote: a file:line, command output, or source link. Before "cannot", try the other routes, then name each one tried in the claim. Worked examples: skills/verify-footnotes/SKILL.md.

## Known Limitations and Deferred Work

- Live mail receipts go stale. Read [LIMITATIONS.md](LIMITATIONS.md).
