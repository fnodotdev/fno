# fno-table

A Claude Code mod. `/fno` opens a side pane with two tabs of rows: your live fleet and your backlog. Keys on the selected row run fno verbs.

It needs Claude Code 2.1.287 or later and the `fno` CLI on your PATH.

## Install

1. Run `/plugin install fno-table@fno`.
2. Type `/fno`. The pane refreshes every 10 seconds while it is open. `/fno close` closes it.

## Keys

| Tab | Key | Runs |
|---|---|---|
| both | `f` / `b` | Switch tab |
| backlog | `s` | `fno agent spawn <node> "/fno:target <node>"` |
| backlog | `t` | `fno do target start <node>` |
| fleet | `p` | `fno agent session peek <agent>` |
| fleet | `o` | `fno mux view <agent>` |
| either | `j` | `fno backlog join <node>` |

Join shows only when the node has a live claim and a bound plan. If `fno backlog join` refuses (exit 3 one wide, 4 no plan, 5 already joined), the pane prints the reason and hides join for that node. A pane the mod opens by itself needs 144 or more columns; 110 after the first manual open.

Rows come from `fno agent session list --json` and `fno backlog board --json`.
