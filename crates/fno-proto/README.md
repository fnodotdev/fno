# fno-proto

The wire contract of the fno mux: the client and server messages, the frame and layout types, and the framing functions that read and write them.

By default the crate depends on `serde` and `serde_json` only. Turn on `async` for `read_msg` and `write_msg` over tokio streams. Turn on `schema` to derive the JSON Schema.

```rust
use fno_proto::{encode, ClientMsg, Command, Dir};

let bytes = encode(&ClientMsg::Command(Command::SplitDir(Dir::Right)))?;
// 4-byte big-endian length, then the JSON body. Write it to the mux socket.
```

Not a Rust author? Read [`schema/mux-wire.schema.json`](schema/mux-wire.schema.json) and [`SPEC.md`](SPEC.md). Together they state every message, the framing, the handshake and the version promise.

The socket path is not resolved here. Ask `fno mux ls --json`.

Licensed under Apache-2.0.
