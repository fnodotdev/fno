# fno

`cargo install fno fno-agents` builds the four fno binaries with your Rust toolchain. The set: the `fno` front door and the three `fno-agents` binaries.

## The mux

Run `fno` bare on a TTY and you get an interactive `$SHELL` in a persistent session. A background server owns the PTY and its emulated screen. The client you are looking at is a thin compositor attached over a Unix socket at `<mux dir>/<session>.sock`. When config resolves a `state_dir`, the mux dir is `<state_dir>/mux`. Otherwise it is `~/.fno/mux` (dir `0700`). Quit the client (`Ctrl-\`), close the terminal, or `kill -9` it. The shell keeps running, and the next `fno` reattaches to the exact same screen (alt-screen programs included).

```sh
fno                      # attach to the "main" session, spawning its server if absent
fno mux server           # run a session server explicitly (scriptable)
fno mux server --session work
```

Phase-1 scope: one full-window pane, one client at a time. Input is never
dropped; render frames are droppable (a slow client just skips to the newest
self-contained frame). The server survives `cargo install` upgrades and
refuses version-skewed clients with both versions named.

## The CLI

This binary is the `fno` front door. It answers each command itself or hands it to the `fno-agents` binary installed beside it. No Python runs. The release tarball carries all four binaries (`fno`, `fno-agents`, `fno-agents-daemon`, `fno-agents-worker`), and `curl -fsSL fno.sh | sh` installs it into `~/.fno/install/bin`. `fno update` keeps the set current.

```sh
curl -fsSL fno.sh | sh
fno --help
```

`cargo install fno fno-agents` installs the full set into the cargo bin dir. The front door alone (`cargo install fno`) refuses an agents command with the install remedy.

Apache-2.0.
