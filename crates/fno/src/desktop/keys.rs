//! Keystrokes to the bytes a pane's PTY expects, plus the cmd shortcuts the
//! app keeps for itself. Pure: the window turns a gpui keystroke into a
//! [`Key`] and sends whatever comes back.

use crate::proto::Command;
use crate::tree::Dir;

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
    Quit,
    /// cmd-1 shows the terminal, cmd-2 the orchestration board.
    View(u8),
}

pub fn shortcut(k: &Key) -> Option<Shortcut> {
    if !k.cmd {
        return None;
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
        ("q", _) => Shortcut::Quit,
        ("1", _) => Shortcut::View(1),
        ("2", _) => Shortcut::View(2),
        _ => return None,
    })
}

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
    fn paste_wraps_only_in_bracketed_mode() {
        let mut modes = Modes::default();
        assert_eq!(paste("a\nb", modes), b"a\rb");
        modes.apply(b"\x1b[?2004h");
        assert_eq!(paste("a\nb", modes), b"\x1b[200~a\rb\x1b[201~");
    }
}
