---
name: fno-app
description: "Drive the fno-app desktop app from the CLI: open, show, focus, shot, state, close."
---

Run `fno whoami guide app` and follow the printed guide. The guide comes from the installed fno CLI, so it always matches the installed version.

If the command refuses, the installed fno predates the guide door. Start with `fno whoami`, then bare `fno mux app`, whose usage refusal names the seven commands: open, show, focus, answer, shot, state, close.

Shots a worker posts to a review board save into the vault under the head they show: `fno agent internal target visual shot-save <node> <png...>` puts each shot at `{vault}/mux/<node>-<head12>-<view>-<role>-<theme>.png`. A later round reads its before pair from the vault at the base head and never re-shoots an old head.

## Known Limitations and Deferred Work

- The doors never build, the shot world seeds the board graph alone, and every command refuses off macOS. See [LIMITATIONS.md](LIMITATIONS.md).

