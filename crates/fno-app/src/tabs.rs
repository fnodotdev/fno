//! The title bar above the main area: where you are on the left (session,
//! view and the focused agent), the Terminal view's session tabs after it,
//! and the four views on the right as one quiet group with their keys. It is
//! rebuilt from every `Layout`, so a rename from another client shows on the
//! next one.

use gpui::{div, prelude::*, px, rgb, Context, Div, ElementId, Stateful};

use super::link::Layout;
use super::ui::{self, Role};
use super::window::{Action, MainView, Root};
use fno::proto::{Command, TabMeta};

#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
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
            label: label(t, i, layout.focus),
            active: i == squad.active_tab,
            action: Action::Send(Command::SelectTab(t.id)),
        })
        .collect()
}

/// The views, their names and their cmd keys, in key order.
const VIEWS: [(&str, MainView, &str); 4] = [
    ("Chat", MainView::Chat, "⌘1"),
    ("HQ", MainView::Orchestration, "⌘2"),
    ("Mods", MainView::Mods, "⌘3"),
    ("Terminal", MainView::Terminal, "⌘4"),
];

/// Where the window is: the session, the view and, where the view follows
/// it, the focused agent.
pub fn context(layout: &Layout, view: MainView) -> Vec<String> {
    let mut out: Vec<String> = layout
        .squads
        .iter()
        .find(|s| s.id == layout.active_squad)
        .map(|s| s.name.clone())
        .into_iter()
        .collect();
    out.extend(VIEWS.iter().find(|v| v.1 == view).map(|v| v.0.to_string()));
    if matches!(view, MainView::Chat | MainView::Terminal) {
        let agent = layout
            .agents
            .iter()
            .find(|a| a.pane_id == Some(layout.focus));
        out.extend(agent.map(|a| a.name.clone()));
    }
    out
}

/// One session tab: plain text, the active one in primary weight over a
/// short accent rule, so tabs never read as the view switch.
fn tab(id: ElementId, t: Tab, root: &Root, cx: &mut Context<Root>) -> Stateful<Div> {
    let p = root.palette;
    let action = t.action;
    div()
        .id(id)
        .h_full()
        .px_2()
        .flex()
        .flex_col()
        .justify_center()
        .max_w(px(200.0))
        .cursor_pointer()
        .text_color(rgb(if t.active { p.fg } else { p.muted }))
        .hover(move |s| s.text_color(rgb(p.fg)))
        .on_click(cx.listener(move |root, _, _, cx| root.act(action.clone(), cx)))
        .child(div().line_clamp(1).text_ellipsis().child(t.label))
        .child(
            div()
                .h(px(2.0))
                .mt(px(4.0))
                .w_full()
                .when(t.active, |d| d.bg(rgb(p.accent))),
        )
}

/// One view in the group: the selected one sits raised in primary text,
/// the others are quiet text. Each acts on click.
fn mode(
    i: usize,
    (name, view, keys): (&'static str, MainView, &'static str),
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
        .child(ui::role(div(), Role::Hint, &p).child(keys))
}

/// The bar: a fixed height that no terminal font size moves, and one
/// hairline at its foot.
pub(super) fn render(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let mut here = div().flex().flex_row().flex_none().items_center().gap_2();
    for (i, part) in context(&root.layout, root.main).into_iter().enumerate() {
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
            tabs = tabs.child(tab(id, t, root, cx));
        }
    }
    let mut modes = div().flex().flex_row().flex_none().items_center().gap_1();
    for (i, v) in VIEWS.into_iter().enumerate() {
        modes = modes.child(mode(i, v, root, cx));
    }
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap_4()
        .h(px(ui::TITLE_H))
        .px_3()
        .border_b_1()
        .border_color(rgb(p.border))
        .child(here)
        .child(tabs)
        .child(div().flex_1())
        .child(modes)
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
                tabs: Vec::new(),
                active_tab: 0,
                panes: 1,
            }],
            active_squad: 1,
            focus: 4,
            agents: vec![serde_json::from_value(serde_json::json!({
                "squad": null, "name": "archer", "pane_id": 4, "badge": null,
                "reason": null, "exited": false,
            }))
            .unwrap()],
            ..Layout::default()
        };
        assert_eq!(
            context(&layout, MainView::Chat),
            ["checkout", "Chat", "archer"]
        );
        assert_eq!(context(&layout, MainView::Mods), ["checkout", "Mods"]);
    }
}
