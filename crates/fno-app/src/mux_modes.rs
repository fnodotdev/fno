//! The mux overlay chords the app runs: keyboard scroll mode, the rename-tab
//! prompt, the focused agent's row menu and the walk to the next waiting
//! agent. Each sends the same wire messages as the TUI client, so a prefix
//! chord does the same thing in both front doors. `window.rs` owns the
//! sockets and the keyboard routing.

use gpui::{div, prelude::*, px, rgb, Div, SharedString};

use crate::keys::Key;
use crate::link::Layout;
use crate::palette::Palette;
use crate::pane_seam;
use crate::ui::{self, LineField};
use crate::window::{Action, MainView, Root};
use fno::app_api::client::{agent_lattice_state, agent_sort::sideline_order};
use fno::app_api::lattice::LatticeState;
use fno::proto::{AgentRow, ClientMsg, Command, Dir, PanePlacement, PaneTarget, MAX_TAB_NAME};

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

/// Keyboard history scroll with no mode held. Shift-pgup and shift-pgdn
/// page, and cmd-up and cmd-down jump to the oldest line and back to live.
/// A bare pgup, pgdn or End scrolls only while the pane already shows
/// history, so a full-screen program keeps those keys on the live screen.
pub(super) fn history_step(k: &Key, pane: u64, rows: i32, scrolled: bool) -> Option<ClientMsg> {
    if k.ctrl || k.alt {
        return None;
    }
    let rows = rows.max(1);
    let by = |delta| Some(ClientMsg::PaneScroll { pane, delta });
    let edge = |top| Some(ClientMsg::PaneScrollEdge { pane, top });
    match (k.cmd, k.shift, k.key) {
        (true, false, "up") => edge(true),
        (true, false, "down") => edge(false),
        (false, true, "pageup") => by(rows),
        (false, true, "pagedown") => by(-rows),
        (false, false, "pageup") if scrolled => by(rows),
        (false, false, "pagedown") if scrolled => by(-rows),
        (false, false, "end") if scrolled => edge(false),
        _ => None,
    }
}

/// The pill that sits over a pane showing history: a click, End or cmd-down
/// goes back to the live screen.
pub(super) fn back_to_live(p: &Palette, cx: &mut gpui::Context<Root>) -> gpui::Stateful<Div> {
    float(p)
        .id("back-to-live")
        .absolute()
        .bottom(px(8.0))
        .right(px(12.0))
        .text_color(rgb(p.fg))
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(p.hover)))
        .on_click(cx.listener(|root, _, _, cx| root.back_to_live(cx)))
        .child("↓ Back to live")
        .child(ui::kbd(format!("End or {}↓", ui::cmd_key()), p).text_color(rgb(p.muted)))
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
        .border_color(rgb(p.faint))
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
    /// Set for an agent's fno label; the tab id is then unused.
    pub agent: Option<String>,
    pub field: LineField,
    /// A double-click on a tab opens the field in the tab itself; the chord
    /// keeps the floating bar.
    pub inline: bool,
}

impl Rename {
    /// The active tab's prompt, seeded with its chosen name when it has one.
    pub(super) fn open(layout: &Layout) -> Option<Self> {
        let squad = layout.squads.iter().find(|s| s.id == layout.active_squad)?;
        let tab = squad.tabs.get(squad.active_tab)?;
        Some(Self::seeded(tab, false))
    }

    /// The prompt for one tab by id, as a double-click names it.
    pub(super) fn open_tab(layout: &Layout, id: u64) -> Option<Self> {
        let squad = layout.squads.iter().find(|s| s.id == layout.active_squad)?;
        let tab = squad.tabs.iter().find(|t| t.id == id)?;
        Some(Self::seeded(tab, true))
    }

    fn seeded(tab: &fno::proto::TabMeta, inline: bool) -> Self {
        let seed = if tab.named { tab.name.as_str() } else { "" };
        Rename {
            tab: tab.id,
            agent: None,
            field: LineField::seeded(seed, MAX_TAB_NAME),
            inline,
        }
    }

    pub(super) fn tab(&self) -> u64 {
        self.tab
    }

    /// The prompt for one agent's fno label, seeded with the current one.
    pub(super) fn for_agent(name: String) -> Self {
        Rename {
            tab: 0,
            field: LineField::seeded(&name, MAX_TAB_NAME),
            agent: Some(name),
            inline: false,
        }
    }

    /// A blank name is valid: the server reverts the tab to its derived label.
    pub(super) fn command(&self) -> Command {
        if let Some(name) = &self.agent {
            return Command::RenameAgent {
                name: name.clone(),
                new_name: self.field.text().to_string(),
            };
        }
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
                    .child(
                        div()
                            .text_color(rgb(p.muted))
                            .child(if self.agent.is_some() {
                                "rename agent (and its harness session where it can)"
                            } else {
                                "rename tab"
                            }),
                    )
                    .child(
                        div()
                            .text_color(rgb(p.fg))
                            .child(SharedString::from(format!("{}▏", self.field.text()))),
                    )
                    .child(
                        ui::kbd("enter sets · blank resets · esc cancels", p)
                            .text_color(rgb(p.muted)),
                    ),
            )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum MenuAct {
    Run(Vec<Action>),
    /// The rename prompt: the active tab, or one agent's fno label.
    Rename(Option<String>),
    /// The in-tab rename field of one tab.
    RenameTab(u64),
    /// Ask the server for the agent's recent transcript.
    Peek(String),
    /// A `fno-agent` call off the UI loop: its argv and the notice on success.
    Shell(Vec<String>, String),
}

/// One row-menu entry: label, action, and whether a rule is drawn above it.
type MenuItem = (&'static str, MenuAct, bool);

/// The row menu of one agent: the mux's row menu without Mail, so the two
/// front doors offer one set. Tab actions live on a tab, not here.
#[derive(Debug, Clone)]
pub(super) struct RowMenu {
    title: String,
    pub items: Vec<MenuItem>,
    pub selected: usize,
    /// Where a right-click opened it; the chord opens it at the corner.
    pub anchor: Option<(f32, f32)>,
}

pub(super) enum MenuKey {
    Move(i32),
    Run(usize),
    Close,
    Swallow,
}

impl RowMenu {
    /// The menu of the agent in the focused pane.
    pub(super) fn open(agents: &[AgentRow], focus: u64) -> Self {
        Self::of(agents.iter().find(|a| a.pane_id == Some(focus)))
    }

    /// A tab's menu: the tab actions the agent menu leaves out.
    pub(super) fn of_tab(squad: u64, tab: u64) -> Self {
        let send = |c| MenuAct::Run(vec![Action::Send(c)]);
        let step = |delta| Command::ReorderTab { squad, tab, delta };
        let close = MenuAct::Run(vec![
            Action::Send(Command::SelectTab(tab)),
            Action::Send(Command::CloseTab),
        ]);
        let items = vec![
            ("Rename tab", MenuAct::RenameTab(tab), false),
            ("Move left", send(step(-1)), false),
            ("Move right", send(step(1)), false),
            ("Close tab", close, true),
        ];
        RowMenu::new("tab".into(), items)
    }

    /// An agent's rows, gated as the mux gates them: a pane-hosted row moves
    /// its pane, a paneless one attaches, an exited one resumes. A bare pane
    /// keeps the tab rows.
    pub(super) fn of(agent: Option<&AgentRow>) -> Self {
        let mut items: Vec<MenuItem> = Vec::new();
        let Some(a) = agent else {
            let zoom = MenuAct::Run(vec![Action::Send(Command::ZoomPane)]);
            items.push(("Zoom pane", zoom, false));
            items.push(("Rename tab", MenuAct::Rename(None), false));
            return RowMenu::new("pane".into(), items);
        };
        let send = |c| MenuAct::Run(vec![Action::Send(c)]);
        let to = |split| PanePlacement {
            target: PaneTarget::CurrentRoute,
            split,
            ..PanePlacement::default()
        };
        let click = crate::left_pane::agent_row_action(a);
        let chat = MenuAct::Run(vec![click, Action::Show(MainView::Chat)]);
        items.push(("Open chat", chat, false));
        match (a.pane_id, a.exited) {
            (Some(pane), false) => {
                items.push(("New tab", send(Command::BreakPane { pane }), false));
                for (label, dir) in [("Split right", Dir::Right), ("Split down", Dir::Down)] {
                    let mv = Command::MovePane {
                        mover: Some(pane),
                        target: None,
                        dir,
                    };
                    items.push((label, send(mv), false));
                }
            }
            (None, false) => {
                let here = PanePlacement {
                    here: true,
                    ..to(None)
                };
                for (label, placement) in [
                    ("Open here", here),
                    ("New tab", to(None)),
                    ("Split right", to(Some(Dir::Right))),
                    ("Split down", to(Some(Dir::Down))),
                ] {
                    if let Some(id) = a.attach_id.clone() {
                        items.push((label, send(Command::AttachAgent { id, placement }), false));
                    }
                }
            }
            _ => {}
        }
        items.push(("Peek", MenuAct::Peek(a.name.clone()), true));
        let diff = Command::ToggleDiffPane {
            agent: Some(a.name.clone()),
            pane: a.pane_id,
        };
        items.push(("Diff", send(diff), false));
        let sid = a.harness_session_id.clone().filter(|s| !s.is_empty());
        if let Some(sid) = &sid {
            let (label, flag, done) = if a.pinned {
                ("Unpin", "--unkeep", "unpinned")
            } else {
                ("Pin", "--keep", "pinned")
            };
            let argv = vec!["reap".into(), flag.into(), sid.clone()];
            let note = format!("{done} {}", a.name);
            items.push((label, MenuAct::Shell(argv, note), false));
            if a.dnd || !a.exited {
                let (label, flag, done) = if a.dnd {
                    ("Remove DND", "--release", "do-not-disturb lifted")
                } else {
                    ("Do not disturb", "--on", "do-not-disturb armed")
                };
                let argv = ["mail-hold", "--session", sid.as_str(), flag].map(String::from);
                let note = format!("{done} for {}", a.name);
                items.push((label, MenuAct::Shell(argv.to_vec(), note), true));
            }
        }
        if a.exited {
            let resume = Command::RespawnAgent {
                name: a.name.clone(),
            };
            items.push(("Resume", send(resume), true));
        } else {
            let stop = Command::StopAgent {
                name: a.name.clone(),
                harness_session_id: sid.clone(),
                pane_id: a.pane_id,
            };
            items.push(("Stop", send(stop), true));
        }
        let copy = |text: String| MenuAct::Run(vec![Action::Copy(text)]);
        items.push(("Copy name", copy(a.name.clone()), true));
        if let Some(node) = &a.node {
            items.push(("Copy node id", copy(node.clone()), false));
        }
        if let Some(sid) = sid {
            items.push(("Copy session id", copy(sid), false));
        }
        if !a.external {
            items.push(("Rename agent", MenuAct::Rename(Some(a.name.clone())), true));
        }
        RowMenu::new(a.name.clone(), items)
    }

    fn new(title: String, items: Vec<MenuItem>) -> Self {
        RowMenu {
            title,
            items,
            selected: 0,
            anchor: None,
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

    pub(super) fn view(&self, root: &Root, cx: &mut gpui::Context<Root>) -> Div {
        let p = root.palette;
        let rows = self.items.iter().enumerate().map(|(i, (label, _, rule))| {
            let fill = if i == self.selected {
                p.row_fill
            } else {
                p.panel
            };
            div()
                .when(*rule, |d| {
                    d.mt(px(4.0))
                        .pt(px(4.0))
                        .border_t_1()
                        .border_color(rgb(p.faint))
                })
                .child(
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
                        .child(
                            div()
                                .w(px(10.0))
                                .text_color(rgb(p.accent))
                                .child(if i == self.selected { "▸" } else { "" }),
                        )
                        .child(*label),
                )
        });
        let (left, top) = self.anchor.unwrap_or((
            pane_seam::default_width(pane_seam::SeamKind::LeftPane) + 12.0,
            ui::TITLE_H + 8.0,
        ));
        let renames = self.items.iter().any(|(l, _, _)| *l == "Rename agent");
        let panel = div()
            .id("row-menu")
            .absolute()
            .top(px(top))
            .left(px(left))
            .min_w(px(220.0))
            .flex()
            .flex_col()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(p.faint))
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
            .when(renames, |d| {
                d.child(
                    ui::kbd(
                        "Rename agent also renames the harness session where it can.",
                        &p,
                    )
                    .text_color(rgb(p.muted))
                    .px_2(),
                )
            })
            .child(
                ui::kbd("enter or 1-9 runs, esc closes", &p)
                    .text_color(rgb(p.muted))
                    .px_2(),
            );
        // A click anywhere off the panel closes the menu.
        div()
            .absolute()
            .inset_0()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|root, _, _, cx| {
                    root.row_menu = None;
                    cx.notify();
                }),
            )
            .child(panel)
    }
}

/// The Peek overlay: the newest lines of an agent's transcript. A click
/// off the panel or esc closes it.
pub(super) fn peek_view(
    name: &str,
    lines: &[String],
    p: &Palette,
    cx: &mut gpui::Context<Root>,
) -> Div {
    let p = *p;
    let tail = lines.len().saturating_sub(40);
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|root, _, _, cx| {
                root.peek = None;
                cx.notify();
            }),
        )
        .child(
            div()
                .id("peek")
                .flex()
                .flex_col()
                .w(px(640.0))
                .max_h(px(480.0))
                .overflow_y_scroll()
                .p_3()
                .rounded(px(6.0))
                .border_1()
                .border_color(rgb(p.faint))
                .bg(rgb(p.panel))
                .font_family(ui::MONO)
                .text_size(px(ui::BODY))
                .text_color(rgb(p.fg))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .text_color(rgb(p.accent))
                        .child(SharedString::from(format!("peek: {name}"))),
                )
                .children(
                    lines
                        .iter()
                        .skip(tail)
                        .map(|l| div().child(SharedString::from(l.clone()))),
                )
                .child(ui::kbd("esc or a click closes", &p).text_color(rgb(p.muted))),
        )
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

/// How many agents wait on the user, as the title bar's pill counts them.
pub(super) fn waiting_count(agents: &[AgentRow], now: u64) -> usize {
    sideline_order(agents, now)
        .into_iter()
        .filter(|a| agent_lattice_state(a) == LatticeState::Blocked)
        .filter(|a| a.pane_id.is_some())
        .count()
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
    fn history_keys_scroll_without_a_mode_and_bare_keys_wait_for_history() {
        let k = |name, shift, cmd| Key {
            shift,
            cmd,
            ..key(name)
        };
        let delta = |m: Option<ClientMsg>| match m {
            Some(ClientMsg::PaneScroll { delta, .. }) => Some(delta),
            _ => None,
        };
        assert_eq!(
            delta(history_step(&k("pageup", true, false), 7, 30, false)),
            Some(30)
        );
        assert_eq!(
            delta(history_step(&k("pagedown", true, false), 7, 30, false)),
            Some(-30)
        );
        assert!(history_step(&k("pageup", false, false), 7, 30, false).is_none());
        assert_eq!(
            delta(history_step(&k("pageup", false, false), 7, 30, true)),
            Some(30)
        );
        assert!(matches!(
            history_step(&k("up", false, true), 7, 30, false),
            Some(ClientMsg::PaneScrollEdge { top: true, .. })
        ));
        assert!(matches!(
            history_step(&k("end", false, false), 7, 30, true),
            Some(ClientMsg::PaneScrollEdge { top: false, .. })
        ));
        assert!(history_step(&k("end", false, false), 7, 30, false).is_none());
    }

    #[test]
    fn copy_node_id_shows_only_for_a_row_that_has_a_node() {
        let row = |node: Option<&str>| -> AgentRow {
            serde_json::from_value(serde_json::json!({
                "squad": null, "name": "w", "pane_id": 4, "badge": null,
                "reason": null, "exited": false, "harness_session_id": "s1",
                "node": node,
            }))
            .unwrap()
        };
        let has = |a: &AgentRow| {
            RowMenu::of(Some(a))
                .items
                .iter()
                .any(|(l, _, _)| *l == "Copy node id")
        };
        assert!(has(&row(Some("node-demo"))));
        assert!(!has(&row(None)));
    }

    #[test]
    fn a_bare_pane_menu_keeps_the_tab_rows_and_digits_pick_a_row() {
        let mut menu = RowMenu::open(&[], 3);
        let labels: Vec<&str> = menu.items.iter().map(|(l, _, _)| *l).collect();
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
            agent: None,
            field: LineField::seeded("api", MAX_TAB_NAME),
            inline: false,
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
