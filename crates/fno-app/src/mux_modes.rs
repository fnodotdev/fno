//! The mux overlay events the app runs: keyboard scroll mode, the rename-tab
//! prompt, the focused agent's row menu and the walk to the next agent that
//! waits on the user. Each one is the TUI client's own behaviour over the
//! same wire messages, so a prefix chord does the same thing in both front
//! doors. The state and the pure decisions live here; `window.rs` owns the
//! sockets and the keyboard routing.

use gpui::{div, prelude::*, px, rgb, Div, SharedString, Stateful};

use crate::keys::Key;
use crate::palette::Palette;
use crate::ui::{self, LineField};
use crate::window::{Action, MainView, Root};
use fno::app_api::client::agent_lattice_state;
use fno::app_api::lattice::LatticeState;
use fno::proto::{AgentRow, ClientMsg, Command, Frame, MAX_TAB_NAME};

/// What one key does while scroll mode holds the keyboard. Every non-cmd key
/// is claimed, so nothing typed in the mode reaches the pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScrollKey {
    /// j/k and the arrows: one line, positive toward older history.
    Lines(i32),
    /// PgUp/PgDn: one page.
    Pages(i32),
    /// Ctrl-u/Ctrl-d: half a page.
    Halves(i32),
    /// g jumps to the oldest line, G to the live bottom.
    Edge { top: bool },
    /// q and Esc.
    Leave,
    /// Any other key: swallowed.
    Ignore,
}

/// The mode's key map, the TUI's: `None` only for a cmd key, which keeps its
/// shortcut.
pub(super) fn scroll_key(k: &Key) -> Option<ScrollKey> {
    if k.cmd {
        return None;
    }
    if k.ctrl {
        return Some(match k.key {
            "u" => ScrollKey::Halves(1),
            "d" => ScrollKey::Halves(-1),
            _ => ScrollKey::Ignore,
        });
    }
    Some(match k.key {
        "escape" | "q" => ScrollKey::Leave,
        "k" | "up" => ScrollKey::Lines(1),
        "j" | "down" => ScrollKey::Lines(-1),
        "pageup" => ScrollKey::Pages(1),
        "pagedown" => ScrollKey::Pages(-1),
        "g" if k.shift => ScrollKey::Edge { top: false },
        "G" => ScrollKey::Edge { top: false },
        "g" => ScrollKey::Edge { top: true },
        _ => ScrollKey::Ignore,
    })
}

/// The wire message one scroll key sends for a pane showing `rows` lines.
pub(super) fn scroll_msg(key: ScrollKey, pane: u64, rows: i32) -> Option<ClientMsg> {
    let rows = rows.max(1);
    let half = (rows / 2).max(1);
    match key {
        ScrollKey::Lines(n) => Some(ClientMsg::PaneScroll { pane, delta: n }),
        ScrollKey::Pages(n) => Some(ClientMsg::PaneScroll {
            pane,
            delta: n * rows,
        }),
        ScrollKey::Halves(n) => Some(ClientMsg::PaneScroll {
            pane,
            delta: n * half,
        }),
        ScrollKey::Edge { top } => Some(ClientMsg::PaneScrollEdge { pane, top }),
        ScrollKey::Leave | ScrollKey::Ignore => None,
    }
}

/// The readout line: the viewport's top line, 1-based, of the pane's whole
/// history. At the live bottom it reads `total - rows + 1`; at the oldest
/// line it reads 1.
pub(super) fn scroll_readout(frame: Option<&Frame>) -> (i64, i64) {
    frame
        .map(|f| {
            let total = i64::from(f.history_lines);
            let n = total - i64::from(f.rows) - i64::from(f.scroll_offset) + 1;
            (n.max(1), total)
        })
        .unwrap_or((1, 1))
}

pub(super) fn scroll_bar(p: &Palette, readout: (i64, i64)) -> Div {
    div()
        .absolute()
        .bottom(px(8.0))
        .right(px(12.0))
        .max_w(px(640.0))
        .px_2()
        .py(px(3.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(rgb(p.border))
        .bg(rgb(p.raised))
        .font_family(ui::MONO)
        .text_size(px(ui::BODY))
        .text_color(rgb(p.fg))
        .child(SharedString::from(format!(
            "scroll · line {} of {} · j/k line · pgup/pgdn page · ctrl-u/d half · g/G ends · q leaves",
            readout.0, readout.1
        )))
}

/// The rename-tab prompt: the tab it targets, resolved to its stable id when
/// the prompt opened, so a tab switch mid-edit cannot retarget the send.
#[derive(Debug)]
pub(super) struct Rename {
    pub tab: u64,
    pub field: LineField,
}

impl Rename {
    pub(super) fn new(tab: u64, current: Option<&str>) -> Self {
        Rename {
            tab,
            field: LineField::seeded(current.unwrap_or_default(), MAX_TAB_NAME),
        }
    }

    /// The command Enter sends. A blank name is valid: the server reverts the
    /// tab to its derived label.
    pub(super) fn command(&self) -> Command {
        Command::RenameTab {
            tab: self.tab,
            name: self.field.text().to_string(),
        }
    }
}

/// The active tab's stable id and its chosen name, when it has one.
pub(super) fn active_tab(layout: &crate::link::Layout) -> Option<(u64, Option<String>)> {
    let squad = layout.squads.iter().find(|s| s.id == layout.active_squad)?;
    let tab = squad.tabs.get(squad.active_tab)?;
    Some((tab.id, tab.named.then(|| tab.name.clone())))
}

pub(super) fn rename_bar(r: &Rename, p: &Palette) -> Div {
    div()
        .absolute()
        .top(px(6.0))
        .left(px(0.0))
        .right(px(0.0))
        .flex()
        .justify_center()
        .child(
            div()
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
                .child(
                    div()
                        .flex_none()
                        .text_color(rgb(p.muted))
                        .child("rename tab"),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_color(rgb(p.fg))
                        .child(SharedString::from(format!("{}▏", r.field.text()))),
                )
                .child(ui::kbd("enter sets · blank resets · esc cancels", p)),
        )
}

/// What one row-menu item does.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum MenuAct {
    Run(Action),
    Rename,
}

/// The focused agent's row menu: its title and the rows Enter or a click
/// runs.
#[derive(Debug, Clone)]
pub(super) struct RowMenu {
    pub title: String,
    pub items: Vec<(String, MenuAct)>,
    pub selected: usize,
}

/// The menu for the pane that has focus. An agent pane gets the agent's
/// rows; a bare pane keeps the tab and layout rows.
pub(super) fn row_menu(agents: &[AgentRow], focus: u64) -> RowMenu {
    let agent = agents.iter().find(|a| a.pane_id == Some(focus));
    let mut items: Vec<(String, MenuAct)> = Vec::new();
    if let Some(a) = agent {
        items.push((
            "Open chat".into(),
            MenuAct::Run(Action::Show(MainView::Chat)),
        ));
        items.push((
            "Copy name".into(),
            MenuAct::Run(Action::Copy(a.name.clone())),
        ));
        if let Some(node) = a.node.as_deref() {
            items.push((
                "Copy node id".into(),
                MenuAct::Run(Action::Copy(node.to_string())),
            ));
        }
    }
    items.push((
        "Zoom pane".into(),
        MenuAct::Run(Action::Send(Command::ZoomPane)),
    ));
    items.push(("Rename tab".into(), MenuAct::Rename));
    RowMenu {
        title: agent.map_or_else(|| "pane".to_string(), |a| a.name.clone()),
        items,
        selected: 0,
    }
}

/// What a key does to the open row menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MenuKey {
    Move(i32),
    Run(usize),
    Close,
    Ignore,
}

pub(super) fn menu_key(k: &Key, selected: usize, len: usize) -> MenuKey {
    if k.cmd || k.ctrl {
        return MenuKey::Ignore;
    }
    match k.key {
        "escape" | "q" => MenuKey::Close,
        "enter" => MenuKey::Run(selected),
        "up" | "k" => MenuKey::Move(-1),
        "down" | "j" => MenuKey::Move(1),
        d if d.len() == 1 => match d.parse::<usize>() {
            Ok(n) if (1..=len).contains(&n) => MenuKey::Run(n - 1),
            _ => MenuKey::Ignore,
        },
        _ => MenuKey::Ignore,
    }
}

pub(super) fn step(selected: usize, delta: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let at = selected as i64 + i64::from(delta);
    at.rem_euclid(len as i64) as usize
}

pub(super) fn menu_view(
    root: &Root,
    menu: &RowMenu,
    cx: &mut gpui::Context<Root>,
) -> Stateful<Div> {
    let p = root.palette;
    let mut body = div().flex().flex_col();
    for (i, (label, _)) in menu.items.iter().enumerate() {
        let on = i == menu.selected;
        body = body.child(
            div()
                .id(("row-menu-item", i))
                .flex()
                .flex_row()
                .gap_2()
                .px_2()
                .py(px(2.0))
                .cursor_pointer()
                .bg(rgb(if on { p.row_fill } else { p.panel }))
                .hover(move |s| s.bg(rgb(p.hover)))
                .text_color(rgb(p.fg))
                .on_click(cx.listener(move |root, _, _, cx| root.row_menu_run(i, cx)))
                .child(
                    div()
                        .flex_none()
                        .text_color(rgb(p.faint))
                        .child(format!("{}", i + 1)),
                )
                .child(label.clone()),
        );
    }
    div()
        .id("row-menu")
        .absolute()
        .top(px(ui::TITLE_H + 8.0))
        .left(px(crate::left_pane::WIDTH + 12.0))
        .min_w(px(220.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(p.border))
        .bg(rgb(p.panel))
        .font_family(ui::MONO)
        .text_size(px(ui::BODY))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .px_2()
                .py(px(3.0))
                .text_color(rgb(p.accent))
                .child(SharedString::from(menu.title.clone())),
        )
        .child(body)
        .child(
            ui::role(div(), ui::Role::Hint, &p)
                .px_2()
                .py(px(3.0))
                .child("↑↓ move · enter or 1-9 runs · esc closes"),
        )
}

/// The panes whose agent waits on the user, in the sideline's order, and
/// the one after `focus` (wrapping). `None` when nothing waits.
pub(super) fn next_waiting(
    agents: &[AgentRow],
    focus: u64,
    now: u64,
) -> Option<(u64, String, usize, usize)> {
    let waiting: Vec<(u64, &str)> = fno::app_api::client::agent_sort::sideline_order(agents, now)
        .into_iter()
        .filter(|a| agent_lattice_state(a) == LatticeState::Blocked)
        .filter_map(|a| a.pane_id.map(|p| (p, a.name.as_str())))
        .collect();
    let panes: Vec<u64> = waiting.iter().map(|(p, _)| *p).collect();
    let at = cycle_after(&panes, focus)?;
    let (pane, name) = waiting.get(at)?;
    Some((*pane, (*name).to_string(), at + 1, waiting.len()))
}

/// The index after `focus` in `panes`, wrapping; the first when focus is not
/// in the list; `None` for an empty list.
pub(super) fn cycle_after(panes: &[u64], focus: u64) -> Option<usize> {
    if panes.is_empty() {
        return None;
    }
    Some(
        panes
            .iter()
            .position(|p| *p == focus)
            .map_or(0, |i| (i + 1) % panes.len()),
    )
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
    fn scroll_keys_match_the_tui_map_and_swallow_the_rest() {
        assert_eq!(scroll_key(&key("k")), Some(ScrollKey::Lines(1)));
        assert_eq!(scroll_key(&key("j")), Some(ScrollKey::Lines(-1)));
        assert_eq!(scroll_key(&key("pageup")), Some(ScrollKey::Pages(1)));
        assert_eq!(scroll_key(&key("g")), Some(ScrollKey::Edge { top: true }));
        let shift_g = Key {
            shift: true,
            ..key("g")
        };
        assert_eq!(scroll_key(&shift_g), Some(ScrollKey::Edge { top: false }));
        let ctrl_u = Key {
            ctrl: true,
            ..key("u")
        };
        assert_eq!(scroll_key(&ctrl_u), Some(ScrollKey::Halves(1)));
        assert_eq!(scroll_key(&key("q")), Some(ScrollKey::Leave));
        assert_eq!(scroll_key(&key("x")), Some(ScrollKey::Ignore));
        let cmd_k = Key {
            cmd: true,
            ..key("k")
        };
        assert_eq!(scroll_key(&cmd_k), None);
    }

    #[test]
    fn scroll_moves_ride_the_shared_offset_in_lines_pages_and_halves() {
        let delta = |k, rows| match scroll_msg(k, 7, rows) {
            Some(ClientMsg::PaneScroll { pane: 7, delta }) => delta,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(delta(ScrollKey::Lines(-1), 40), -1);
        assert_eq!(delta(ScrollKey::Pages(1), 40), 40);
        assert_eq!(delta(ScrollKey::Halves(-1), 40), -20);
        // A one-row pane still moves a line per half-page key.
        assert_eq!(delta(ScrollKey::Halves(1), 1), 1);
        assert!(scroll_msg(ScrollKey::Leave, 7, 40).is_none());
    }

    #[test]
    fn the_readout_reads_one_at_the_oldest_line() {
        let f = Frame {
            rows: 10,
            history_lines: 100,
            scroll_offset: 90,
            ..Frame::default()
        };
        assert_eq!(scroll_readout(Some(&f)), (1, 100));
        let live = Frame {
            scroll_offset: 0,
            ..f
        };
        assert_eq!(scroll_readout(Some(&live)), (91, 100));
        assert_eq!(scroll_readout(None), (1, 1));
    }

    #[test]
    fn a_rename_sends_the_typed_name_to_the_tab_it_opened_on() {
        let mut r = Rename::new(4, Some("api"));
        r.field.edit("x", Some("x"));
        assert_eq!(
            r.command(),
            Command::RenameTab {
                tab: 4,
                name: "apix".into()
            }
        );
        r.field.clear();
        assert_eq!(
            r.command(),
            Command::RenameTab {
                tab: 4,
                name: String::new()
            }
        );
    }

    #[test]
    fn the_menu_keys_move_wrap_pick_by_digit_and_close() {
        assert_eq!(menu_key(&key("down"), 0, 3), MenuKey::Move(1));
        assert_eq!(menu_key(&key("2"), 0, 3), MenuKey::Run(1));
        assert_eq!(menu_key(&key("9"), 0, 3), MenuKey::Ignore);
        assert_eq!(menu_key(&key("enter"), 2, 3), MenuKey::Run(2));
        assert_eq!(menu_key(&key("escape"), 0, 3), MenuKey::Close);
        assert_eq!(step(0, -1, 3), 2);
        assert_eq!(step(2, 1, 3), 0);
        assert_eq!(step(0, 1, 0), 0);
    }

    #[test]
    fn a_bare_pane_menu_keeps_the_tab_rows() {
        let menu = row_menu(&[], 3);
        assert_eq!(menu.title, "pane");
        let labels: Vec<&str> = menu.items.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["Zoom pane", "Rename tab"]);
    }

    #[test]
    fn the_waiting_walk_goes_after_the_focus_and_wraps() {
        assert_eq!(cycle_after(&[], 1), None);
        assert_eq!(cycle_after(&[4, 6, 9], 4), Some(1));
        assert_eq!(cycle_after(&[4, 6, 9], 9), Some(0));
        // A focus outside the waiting set lands on the first waiting pane.
        assert_eq!(cycle_after(&[4, 6, 9], 2), Some(0));
    }
}
