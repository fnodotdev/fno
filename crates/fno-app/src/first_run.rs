//! The first-run path: what a launch shows before the app has a mux to talk
//! to, and what the first empty pane shows once it does. The offer card
//! replaces a bare closed-link toast; the splash mark and the key card sit
//! above and below the start view's composer.

use std::time::Duration;

use gpui::{
    div, prelude::*, px, rgb, Animation, AnimationExt, Context, CursorStyle, MouseButton, Stateful,
};

use crate::link::LinkError;
use crate::palette::Palette;
use crate::ui::{self, Role};
use crate::window::Root;

/// One splash cell's width. A terminal cell is twice as tall as it is wide,
/// so a cell draws `CELL` wide and `2 * CELL` tall and the F keeps its shape.
const CELL: f32 = 5.0;

/// Where the no-server offer stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Offer {
    /// Nothing listens on the session socket; the card asks.
    Ask,
    /// The server spawn is in flight.
    Starting,
}

/// Whether a failed attach is the "no server" case the offer answers. A
/// server that is up but too old, or a socket that failed mid-read, keep
/// their own message: starting a second server would not fix them.
pub(crate) fn offers_start(e: &LinkError) -> bool {
    matches!(e, LinkError::Connect { .. })
}

/// The mark as it reads at `t` seconds (`None` = finished), in the text
/// color, from the same frames the terminal splash plays.
pub(crate) fn mark(p: &Palette, t: Option<f64>) -> gpui::Div {
    let width = fno::app_api::splash::mark_cells(None)
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0);
    let mut out = div()
        .flex()
        .flex_col()
        .items_start()
        .w(px(CELL * width as f32));
    for mut row in fno::app_api::splash::mark_cells(t) {
        // Trimmed rows pad back out so the F holds its place while the
        // stamp grows.
        row.resize(width, false);
        let mut line = div().flex().flex_row().h(px(CELL * 2.0));
        let mut at = 0;
        while at < row.len() {
            let ink = row[at];
            let run = row[at..].iter().take_while(|c| **c == ink).count();
            let cell = div().w(px(CELL * run as f32)).h_full();
            line = line.child(if ink { cell.bg(rgb(p.fg)) } else { cell });
            at += run;
        }
        out = out.child(line);
    }
    out
}

/// The mark playing once from the first paint, then holding the finished
/// frame. A shot window passes `still` so the capture is the last frame.
pub(crate) fn mark_playing(p: Palette, still: bool) -> gpui::AnyElement {
    if still {
        return mark(&p, None).into_any_element();
    }
    let secs = fno::app_api::splash::MARK_SECS;
    div()
        .with_animation(
            "start-mark",
            Animation::new(Duration::from_secs_f64(secs)),
            move |_, delta| mark(&p, Some(f64::from(delta) * secs)),
        )
        .into_any_element()
}

/// The three keys a first launch needs: palette, new session, split.
pub(crate) fn key_card(p: &Palette) -> gpui::Div {
    let row = |keys: &'static str, label: &'static str| {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .child(div().w(px(36.0)).child(ui::kbd(keys, p)))
            .child(ui::text(label, Role::Body, p))
    };
    div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .px_4()
        .py_3()
        .rounded(px(8.0))
        .border_1()
        .border_color(rgb(p.border))
        .bg(rgb(p.raised))
        .child(row("⌘K", "palette: jump to an agent, node or view"))
        .child(row("⌘N", "new session: an agent, a lead or a shell"))
        .child(row("⌘D", "split the pane"))
}

/// The offer card: no server runs, so say so and offer to start one.
pub(crate) fn offer_card(root: &Root, state: Offer, cx: &mut Context<Root>) -> gpui::Div {
    let p = root.palette;
    let starting = state == Offer::Starting;
    let button: Stateful<gpui::Div> = div()
        .id("start-mux")
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px_3()
        .py_1()
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(p.border))
        .bg(rgb(p.raised))
        .when(!starting, |d| {
            d.cursor(CursorStyle::PointingHand).on_mouse_down(
                MouseButton::Left,
                cx.listener(|root, _, _, cx| root.start_mux(cx)),
            )
        })
        .child(ui::text(
            if starting {
                "Starting the mux…"
            } else {
                "Start the mux"
            },
            Role::Body,
            &p,
        ))
        .when(!starting, |d| d.child(ui::kbd("⏎", &p)));
    div()
        .flex()
        .flex_col()
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(18.0))
        .child(mark(&p, None))
        .child(ui::text("No mux is running", Role::Heading, &p))
        .child(ui::text(
            format!(
                "fno attaches to a mux server. None answers for session {}.",
                root.session
            ),
            Role::Meta,
            &p,
        ))
        .child(button)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_missing_socket_offers_a_start() {
        let missing = LinkError::Connect {
            path: "/tmp/none.sock".into(),
            source: std::io::ErrorKind::NotFound.into(),
        };
        assert!(offers_start(&missing));
        assert!(!offers_start(&LinkError::Closed("bye".into())));
        assert!(!offers_start(&LinkError::NoLayout));
    }
}
