# The fno desktop app

`fno-desktop` is a macOS window onto a running mux session. It draws the frames the mux server sends and types into the focused pane. It runs no terminal emulator of its own, so it shows exactly what the TUI shows.

## Build and run

The app is macOS only and is built from source. It is not part of `cargo install fno`.

```bash
cargo build --release --manifest-path crates/fno/Cargo.toml --features desktop --bin fno-desktop
T="$(cargo metadata --format-version 1 --no-deps --manifest-path crates/fno/Cargo.toml | jq -r .target_directory)/release/fno-desktop"
codesign -s - "$T"
fno mux            # in another terminal: the app needs a running mux server
"$T"
```

The `codesign -s -` step gives the binary an ad-hoc signature. No developer account is needed.

## Flags

| Flag | Default | What it does |
|---|---|---|
| `--session <name>` | `FNO_SERVER`, then `default` | The mux session to attach to. |
| `--font <family>` | `Menlo` | The terminal font family. |
| `--font-size <pt>` | `13` | The terminal font size. |
| `--probe` | off | Attach with no window, print one line, and exit. |

`--probe` attaches as an observer, so it never resizes a pane. It waits up to 5 seconds for the layout and one frame per pane. Then it prints `attached session=<s> panes=<n> frames=<m>` and exits 0. With no server it prints the socket path it tried and exits 1.

## Keys

Keys go to the focused pane as a terminal sends them. The mux prefix key passes through, so every mux binding works as it does in the TUI. The app keeps these cmd shortcuts for itself:

| Shortcut | Action |
|---|---|
| cmd-d | Split right |
| cmd-shift-d | Split down |
| cmd-] / cmd-[ | Next pane / previous pane |
| cmd-t | New tab |
| cmd-w | Close pane |
| cmd-v | Paste (bracketed when the pane asks for it) |
| cmd-q | Detach and quit |

A click on a pane focuses it.
