# The fno mux wire

This page states what the JSON Schema cannot: how messages are framed, how a connection starts, which direction may drop data, and what the version numbers promise.

The shapes of every message are in [`schema/mux-wire.schema.json`](schema/mux-wire.schema.json). CI regenerates that file from the Rust types and fails on any difference, so the file is never older than the code.

## Framing

A message is a 4-byte unsigned big-endian length, then that many bytes of UTF-8 JSON. The JSON body is one `ClientMsg` (client to server) or one `ServerMsg` (server to client). The largest body is `MAX_MSG_BYTES`, 32 MiB. A longer length is a protocol error: close the connection.

A reader that sees a clean end of stream before the first length byte treats the connection as closed. A stream that ends inside a message is malformed.

## The socket

The server listens on a Unix socket at `~/.fno/mux/<session>.sock`. The directory has mode 0700, because the socket accepts keystrokes into your shell. The default session is `main`. `fno mux ls --json` lists live servers and their socket paths. This crate does not resolve the path: the state root can move with fno's config, so ask `fno`.

## Starting a connection

A long-lived client sends `Attach { proto, build, rows, cols, cwd }` as its first message. `proto` is the `PROTO_VERSION` the client was built against. `build` is its own version string, used only in error text. The server answers with the layout and frames, or with an error that names both versions.

A one-shot caller (a script that sends one command) sends `Control { proto, build, verb }` instead. The server answers with exactly one reply and closes. It registers no client and streams no frames. The field is named `verb` on the wire and has kept that name across builds.

`Query` and `KillServer` are the only messages allowed before the handshake. Their JSON shapes are frozen. Do not depend on any other message being accepted before `Attach` or `Control`.

## Channel rules

Client-to-server input and commands are reliable: the server never drops them. Server-to-client render `Frame`s may be dropped. Each frame carries the full grid and the cursor, never a delta, so a client that misses a frame loses nothing once the next one arrives.

## Versions

`PROTO_VERSION` is an integer that rises on any wire change a build could misread. `MIN_COMPAT_PROTO` is the lowest client version a server still admits. A client below the floor is refused at the handshake with an error naming both versions. A client at or above the floor is admitted, so a newer server stays usable by older clients inside that range.

Within the range, new fields are additive and carry `#[serde(default)]`. An older reader ignores a field it does not know, and a newer reader fills a default for one an older writer omitted. A new message variant is not additive: the handshake keeps builds that cannot read it apart.

The crate version follows the same rule on its own number. While it is 0.x, an additive wire change raises the patch. Removing or renaming a field raises the minor. The check that enforces this is `scripts/ci/check-proto-version-bump.sh` in the fno repository.

## What this crate does not cover

TypeScript types are a separate artifact. The web bridge (`fno mux serve --web`) speaks HTTP and WebSocket JSON that is not this wire. A client that needs a layout of its own to draw has no extension point here yet: it can render the layout and frames the server sends, and it can send commands.
