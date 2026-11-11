# The fno desktop app

`fno-desktop` is a macOS window onto a running mux session. It draws the frames the mux server sends and types into the focused pane. It runs no terminal emulator of its own, so it shows exactly what the TUI shows.

## Build and run

The app is macOS only and is built from source. It is not part of `cargo install fno`.

The build compiles Metal shaders, so it needs Xcode and its Metal toolchain. If the build stops on `missing Metal Toolchain`, install it once with `xcodebuild -downloadComponent MetalToolchain`. To build without it, add `gpui/runtime_shaders` to `--features`: the app then compiles its shaders at launch.

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
| `--probe` | off | Attach with no window, print two lines, and exit. |

`--probe` attaches as an observer, so it never resizes a pane. It waits up to 5 seconds for the layout and one frame per pane. Then it prints `attached session=<s> panes=<n> frames=<m>`. Next it reads the fleet, the open questions and the backlog the way the window does, and prints `agents=<n> questions=<m> seats=<k>`. After a good attach it exits 0. With no server it prints the socket path it tried and exits 1. A failed fleet or question read prints its reason on stderr.

## The window

The left pane has four sections. Agents come in the order the TUI sideline uses. The pane shows the first 8, and a "+N more" row shows the rest. A click focuses the agent's pane, or opens a paneless agent in portal 0. Sessions are the mux squads. A click switches to one. Questions are the open questions that wait on you, newest first. Machines are the fleet's hosts with their seat counts. A section whose read failed shows the error above its last good rows. While the window is active, the fleet and the questions refresh every 60 seconds. They also refresh at once after an answer, and on your return to the window. The backlog behind seat titles refreshes every 5 minutes.

The strip above the grid shows the active session's tabs. A tab without a chosen name shows its pane's title. A click selects a tab. The Terminal and Board buttons at the right of the strip switch the main view.

cmd-2 or Board opens orchestration mode, the command center board. TABLE lists every seat with its node and status. With more than one host in the fleet, a MACHINE column shows too. FEED lists each open question with its options, and the recommended option is marked. A click on an option answers it through the same door the TUI uses. The question then shows "sending", then "answered", until the next read drops it. A refused answer shows its reason and keeps the options. cmd-1 or Terminal goes back to the terminal.

Release the mouse on a button, a row or an option to act on it. Release somewhere else to cancel. A notice shows in the bottom corner for 4 seconds.

## Keys

Keys go to the focused pane as a terminal sends them. The mux prefix key passes through, so every mux binding works as it does in the TUI. The app keeps these cmd shortcuts for itself:

| Shortcut | Action |
|---|---|
| cmd-d | Split right |
| cmd-shift-d | Split down |
| cmd-] / cmd-[ | Next pane / previous pane |
| cmd-t | New tab |
| cmd-w | Close pane |
| cmd-c | Copy the focused pane's selection |
| cmd-v | Paste (bracketed when the pane asks for it) |
| cmd-1 / cmd-2 | Terminal / orchestration board |
| cmd-q | Detach and quit |

Typed text goes through the macOS input system, so dead keys, option-compose and IME input work. Option on a letter types the composed character, as in Terminal.app. Option on an arrow or backspace still sends the meta form.

## Mouse

A click on a pane focuses it. A drag selects text, and the release copies it. The wheel scrolls the pane's history, and `[+N]` at the top right shows how many lines up the pane is. A click on a border or a gap does nothing.
