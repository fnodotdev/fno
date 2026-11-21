//! The mux overlay chords the app runs: keyboard scroll mode, the rename-tab
//! prompt, the focused agent's row menu and the walk to the next waiting
//! agent. Each sends the same wire messages as the TUI client, so a prefix
//! chord does the same thing in both front doors. `window.rs` owns the
//! sockets and the keyboard routing.

use gpui::{div, prelude::*, px, rgb, Div, SharedString, Stateful};

use crate::keys::Key;
use crate::link::Layout;
use crate::palette::Palette;
use crate::ui::{self, LineField};
use crate::window::{Action, MainView, Root};
use fno::app_api::client::{agent_lattice_state, agent_sort::sideline_order};
use fno::app_api::lattice::LatticeState;
use fno::proto::{AgentRow, ClientMsg, Command, MAX_TAB_NAME};

/// What one key does while scroll mode holds the keyboard.
pub(super) enum Step {
    Send(ClientMsg),
    Leave,
    /// Claimed and dropped: nothing typed in the mode reaches the pane.
    Swallow,
    /// A cmd key keeps its shortcut.
    Pass,
}

/// The TUI's key map: j/k line, pgup/pgdn page, ctrl-u/d half, g/G ends,
/// q or esc leaves. Positive deltas scroll toward older history.
pub(super) fn scroll_step(k: &Key, pane: u64, rows: i32) -> Step {
    if k.cmd {
        return Step::Pass;
    }
    let rows = rows.max(1);
    let half = (rows / 2).max(1);
    let by = |delta| Step::Send(ClientMsg::PaneScroll { pane, delta });
    let edge = |top| Step::Send(ClientMsg::PaneScrollEdge { pane, top });
    match (k.ctrl, k.key) {
        (true, "u") => by(half),
        (true, "d") => by(-half),
        (true, _) => Step::Swallow,
        (_, "escape" | "q") => Step::Leave,
        (_, "k" | "up") => by(1),
        (_, "j" | "down") => by(-1),
        (_, "pageup") => by(rows),
        (_, "pagedown") => by(-rows),
        (_, "g") => edge(!k.shift),
        (_, "G") => edge(false),
        _ => Step::Swallow,
    }
}

/// The shared look of the floating one-line bars.
fn float(p: &Palette) -> Div {
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
}

pub(super) fn scroll_bar(p: &Palette) -> Div {
    float(p)
        .absolute()
        .bottom(px(8.0))
        .right(px(12.0))
        .text_color(rgb(p.fg))
        .child("scroll · j/k line · pgup/pgdn page · ctrl-u/d half · g/G ends · q leaves")
}

/// The rename-tab prompt. The tab is resolved to its stable id when it
/// opens, so a tab switch mid-edit cannot retarget the send.
#[derive(Debug)]
pub(super) struct Rename {
    tab: u64,
    pub field: LineField,
}

impl Rename {
    /// The active tab's prompt, seeded with its chosen name when it has one.
    pub(super) fn open(layout: &Layout) -> Option<Self> {
        let squad = layout.squads.iter().find(|s| s.id == layout.active_squad)?;
        let tab = squad.tabs.get(squad.active_tab)?;
        let seed = if tab.named { tab.name.as_str() } else { "" };
        Some(Rename {
            tab: tab.id,
            field: LineField::seeded(seed, MAX_TAB_NAME),
        })
    }

    /// A blank name is valid: the server reverts the tab to its derived label.
    pub(super) fn command(&self) -> Command {
        Command::RenameTab {
            tab: self.tab,
            name: self.field.text().to_string(),
        }
    }

    pub(super) fn bar(&self, p: &Palette) -> Div {
        div()
            .absolute()
            .top(px(6.0))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                float(p)
                    .child(div().text_color(rgb(p.muted)).child("rename tab"))
                    .child(
                        div()
                            .text_color(rgb(p.fg))
                            .child(SharedString::from(format!("{}▏", self.field.text()))),
                    )
                    .child(ui::kbd("enter sets · blank resets · esc cancels", p)),
            )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum MenuAct {
    Run(Action),
    Rename,
}

/// The row menu of the agent in the focused pane.
#[derive(Debug, Clone)]
pub(super) struct RowMenu {
    title: String,
    pub items: Vec<(&'static str, MenuAct)>,
    pub selected: usize,
}

pub(super) enum MenuKey {
    Move(i32),
    Run(usize),
    Close,
    Swallow,
}

impl RowMenu {
    /// A bare pane keeps the tab and layout rows; an agent pane adds its own.
    pub(super) fn open(agents: &[AgentRow], focus: u64) -> Self {
        let agent = agents.iter().find(|a| a.pane_id == Some(focus));
        let mut items = Vec::new();
        if let Some(a) = agent {
            items.push(("Open chat", MenuAct::Run(Action::Show(MainView::Chat))));
            items.push(("Copy name", MenuAct::Run(Action::Copy(a.name.clone()))));
            if let Some(node) = &a.node {
                items.push(("Copy node id", MenuAct::Run(Action::Copy(node.clone()))));
            }
        }
        items.push(("Zoom pane", MenuAct::Run(Action::Send(Command::ZoomPane))));
        items.push(("Rename tab", MenuAct::Rename));
        RowMenu {
            title: agent.map_or_else(|| "pane".into(), |a| a.name.clone()),
            items,
            selected: 0,
        }
    }

    pub(super) fn key(&self, k: &Key) -> MenuKey {
        let len = self.items.len();
        match k.key {
            "escape" | "q" => MenuKey::Close,
            "enter" => MenuKey::Run(self.selected),
            "up" | "k" => MenuKey::Move(-1),
            "down" | "j" => MenuKey::Move(1),
            d => match d.parse::<usize>() {
                Ok(n) if (1..=len).contains(&n) => MenuKey::Run(n - 1),
                _ => MenuKey::Swallow,
            },
        }
    }

    pub(super) fn step(&mut self, by: i32) {
        let len = self.items.len() as i32;
        self.selected = (self.selected as i32 + by).rem_euclid(len.max(1)) as usize;
    }

    pub(super) fn view(&self, root: &Root, cx: &mut gpui::Context<Root>) -> Stateful<Div> {
        let p = root.palette;
        let rows = self.items.iter().enumerate().map(|(i, (label, _))| {
            let fill = if i == self.selected {
                p.row_fill
            } else {
                p.panel
            };
            div()
                .id(("row-menu-item", i))
                .flex()
                .gap_2()
                .px_2()
                .py(px(2.0))
                .cursor_pointer()
                .bg(rgb(fill))
                .hover(move |s| s.bg(rgb(p.hover)))
                .on_click(cx.listener(move |root, _, _, cx| root.row_menu_run(i, cx)))
                .child(div().text_color(rgb(p.faint)).child(format!("{}", i + 1)))
                .child(*label)
        });
        div()
            .id("row-menu")
            .absolute()
            .top(px(ui::TITLE_H + 8.0))
            .left(px(crate::left_pane::WIDTH + 12.0))
            .min_w(px(220.0))
            .flex()
            .flex_col()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(p.border))
            .bg(rgb(p.panel))
            .font_family(ui::MONO)
            .text_size(px(ui::BODY))
            .text_color(rgb(p.fg))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .px_2()
                    .py(px(3.0))
                    .text_color(rgb(p.accent))
                    .child(SharedString::from(self.title.clone())),
            )
            .children(rows)
            .child(ui::kbd("↑↓ move · enter or 1-9 runs · esc closes", &p).px_2())
    }
}

/// The waiting agent after the focused pane, in the sideline's order and
/// wrapping: its pane, name, 1-based place and the waiting count.
pub(super) fn next_waiting(
    agents: &[AgentRow],
    focus: u64,
    now: u64,
) -> Option<(u64, String, usize, usize)> {
    let waiting: Vec<(u64, &str)> = sideline_order(agents, now)
        .into_iter()
        .filter(|a| agent_lattice_state(a) == LatticeState::Blocked)
        .filter_map(|a| a.pane_id.map(|p| (p, a.name.as_str())))
        .collect();
    let panes: Vec<u64> = waiting.iter().map(|(p, _)| *p).collect();
    let at = cycle_after(&panes, focus)?;
    let (pane, name) = waiting.get(at)?;
    Some((*pane, (*name).to_string(), at + 1, waiting.len()))
}

/// The index after `focus`, wrapping; the first when focus is not listed.
fn cycle_after(panes: &[u64], focus: u64) -> Option<usize> {
    let at = panes.iter().position(|p| *p == focus);
    (!panes.is_empty()).then(|| at.map_or(0, |i| (i + 1) % panes.len()))
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
    fn scroll_keys_send_the_shared_offset_moves_and_swallow_the_rest() {
        let delta = |k: Key, rows| match scroll_step(&k, 7, rows) {
            Step::Send(ClientMsg::PaneScroll { pane: 7, delta }) => delta,
            _ => panic!("{} sent no PaneScroll", k.key),
        };
        let ctrl = |name| Key {
            ctrl: true,
            ..key(name)
        };
        assert_eq!((delta(key("k"), 40), delta(key("j"), 40)), (1, -1));
        assert_eq!(delta(key("pageup"), 40), 40);
        assert_eq!((delta(ctrl("u"), 40), delta(ctrl("d"), 1)), (20, -1));
        let shift_g = Key {
            shift: true,
            ..key("g")
        };
        let top = |k: Key| {
            matches!(
                scroll_step(&k, 7, 9),
                Step::Send(ClientMsg::PaneScrollEdge { top: true, .. })
            )
        };
        assert!(top(key("g")) && !top(shift_g));
        assert!(matches!(scroll_step(&key("q"), 7, 9), Step::Leave));
        assert!(matches!(scroll_step(&key("x"), 7, 9), Step::Swallow));
        let cmd_k = Key {
            cmd: true,
            ..key("k")
        };
        assert!(matches!(scroll_step(&cmd_k, 7, 9), Step::Pass));
    }

    #[test]
    fn a_bare_pane_menu_keeps_the_tab_rows_and_digits_pick_a_row() {
        let mut menu = RowMenu::open(&[], 3);
        let labels: Vec<&str> = menu.items.iter().map(|(l, _)| *l).collect();
        assert_eq!(labels, ["Zoom pane", "Rename tab"]);
        assert!(matches!(menu.key(&key("2")), MenuKey::Run(1)));
        assert!(matches!(menu.key(&key("3")), MenuKey::Swallow));
        menu.step(-1);
        assert_eq!(menu.selected, 1);
        menu.step(1);
        assert_eq!(menu.selected, 0);
    }

    #[test]
    fn a_rename_names_its_tab_and_the_waiting_walk_wraps() {
        let mut r = Rename {
            tab: 4,
            field: LineField::seeded("api", MAX_TAB_NAME),
        };
        r.field.edit("x", Some("x"));
        assert_eq!(
            r.command(),
            Command::RenameTab {
                tab: 4,
                name: "apix".into()
            }
        );
        assert_eq!(cycle_after(&[], 1), None);
        assert_eq!(cycle_after(&[4, 6, 9], 9), Some(0));
        assert_eq!(cycle_after(&[4, 6, 9], 2), Some(0));
    }
}
