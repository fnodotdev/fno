# Spawn receipts

How `spawn.sh` parses a launch receipt, and the node-claim gap on non-claude builds. One hop from [SKILL.md](../SKILL.md#spawn-default-command).

## Receipt families (keyed by the command spawn.sh ran)

`spawn.sh` branches the receipt parse on the command/mode it ran - never on
sniffing output (Group 1 put claude creation on `spawn`, so claude
takes the JSON `.short_id` family below, not a bare 8-hex line):

- **`spawn --once` / `--as headless`**: a **client-side one-shot**
  (`codex exec` / `gemini -p`). stdout is the model REPLY verbatim, not a
  short-id. Success = exit 0 AND a non-empty reply; an empty reply (even on
  exit 0) is FAILED, never a fabricated answer.
- **`spawn` / `host`**: stdout is JSON
  carrying `{"short_id",...}` (compact or pretty depending on runtime and
  substrate). `.short_id` is parsed with `jq`; `thread` uses a session-shaped
  handle: Claude receipts use an 8-hex id, Codex receipts use a full UUID,
  OpenCode receipts use `ses_*`, and keeper-lane receipts use their full session
  identity. The default/pane lane accepts the runtime's identifier-shaped
  handle: Rust returns a name-slug `short_id`; Python pane receipts have an
  empty worker-socket id, so `spawn.sh` uses the receipt's registry `name` only
  after matching provider/status and requiring both mux session and pane id.
  Empty, mismatched, or malformed receipts (even on exit 0) are FAILED.

Report only what `spawn.sh` actually captured - a real short-id, or a real reply.
"No valid receipt" is FAILED, full stop.

## Durable node-claim limitation (codex/gemini builds)

A claude `/target` build worker acquires the durable `node:<id>` claim itself. A
codex/gemini build runs through `fno agents spawn`, which does NOT run the
`/target` harness, so the worker does not self-acquire `node:<id>` today. The
dispatcher's `dispatch:<node>` reservation only covers the ~3-minute boot window;
after it expires, a second dispatch of the same node under a **different**
`--name` can double-launch. This is a provider-integration limitation tracked as
a follow-up. Re-dispatching the same node with the same derived name is always
safe (the registry same-name guard catches it).

