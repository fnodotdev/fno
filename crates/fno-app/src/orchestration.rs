//! Orchestration mode (cmd-2): the command center board. Seats lists every
//! machine's seats and their nodes; Questions lists what waits on the user,
//! each option one click. Both read the shared HQ board, so they match
//! `/hq/model.json`. A failed read shows its error above the last good rows.

use gpui::{div, prelude::*, px, rgb, Context, Div, ElementId, Pixels, Stateful};

use super::ui::{self, Role};
use super::window::{Action, AnswerState, Root};
use fno::app_api::lattice::{lattice_glyph, LatticeState};

/// A node id's width in advances: an 11-character id shows whole.
const NODE_W: usize = 11;
/// The questions column's width.
const QUESTIONS_W: f32 = 440.0;

/// A failed read in the error color. Before the first read the body says
/// "reading…" instead, so this shows nothing.
fn error_line(text: &str, root: &Root) -> Option<Div> {
    root.board.read_yet().then(|| {
        ui::text(text.to_string(), Role::Meta, &root.palette)
            .text_color(rgb(root.palette.error))
            .py_1()
    })
}

/// A seat status word's glyph: the lattice shapes the sidebar uses, and the
/// question diamond for a parked seat, which waits on someone.
fn status_glyph(word: &str) -> char {
    let state = match word {
        "parked" => return '◇',
        "blocked" => LatticeState::Blocked,
        "writing" | "working" => LatticeState::Working,
        "unknown" => LatticeState::Unmeasured,
        _ => LatticeState::Idle,
    };
    lattice_glyph(state).0
}

/// One fixed column `chars` advances wide, or the rest of the row when 0.
fn col(text: String, chars: usize, r: Role, root: &Root, adv: Pixels) -> Div {
    if chars == 0 {
        ui::clamp(text, r, &root.palette)
    } else {
        ui::cell(text, chars, false, r, &root.palette, adv)
    }
}

fn table(root: &Root) -> Div {
    let p = root.palette;
    let adv = root.adv;
    let mut out = div()
        .flex()
        .flex_col()
        .w_full()
        .child(ui::section("Seats", &p).px_3());
    if let Some(e) = root.board.fleet_error() {
        out = out.children(error_line(e, root));
    }
    let machines = root
        .board
        .fleet()
        .map(|f| f.machines.as_slice())
        .unwrap_or_default();
    // One host in view: a machine column would repeat one name.
    let host_of = |m: &fno::app_api::client::org_seats::Machine| {
        m.display_name.clone().unwrap_or_else(|| m.host.clone())
    };
    // Count only hosts with a seat row: a seatless host shows no row.
    let mut names: Vec<String> = machines
        .iter()
        .filter(|m| m.leads.iter().any(|l| !l.seats.is_empty()))
        .map(host_of)
        .collect();
    names.sort();
    names.dedup();
    let hosts = names.len() > 1;
    // The title column takes the rest of the row. Mono cells measure exactly,
    // so the title is cut whole to that many cells: the sidebar, the
    // questions column, the gaps, the glyph and the fixed columns come off.
    let fixed = 2 + 18 + 9 + NODE_W + if hosts { 14 } else { 0 };
    // One gap between each pair of cells, and the row's own side padding.
    let gaps = if hosts { 5.0 } else { 4.0 };
    let rest = root.width - px(super::left_pane::WIDTH) - px(QUESTIONS_W) - px(24.0 + 24.0);
    let rest = rest - adv * fixed as f32 - px(8.0) * gaps;
    let title_w = (rest / adv).floor().max(4.0) as usize;
    let head = |t: &str, chars: usize| col(t.to_string(), chars, Role::Hint, root, adv);
    let mut heads = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .h(px(20.0))
        .px_3()
        .child(div().w(adv * 2.0).flex_none());
    if hosts {
        heads = heads.child(head("Machine", 14));
    }
    out = out.child(
        heads
            .child(head("Seat", 18))
            .child(head("Status", 9))
            .child(head("Node", NODE_W))
            .child(head("Title", 0)),
    );
    if !root.board.read_yet() {
        return out.child(ui::quiet("reading…", &p).px_3());
    }
    for (n, m) in machines.iter().enumerate() {
        let host = host_of(m);
        for (k, seat) in m.leads.iter().flat_map(|l| &l.seats).enumerate() {
            let work = seat.work.as_ref();
            let id = ElementId::Name(format!("seat-{n}-{k}").into());
            let mut row = ui::row(id, false, &p).child(
                div()
                    .w(adv * 2.0)
                    .flex_none()
                    .text_color(rgb(p.status(seat.status)))
                    .child(status_glyph(seat.status).to_string()),
            );
            if hosts {
                row = row.child(col(host.clone(), 14, Role::Meta, root, adv));
            }
            out = out.child(
                row.child(col(seat.name.clone(), 18, Role::Name, root, adv))
                    .child(col(seat.status.to_string(), 9, Role::Meta, root, adv))
                    .child(col(
                        work.map(|w| w.id.clone()).unwrap_or_default(),
                        NODE_W,
                        Role::Meta,
                        root,
                        adv,
                    ))
                    .child(col(
                        ui::cut(
                            &work.and_then(|w| w.title.clone()).unwrap_or_default(),
                            title_w,
                        ),
                        0,
                        Role::Body,
                        root,
                        adv,
                    )),
            );
        }
    }
    let any_seat = machines
        .iter()
        .any(|m| m.leads.iter().any(|l| !l.seats.is_empty()));
    if !any_seat && root.board.fleet_error().is_none() {
        out = out.child(ui::quiet("no seats", &p).px_3());
    }
    out
}

/// The options of one question, or where the answer the user clicked stands.
/// Each option is a filled row; the recommended one alone carries the
/// accent, on its glyph and its tag, never an outline.
fn options(q: &fno::needs_overlay::QuestionItem, root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let mut out = div().flex().flex_col().gap_1().pt_2();
    let state = root.answers.get(&q.id);
    if let Some(AnswerState::Sending(n) | AnswerState::Done(n)) = state {
        let (text, color) = match state {
            Some(AnswerState::Done(_)) => (format!("answered: option {n}"), p.ok),
            _ => (format!("sending option {n}…"), p.muted),
        };
        return out.child(ui::text(text, Role::Meta, &p).text_color(rgb(color)).py_1());
    }
    if let Some(AnswerState::Refused(why)) = state {
        out = out.child(
            ui::text(format!("answer refused: {why}"), Role::Fact, &p).text_color(rgb(p.error)),
        );
    }
    let recommended = q.recommendation.as_ref().map(|r| r.option);
    for o in &q.options {
        let action = Action::Answer {
            id: q.id.clone(),
            option: o.n,
        };
        let pick = recommended == Some(o.n);
        let id = ElementId::Name(format!("answer-{}-{}", q.id, o.n).into());
        out = out.child(
            div()
                .id(id)
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .py(px(5.0))
                .rounded(px(4.0))
                .bg(rgb(p.raised))
                .cursor_pointer()
                .hover(move |s| s.bg(rgb(p.row_fill)))
                .active(move |s| s.bg(rgb(p.pressed)))
                // On click: a press dragged off the option answers nothing.
                .on_click(cx.listener(move |root, _, _, cx| root.act(action.clone(), cx)))
                .child(
                    div()
                        .flex_none()
                        .w(root.adv * 2.0)
                        .text_color(rgb(if pick { p.accent } else { p.faint }))
                        .child(if pick { "●" } else { "○" }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(ui::PROSE)
                        .text_size(px(13.0))
                        .child(o.text.clone()),
                )
                .when(pick, |d| d.child(ui::tag("recommended", p.accent, &p))),
        );
    }
    out
}

fn feed(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let mut out = div().flex().flex_col().child(ui::section("Questions", &p));
    if let Some(e) = root.board.questions_error() {
        out = out.children(error_line(e, root));
    }
    if !root.board.read_yet() {
        return out.child(ui::quiet("reading…", &p));
    }
    let questions = root.board.questions_newest_first();
    if questions.is_empty() && root.board.questions_error().is_none() {
        out = out.child(ui::quiet("nothing waits on you", &p));
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
                .gap_1()
                .mb_3()
                .p_3()
                .rounded(px(6.0))
                .bg(rgb(p.panel))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(div().flex_none().text_color(rgb(p.warn)).child("◇"))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .font_family(ui::PROSE)
                                .text_size(px(13.0))
                                .text_color(rgb(p.fg))
                                .child(q.title.clone()),
                        ),
                )
                .child(ui::text(about, Role::Fact, &p))
                .child(options(q, root, cx)),
        );
    }
    out
}

/// Seats and Questions side by side, each scrolling on its own, so a long
/// seat list never pushes the questions out of sight. Space sets them apart.
/// The bottom pad keeps the last row whole above the window edge.
pub(super) fn render(root: &Root, cx: &mut Context<Root>) -> Div {
    let column = |id: &'static str| -> Stateful<Div> {
        div()
            .id(id)
            .h_full()
            .overflow_x_hidden()
            .overflow_y_scroll()
            .pb_8()
    };
    div()
        .flex()
        .flex_row()
        .flex_1()
        .min_h_0()
        .w_full()
        .gap_6()
        .child(column("board-table").flex_1().min_w_0().child(table(root)))
        .child(
            column("board-feed")
                .w(px(QUESTIONS_W))
                .flex_none()
                .pr_4()
                .child(feed(root, cx)),
        )
}
