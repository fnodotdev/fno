//! The title bar above the main area (layout D6): the F[no] mark and the
//! Chat and Terminal switch on the left (the lit one is the active view),
//! where you are after them (session, view and the focused agent), the
//! Terminal view's session tabs, and on the right Search, Questions with
//! its count, HQ and Mods. It is rebuilt from every `Layout`, so a rename
//! from another client shows on the next one.

use gpui::{div, prelude::*, px, rgb, Context, Div, ElementId, Stateful};

use super::link::Layout;
use super::mux_modes;
use super::ui::{self, Role};
use super::window::{Action, MainView, Root};
use fno::proto::{Command, TabMeta};

#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    pub id: u64,
    pub label: String,
    pub active: bool,
    pub action: Action,
}

/// A chosen name as the operator typed it. Otherwise the title of the pane
/// the user is on in that tab (or its first pane), so two tabs in one repo
/// never both read as the repo name.
fn label(t: &TabMeta, i: usize, focus: u64) -> String {
    if t.named {
        return fno::app_api::client::tab_label_text(&t.name, i, true);
    }
    let pane = t
        .panes
        .iter()
        .find(|p| p.id == focus)
        .or_else(|| t.panes.first())
        .map(|p| p.label.trim())
        .filter(|l| !l.is_empty());
    match pane {
        Some(l) => l.to_string(),
        None => fno::app_api::client::tab_label_text(&t.name, i, false),
    }
}

pub fn strip(layout: &Layout) -> Vec<Tab> {
    let Some(squad) = layout.squads.iter().find(|s| s.id == layout.active_squad) else {
        return Vec::new();
    };
    squad
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| Tab {
            id: t.id,
            label: label(t, i, layout.focus),
            active: i == squad.active_tab,
            action: Action::Send(Command::SelectTab(t.id)),
        })
        .collect()
}

/// The views and their names, in the order the bar shows them. Views have
/// no key; ⌘/ lists the keys.
pub(super) const VIEWS: [(&str, MainView); 5] = [
    ("Chat", MainView::Chat),
    ("HQ", MainView::Orchestration),
    ("Mods", MainView::Mods),
    ("Terminal", MainView::Terminal),
    ("Inbox", MainView::Inbox),
];

/// The Chat and Terminal switch at the bar's left edge.
const SWITCH: [(&str, MainView); 2] = [("Chat", MainView::Chat), ("Terminal", MainView::Terminal)];

/// The views on the bar's right, after Search. Questions follows them as
/// the Inbox view, with its open count.
const PANELS: [(&str, MainView); 2] = [("HQ", MainView::Orchestration), ("Mods", MainView::Mods)];

/// Where the window is: the session, the view and the agent the view
/// shows. Chat names the agent whose session `chat` streams, which a
/// `--chat` pin can set apart from the focused pane; Terminal names the
/// focused pane's agent.
pub fn context(layout: &Layout, view: MainView, chat: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = layout
        .squads
        .iter()
        .find(|s| s.id == layout.active_squad)
        .map(|s| s.name.clone())
        .into_iter()
        .collect();
    out.extend(VIEWS.iter().find(|v| v.1 == view).map(|v| v.0.to_string()));
    let agent = match (view, chat) {
        (MainView::Chat, Some(sid)) => layout
            .agents
            .iter()
            .find(|a| a.harness_session_id.as_deref() == Some(sid)),
        (MainView::Chat | MainView::Terminal, _) => layout
            .agents
            .iter()
            .find(|a| a.pane_id == Some(layout.focus)),
        _ => None,
    };
    out.extend(agent.map(|a| a.name.clone()));
    out
}

/// One session tab: plain text, the active one in primary weight over a
/// short accent rule, so tabs never read as the view switch. The rule
/// pins to the foot out of flow, so the label alone centers on the
/// bar's one midline with the breadcrumb and the view switch.
fn tab(
    id: ElementId,
    t: Tab,
    key: Option<usize>,
    root: &Root,
    cx: &mut Context<Root>,
) -> Stateful<Div> {
    let p = root.palette;
    let action = t.action;
    let tab_id = t.id;
    // The rename field replaces the tab's own cell, so its text keeps the
    // tab's size and sits on the raised fill at primary contrast.
    if let Some(r) = root
        .rename
        .as_ref()
        .filter(|r| r.inline && r.tab() == tab_id)
    {
        return div().id(id).h_full().px_1().flex().items_center().child(
            div()
                .flex()
                .items_center()
                .min_w(px(120.0))
                .px_2()
                .py(px(1.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(rgb(p.accent))
                .bg(rgb(p.raised))
                .text_color(rgb(p.fg))
                .whitespace_nowrap()
                .child(format!("{}▏", r.field.text())),
        );
    }
    div()
        .id(id)
        .relative()
        .h_full()
        .px_2()
        .flex()
        .flex_col()
        .justify_center()
        .max_w(px(200.0))
        .cursor_pointer()
        .on_mouse_down(
            gpui::MouseButton::Right,
            cx.listener(move |root, ev: &gpui::MouseDownEvent, _, cx| {
                root.tab_menu_open(tab_id, ev.position, cx)
            }),
        )
        .text_color(rgb(if t.active { p.fg } else { p.muted }))
        .hover(move |s| s.text_color(rgb(p.fg)))
        .on_click(cx.listener(move |root, ev: &gpui::ClickEvent, _, cx| {
            if ev.click_count() >= 2 {
                root.rename_tab(tab_id, cx);
            } else {
                root.act(action.clone(), cx);
            }
        }))
        .child(
            div()
                .flex()
                .flex_row()
                .items_baseline()
                .gap(px(6.0))
                .child(div().line_clamp(1).text_ellipsis().child(t.label))
                .children(key.map(|n| ui::badge(n, &p))),
        )
        .child(
            div()
                .absolute()
                .bottom_0()
                .left(px(8.0))
                .right(px(8.0))
                .h(px(2.0))
                .when(t.active, |d| d.bg(rgb(p.accent))),
        )
}

/// One view in the group: the selected one sits raised in primary text,
/// the others are quiet text. Each acts on click.
fn mode(
    i: usize,
    name: String,
    view: MainView,
    root: &Root,
    cx: &mut Context<Root>,
) -> Stateful<Div> {
    let p = root.palette;
    let on = root.main == view;
    div()
        .id(ElementId::NamedInteger("mode".into(), i as u64))
        .h(px(22.0))
        .px_2()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .rounded(px(4.0))
        .when(on, |d| d.bg(rgb(p.raised)))
        .text_color(rgb(if on { p.fg } else { p.muted }))
        .cursor_pointer()
        .hover(move |s| s.text_color(rgb(p.fg)))
        .on_click(cx.listener(move |root, _, _, cx| root.act(Action::Show(view), cx)))
        .child(name)
}

/// The bar: a fixed height that no terminal font size moves, and one
/// hairline at its foot.
pub(super) fn render(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let mut here = div().flex().flex_row().flex_none().items_center().gap_2();
    let chat = root.chat.key.as_ref().map(|k| k.session_id.as_str());
    for (i, part) in context(&root.layout, root.main, chat)
        .into_iter()
        .enumerate()
    {
        if i > 0 {
            here = here.child(ui::role(div(), Role::Hint, &p).child("/"));
        }
        let r = if i == 1 { Role::Heading } else { Role::Meta };
        here = here.child(ui::role(div(), r, &p).whitespace_nowrap().child(part));
    }
    let mut tabs = div().flex().flex_row().h_full().min_w_0().overflow_hidden();
    if root.main == MainView::Terminal {
        for (i, t) in strip(&root.layout).into_iter().enumerate() {
            let id = ElementId::NamedInteger("tab".into(), i as u64);
            // While cmd is held, cmd-1..9 badges mark the first nine tabs.
            let key = (root.picking && i < 9).then_some(i + 1);
            tabs = tabs.child(tab(id, t, key, root, cx));
        }
    }
    let mark = ui::role(div(), Role::Heading, &p)
        .flex_none()
        .child("F[no]");
    let mut switch = div().flex().flex_row().flex_none().items_center().gap_1();
    for (i, (name, view)) in SWITCH.into_iter().enumerate() {
        switch = switch.child(mode(i, name.into(), view, root, cx));
    }
    let open = root.board.questions().len();
    let questions = match open {
        0 => "Questions".to_string(),
        n => format!("Questions {n}"),
    };
    let search =
        div()
            .id("search")
            .h(px(22.0))
            .px_2()
            .flex()
            .items_center()
            .rounded(px(4.0))
            .text_color(rgb(p.muted))
            .cursor_pointer()
            .hover(move |s| s.text_color(rgb(p.fg)))
            .on_click(cx.listener(|_, _, window, cx| {
                window.dispatch_action(Box::new(super::window::GoTo), cx)
            }))
            .child("Search");
    let mut modes = div().flex().flex_row().flex_none().items_center().gap_1();
    modes = modes.child(search);
    modes = modes.child(mode(2, questions, MainView::Inbox, root, cx));
    for (i, (name, view)) in PANELS.into_iter().enumerate() {
        modes = modes.child(mode(3 + i, name.into(), view, root, cx));
    }
    let waiting = mux_modes::waiting_count(
        &root.layout.agents,
        fno::app_api::digest_overlay::now_secs(),
    );
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap_4()
        .h(px(ui::TITLE_H))
        // The left inset clears the traffic lights the transparent title bar draws.
        .pl(px(78.0))
        .pr_3()
        .border_b_1()
        .border_color(rgb(p.border))
        .child(mark)
        .child(switch)
        .child(here)
        .child(tabs)
        .child(div().flex_1())
        .when(waiting > 0, |bar| {
            bar.child(waiting_pill(waiting, root, cx))
        })
        .child(modes)
}

/// The next-waiting pill: shown only while an agent waits on the user. A
/// click walks to the next waiting agent, the way prefix+Z does.
fn waiting_pill(n: usize, root: &Root, cx: &mut Context<Root>) -> Stateful<Div> {
    let p = root.palette;
    div()
        .id("next-waiting")
        .flex_none()
        .px_2()
        .py(px(1.0))
        .rounded(px(10.0))
        .border_1()
        .border_color(rgb(p.warn))
        .text_color(rgb(p.warn))
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(p.hover)))
        .on_click(cx.listener(|root, _, _, cx| root.focus_next_waiting(cx)))
        .child(format!("{n} waiting ▸"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_names_the_session_the_view_and_the_focused_agent() {
        let layout: Layout = Layout {
            squads: vec![fno::proto::SquadMeta {
                id: 1,
                name: "checkout".into(),
                canonical_cwd: "/r".into(),
                project: None,
                tabs: Vec::new(),
                active_tab: 0,
                panes: 1,
            }],
            active_squad: 1,
            focus: 4,
            agents: vec![
                serde_json::from_value(serde_json::json!({
                    "squad": null, "name": "archer", "pane_id": 4, "badge": null,
                    "reason": null, "exited": false,
                }))
                .unwrap(),
                serde_json::from_value(serde_json::json!({
                    "squad": null, "name": "scout", "pane_id": 5, "badge": null,
                    "reason": null, "exited": false, "harness_session_id": "s1",
                }))
                .unwrap(),
            ],
            ..Layout::default()
        };
        assert_eq!(
            context(&layout, MainView::Chat, None),
            ["checkout", "Chat", "archer"]
        );
        // A pinned chat names the agent it streams, not the focused pane.
        assert_eq!(
            context(&layout, MainView::Chat, Some("s1")),
            ["checkout", "Chat", "scout"]
        );
        assert_eq!(
            context(&layout, MainView::Terminal, Some("s1")),
            ["checkout", "Terminal", "archer"]
        );
        assert_eq!(context(&layout, MainView::Mods, None), ["checkout", "Mods"]);
    }
}
