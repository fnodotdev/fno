# Chat event stream

The mux serves one agent session's conversation as typed chat events. A chat client (the desktop app's chat mode) reads them to draw turns, thinking, tool calls, images, subagents and compactions. It does not read raw terminal panes.

## The type

`crates/fno/src/proto/chat_event.rs` holds the one shape for every harness.

- `SessionKey { harness, session_id, subagent }` names one stream. A subagent is its own stream, so two files never mix in one sequence.
- `ChatEvent { seq, at_ms, source, body }`. `seq` starts at 0 for each stream and has no gaps. `source` is the byte offset of the line, or the store row id.
- `ChatBody` is tagged by `kind`. The kinds: `user_message`, `text_started`, `text_delta`, `text_ended`, `thinking_started`, `thinking_delta`, `thinking_ended`, `tool_called`, `tool_finished`, `question_asked`, `question_answered`, `subagent_started`, `compacted`, `turn_started`, `turn_ended`, `unknown`.

Text and thinking use the started, delta, ended split from opencode's event schema. A replay emits one delta with the full text.

A `user_message` has an `origin`. `typed` means a person typed it. `meta` means the harness put it there: Claude `isMeta` rows, text that starts with `<`, and the Codex AGENTS.md preamble. Meta messages are kept and marked, never dropped.

A body this build cannot read decodes as `unknown`, never as an error. An older client therefore reads a newer server's stream. The readers also emit `unknown` for a source row kind they have no mapping for, so a new harness block shows up in the UI.

## The wire

Wire v127 adds two messages.

- `ClientMsg::ChatTail { proto, build, session, from_seq }` is a first message, versioned like `Control`. It is not sent after `Attach`. A live attached connection that sends it is closed as a protocol violation.
- `ServerMsg::ChatEvents { session, events }` is one batch in `seq` order. A batch holds at most 256 events and about 8 MiB.

The server answers a version skew with `Bye`. It answers an unsupported harness or a missing transcript with one `Err` (`BAD_REQUEST`) that names it, then closes. The stream task polls in `spawn_blocking`. While idle, it polls every 250 ms. After the client goes away, the task ends. The core loop never sees it.

A client without tokio uses `chat_events::tail_sync(socket, session, from_seq)`. It returns an iterator of batches.

## Resume

`seq` is a pure function of the source. The server stores no sequence. A client that reconnects sends the last `seq` it holds plus one as `from_seq`. The server re-reads the source from the start and drops every event below it.

Two rules keep a live tail and a replay equal:

- **Line hold (JSONL).** A partial last line waits for its newline. A Claude `Agent` call also waits for its subagent `.meta.json`. It waits only while it is the last complete line. A `compact_boundary` waits for the next row, which can carry the summary.
- **Watermark (opencode).** opencode rewrites a part row while it streams. The reader emits parts in store order and stops at the first part that is not final. A text or reasoning part with `time.end` is final. A part with no `time` at all (a user part) is final too. A pending or running tool emits its call once and holds until it completes or fails.

One race is left. A Claude subagent meta file that lands after a later row of the parent places `subagent_started` differently in a live tail and in a replay.

## Images

An image smaller than or equal to 512 KiB decoded (`CHAT_IMAGE_INLINE_MAX`) rides inline as base64, with its size and sha256. A larger image carries its size and sha256 only. Fetching bytes by hash is not built yet.

## Mapping

| Event | Claude JSONL | Codex rollout | opencode store |
|---|---|---|---|
| user message | `user` row, string or `text`/`image` blocks | `response_item` `message`, role `user` | `text` or `file` part in a user message; `subtask` part as meta |
| assistant text | `assistant` row, `text` block | `response_item` `message`, role `assistant` | `text` part in an assistant message |
| thinking | `thinking` block; empty text with a signature, or `redacted_thinking`, reads `redacted` | `response_item` `reasoning`; `encrypted_content` only reads `redacted` | `reasoning` part |
| tool call | `tool_use` block | `function_call` (arguments parsed as JSON, kept raw on failure), `custom_tool_call`, `local_shell_call`, `web_search_call` | `tool` part |
| tool result | `tool_result` block in a `user` row | `function_call_output`, `custom_tool_call_output` | `tool` part `completed` or `error` |
| question | `tool_use` named `AskUserQuestion`; its `tool_result` is the answer | `event_msg` `request_user_input` | `tool` part named `question` |
| subagent | `tool_use` named `Agent`, linked through `<sid>/subagents/agent-<id>.meta.json` `toolUseId` | not linked: no rollout on record carries a child id, so a spawn is a plain tool call | `tool` part named `task`, `state.metadata.sessionId` |
| compaction | `system` `compact_boundary`, summary from the next `isCompactSummary` row | `compacted` item | `compaction` part |
| turn | none | `event_msg` `task_started`, `task_complete`, `turn_aborted` | `step-start`, `step-finish` parts |

Some rows emit nothing, because they are session facts, not chat:

- Claude rows with no `message` object (`attachment`, `api-request`, `custom-title`, ...), and `api-request-blob`.
- Codex `session_meta`, `turn_context`, `world_state`, `token_count`, and every `event_msg/item_completed`. Each `item_completed` repeats a `response_item`.
- opencode `agent`, `snapshot`, `patch` and `retry` parts.

Claude writes one assistant content block per row, so Claude needs no streamed-versus-complete de-dup. Codex does repeat items, so the Codex reader takes content from `response_item` only.

## Check it on live sessions

```
cargo run --manifest-path crates/fno/Cargo.toml --example chat_events -- claude newest
cargo run --manifest-path crates/fno/Cargo.toml --example chat_events -- codex newest
cargo run --manifest-path crates/fno/Cargo.toml --example chat_events -- opencode newest
```

Each prints `chat_events=<n> unknown=<m>` and a count per kind. A nonzero `unknown` names a source kind this table must gain.

## Not covered

Pi, agy and footnote-harness sessions get an unsupported-harness error. The phone and web WebSocket route is not built.
