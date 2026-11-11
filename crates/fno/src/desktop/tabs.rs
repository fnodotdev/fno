//! The tab strip above the grid: the active squad's tabs, labelled the way
//! the TUI strip labels them. It is rebuilt from every `Layout`, so a rename
//! from another client shows on the next one.

use gpui::{div, prelude::*, px, rgb, Context, Div, MouseButton};

use super::link::Layout;
use super::window::{Action, Root};
use crate::proto::Command;

#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    pub label: String,
    pub active: bool,
    pub action: Action,
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
            label: crate::client::tab_label_text(&t.name, i, t.named),
            active: i == squad.active_tab,
            action: Action::Send(Command::SelectTab(t.id)),
        })
        .collect()
}

pub(super) fn render(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let mut bar = div()
        .flex()
        .flex_row()
        .flex_none()
        .gap_1()
        .px_2()
        .py_1()
        .bg(rgb(p.panel));
    for tab in strip(&root.layout) {
        let (fg, bg) = if tab.active {
            (p.fg, p.border)
        } else {
            (p.muted, p.panel)
        };
        let action = tab.action.clone();
        bar = bar.child(
            div()
                .px_3()
                .py(px(2.0))
                .rounded_md()
                .bg(rgb(bg))
                .text_color(rgb(fg))
                .whitespace_nowrap()
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |root, _, _, cx| root.act(action.clone(), cx)),
                )
                .child(tab.label),
        );
    }
    bar
}
