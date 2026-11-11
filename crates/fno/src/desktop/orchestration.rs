//! Orchestration mode (cmd-2): the command center board. The TABLE lists
//! every machine's seats and their nodes; the FEED lists the questions that
//! wait on the user, each option one click. Both read the shared HQ board,
//! so they match `/hq/model.json`. A failed read shows its error above the
//! last good rows.

use gpui::{div, prelude::*, px, rgb, Context, Div, MouseButton, Stateful};

use super::window::{Action, Root};

fn section(title: &'static str, root: &Root) -> Div {
    div()
        .pt_4()
        .pb_2()
        .text_xs()
        .text_color(rgb(root.palette.muted))
        .child(title)
}

/// A failed read in the error color; before the first read, a muted wait.
fn error_line(text: &str, root: &Root) -> Div {
    let (color, text) = if root.board.read_yet() {
        (root.palette.error, text)
    } else {
        (root.palette.muted, "reading…")
    };
    div().py_1().text_color(rgb(color)).child(text.to_string())
}

fn table(root: &Root) -> Div {
    let p = root.palette;
    let mut out = div()
        .flex()
        .flex_col()
        .w_full()
        .child(section("TABLE", root));
    if let Some(e) = root.board.fleet_error() {
        out = out.child(error_line(e, root));
    }
    // A width of 0 takes the rest of the row: the title column.
    let cell = |text: String, w: f32, color: u32| {
        let d = div();
        let d = if w > 0.0 {
            d.w(px(w)).flex_none()
        } else {
            d.flex_1().min_w_0()
        };
        d.px_2().truncate().text_color(rgb(color)).child(text)
    };
    out = out.child(
        div()
            .flex()
            .flex_row()
            .w_full()
            .py_1()
            .text_xs()
            .child(cell("MACHINE".into(), 110.0, p.muted))
            .child(cell("SEAT".into(), 170.0, p.muted))
            .child(cell("STATUS".into(), 110.0, p.muted))
            .child(cell("NODE".into(), 90.0, p.muted))
            .child(cell("TITLE".into(), 0.0, p.muted)),
    );
    let machines = root
        .board
        .fleet()
        .map(|f| f.machines.as_slice())
        .unwrap_or_default();
    for m in machines {
        let host = m.display_name.clone().unwrap_or_else(|| m.host.clone());
        for seat in m.leads.iter().flat_map(|l| &l.seats) {
            let work = seat.work.as_ref();
            out = out.child(
                div()
                    .flex()
                    .flex_row()
                    .w_full()
                    .py_1()
                    .border_t_1()
                    .border_color(rgb(p.border))
                    .child(cell(host.clone(), 110.0, p.muted))
                    .child(cell(seat.name.clone(), 170.0, p.fg))
                    .child(cell(seat.status.to_string(), 110.0, p.fg))
                    .child(cell(
                        work.map(|w| w.id.clone()).unwrap_or_default(),
                        90.0,
                        p.muted,
                    ))
                    .child(cell(
                        work.and_then(|w| w.title.clone()).unwrap_or_default(),
                        0.0,
                        p.fg,
                    )),
            );
        }
    }
    if machines.is_empty() && root.board.fleet_error().is_none() {
        out = out.child(div().py_1().text_color(rgb(p.muted)).child("no seats"));
    }
    out
}

fn feed(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let mut out = div().flex().flex_col().child(section("FEED", root));
    if let Some(e) = root.board.questions_error() {
        out = out.child(error_line(e, root));
    }
    let questions = root.board.questions_newest_first();
    if questions.is_empty() && root.board.questions_error().is_none() {
        out = out.child(
            div()
                .py_1()
                .text_color(rgb(p.muted))
                .child("nothing waits on you"),
        );
    }
    for q in questions {
        let q = &q.item;
        let recommended = q.recommendation.as_ref().map(|r| r.option);
        let mut options = div().flex().flex_col().gap_2().pt_2();
        for o in &q.options {
            let action = Action::Answer {
                id: q.id.clone(),
                option: o.n,
            };
            let border = if recommended == Some(o.n) {
                p.accent
            } else {
                p.border
            };
            options = options.child(
                div()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(border))
                    .cursor_pointer()
                    .child(format!("{}. {}", o.n, o.text))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |root, _, _, cx| root.act(action.clone(), cx)),
                    ),
            );
        }
        let about = [q.node.clone(), q.node_title.clone(), q.host.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        out = out.child(
            div()
                .flex()
                .flex_col()
                .my_1()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(rgb(p.panel))
                .child(div().text_color(rgb(p.warn)).child(q.title.clone()))
                .child(div().text_xs().text_color(rgb(p.muted)).child(about))
                .child(options),
        );
    }
    out
}

/// TABLE and FEED side by side, each scrolling on its own, so a long seat
/// list never pushes the questions out of sight.
pub(super) fn render(root: &Root, cx: &mut Context<Root>) -> Div {
    let column = |id: &'static str| -> Stateful<Div> {
        div()
            .id(id)
            .h_full()
            .overflow_x_hidden()
            .overflow_y_scroll()
            .px_4()
            .pb_4()
    };
    div()
        .flex()
        .flex_row()
        .flex_1()
        .min_h_0()
        .w_full()
        .child(column("board-table").flex_1().min_w_0().child(table(root)))
        .child(
            column("board-feed")
                .w(px(440.0))
                .flex_none()
                .border_l_1()
                .border_color(rgb(root.palette.border))
                .child(feed(root, cx)),
        )
}
