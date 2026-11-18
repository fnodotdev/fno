---
name: using-fno
description: Explain when to use Footnote workflow skills versus atomic fno CLI commands.
---

# You are in a footnote-enabled project

This workspace has the `footnote` plugin installed. Two surfaces compose: workflow skills (reasoning, orchestration) and atomic `fno` CLI commands (mechanical, lock-protected state transitions). Skills call CLI commands internally. Never hand-edit the state files the CLI manages.

**Workflow commands** run plugin-qualified: `/fno:target` (idea to green PR), `/fno:think`, `/fno:review`, `/fno:ship pr`, `/fno:fix`. Codex spells `$fno:`. **CLI primitives** run exact state transitions: `fno backlog done <id>`, `fno do pr status <n>`, `fno agent mail send`, `fno agent spawn`, `fno whoami`. A state transition runs the CLI primitive, never a skill.

**Worktree-first:** enter a feature worktree before editing or committing; keep the canonical checkout unclogged. A project whose resolved `worktree.policy` is `never` works in place.

**Missing `fno`?** Nothing installs `fno` unasked: the hooks only print the install plan. If `fno` is absent, tell the user the installer fetches the release tarball, checks its sha256, and links the binaries into `~/.local/bin`, then ask. On a yes run `bash "${CLAUDE_PLUGIN_ROOT}/.claude-plugin/postinstall.sh"`; `installer exit 0` means done. Then run the command the user asked for right here.

**Full guide:** this stub is the only injected part. The version-matched guide (relay contract, mail receipts, panes, mesh, fno-app) prints on demand:

    fno whoami guide          # the whole guide + topic index
    fno whoami guide mail     # one topic

## Known Limitations and Deferred Work

- Live mail receipts can go stale; read [LIMITATIONS.md](LIMITATIONS.md) before trusting one.
