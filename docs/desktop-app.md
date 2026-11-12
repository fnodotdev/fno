# The fno desktop app

`fno-app` is a macOS window onto a running mux session. It opens in chat mode: the focused agent's conversation, drawn as turns. The terminal view draws the frames the mux server sends and types into the focused pane. The app runs no terminal emulator and parses no transcript of its own, so it shows exactly what the mux serves.

## Build and run

The app is macOS only and is built from source. It is not part of `cargo install fno`.

The build compiles Metal shaders, so it needs Xcode and its Metal toolchain. If the build stops on `missing Metal Toolchain`, install it once with `xcodebuild -downloadComponent MetalToolchain`. To build without it, add `--features gpui/runtime_shaders`: the app then compiles its shaders at launch.

```bash
cargo build --release --manifest-path crates/fno-app/Cargo.toml --bin fno-app
T="$(cargo metadata --format-version 1 --no-deps --manifest-path crates/fno-app/Cargo.toml | jq -r .target_directory)/release/fno-app"
codesign -s - "$T"
fno mux            # in another terminal: the app needs a running mux server
"$T"
```

The `codesign -s -` step gives the binary an ad-hoc signature. No developer account is needed.

Each release also ships `fno-app-macos-arm64.tar.gz` for Apple Silicon, ad-hoc signed, so Gatekeeper asks once. Clear the prompt with `xattr -d com.apple.quarantine fno-app`. The app runs the `fno` and `fno-agents` it finds beside itself, then on `PATH`, so install fno first.

## Flags

| Flag | Default | What it does |
|---|---|---|
| `--session <name>` | `FNO_SERVER`, then `default` | The mux session to attach to. |
| `--font <family>` | `Menlo` | The terminal font family. |
| `--font-size <pt>` | `13` | The terminal font size. |
| `--mode <view>` | `chat` | The view the window opens on: `chat`, `terminal`, `orchestration` or `extensions`. |
| `--probe` | off | Attach with no window, print two lines, and exit. |
| `--chat <id>` | none | Show one claude session's chat instead of the focused agent's. `newest` picks the most recently written transcript. With `--probe`, count its events instead of attaching. |

`--probe` attaches as an observer, so it never resizes a pane. It waits up to 5 seconds for the layout and one frame per pane. Then it prints `attached session=<s> panes=<n> frames=<m>`. Next it reads the fleet, the open questions and the backlog the way the window does, and prints `agents=<n> questions=<m> seats=<k>`. After a good attach it exits 0. With no server it prints the socket path it tried and exits 1. A failed fleet or question read prints its reason on stderr.

`--probe --chat <id>` asks the server for that claude session's chat stream. It counts the events until 2 seconds pass with no new batch. Then it prints `chat_events=<n> session=<id>`. If n is at least 1, it exits 0. A session the server cannot find prints the server's reason and exits 1.

## The window

The left pane has four sections. Agents come in the order the TUI sideline uses. The pane shows the first 8, and a "+N more" row shows the rest. Each agent card shows a state glyph, its name, its harness and state word. A bound agent also shows its node and PR. The Agents header counts the agents per state, so a blocked agent below the fold still shows. Only the agent you are on wears the cursor. A click focuses the agent's pane, or opens a paneless agent in portal 0. Sessions are the mux squads. A click switches to one. Questions are the open questions that wait on you, newest first. Machines are the fleet's hosts with their seat counts. A section whose read failed shows the error above its last good rows. While the window is active, the fleet and the questions refresh every 60 seconds. They also refresh at once after an answer, and on your return to the window. The backlog behind seat titles refreshes every 5 minutes.

The strip above the grid shows the active session's tabs. A tab without a chosen name shows its pane's title. A click selects a tab. The Chat, Board, Extensions and Terminal buttons at the right of the strip switch the main view.

## Chat mode

cmd-1 or Chat shows the conversation of the agent in the focused pane. The app reads it from the mux server's chat stream, which serves claude, codex and opencode sessions in one event shape. The conversation sits in a centred column. Your turns sit on the right in a bubble.

The agent's text renders as markdown: headings, bold, italic, strikethrough, quotes and tables. Inline code uses the terminal font, and so do code blocks. Bullet, numbered and task lists keep their markers. A link shows in the accent colour, and a click opens it. Images in your turns, in tool output and in markdown draw inline. An image too big for the stream shows its size instead.

Thinking takes one line, and a click opens its text. Each tool call takes one muted line with its state: … while it runs, ✓ for a success, ✗ for a failure. A click opens the call's sheet with its input and output in the terminal font, each with a Copy button. Output past 20,000 characters is cut, and the sheet says how many were cut. Escape, Close or a click outside closes the sheet. A kind the app does not know shows as one plain line. New turns append as the agent writes them. The view follows them while it is scrolled to the bottom.

Focus another pane to read its agent. The app keeps each conversation it read, so focusing back resumes where it left off. A pane with no agent session says so. A session with no turns yet says so after 2 seconds. With `--chat <id>`, the view stays on that session whatever has focus.

The composer at the bottom takes your message. Enter or Send sends it, Shift-Enter adds a line, Backspace deletes one character, and cmd-v pastes into it. While an input method composes, Enter goes to the input method. The app types the text into the session's own pane as a paste. It waits 800 ms, then presses Enter in that same pane, as the TUI reply path does. Your turn shows at once with "sending…". After the transcript records it, the turn moves into the conversation. If no pane on screen hosts the session, the app says so and sends nothing.

If the chat stream ends, the composer gives way to the reason and a Reconnect button. A normal end with no known cause reads muted. When the connection's own exit reason is known, it folds into that line and reads in the error colour. No corner toast repeats it while no sheet covers the conversation. A failure reads in the error colour. Reconnect resumes from the last event the view holds.

cmd-2 or Board opens orchestration mode, the command center board. TABLE lists every seat with its node and status. With more than one host in the fleet, a MACHINE column shows too. FEED lists each open question with its options, and the recommended option is marked. A click on an option answers it through the same door the TUI uses. The question then shows "sending", then "answered", until the next read drops it. A refused answer shows its reason and keeps the options. cmd-4 or Terminal goes to the terminal.

cmd-3 or Extensions lists the mods and layouts from `fno config plugins ls --json`, the feed the mux settings tab reads. A row shows its title, its `pack/id` and its summary. An On or Off button switches it. A built-in mod shows "Built in" and has no switch. A click on a name shows its preview image and its rendered recipe on the right. A mod that runs code asks first: the first switch-on shows the command and the commit, and nothing runs until you press "Turn it on and let it run". The list reads when the view shows, after a switch, and when an install pane closes. A failed read shows its error above the last good rows. A switch shows "..." until the CLI answers, and takes no second press meanwhile. A code mod's row says "Runs code". Up and down move the selection, Space switches the selected mod, `o` opens it, `i` opens the URL field and Enter confirms an armed code mod. "Install from URL" and "Open in a pane" run `fno config plugins install` or `open` in a pane split to the right of the focused pane, and switch to the terminal once that pane exists. When the pane cannot open, the notice names the exit code and the first line the launcher printed, and the view stays. The app never installs by itself: the pane shows what the pack runs and asks you, as the CLI does. "Browse published packs" reads `fno config plugins gallery --json`, and a click on a row fills the URL field. Typed text and cmd-v go to the URL field while it is open. Escape closes it, and an armed code-mod confirm stays.

The window follows the macOS appearance. A Mac in dark mode gets footnote-superscript. A Mac in light mode gets footnote-paper. When the system switches, the window repaints with no restart. A `mux.theme` pin in your config wins over the appearance, as it does in the TUI, so a pinned dark theme stays dark. Programs inside panes keep the colors the mux server gave them. A pane that paints its own dark ground stays dark in a light window.

Release the mouse on a button, a row or an option to act on it. Release somewhere else to cancel. A notice shows in the bottom corner for 4 seconds.

## Keys

In the terminal view, keys go to the focused pane as a terminal sends them. The mux prefix key passes through, so every mux binding works as it does in the TUI. The app keeps these cmd shortcuts for itself:

| Shortcut | Action |
|---|---|
| cmd-d | Split right |
| cmd-shift-d | Split down |
| cmd-] / cmd-[ | Next pane / previous pane |
| cmd-t | New tab |
| cmd-w | Close pane |
| cmd-c | Copy the focused pane's selection |
| cmd-v | Paste (bracketed when the pane asks for it) |
| cmd-1 / cmd-2 / cmd-3 / cmd-4 | Chat / orchestration board / extensions / terminal |
| cmd-q | Detach and quit |

Typed text goes through the macOS input system, so dead keys, option-compose and IME input work. Option on a letter types the composed character, as in Terminal.app. Option on an arrow or backspace still sends the meta form.

## Mouse

A click on a pane focuses it. A drag selects text, and the release copies it. The wheel scrolls the pane's history, and `[+N]` at the top right shows how many lines up the pane is. A click on a border or a gap does nothing.
