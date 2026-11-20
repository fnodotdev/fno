# New-user persona labeled set

Each case is one change and the verdict a careful new-user review gives it. The persona scores itself on this set before and after an edit (see the SKILL.md "Editing this persona" section). These six are seed cases written with the skill. Replace or extend them with real changes whose verdict the user checked in the weekly slice, and keep both verdicts represented.

## case-1

- change: README quickstart adds `fno do target start <node>` as the second command, before any step creates a node.
- right verdict: fail
- why: missing step. The reader has no node yet, and the page has not said what a node is.

## case-2

- change: `fno config setup` refusal text grows a final line `next: fno config setup run --repair`.
- right verdict: pass
- why: the refusal now names the next command. It closes a dead end.

## case-3

- change: crates/fno-agent/src/eval/evals_trend.rs renames an internal fold function.
- right verdict: skip
- why: no file on the new-user surface.

## case-4

- change: the getting-started page says "the lead seat dispatches your node to a lane". No earlier line defines lead, seat or lane.
- right verdict: fail
- why: undefined words on the first page a new user reads, with no link to a definition.

## case-5

- change: postinstall.sh prints `installer exit 0` and stops. `~/.local/bin` is not on PATH, so the next `fno` command is not found.
- right verdict: fail
- why: false success. The output says done, and the reader cannot run the next step.

## case-6

- change: README fixes a typo ("recieve" to "receive") in the install section.
- right verdict: pass
- why: on the surface, and no check finds a step the reader cannot get past.
