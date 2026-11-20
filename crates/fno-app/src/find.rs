//! The find bar over the focused pane or the chat: its state, the keys it
//! takes, and its one-line paint. The terminal half drives the mux server's
//! in-scrollback search submit-then-step, the TUI's model: the query edits
//! locally, the first Enter opens the search, later Enters and Shift+Enters
//! step the server's matches. The chat half is a client-side scan
//! ([`chat::find`]), so its matches stay live while the agent streams.

use std::ops::Range;

use gpui::{div, px, rgb, Div, ParentElement, SharedString, Styled};

use crate::chat;
use crate::keys::{self, Key};
use crate::palette::Palette;
use crate::ui;
use crate::window::MainView;
use fno::proto::BlockDir;

/// The query cap, the TUI's search limit.
const QUERY_MAX: usize = 256;

/// What the bar searches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The focused pane's server-side scrollback.
    Pane(u64),
    /// The shown conversation.
    Chat,
}

/// The find bar's state, whatever it searches.
#[derive(Debug)]
pub struct Find {
    pub query: String,
    pub target: Target,
    /// The last server reply: `3/12`. Chat find fills both from its scan.
    pub total: u32,
    /// 1-based; 0 is none.
    pub current: u32,
    /// A search landed since the last query edit (terminal). The counter
    /// hides until then.
    pub searched: bool,
    /// Chat matches, oldest row first.
    pub hits: Vec<chat::find::ChatMatch>,
}

impl Find {
    pub fn new(target: Target) -> Self {
        Find {
            query: String::new(),
            target,
            total: 0,
            current: 0,
            searched: false,
            hits: Vec::new(),
        }
    }

    /// Append typed text, capped at the TUI's limit. An edit takes the last
    /// result back: the counter hides until the next Enter (terminal) or the
    /// re-scan the window runs (chat).
    pub fn edit(&mut self, text: &str) {
        let len = self.query.chars().count();
        self.query
            .extend(text.chars().take(QUERY_MAX.saturating_sub(len)));
        self.edited();
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.edited();
    }

    fn edited(&mut self) {
        self.searched = false;
        self.total = 0;
        self.current = 0;
        self.hits.clear();
    }

    /// The counter text: none while the query has no results yet, `no
    /// matches` on a zero reply, else `current/total`.
    pub fn counter(&self) -> Option<String> {
        if !self.searched || self.query.is_empty() {
            return None;
        }
        Some(if self.total == 0 {
            "no matches".into()
        } else {
            format!("{}/{}", self.current, self.total)
        })
    }
}

/// Enter walks to the older match, Shift+Enter to the newer: the same
/// directions the server steps `Prev`/`Next` in, and the same the chat scan
/// keeps (its hits are oldest first).
pub fn step_dir(shift: bool) -> BlockDir {
    if shift {
        BlockDir::Next
    } else {
        BlockDir::Prev
    }
}

/// The keys the bar consumes while it is open. Cmd keys keep their
/// shortcuts; arrows and every other key keep its pane and chat routing.
#[derive(Debug, Clone, PartialEq)]
pub enum FindKey {
    Edit(String),
    Backspace,
    Enter { shift: bool },
    Escape,
}

pub fn key(k: &Key) -> Option<FindKey> {
    if k.cmd {
        return None;
    }
    match k.key {
        "enter" => Some(FindKey::Enter { shift: k.shift }),
        "escape" => Some(FindKey::Escape),
        "backspace" => Some(FindKey::Backspace),
        // is_text also admits space by key name: its key_char can be absent,
        // and a dropped Edit would send the space into the pane instead.
        _ if keys::is_text(k) => Some(FindKey::Edit(
            k.key_char.map(str::to_string).unwrap_or_else(|| " ".into()),
        )),
        _ => None,
    }
}

/// Whether the bar paints over `main` now: its target's view shows. The
/// window closes the bar whenever that stops, so this is a mount check.
pub fn over_view(f: &Find, main: MainView) -> bool {
    match (&f.target, main) {
        (Target::Pane(_), MainView::Terminal) | (Target::Chat, MainView::Chat) => true,
        _ => false,
    }
}

/// The painted match ranges for one chat row's `block`: `(byte range, is
/// the current match)`. A range past the text it lands in never paints (a
/// table's cells share one block index; each cell filters to itself).
pub fn chat_ranges(
    f: Option<&Find>,
    item: usize,
    block: usize,
    text_len: usize,
) -> Vec<(Range<usize>, bool)> {
    let Some(f) = f else {
        return Vec::new();
    };
    if !matches!(f.target, Target::Chat) {
        return Vec::new();
    }
    let current = f
        .current
        .checked_sub(1)
        .and_then(|c| usize::try_from(c).ok());
    f.hits
        .iter()
        .enumerate()
        .filter(|(_, h)| h.item == item && h.block == block && h.range.end <= text_len)
        .map(|(i, h)| (h.range.clone(), Some(i) == current))
        .collect()
}

/// The one-line bar over the view's top right: query, caret, counter and
/// the keys. Raised, border, muted and accent only: the app's existing
/// terminal-native look.
pub fn bar(f: &Find, p: Palette, picking: bool) -> Div {
    let counter = f.counter().map(|c| {
        let color = if f.total > 0 { p.accent } else { p.muted };
        div().flex_none().text_color(rgb(color)).child(c)
    });
    div()
        .absolute()
        .top(px(6.0))
        .right(px(6.0))
        .max_w(px(520.0))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .py(px(3.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(rgb(p.border))
        .bg(rgb(p.raised))
        .font_family(ui::MONO)
        .text_size(px(ui::BODY))
        .child(div().flex_none().text_color(rgb(p.muted)).child("find"))
        .child(
            div()
                .min_w_0()
                .text_color(rgb(p.fg))
                .child(SharedString::from(format!("{}▏", f.query))),
        )
        .children(counter)
        // Badge rule: the keys show only while cmd is held.
        .children(picking.then(|| {
            div()
                .flex_none()
                .text_color(rgb(p.muted))
                .child("⏎ older  ⇧⏎ newer  esc")
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::find::ChatMatch;

    fn mk<'a>(name: &'a str, key_char: Option<&'a str>, cmd: bool, shift: bool) -> Key<'a> {
        Key {
            key: name,
            key_char,
            cmd,
            shift,
            ..Key::default()
        }
    }

    #[test]
    fn enter_walks_older_and_shift_enter_walks_newer() {
        assert_eq!(step_dir(false), BlockDir::Prev);
        assert_eq!(step_dir(true), BlockDir::Next);
    }

    #[test]
    fn the_bar_takes_text_enter_and_escape_and_leaves_the_rest_routed() {
        assert_eq!(
            key(&mk("f", Some("f"), false, false)),
            Some(FindKey::Edit("f".into()))
        );
        assert_eq!(
            key(&mk("space", Some(" "), false, false)),
            Some(FindKey::Edit(" ".into()))
        );
        // A space keystroke whose key_char went absent still edits: the
        // fallback keeps it out of the pane.
        assert_eq!(
            key(&mk("space", None, false, false)),
            Some(FindKey::Edit(" ".into()))
        );
        assert_eq!(
            key(&mk("enter", None, false, false)),
            Some(FindKey::Enter { shift: false })
        );
        assert_eq!(
            key(&mk("enter", None, false, true)),
            Some(FindKey::Enter { shift: true })
        );
        assert_eq!(
            key(&mk("escape", None, false, false)),
            Some(FindKey::Escape)
        );
        assert_eq!(
            key(&mk("backspace", None, false, false)),
            Some(FindKey::Backspace)
        );
        // Cmd keys keep their shortcuts, arrows keep their pane routing, and
        // a ctrl combo never edits the query.
        assert_eq!(key(&mk("f", Some("f"), true, false)), None);
        assert_eq!(key(&mk("up", None, false, false)), None);
        let ctrl = Key {
            key: "a",
            ctrl: true,
            ..Key::default()
        };
        assert_eq!(key(&ctrl), None);
    }

    #[test]
    fn edits_cap_at_256_and_the_counter_hides_until_a_search_lands() {
        let mut f = Find::new(Target::Chat);
        let chunk = "x".repeat(100);
        f.edit(&chunk);
        f.edit(&chunk);
        f.edit(&chunk);
        assert_eq!(f.query.chars().count(), QUERY_MAX);
        assert!(!f.searched);
        assert_eq!(f.counter(), None);
        f.searched = true;
        f.total = 2;
        f.current = 2;
        assert_eq!(f.counter().as_deref(), Some("2/2"));
        f.total = 0;
        f.current = 0;
        assert_eq!(f.counter().as_deref(), Some("no matches"));
        // An edit takes the result back even at the cap.
        f.edit("y");
        assert_eq!(f.query.chars().count(), QUERY_MAX);
        assert_eq!(f.counter(), None);
    }

    #[test]
    fn chat_ranges_flag_only_the_current_match_inside_its_text() {
        let mut f = Find::new(Target::Chat);
        f.hits = vec![
            ChatMatch {
                item: 1,
                block: 0,
                range: 0..4,
            },
            ChatMatch {
                item: 1,
                block: 0,
                range: 6..12,
            },
            ChatMatch {
                item: 2,
                block: 1,
                range: 0..4,
            },
        ];
        f.total = 3;
        f.current = 2;
        assert_eq!(
            chat_ranges(Some(&f), 1, 0, 20),
            vec![(0..4, false), (6..12, true)]
        );
        // A range past the drawn text never paints.
        assert!(chat_ranges(Some(&f), 2, 1, 2).is_empty());
        // No bar, or a pane target: chat rows paint nothing.
        assert!(chat_ranges(None, 1, 0, 20).is_empty());
        let mut pane = Find::new(Target::Pane(7));
        pane.hits = f.hits.clone();
        pane.total = 3;
        pane.current = 1;
        assert!(chat_ranges(Some(&pane), 1, 0, 20).is_empty());
    }
}
