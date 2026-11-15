//! The key sheet, ctrl-b ?: every key the app reads, in the mux which-key
//! modal's look. A centered panel over a dimmed window, the title and an
//! esc hint on top, sections headed in the accent, the key column in bold
//! accent, and a dim footer. Esc, `q`, `?` or a click outside closes it.

use gpui::{div, prelude::*, px, rgb, rgba, Context, Div, FontWeight};

use super::keys::SHEET;
use super::ui::{self, Role};
use super::window::Root;

/// The key column's width in cells: the widest key in the table.
fn key_cells() -> usize {
    SHEET
        .iter()
        .flat_map(|(_, rows)| rows.iter())
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0)
}

pub(super) fn render(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let key_w = root.adv * (key_cells() + 2) as f32;
    let mut body = div().flex().flex_col();
    for (i, (title, rows)) in SHEET.iter().enumerate() {
        body = body.child(
            div()
                .when(i > 0, |d| d.pt(px(12.0)))
                .pb(px(2.0))
                .text_color(rgb(p.accent))
                .child(*title),
        );
        for (keys, label) in *rows {
            body = body.child(
                div()
                    .flex()
                    .flex_row()
                    .whitespace_nowrap()
                    .child(
                        div()
                            .w(key_w)
                            .flex_none()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(p.accent))
                            .child(*keys),
                    )
                    .child(div().text_color(rgb(p.fg)).child(*label)),
            );
        }
    }
    let panel = div()
        .id("key-sheet")
        .flex()
        .flex_col()
        .max_h(px(780.0))
        .px_4()
        .py_3()
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(p.border))
        .bg(rgb(p.panel))
        .font_family(ui::MONO)
        .text_size(px(ui::BODY))
        // A click on the panel stays on it.
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .flex_row()
                .items_baseline()
                .child(
                    div()
                        .flex_1()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("keybindings"),
                )
                .child(ui::kbd("esc", &p)),
        )
        .child(
            ui::role(div(), Role::Hint, &p)
                .pb(px(8.0))
                .child("ctrl+b is the prefix"),
        )
        .child(div().id("key-sheet-body").overflow_y_scroll().child(body))
        .child(
            ui::role(div(), Role::Hint, &p)
                .pt(px(10.0))
                .child("esc close"),
        );
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x0000_0066))
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|root, _, _, cx| root.close_sheet(cx)),
        )
        .child(panel)
}
