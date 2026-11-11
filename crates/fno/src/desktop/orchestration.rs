//! Orchestration mode (cmd-2): the command center board. The TABLE lists
//! every machine's seats and their nodes; the FEED lists the questions that
//! wait on the user, each option one click. Both read the shared HQ board,
//! so they match `/hq/model.json`. A failed read shows its error above the
//! last good rows.

use gpui::{div, prelude::*, px, rgb, Context, Div, ElementId, Stateful};

use super::window::{Action, AnswerState, Root};

fn section(title: &'static str, root: &Root) -> Div {
    div()
        .pt_4()
        .pb_2()
        .text_xs()
        .text_color(rgb(root.palette.muted))
        .child(title)
}

/// A muted line in the body of a list: a wait, or an empty list.
fn quiet(text: &str, root: &Root) -> Div {
    div()
        .py_2()
        .text_color(rgb(root.palette.muted))
        .child(text.to_string())
}

/// A failed read in the error color. Before the first read the body says
/// "reading…" instead, so this shows nothing.
fn error_line(text: &str, root: &Root) -> Option<Div> {
    root.board.read_yet().then(|| {
        div()
            .py_1()
            .text_color(rgb(root.palette.error))
            .child(text.to_string())
    })
}

/// One table cell. A width of 0 takes the rest of the row: the title column.
/// The text sits in its own full-width box, so a long one ends in an
/// ellipsis and never clips mid-glyph.
fn cell(text: String, w: f32, color: u32) -> Div {
    let d = div();
    let d = if w > 0.0 {
        d.w(px(w)).flex_none()
    } else {
        d.flex_1().min_w_0()
    };
    d.px_2()
        .overflow_hidden()
        .child(div().w_full().truncate().text_color(rgb(color)).child(text))
}

fn table(root: &Root) -> Div {
    let p = root.palette;
    let mut out = div()
        .flex()
        .flex_col()
        .w_full()
        .child(section("TABLE", root));
    if let Some(e) = root.board.fleet_error() {
        out = out.children(error_line(e, root));
    }
    let machines = root
        .board
        .fleet()
        .map(|f| f.machines.as_slice())
        .unwrap_or_default();
    // One host in view: the MACHINE column would repeat one name.
    let hosts = machines.len() > 1;
    let mut head = div().flex().flex_row().w_full().py_1().text_xs();
    if hosts {
        head = head.child(cell("MACHINE".into(), 110.0, p.muted));
    }
    out = out.child(
        head.child(cell("SEAT".into(), 170.0, p.muted))
            .child(cell("STATUS".into(), 90.0, p.muted))
            .child(cell("NODE".into(), 90.0, p.muted))
            .child(cell("TITLE".into(), 0.0, p.muted)),
    );
    if !root.board.read_yet() {
        return out.child(quiet("reading…", root));
    }
    for m in machines {
        let host = m.display_name.clone().unwrap_or_else(|| m.host.clone());
        for seat in m.leads.iter().flat_map(|l| &l.seats) {
            let work = seat.work.as_ref();
            let mut row = div()
                .flex()
                .flex_row()
                .w_full()
                .py_1()
                .border_t_1()
                .border_color(rgb(p.border));
            if hosts {
                row = row.child(cell(host.clone(), 110.0, p.muted));
            }
            out = out.child(
                row.child(cell(seat.name.clone(), 170.0, p.fg))
                    .child(cell(seat.status.to_string(), 90.0, p.status(seat.status)))
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
        out = out.child(quiet("no seats", root));
    }
    out
}

/// The options of one question, or where the answer the user clicked stands.
fn options(q: &crate::needs_overlay::QuestionItem, root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let mut out = div().flex().flex_col().gap_2().pt_2();
    let state = root.answers.get(&q.id);
    if let Some(AnswerState::Sending(n) | AnswerState::Done(n)) = state {
        let (text, color) = match state {
            Some(AnswerState::Done(_)) => (format!("answered: option {n}"), p.ok),
            _ => (format!("sending option {n}…"), p.muted),
        };
        return out.child(div().py_1().text_color(rgb(color)).child(text));
    }
    if let Some(AnswerState::Refused(why)) = state {
        out = out.child(
            div()
                .text_xs()
                .text_color(rgb(p.error))
                .child(format!("answer refused: {why}")),
        );
    }
    let recommended = q.recommendation.as_ref().map(|r| r.option);
    for o in &q.options {
        let action = Action::Answer {
            id: q.id.clone(),
            option: o.n,
        };
        let (border, mark) = if recommended == Some(o.n) {
            (p.ok, " · recommended")
        } else {
            (p.border, "")
        };
        let id = ElementId::Name(format!("answer-{}-{}", q.id, o.n).into());
        out = out.child(
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(border))
                .cursor_pointer()
                .hover(move |s| s.bg(rgb(p.border)))
                .active(move |s| s.bg(rgb(p.focus)))
                // On click: a press dragged off the option answers nothing.
                .on_click(cx.listener(move |root, _, _, cx| root.act(action.clone(), cx)))
                .child(format!("{}. {}", o.n, o.text))
                .when(!mark.is_empty(), |d| {
                    d.child(div().text_xs().text_color(rgb(p.ok)).child(mark))
                }),
        );
    }
    out
}

fn feed(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let mut out = div().flex().flex_col().child(section("FEED", root));
    if let Some(e) = root.board.questions_error() {
        out = out.children(error_line(e, root));
    }
    if !root.board.read_yet() {
        return out.child(quiet("reading…", root));
    }
    let questions = root.board.questions_newest_first();
    if questions.is_empty() && root.board.questions_error().is_none() {
        out = out.child(quiet("nothing waits on you", root));
    }
    for q in questions {
        let q = &q.item;
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
                .child(options(q, root, cx)),
        );
    }
    out
}

/// TABLE and FEED side by side, each scrolling on its own, so a long seat
/// list never pushes the questions out of sight. The bottom pad keeps the
/// last row whole above the window edge.
pub(super) fn render(root: &Root, cx: &mut Context<Root>) -> Div {
    let column = |id: &'static str| -> Stateful<Div> {
        div()
            .id(id)
            .h_full()
            .overflow_x_hidden()
            .overflow_y_scroll()
            .px_4()
            .pb_8()
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
