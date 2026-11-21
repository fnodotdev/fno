//! The left pane's foot: the account on the left and a Settings gear on
//! the right. It sits below the scroll list, outside it, so it never
//! scrolls away. No machine chip: Machines stay in the list.

use std::sync::OnceLock;

use gpui::{div, prelude::*, px, rgb, Div};

use super::palette::Palette;
use super::ui::{self, Role};
use super::window::OpenSettings;

static NAME: OnceLock<Option<String>> = OnceLock::new();

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

/// The footer row: a round initial and the name, then the gear.
pub(super) fn render(p: &Palette) -> Div {
    let name = chip_name(NAME.get().and_then(|n| n.as_deref())).to_string();
    let initial = name
        .chars()
        .next()
        .unwrap_or('Y')
        .to_uppercase()
        .to_string();
    let pp = *p;
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
        .child(
            div()
                .id("left-footer-settings")
                .flex_none()
                .cursor_pointer()
                .text_color(rgb(p.muted))
                .hover(move |s| s.text_color(rgb(pp.fg)))
                .child("⚙")
                .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx)),
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
