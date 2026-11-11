//! The strip above the grid: the active squad's tabs and the Terminal / Board
//! switch. It is rebuilt from every `Layout`, so a rename from another client
//! shows on the next one.

use gpui::{div, prelude::*, px, rgb, Context, Div, ElementId, Stateful};

use super::link::Layout;
use super::window::{Action, MainView, Root};
use crate::proto::{Command, TabMeta};

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
        return crate::client::tab_label_text(&t.name, i, true);
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
        None => crate::client::tab_label_text(&t.name, i, false),
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

/// One strip button. It acts on click, so a press dragged off it does
/// nothing.
fn pill(
    id: ElementId,
    text: String,
    active: bool,
    action: Action,
    root: &Root,
    cx: &mut Context<Root>,
) -> Stateful<Div> {
    let p = root.palette;
    let (fg, bg) = if active {
        (p.fg, p.border)
    } else {
        (p.muted, p.panel)
    };
    div()
        .id(id)
        .px_3()
        .py(px(2.0))
        .rounded_md()
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .whitespace_nowrap()
        .max_w(px(220.0))
        .truncate()
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(p.border)).text_color(rgb(p.fg)))
        .active(move |s| s.bg(rgb(p.focus)))
        .on_click(cx.listener(move |root, _, _, cx| root.act(action.clone(), cx)))
        .child(text)
}

/// The session's tabs on the left, the mode switch on the right. The board
/// shows no session tabs: they select nothing there.
pub(super) fn render(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let mut tabs = div().flex().flex_row().gap_1().min_w_0().overflow_hidden();
    if root.main == MainView::Terminal {
        for (i, tab) in strip(&root.layout).into_iter().enumerate() {
            let id = ElementId::NamedInteger("tab".into(), i as u64);
            tabs = tabs.child(pill(id, tab.label, tab.active, tab.action, root, cx));
        }
    }
    let mut modes = div()
        .flex()
        .flex_row()
        .flex_none()
        .gap_1()
        .p(px(2.0))
        .rounded_md()
        .border_1()
        .border_color(rgb(p.border));
    let views = [
        ("Terminal", MainView::Terminal),
        ("Board", MainView::Orchestration),
    ];
    for (i, (name, view)) in views.into_iter().enumerate() {
        let id = ElementId::NamedInteger("mode".into(), i as u64);
        let on = root.main == view;
        modes = modes.child(pill(id, name.into(), on, Action::Show(view), root, cx));
    }
    div()
        .flex()
        .flex_row()
        .flex_none()
        .gap_2()
        .px_2()
        .py_1()
        .bg(rgb(p.panel))
        .child(tabs)
        .child(div().flex_1())
        .child(modes)
}
