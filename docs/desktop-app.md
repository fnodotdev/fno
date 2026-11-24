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

The `codesign -s -` step gives the binary an ad-hoc signature. No developer account is needed. An agent session declares the build: prefix `FNO_BUILD_WHY="<why>"` (mux shots, a bug repro). A human terminal builds freely.

Each app release ships `fno-app-macos.tar.gz` on its own `fno-app-v<version>` release. The bundle holds a universal binary, so it runs on Apple silicon and Intel Macs, macOS 11 or later. It holds `fno-app.app`, signed with a Developer ID, notarized and stapled, so it opens with no prompt. If the release was built without the notary secrets, the bundle is Developer ID signed but not notarized, so Gatekeeper asks once. If it was built without the certificate secrets, the bundle is ad-hoc signed instead. Then Gatekeeper asks once. Clear the prompt with `xattr -d com.apple.quarantine fno-app.app`. The app runs the `fno` and `fno-agent` it finds beside itself, then on `PATH`, so install fno first.

## Updates

The app checks the `fno-app-v*` release feed at launch and every 6 hours after, until it stages a build. It downloads the newest stable release in the background and proves it before it offers it. Four proofs must pass. The sha256 file must match. `codesign --verify --deep --strict` must pass. The Team ID must equal the running app's Team ID. The new binary must name the release version. A build that fails any proof is dropped, and the app tries again at the next check.

The check needs a Team ID. An ad-hoc signed or unsigned app has none, so it never checks for updates and never installs one. Install a new build of such an app by hand.

When a build is ready, a card appears at the bottom left: `fno-app <version> is ready`, with Restart and Later. Restart quits the app, swaps the staged bundle in place of the running one, and opens it. If the swap fails, the old bundle stays in place and opens again. Later hides the card. The app offers the same build again at the next check.

The first launch of the new build shows the release notes once, in the same card. The notes come from the `## <version>` section of `crates/fno-app/CHANGELOG.md`, which the release workflow reads. A version with no section gets the one-line default.

A dev build run outside a `.app` bundle never checks for updates. Staged builds live in `~/Library/Application Support/fno-app/update/`.

## Versions

The app has its own version line, starting at 0.0.1, separate from fno. `sync-version.sh` never stamps it. To release the app, add a `## <version>` section to `crates/fno-app/CHANGELOG.md`, set `version` in `crates/fno-app/Cargo.toml` and push a `fno-app-v<version>` tag. The `release-app` workflow builds that tag and publishes it.

The app never reads that version to judge compatibility. It reads the wire version the mux server stamps in its `.ver` file. The app needs wire v131 or newer. A server below that, or one with no stamp, makes the app stop with one line. The line names both versions. Run `fno update`, then `fno agent restart --mux`.

## Flags

| Flag | Default | What it does |
|---|---|---|
| `--session <name>` | `FNO_SERVER`, then `default` | The mux session to attach to. |
| `--font <family>` | `app.font`, then `SF Mono` | The terminal font family; overrides what the Settings page wrote. |
| `--font-size <pt>` | `app.font_size`, then `13` | The terminal font size; overrides the Settings row. |
| `--mode <view>` | `mux.launch_screen`, then the saved view, then `chat` | The view the window opens on: `chat`, `terminal`, `orchestration` or `mods`. |
| `--probe` | off | Attach with no window, print two lines, and exit. |
| `--chat <pin>` | none | Show one session's chat instead of the focused agent's. The pin is `<harness>:<id|newest>` (`claude`, `codex` or `opencode`), a bare session id (the transcript that carries it names the harness), or `newest` (claude: the newest transcript in the current project, ranked by session start; codex: the newest in its sessions tree). With `--probe`, count its events instead of attaching. |

`--probe` attaches as an observer, so it never resizes a pane. It waits up to 5 seconds for the layout and one frame per pane. Then it prints `attached session=<s> panes=<n> frames=<m>`. Next it reads the fleet, the open questions and the backlog the way the window does, and prints `agents=<n> questions=<m> seats=<k>`. After a good attach it exits 0. With no server it prints the socket path it tried and exits 1. A failed fleet or question read prints its reason on stderr.

`--probe --chat <pin>` asks the server for that session's chat stream. It counts the events until 2 seconds pass with no new batch. Then it prints `chat_events=<n> session=<id>`. If n is at least 1, it exits 0. A session the server cannot find prints the server's reason and exits 1.

## The window

The left pane has three sections, one line per row. Workspaces are the mux squads. Each workspace shows its header row, then its agents, then its shell panes. A click on the header switches to the workspace. The agents come in the order the TUI sideline uses, and a workspace with no agents and no shells says so. The caret on a header folds or opens that workspace, the fold persists across launches, and nothing folds on its own. An agent that runs outside every workspace lands in a trailing Elsewhere group. A folded workspace still shows a blocked agent, because its header counts the agents per state. Each agent row leads with its harness mark, with a small state glyph on the mark's corner. Then come its name and its facts in fixed columns. The columns are the model, the context percent, the token count, the session age, and the node and PR. Rows show no cost. How long ago it last acted sits at the right edge. The columns line up down the list, and a column no agent fills takes no room. If the pane is too narrow for every column, whole columns drop from the right. A fact shows whole or not at all. If no column fits beside the name, the name cuts. The app reads the facts through the same formatters as the mux card. A click focuses the agent's pane, or opens a paneless agent in portal 0. A shell row shows the pane label and its working directory, and a click focuses the pane. The circled plus on the Workspaces header opens an inline field that names a new workspace. The circled plus on a workspace header opens New worker (the composer starts in that workspace) or New shell. A right-click on a workspace header opens New Tab, Rename Workspace, Move Up, Move Down and Close. Close asks first and names the pane count. The group button on the Workspaces header cycles the view between workspaces, agent state (Needs input, Working, Done) and machine. One row counts the open questions that wait on you. A click opens HQ, where the list itself lives. Machines are the fleet's hosts with their seat counts. A section whose read failed shows the error above its last good rows. While the window is active, the fleet and the questions refresh every 60 seconds. They also refresh at once after an answer, and on your return to the window. The backlog behind seat titles refreshes every 5 minutes.

The title bar has a fixed height that no `--font-size` changes. On the left it names where you are: the session, the view and, in Chat and Terminal, the focused agent. In Terminal, the session's tabs follow. A tab without a chosen name shows its pane's title, and a click selects it. On the right, Chat, HQ, Mods and Terminal switch the main view, each with its cmd key. The chrome uses SF Mono for rows, ids and numbers. Chat turns and mod descriptions use the system font.

## Chat mode

Chat shows the conversation of the agent in the focused pane. The app reads it from the mux server's chat stream, which serves claude, codex and opencode sessions in one event shape. A header line names the agent, its node, its state and its cost. The conversation sits in a centred column. Each turn opens with its author and the time it began. Your turns sit on the right in a bubble.

The agent's text renders as markdown: headings, bold, italic, strikethrough, quotes and tables. Inline code uses the terminal font, and so do code blocks. Bullet, numbered and task lists keep their markers. A link shows in the accent colour, and a click opens it. Images in your turns, in tool output and in markdown draw inline. An image too big for the stream shows its size instead.

Thinking takes one line, and a click opens its text. System-injected rows, a task notification for example, fold the same way: one muted caret line, and a click opens the full text. Each tool call takes one muted line with its state: … while it runs, ✓ for a success, ✗ for a failure. A click opens the call's sheet with its input and output in the terminal font, each with a Copy button. Output past 20,000 characters is cut, and the sheet says how many were cut. Escape, Close or a click outside closes the sheet. A kind the app does not know shows as one plain line. New turns append as the agent writes them. The view follows them while it is scrolled to the bottom.

Focus another pane to read its agent. The app keeps each conversation it read, so focusing back resumes where it left off. A pane with no agent session says so. A session with no turns yet says so after 2 seconds. With `--chat <pin>`, the view stays on that session whatever has focus.

When the session you are reading has a pending question that waits on you, the chat shows it as a card above the composer. The card names the question and shows one button per option, with the recommended option marked. A tap answers it through the same door the Questions list uses, and the card shows the answer state. A session with no pending question shows no card.

The composer at the bottom takes your message. Enter sends it, Shift-Enter adds a line, Backspace deletes one character, and cmd-v pastes into it. Pasting an image with no text on the clipboard attaches it to the draft as a chip. A send puts the image back on the clipboard and types the harness's paste key, so the session receives it as an image. A harness with no verified paste key takes the text alone and says so. Backspace on an empty draft removes the chip. The hint beside the field names these keys, and a click on it sends. While an input method composes, Enter goes to the input method. The app types the text into the session's own pane as a paste. It waits 800 ms, then presses Enter in that same pane, as the TUI reply path does. Your turn shows at once with "sending…". After the transcript records it, the turn moves into the conversation. If no pane on screen hosts the session, the app says so and sends nothing.

If the chat stream ends, the composer gives way to the reason and a Reconnect button. A normal end with no known cause reads muted. When the connection's own exit reason is known, it folds into that line and reads in the error colour. No corner toast repeats it while no sheet covers the conversation. A failure reads in the error colour. Reconnect resumes from the last event the view holds.

HQ opens orchestration mode, the command center. Seats lists every seat with a status glyph, its status, its node id and the node's title. With more than one host in the fleet, a Machine column shows too. Questions lists each open question with its options. The recommended option carries the accent and a "recommended" tag. A click on an option answers it through the same door the TUI uses. The question then shows "sending", then "answered", until the next read drops it. A refused answer shows its reason and keeps the options. Terminal goes to the terminal. Splits there are divided by one hairline, and the panes without focus dim.

Mods lists the mods and layouts from `fno config plugins ls --json`, the feed the mux settings tab reads. The list splits into Installed, what you have, and Available, what you can install. Each row is one line: the title, its `pack/id` and a kind tag such as "Table", "Runs code" or "Built in". An installed row also starts with a check. The panel shows state and holds no toggle. A click on a row shows the mod on the right. The detail shows its title and one sentence on what it is for. Its state and the one action that changes it follow. An installed mod shows "✓ Installed", Turn off and Uninstall. Turn off switches that one mod off and keeps the pack. Uninstall runs `fno config plugins remove` and removes the whole pack: a pack installed from a URL loses its directory, and the row leaves the list. An active pack refuses until `fno config plugins deactivate` runs. An available mod shows Install. A mod that ships with fno shows "Built in" and no action. Open shows the mod in a pane where it has one. A labeled preview image follows, then the recipe's "What it shows" section. A mod that runs code asks first: Install shows the command and the commit, and nothing runs until you press "Install and let it run". The list reads as the view shows, after an install or uninstall, and after an install pane closes. A failed read shows its error under the Installed header above the last good rows. A mod whose install or uninstall is still out dims and takes no second press. Up and down move the selection. Space installs or turns off the selected mod, and `o` opens it. `i` opens the URL field, and Enter confirms an armed code mod. "Install from URL" and Open run `fno config plugins install` or `open` in a pane. The pane splits to the right of the focused pane. The view switches to the terminal once that pane exists. When the pane cannot open, the notice names the exit code and the first line the launcher printed, and the view stays. The app never installs by itself: the pane shows what the pack runs and asks you, as the CLI does. "Browse published packs" reads `fno config plugins gallery --json` into Available, and a click on a pack fills the URL field. Typed text and cmd-v go to the URL field while it is open. Escape closes it, and an armed code-mod confirm stays. To write a mod pack of your own, follow [Author a mod pack](guides/author-a-mod.md).

The window follows the macOS appearance. A Mac in dark mode gets footnote-superscript. A Mac in light mode gets footnote-paper. When the system switches, the window repaints with no restart. A `mux.theme` pin in your config wins over the appearance, as it does in the TUI, so a pinned dark theme stays dark. Programs inside panes keep the colors the mux server gave them. A pane that paints its own dark ground stays dark in a light window.

Release the mouse on a button, a row or an option to act on it. Release somewhere else to cancel. A notice shows in the bottom corner for 4 seconds.

## Keys

Views have no key: click Chat, HQ, Mods or Terminal in the bar. Cmd-K or Help > Go to… opens the go-to palette. It finds an agent, workspace, node, question or view by name, and Enter goes there. Cmd-/ or Help > Keyboard Shortcuts opens the key sheet. It lists every key the app reads. Esc, `q`, cmd-/ or a click outside closes it.

In the terminal view the mux prefix works as in the TUI. Ctrl-b, or whatever `mux.prefix` in your fno config names, starts a mux action. The next key resolves it. Ctrl-b 1-9 picks a tab. Ctrl-b ctrl-b sends one ctrl-b to the pane. Ctrl-b arrows focus panes, ctrl resizes, and shift moves a pane. Ctrl-b x closes a pane, the semicolon chord returns to the last pane, and ctrl-b c opens a tab. Ctrl-b d detaches and quits the app. An unbound key after the prefix beeps, names the key in a notice, and sends nothing. The key sheet lists the prefix under its own section.

The prefix belongs to the mux. A Claude Code ctrl-b pressed in a focused pane therefore never reaches the pane's program, the same as inside the terminal TUI. To hand ctrl-b back to the pane, set `mux.prefix` in your fno config, for example `ctrl+a`, and restart the app. Ctrl-a is the shell's line-start key, so a shell pane loses that chord to the prefix. An alt prefix does not fire in the app: option composes a text character first.

In the terminal view, other keys go to the focused pane as a terminal sends them. The app keeps these cmd shortcuts for itself:

| Shortcut | Action |
|---|---|
| cmd-d | Split right |
| cmd-shift-d | Split down |
| cmd-] / cmd-[ | Next pane / previous pane |
| cmd-t | New tab |
| cmd-w | Close pane |
| cmd-c | Copy the focused pane's selection |
| cmd-v | Paste (bracketed when the pane asks for it) |
| cmd-f | Find in the focused pane (terminal view), or in the chat (chat view) |
| cmd-1 to cmd-9 | In the terminal view, the session's first nine tabs. In other views, the first nine agent rows. Hold cmd to see the numbers. |
| cmd-k | Open or close the go-to palette |
| cmd-/ | Open or close the key sheet |
| cmd-, | Open or close Settings |
| cmd-q | Detach and quit |

Cmd-f opens the find bar over the focused view. Typing edits the query. Enter searches in the terminal. The chat scan runs live. Up steps toward the older match. Down steps toward the newer match. The chords ⌘G and ⇧⌘G step to the newer and older match, as in other Mac find bars. Esc closes the bar and clears the highlight. The terminal search reaches the pane's whole server-side scrollback, so a co-viewer sees the jump and the highlight. The counter reads `3/12`, or `no matches`. In the chat the scan runs in the app: matches tint in place and the view follows the current match.

Typed text goes through the macOS input system, so dead keys, option-compose and IME input work. Option on a letter types the composed character, as in Terminal.app. Option on an arrow or backspace still sends the meta form.

## Settings

Cmd-comma or fno > Settings… opens Settings: one panel over the window, in the key sheet's look. It has five sections: Look, Alerts, Machines, Keys and Updates. Esc or a click outside closes it, and a cmd key closes it and runs.

Type to filter: a query matches a row's label, its dotted config key and its section name. The search runs across every section at once. Enter on a boolean flips it. Enter on a short choice steps it to the next value. A row ending in `›` opens a choice page: one numbered line per option, the check on the current value, Enter confirms, Esc cancels. A row you cannot change here dims with the reason under it, and Enter on it writes nothing.

Look holds the theme pin, the terminal font, the font size and the first view. The theme row's page offers Follow macOS plus the built-in themes. Confirming repaints the window at once, the way the appearance observer does. The font page lists the installed monospaced families your Mac has. Font and size apply to the panes live. The chrome (the title bar, lists and labels) stays SF Mono at 12pt. The first-view row writes `mux.launch_screen`, the key the mux reads too. Its three values: last view (the default, the window reopens where you left it), agents and hq.

When an agent turns blocked or done outside the window, the app fires three signals. Alerts holds their switches: the sound, the macOS notification and the Dock badge. They are the `app.alert_*` keys, separate from the daemon's `mux.notify_*` knobs, which govern the TUI.

Machines lists the fleet's hosts read-only: add or connect a host from the mux. Keys restates the key sheet read-only: keys are fixed. Updates shows the app's version. Updates arrive with the release.

Rows write through the same door `fno config set` runs, into the per-user global file, because a Finder-launched app has no project to pin to. A key a project-local file decides already dims with its path. The global write cannot beat it. A refused write shows the CLI's first line under the row and keeps the old value.

The flags still win: `--font`, `--font-size` and `--mode` override the config rows for that launch. Without a flag, the launch ladder is the config value, then the saved view, then the default.

## Mouse

A click on a pane focuses it. A drag selects text, and the release copies it. The wheel scrolls the pane's history, and `[+N]` at the top right shows how many lines up the pane is. A click on a border or a gap does nothing.

## Measured performance

Both clients read the same mux server, so the two columns measure the clients, not the data source. One throwaway server holds 30 idle agent panes, one 10,000-line pane and one busy pane. Each cell is p50 over p90 from 20 runs. Timed rows read in milliseconds. The idle row reads in CPU percent and KB.

The harness is `scripts/ops/mux-client-bench.sh` in the fno repo. Its report carries the full provenance: host, macOS and binary versions, date and world shape. Failed runs are named, never averaged in.

| Metric | TUI | fno-app | How it is measured |
|---|---|---|---|
| Launch to first frame | pending | pending | TUI: first output byte on a fresh pty attach. fno-app: one cold offscreen `--shot` frame, wall time, png encode included. |
| Keystroke to paint, busy pane | pending | unmeasured | TUI: a marker keystroke until its echo paints. The app needs synthetic keyboard events plus screen capture, both gated by macOS permissions. No number until a run with those grants. |
| Switch focused agent | pending | unmeasured | TUI: `mux view` flip until the new pane paints. The app drains its control file on a 400ms poll and acks nothing, so the flip is visible only through screen capture. |
| Scroll a 10,000-line pane | pending | unmeasured | TUI: page-up presses in scroll mode until the paint settles. The app needs synthetic scroll-wheel events plus screen capture. |
| Idle CPU / RSS, 30 agents | pending | pending | `ps` sampling at 1s for 30s against the same world. |

The unmeasured cells are honest gaps, not absences of a client. The app's input and paint are observable only through macOS accessibility and screen-recording grants. The bench holds neither grant yet. A run with them fills the cells with the same p50 over p90 shape.
