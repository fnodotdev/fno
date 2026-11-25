//! Keystrokes to the bytes a pane's PTY expects, plus the cmd shortcuts the
//! app keeps for itself. Pure: the window turns a gpui keystroke into a
//! [`Key`] and sends whatever comes back.

use fno::proto::Command;
use fno::tree::Dir;

/// The focused pane's input modes, tracked from `ModeSync` bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modes {
    /// DECCKM (`?1`): arrows send `ESC O` instead of `ESC [`.
    pub app_cursor: bool,
    /// `?2004`: a paste is wrapped in `ESC [200~ ... ESC [201~`.
    pub bracketed_paste: bool,
}

impl Modes {
    /// Apply every `ESC [ ? <n> h|l` in a `ModeSync` byte string. The full
    /// number is parsed, so `?1004h` never reads as `?1h`.
    pub fn apply(&mut self, bytes: &[u8]) {
        let mut i = 0;
        while i + 3 < bytes.len() {
            if &bytes[i..i + 3] != b"\x1b[?" {
                i += 1;
                continue;
            }
            let start = i + 3;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            let on = match bytes.get(end) {
                Some(b'h') => true,
                Some(b'l') => false,
                _ => {
                    i = end;
                    continue;
                }
            };
            match &bytes[start..end] {
                b"1" => self.app_cursor = on,
                b"2004" => self.bracketed_paste = on,
                _ => {}
            }
            i = end + 1;
        }
    }
}

/// One keystroke, as the window reads it from gpui.
#[derive(Debug, Clone, Default)]
pub struct Key<'a> {
    /// The key name: `a`, `up`, `enter`, `f5`, ...
    pub key: &'a str,
    /// The text the key would type, with shift and the layout applied.
    pub key_char: Option<&'a str>,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub cmd: bool,
}

/// What a cmd shortcut does. Cmd keys never reach the pane.
#[derive(Debug, Clone, PartialEq)]
pub enum Shortcut {
    Command(Command),
    Paste,
    /// Cut and select-all act on the chat composer only.
    Cut,
    SelectAll,
    Quit,
    /// cmd-1..9 picks that tab in the terminal view, else that agent row.
    Pick(u8),
    /// cmd-n: the launch composer, over whatever view is showing.
    NewAgent,
    /// cmd-f: the find bar over the focused pane, or over the chat.
    Find,
    /// cmd-g steps to the newer match, shift-cmd-g to the older: the
    /// standard find chords, while the bar is open.
    FindStep {
        older: bool,
    },
    /// ⌥⌘b: show or hide the session panel.
    TogglePanel,
}

pub fn shortcut(k: &Key) -> Option<Shortcut> {
    if !k.cmd {
        return None;
    }
    if let Ok(n @ 1..=9) = k.key.parse::<u8>() {
        // Views have no key: the bar's view group switches them.
        return (!k.alt && !k.shift).then_some(Shortcut::Pick(n));
    }
    // ⌘⌥-arrows resize the focused pane. cmd keeps the key out of the
    // pane's bytes, alt keeps it out of the composer's cmd-arrows (line and
    // draft movement); ctrl+arrows stay terminal input, word jumps in a shell.
    if k.cmd && k.alt {
        let dir = match k.key {
            "b" => return Some(Shortcut::TogglePanel),
            "left" => Dir::Left,
            "right" => Dir::Right,
            "up" => Dir::Up,
            "down" => Dir::Down,
            _ => return None,
        };
        return Some(Shortcut::Command(Command::ResizeDir(dir)));
    }
    Some(match (k.key, k.shift) {
        ("d", false) => Shortcut::Command(Command::SplitDir(Dir::Right)),
        ("d", true) => Shortcut::Command(Command::SplitDir(Dir::Down)),
        ("]", _) => Shortcut::Command(Command::NextPane),
        ("[", _) => Shortcut::Command(Command::LastPane),
        ("t", _) => Shortcut::Command(Command::NewTab),
        ("w", _) => Shortcut::Command(Command::ClosePane),
        ("c", _) => Shortcut::Command(Command::CopySelection),
        ("v", _) => Shortcut::Paste,
        ("x", _) => Shortcut::Cut,
        ("a", _) => Shortcut::SelectAll,
        ("f", false) => Shortcut::Find,
        ("g", false) => Shortcut::FindStep { older: false },
        ("g", true) => Shortcut::FindStep { older: true },
        ("q", _) => Shortcut::Quit,
        ("n", false) => Shortcut::NewAgent,
        _ => return None,
    })
}

/// The key that opens and closes the key sheet. Help > Keyboard Shortcuts
/// runs the same action. Ctrl-b is the mux prefix here, as in the terminal
/// client: the terminal view claims it before pane input.
pub const SHEET_KEY: &str = "cmd-/";

/// The key that opens and closes the go-to palette. Help > Go to… runs the
/// same action.
pub const GOTO_KEY: &str = "cmd-k";

/// The key that opens and closes the Settings panel. fno > Settings…
/// runs the same action.
pub const SETTINGS_KEY: &str = "cmd-,";

/// Keys bound to gpui actions, not to `shortcut()`: the key sheet's own
/// test skips them, as it skips ⌘/ today.
pub const ACTION_KEYS: &[&str] = &["/", "k", ","];

/// The key sheet: every key the app reads, by section, as the sheet shows
/// them.
pub const SHEET: &[(&str, &[(&str, &str)])] = &[
    (
        "global",
        &[
            ("⌘/", "show this sheet"),
            ("⌘n", "start a session or a lead"),
            ("⌘k", "go to an agent, session, node, question or view"),
            ("⌘1-⌘9", "pick an agent row, or a tab in the terminal"),
            ("hold ⌘", "show the numbers"),
            ("⌘c", "copy"),
            ("⌘v", "paste"),
            ("⌘f", "find in the pane or chat"),
            ("⌘q", "detach and quit"),
            ("⌘,", "settings"),
            ("⌥⌘b", "show or hide the session panel"),
        ],
    ),
    (
        "terminal",
        &[
            ("⌘d", "split right"),
            ("⇧⌘d", "split down"),
            ("⌘]", "next pane"),
            ("⌘[", "previous pane"),
            ("⌘t", "new tab"),
            ("⌘w", "close pane"),
            ("⌘⌥→ ⌘⌥↓", "grow the focused pane"),
            ("⌘⌥← ⌘⌥↑", "shrink the focused pane"),
            ("drag a split line", "resize both panes"),
        ],
    ),
    (
        "mux prefix",
        &[
            ("ctrl-b", "the mux prefix; the next key runs a mux action"),
            ("ctrl-b 1-9", "pick a tab"),
            ("ctrl-b ctrl-b", "send ctrl-b to the pane"),
            ("ctrl-b arrows", "focus panes; ctrl resizes, shift moves"),
            ("ctrl-b x ; c", "close pane, last pane, new tab"),
            ("ctrl-b d", "detach and quit"),
            ("ctrl-b [ ]", "jump to the previous or next command block"),
            ("ctrl-b pgup", "scroll mode: j k pgup pgdn g G, q leaves"),
            ("ctrl-b ,", "rename the tab"),
            ("ctrl-b P", "row menu for the focused agent"),
            ("ctrl-b Z", "focus the next agent waiting on you"),
        ],
    ),
    (
        "find",
        &[
            ("⏎", "search"),
            ("↑", "older match"),
            ("↓", "newer match"),
            ("⌘g", "newer match"),
            ("⇧⌘g", "older match"),
            ("esc", "close find"),
        ],
    ),
    (
        "chat",
        &[
            ("⏎", "send"),
            ("⇧⏎", "new line"),
            ("⌘x", "cut"),
            ("⌘a", "select all"),
            ("⌥← ⌥→", "move by word"),
            ("⌘← ⌘→", "line start, line end"),
            ("⌘↑ ⌘↓", "draft start, draft end"),
            ("esc", "close a detail sheet"),
        ],
    ),
    (
        "mods",
        &[
            ("↑ ↓", "move"),
            ("space", "install or uninstall"),
            ("o", "open"),
            ("i", "install from a URL"),
            ("⏎", "confirm"),
            ("esc", "cancel"),
        ],
    ),
];

/// The xterm modifier parameter: 1 + shift + 2*alt + 4*ctrl.
fn modifier_param(k: &Key) -> u8 {
    1 + k.shift as u8 + 2 * k.alt as u8 + 4 * k.ctrl as u8
}

/// A cursor-style key (`ESC [ X` / `ESC O X`, or `ESC [ 1 ; m X` with mods).
fn cursor_key(k: &Key, modes: Modes, final_byte: char) -> Vec<u8> {
    let m = modifier_param(k);
    if m > 1 {
        format!("\x1b[1;{m}{final_byte}").into_bytes()
    } else if modes.app_cursor {
        format!("\x1bO{final_byte}").into_bytes()
    } else {
        format!("\x1b[{final_byte}").into_bytes()
    }
}

/// A tilde key (`ESC [ n ~`, or `ESC [ n ; m ~` with mods).
fn tilde_key(k: &Key, n: u8) -> Vec<u8> {
    let m = modifier_param(k);
    if m > 1 {
        format!("\x1b[{n};{m}~").into_bytes()
    } else {
        format!("\x1b[{n}~").into_bytes()
    }
}

/// The scanner holds a prefix, a chord candidate or a tab number: the next
/// keystroke, text included, belongs to it.
pub fn chord_in_flight(scanner: &fno::ui::keys::Scanner) -> bool {
    scanner.prefix_pending() || scanner.chord_pending() || scanner.digits_pending()
}

/// True for a key that types text: the window leaves it to the platform
/// input handler, so dead keys, option-compose and IME compose before the
/// text reaches the pane. Ctrl, cmd and named keys are encoded here.
pub fn is_text(k: &Key) -> bool {
    !k.cmd
        && !k.ctrl
        && named(k, Modes::default()).is_none()
        && (k.key_char.is_some_and(|t| !t.is_empty()) || k.key == "space")
}

fn named(k: &Key, modes: Modes) -> Option<Vec<u8>> {
    match k.key {
        "enter" => Some(b"\r".to_vec()),
        "tab" if k.shift => Some(b"\x1b[Z".to_vec()),
        "tab" => Some(b"\t".to_vec()),
        "backspace" => Some(vec![0x7f]),
        "escape" => Some(vec![0x1b]),
        "up" => Some(cursor_key(k, modes, 'A')),
        "down" => Some(cursor_key(k, modes, 'B')),
        "right" => Some(cursor_key(k, modes, 'C')),
        "left" => Some(cursor_key(k, modes, 'D')),
        "home" => Some(cursor_key(k, modes, 'H')),
        "end" => Some(cursor_key(k, modes, 'F')),
        "insert" => Some(tilde_key(k, 2)),
        "delete" => Some(tilde_key(k, 3)),
        "pageup" => Some(tilde_key(k, 5)),
        "pagedown" => Some(tilde_key(k, 6)),
        "f1" => Some(b"\x1bOP".to_vec()),
        "f2" => Some(b"\x1bOQ".to_vec()),
        "f3" => Some(b"\x1bOR".to_vec()),
        "f4" => Some(b"\x1bOS".to_vec()),
        "f5" => Some(tilde_key(k, 15)),
        "f6" => Some(tilde_key(k, 17)),
        "f7" => Some(tilde_key(k, 18)),
        "f8" => Some(tilde_key(k, 19)),
        "f9" => Some(tilde_key(k, 20)),
        "f10" => Some(tilde_key(k, 21)),
        "f11" => Some(tilde_key(k, 23)),
        "f12" => Some(tilde_key(k, 24)),
        _ => None,
    }
    .map(|mut bytes| {
        // Alt on a plain named key is ESC-prefixed; cursor and tilde keys
        // already carry alt in their modifier parameter.
        if k.alt && bytes.len() == 1 {
            bytes.insert(0, 0x1b);
        }
        bytes
    })
}

/// The bytes for one non-cmd keystroke, or `None` for a key that types
/// nothing (a bare modifier, an unknown function key). Option on a text key
/// composes through the input handler, so it is never meta here.
pub fn encode(k: &Key, modes: Modes) -> Option<Vec<u8>> {
    if k.cmd {
        return None;
    }
    if let Some(bytes) = named(k, modes) {
        return Some(bytes);
    }
    let mut out = Vec::new();
    if k.ctrl {
        let byte = match k.key {
            "space" | "@" | "2" => 0x00,
            "[" => 0x1b,
            "\\" => 0x1c,
            "]" => 0x1d,
            "^" | "6" => 0x1e,
            "_" | "-" => 0x1f,
            s if s.len() == 1 && s.as_bytes()[0].is_ascii_alphabetic() => {
                s.as_bytes()[0].to_ascii_lowercase() & 0x1f
            }
            _ => return None,
        };
        if k.alt {
            out.push(0x1b);
        }
        out.push(byte);
        return Some(out);
    }
    let text = match k.key_char {
        Some(t) if !t.is_empty() => t,
        _ if k.key == "space" => " ",
        _ => return None,
    };
    Some(text.as_bytes().to_vec())
}

/// Paste text as the pane expects it: newlines become `\r` (what Enter
/// sends), and with bracketed paste on, the text is wrapped and any embedded
/// end marker is stripped so a paste can never close its own bracket.
pub fn paste(text: &str, modes: Modes) -> Vec<u8> {
    let body = text.replace("\r\n", "\r").replace('\n', "\r");
    if !modes.bracketed_paste {
        return body.into_bytes();
    }
    let body = body.replace("\x1b[201~", "");
    let mut out = b"\x1b[200~".to_vec();
    out.extend(body.bytes());
    out.extend(b"\x1b[201~");
    out
}

/// What one scanned mux event does in this app. The scanner is the mux's
/// own ([`fno::ui::keys::Scanner`]), so both front doors keep one key
/// semantic; the app claims the prefix at the same layer the terminal
/// client does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MuxAction {
    /// Bytes that still reach the pane: pass-through, the prefix-prefix
    /// literal, a released lone Esc.
    Input(Vec<u8>),
    /// A wire command: split, focus, resize, swap, zoom, pane and tab moves.
    Command(Command),
    /// A 1-based tab ordinal the window resolves against the active strip.
    Tab(usize),
    /// prefix+d detaches, the cmd-q arm.
    Quit,
    /// An unbound key after the prefix: beep, name the key, forward nothing.
    Bell,
    /// A key with no app surface yet: name it in the notice, forward nothing.
    Notice(String),
    /// prefix+`[` / `]` / v / r: walk, select or rerun the focused pane's
    /// command blocks over the wire.
    Block(BlockOp),
    /// prefix+PgUp: keyboard scroll mode on the focused pane.
    ScrollMode,
    /// prefix+`,`: the rename-tab prompt for the active tab.
    Rename,
    /// prefix+P: the focused agent's row menu.
    RowMenu,
    /// prefix+Z: focus the next pane whose agent waits on the user.
    NextWaiting,
    /// A fleet overlay chord: the app's own surface for it opens.
    Overlay(Overlay),
}

/// The app surface a mux overlay chord opens in place of the TUI overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    /// The go-to palette: the selector and the navigator.
    GoTo,
    /// The Inbox view: questions, decisions and the activity feed.
    Inbox,
    /// The command center board: backlog, team and org.
    Board,
}

/// One command-block action the wire carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockOp {
    Jump(fno::proto::BlockDir),
    Select(fno::proto::BlockDir),
    Rerun,
}

/// Translate one scanner event. Forward, wire commands, tab ordinals,
/// detach, the five overlay events and the fleet overlay chords run app-side;
/// every other client-local overlay event has no app surface yet and becomes
/// a notice naming it, so a prefix chord never leaks into the pane.
pub fn translate(event: fno::ui::keys::Event) -> MuxAction {
    use fno::ui::keys::Event;
    match event {
        Event::Forward(chunk) => MuxAction::Input(chunk),
        Event::Cmd(c) => MuxAction::Command(c),
        Event::SelectTabIdx(n) => MuxAction::Tab(n),
        Event::Detach => MuxAction::Quit,
        Event::Bell => MuxAction::Bell,
        Event::BlockJump(dir) => MuxAction::Block(BlockOp::Jump(dir)),
        Event::BlockSelect(dir) => MuxAction::Block(BlockOp::Select(dir)),
        Event::BlockRerun => MuxAction::Block(BlockOp::Rerun),
        Event::ScrollModeOpen => MuxAction::ScrollMode,
        Event::OpenRename => MuxAction::Rename,
        Event::OpenSidelineMenu => MuxAction::RowMenu,
        Event::FocusNextBlocked => MuxAction::NextWaiting,
        Event::OpenSelector | Event::OpenNav => MuxAction::Overlay(Overlay::GoTo),
        Event::OpenAnswers | Event::OpenDecisions | Event::OpenFeed | Event::FocusFeed => {
            MuxAction::Overlay(Overlay::Inbox)
        }
        Event::OpenBacklogBoard | Event::OpenTeam | Event::OpenOrgView => {
            MuxAction::Overlay(Overlay::Board)
        }
        other => MuxAction::Notice(format!("{other:?} has no app action yet")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> Key<'_> {
        Key {
            key: name,
            ..Key::default()
        }
    }

    #[test]
    fn cmd_digits_pick_a_row_views_have_no_key_and_encode_maps_ctrl_b() {
        let cmd = |k| Key {
            key: k,
            cmd: true,
            ..Key::default()
        };
        let opt = |k| Key {
            alt: true,
            ..cmd(k)
        };
        assert_eq!(shortcut(&cmd("1")), Some(Shortcut::Pick(1)));
        assert_eq!(shortcut(&cmd("9")), Some(Shortcut::Pick(9)));
        assert_eq!(shortcut(&opt("1")), None);
        assert_eq!(shortcut(&opt("b")), Some(Shortcut::TogglePanel));
        assert_eq!(shortcut(&cmd("x")), Some(Shortcut::Cut));
        assert_eq!(shortcut(&cmd("a")), Some(Shortcut::SelectAll));
        assert_eq!(shortcut(&cmd("f")), Some(Shortcut::Find));
        let ctrl = |k| Key {
            key: k,
            ctrl: true,
            ..Key::default()
        };
        assert_eq!(encode(&ctrl("b"), Modes::default()), Some(vec![0x02]));
        // Every cmd-letter the sheet lists is a shortcut the app runs;
        // ⌘/ and ⌘k are the sheet's and palette's own action bindings.
        for (_, rows) in SHEET {
            for (keys, _) in *rows {
                let letter = keys
                    .strip_prefix('⌘')
                    .filter(|l| l.len() == 1 && !ACTION_KEYS.contains(l));
                let Some(letter) = letter else {
                    continue;
                };
                assert!(shortcut(&cmd(letter)).is_some(), "{keys} runs nothing");
            }
        }
    }

    #[test]
    fn arrows_follow_deckcm_from_modesync() {
        let mut modes = Modes::default();
        assert_eq!(encode(&key("up"), modes).unwrap(), b"\x1b[A");
        // ?1004h (focus events) must not read as ?1h.
        modes.apply(b"\x1b[?1004h\x1b[?1h");
        assert!(modes.app_cursor);
        assert_eq!(encode(&key("up"), modes).unwrap(), b"\x1bOA");
        modes.apply(b"\x1b[?1l");
        assert_eq!(encode(&key("up"), modes).unwrap(), b"\x1b[A");
        let ctrl_c = Key {
            key: "c",
            ctrl: true,
            ..Key::default()
        };
        assert_eq!(encode(&ctrl_c, modes).unwrap(), vec![0x03]);
        // Option on a text key composes through the input handler; named
        // and ctrl keys never do.
        let option_bang = Key {
            key: "1",
            key_char: Some("⁄"),
            alt: true,
            shift: true,
            ..Key::default()
        };
        assert!(is_text(&option_bang));
        assert!(!is_text(&ctrl_c) && !is_text(&key("up")));
        let alt_back = Key {
            key: "backspace",
            alt: true,
            ..Key::default()
        };
        assert_eq!(encode(&alt_back, modes).unwrap(), b"\x1b\x7f");
        let cmd_d = Key {
            key: "d",
            cmd: true,
            ..Key::default()
        };
        assert_eq!(encode(&cmd_d, modes), None);
        assert_eq!(
            shortcut(&cmd_d),
            Some(Shortcut::Command(Command::SplitDir(Dir::Right)))
        );
    }

    #[test]
    fn cmd_alt_arrows_resize_and_the_other_arrows_stay_where_they_were() {
        fn mk(key: &str, cmd: bool, alt: bool) -> Key<'_> {
            Key {
                key,
                cmd,
                alt,
                ..Key::default()
            }
        }
        assert_eq!(
            shortcut(&mk("right", true, true)),
            Some(Shortcut::Command(Command::ResizeDir(Dir::Right)))
        );
        assert_eq!(
            shortcut(&mk("down", true, true)),
            Some(Shortcut::Command(Command::ResizeDir(Dir::Down)))
        );
        assert_eq!(
            shortcut(&mk("left", true, true)),
            Some(Shortcut::Command(Command::ResizeDir(Dir::Left)))
        );
        assert_eq!(
            shortcut(&mk("up", true, true)),
            Some(Shortcut::Command(Command::ResizeDir(Dir::Up)))
        );
        // cmd alone stays the composer's line and draft movement.
        assert_eq!(shortcut(&mk("left", true, false)), None);
        // ctrl+arrow stays terminal input: a modifier-param cursor sequence.
        let ctrl_left = Key {
            key: "left",
            ctrl: true,
            ..Key::default()
        };
        assert_eq!(
            encode(&ctrl_left, Modes::default()),
            Some(b"\x1b[1;5D".to_vec())
        );
    }

    #[test]
    fn paste_wraps_only_in_bracketed_mode() {
        let mut modes = Modes::default();
        assert_eq!(paste("a\nb", modes), b"a\rb");
        modes.apply(b"\x1b[?2004h");
        assert_eq!(paste("a\nb", modes), b"\x1b[200~a\rb\x1b[201~");
    }

    /// The key after the prefix is a text key, which
    /// the window receives through the input handler. That path must hand it
    /// to the scanner while a chord is in flight, and only then.
    #[test]
    fn a_text_key_after_the_prefix_belongs_to_the_scanner() {
        use fno::ui::keys::{Event, Scanner};
        use std::time::Instant;
        let mut scanner = Scanner::default();
        assert!(!chord_in_flight(&scanner), "plain typing stays pane input");
        scanner.scan(&[0x02], Instant::now());
        assert!(chord_in_flight(&scanner), "the prefix claims the next key");
        let events = scanner.scan(b"c", Instant::now());
        assert!(
            matches!(events.as_slice(), [Event::Cmd(_)]),
            "prefix+c runs a command, got {events:?}"
        );
        assert!(!chord_in_flight(&scanner), "the chord resolved");
        scanner.scan(&[0x02], Instant::now());
        scanner.scan(b"3", Instant::now());
        assert!(
            chord_in_flight(&scanner),
            "a held tab number still owns the next key"
        );
    }

    /// The app_prefix contract: the shared scanner claims the prefix before
    /// any pane input, and translate maps the actionable arms. `u` is
    /// unbound in the default table, so a post-prefix u is the Bell arm.
    #[test]
    fn app_prefix_claims_chords_and_translate_maps_the_arms() {
        use fno::ui::keys::{Event, Scanner};
        use std::time::Instant;
        // Ctrl+B alone is pending: no events, nothing to forward.
        let mut scanner = Scanner::default();
        assert!(scanner.scan(&[0x02], Instant::now()).is_empty());
        // prefix-prefix forwards exactly one literal prefix byte.
        assert_eq!(
            scanner.scan(&[0x02], Instant::now()),
            vec![Event::Forward(vec![0x02])]
        );
        // Ctrl+B then 3 holds the tab number (tab 34 stays reachable) and
        // forwards nothing; the quiet-window flush selects tab 3.
        let mut scanner = Scanner::default();
        scanner.scan(&[0x02], Instant::now());
        assert!(scanner.scan(&[b'3'], Instant::now()).is_empty());
        assert!(scanner.digits_pending());
        assert_eq!(scanner.flush_digits(), Some(Event::SelectTabIdx(3)));
        // Ctrl+B then an unbound key is one Bell and no forward bytes.
        let mut scanner = Scanner::default();
        scanner.scan(&[0x02], Instant::now());
        assert_eq!(scanner.scan(&[b'u'], Instant::now()), vec![Event::Bell]);
        // A lone Esc held as a chord candidate still reaches the pane once
        // the quiet window releases it.
        let mut scanner = Scanner::default();
        assert!(scanner.scan(&[0x1b], Instant::now()).is_empty());
        let released = scanner.flush_chord().unwrap();
        assert_eq!(translate(released), MuxAction::Input(vec![0x1b]));
        // The translate map: every arm the app runs, and the notice arms.
        assert_eq!(
            translate(Event::Cmd(Command::NextPane)),
            MuxAction::Command(Command::NextPane)
        );
        assert_eq!(translate(Event::SelectTabIdx(3)), MuxAction::Tab(3));
        assert_eq!(translate(Event::Detach), MuxAction::Quit);
        assert_eq!(
            translate(Event::Forward(vec![0x02])),
            MuxAction::Input(vec![0x02])
        );
        assert_eq!(translate(Event::Bell), MuxAction::Bell);
        assert_eq!(
            translate(Event::OpenSelector),
            MuxAction::Overlay(Overlay::GoTo)
        );
        assert_eq!(
            translate(Event::OpenAnswers),
            MuxAction::Overlay(Overlay::Inbox)
        );
        assert_eq!(
            translate(Event::OpenBacklogBoard),
            MuxAction::Overlay(Overlay::Board)
        );
        assert!(matches!(translate(Event::OpenReview), MuxAction::Notice(_)));
        // The five overlay events each run an app arm, never a notice.
        use fno::proto::BlockDir;
        assert_eq!(
            translate(Event::BlockJump(BlockDir::Prev)),
            MuxAction::Block(BlockOp::Jump(BlockDir::Prev))
        );
        assert_eq!(translate(Event::ScrollModeOpen), MuxAction::ScrollMode);
        assert_eq!(translate(Event::OpenRename), MuxAction::Rename);
        assert_eq!(translate(Event::OpenSidelineMenu), MuxAction::RowMenu);
        assert_eq!(translate(Event::FocusNextBlocked), MuxAction::NextWaiting);
    }
}
