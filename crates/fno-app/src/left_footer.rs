//! The left pane's foot: the account on the left, and a host button and
//! a Settings gear on the right. It sits below the scroll list, outside
//! it, so it never scrolls away. No machine chip: Machines stay in the
//! list.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use gpui::{div, prelude::*, px, rgb, Div};

use super::palette::Palette;
use super::ui::{self, Role};
use super::window::{OpenSettings, ShowHosts};

static NAME: OnceLock<Option<String>> = OnceLock::new();

static COMPUTER: AtomicBool = AtomicBool::new(false);

/// Shot door: draw the computer mark instead of the globe, so the
/// variant shots come from the same binary. The default stays the globe.
pub fn draw_computer(yes: bool) {
    COMPUTER.store(yes, Ordering::Relaxed);
}

/// The footer's gear: the real gear glyph in the marks' 12px box, at a
/// larger font so its ink paints 24x24 like the globe and the computer.
fn gear_mark(p: &Palette) -> Div {
    div()
        .flex_none()
        .size(px(12.0))
        .flex()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .child(
            div()
                .text_color(rgb(p.muted))
                .text_size(px(20.0))
                .line_height(px(12.0))
                .child("⚙"),
        )
}

/// The footer's host mark at the gear's 12px: the TUI line globe, a
/// circle with its equator and meridian, or the computer. One line
/// weight, monochrome, so it takes both palettes the way the gear does.
fn host_mark(p: &Palette) -> Div {
    if COMPUTER.load(Ordering::Relaxed) {
        return div()
            .flex_none()
            .size(px(12.0))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(1.0))
            .child(
                div()
                    .flex_none()
                    .w(px(12.0))
                    .h(px(10.0))
                    .border_1()
                    .border_color(rgb(p.muted))
                    .rounded(px(2.0)),
            )
            .child(div().flex_none().w(px(6.0)).h(px(1.0)).bg(rgb(p.muted)));
    }
    div()
        .relative()
        .flex_none()
        .size(px(12.0))
        .child(
            div()
                .absolute()
                .inset_0()
                .border_1()
                .border_color(rgb(p.muted))
                .rounded_full(),
        )
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(5.5))
                .h(px(1.0))
                .bg(rgb(p.muted)),
        )
        .child(
            div()
                .absolute()
                .left(px(3.0))
                .right(px(3.0))
                .top_0()
                .bottom_0()
                .border_1()
                .border_color(rgb(p.muted))
                .rounded_full(),
        )
}

/// Read the macOS full user name once, off the UI thread, at launch.
pub fn prime() {
    std::thread::spawn(|| {
        NAME.get_or_init(|| {
            fno::process_admission::std_output(std::process::Command::new("id").arg("-F"))
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        });
    });
}

/// The chip's name: the read name, else "You" (blank or not read yet).
pub fn chip_name(read: Option<&str>) -> &str {
    read.map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or("You")
}

/// The footer row: a round initial and the name, then the host and
/// gear buttons.
pub(super) fn render(p: &Palette) -> Div {
    let name = chip_name(NAME.get().and_then(|n| n.as_deref())).to_string();
    let initial = name
        .chars()
        .next()
        .unwrap_or('Y')
        .to_uppercase()
        .to_string();
    div()
        .flex_none()
        .h(px(ui::ROW_H + 8.0))
        .px_3()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .border_t_1()
        .border_color(rgb(p.border))
        .child(
            div()
                .flex_none()
                .size(px(18.0))
                .rounded_full()
                .bg(rgb(p.raised))
                .flex()
                .items_center()
                .justify_center()
                .child(ui::text(initial, Role::Label, p)),
        )
        .child(ui::clamp(name, Role::Name, p))
        // The host button: the mark sits at the gear's 12px. One click
        // opens the connect-a-host step.
        .child(
            ui::clickable(
                div()
                    .id("left-footer-hosts")
                    .flex_none()
                    .p(px(3.0))
                    .rounded(px(4.0)),
                p,
            )
            .on_click(|_, window, cx| window.dispatch_action(Box::new(ShowHosts), cx))
            .child(host_mark(p)),
        )
        .child(
            ui::clickable(
                div()
                    .id("left-footer-settings")
                    .flex_none()
                    .p(px(3.0))
                    .rounded(px(4.0)),
                p,
            )
            .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx))
            .child(gear_mark(p)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chip_reads_you_when_no_name_is_known() {
        assert_eq!(chip_name(None), "You");
        assert_eq!(chip_name(Some("  \n")), "You");
        assert_eq!(chip_name(Some("Jason Noah Choi\n")), "Jason Noah Choi");
    }
}
