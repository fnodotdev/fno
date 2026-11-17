# fno-proto

The wire contract of the fno mux. It holds the client and server messages, the frame and layout types, and the framing functions.

By default the crate depends on `serde` and `serde_json` only. Turn on `async` for `read_msg` and `write_msg` over tokio streams. Turn on `schema` to derive the JSON Schema.

```rust
use fno_proto::{encode, ClientMsg, Command, Dir};

let bytes = encode(&ClientMsg::Command(Command::SplitDir(Dir::Right)))?;
// 4-byte big-endian length, then the JSON body. Write it to the mux socket.
```

Not a Rust author? Read [`schema/mux-wire.schema.json`](schema/mux-wire.schema.json) and [`SPEC.md`](SPEC.md). Together they state every message, the framing, the handshake and the version promise.

Writing a TypeScript client? [`schema/ts/`](schema/ts) carries the generated bindings, one file per type. `index.ts` re-exports everything. `version.ts` exports the `PROTO_VERSION` and `MIN_COMPAT_PROTO` numbers a client gates its attach handshake on. Refresh with `cargo run --features ts --example gen-ts` from the crate directory. Every client action goes through the daemon. No client reads graph.db or files.

Authoring a pack's UI? The same door publishes the mod and layout surface as [`schema/pack-ui.schema.json`](schema/pack-ui.schema.json). The authoring page that walks it end to end is `docs/guides/author-a-mod.md` in the fno repository.

The socket path is not resolved here. Ask `fno mux ls --json`.

Licensed under Apache-2.0.
