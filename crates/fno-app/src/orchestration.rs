//! Orchestration mode: the command center board. Seats lists every
//! machine's seats and their nodes; Next lists the ready, unclaimed backlog
//! rows; Questions lists what waits on the user, each option one click; the
//! composer routes a message to the owning lead. All read the shared HQ
//! board, so they match `/hq/model.json`. A failed read shows its error
//! above the last good rows.

use gpui::{div, prelude::*, px, rgb, Context, Div, ElementId, Pixels, Stateful};

use super::chat::composer::Draft;
use super::keys::Key;
use super::left_pane;
use super::ui::{self, Role};
use super::window::{Action, AnswerState, MailState, Root};
use fno::app_api::client::org_seats::Machine;
use fno::app_api::lattice::{lattice_glyph, LatticeState};
use fno::app_api::web_hq::Board;

/// A node id's width in advances: an 11-character id shows whole.
const NODE_W: usize = 11;
/// The questions column's width.
const QUESTIONS_W: f32 = 440.0;
/// The next queue's column width.
const NEXT_W: f32 = 300.0;
/// The next queue shrinks to this before the seat table gives up a cell.
const NEXT_MIN_W: f32 = 140.0;

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

/// A machine's shown name.
fn host_name(m: &Machine) -> String {
    m.display_name.clone().unwrap_or_else(|| m.host.clone())
}

/// The seat table's column widths, in mono cells.
struct Widths {
    /// More than one host has seats, so the Machine column shows.
    hosts: bool,
    host_w: usize,
    seat_w: usize,
    title_w: usize,
    /// The narrowest the table column may get: its side padding, the fixed
    /// cells with their gaps, and room for the Title heading. Under it the
    /// column's own clip would cut a heading short.
    min_w: Pixels,
}

fn widths(root: &Root, machines: &[Machine]) -> Widths {
    let adv = root.adv;
    // Count only hosts with a seat row: a seatless host shows no row. One
    // host in view: a machine column would repeat one name.
    let mut names: Vec<String> = machines
        .iter()
        .filter(|m| m.leads.iter().any(|l| !l.seats.is_empty()))
        .map(host_name)
        .collect();
    names.sort();
    names.dedup();
    let hosts = names.len() > 1;
    // Machine and Seat take their widest value, capped, so the title keeps
    // the rest of the row.
    let host_w = names
        .iter()
        .map(|n| n.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(7, 14);
    let seat_w = machines
        .iter()
        .flat_map(|m| m.leads.iter().flat_map(|l| &l.seats))
        .map(|s| s.name.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(4, 18);
    // The title column takes the rest of the row. Mono cells measure exactly,
    // so the title is cut whole to that many cells: the sidebar, the
    // questions column, the gaps, the glyph and the fixed columns come off.
    let fixed = 2 + seat_w + 9 + NODE_W + if hosts { host_w } else { 0 };
    // One gap between each pair of cells, and the row's own side padding.
    let gaps = if hosts { 5.0 } else { 4.0 };
    let gap = px(8.0) * gaps;
    let min_w = px(24.0) + adv * (fixed + 5) as f32 + gap;
    let rest =
        root.width - px(super::left_pane::WIDTH) - px(QUESTIONS_W) - px(NEXT_W) - px(24.0 * 3.0);
    let rest = rest - adv * fixed as f32 - gap;
    let title_w = (rest / adv).floor().max(4.0) as usize;
    Widths {
        hosts,
        host_w,
        seat_w,
        title_w,
        min_w,
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
    let Widths {
        hosts,
        host_w,
        seat_w,
        title_w,
        ..
    } = widths(root, machines);
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
        heads = heads.child(head("Machine", host_w));
    }
    out = out.child(
        heads
            .child(head("Seat", seat_w))
            .child(head("Status", 9))
            .child(head("Node", NODE_W))
            .child(head("Title", 0)),
    );
    if !root.board.read_yet() {
        return out.child(ui::quiet("reading…", &p).px_3());
    }
    for (n, m) in machines.iter().enumerate() {
        let host = host_name(m);
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
                row = row.child(col(host.clone(), host_w, Role::Meta, root, adv));
            }
            out = out.child(
                row.child(col(seat.name.clone(), seat_w, Role::Name, root, adv))
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
    let sending_words = matches!(
        state,
        Some(AnswerState::SendingWords) | Some(AnswerState::DoneWords)
    );
    if let Some(AnswerState::Sending(n) | AnswerState::Done(n)) = state {
        let (text, color) = match state {
            Some(AnswerState::Done(_)) => (format!("answered: option {n}"), p.ok),
            _ => (format!("sending option {n}…"), p.muted),
        };
        return out.child(ui::text(text, Role::Meta, &p).text_color(rgb(color)).py_1());
    }
    if sending_words {
        let done = matches!(state, Some(AnswerState::DoneWords));
        let (text, color) = if done {
            ("answered".to_string(), p.ok)
        } else {
            ("sending your answer…".to_string(), p.muted)
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

/// True when a read landed, none of the three reads is failing, and seats,
/// questions and the next queue are all empty: the board area then shows the
/// designed empty state and the per-section quiet lines stand down. A fleet
/// failure counts, or an org read outage would paint "the fleet is quiet"
/// over an error the seats column never gets to show.
fn quiet_board(board: &Board) -> bool {
    board.read_yet()
        && board.fleet_error().is_none()
        && board.questions_error().is_none()
        && board.questions_newest_first().is_empty()
        && board.next_error().is_none()
        && board.next().is_empty()
        && !any_seat(board)
}

/// True when any machine's fold lists a seat.
fn any_seat(board: &Board) -> bool {
    board.fleet().is_some_and(|f| {
        f.machines
            .iter()
            .any(|m| m.leads.iter().any(|l| !l.seats.is_empty()))
    })
}

/// The designed empty state: one quiet block, the idle lattice glyph, one
/// hint line. The composer stays visible under it.
fn empty_state(root: &Root) -> Div {
    let p = root.palette;
    div()
        .flex()
        .flex_col()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            div()
                .text_color(rgb(p.faint))
                .child(lattice_glyph(LatticeState::Idle).0.to_string()),
        )
        .child(ui::text("the fleet is quiet", Role::Body, &p))
        .child(ui::text(
            "seats, next work and questions appear here",
            Role::Meta,
            &p,
        ))
}

/// The Next column: ready, unclaimed rows in keeper order, one line each in
/// the app's one-line density. A failed read shows its error above the last good
/// rows, as the seats and questions do.
fn next_column(root: &Root) -> Div {
    let p = root.palette;
    let adv = root.adv;
    let mut out = div().flex().flex_col().child(ui::section("Next", &p));
    if let Some(e) = root.board.next_error() {
        out = out.children(error_line(e, root));
    }
    if !root.board.read_yet() {
        return out.child(ui::quiet("reading…", &p));
    }
    let rows = root.board.next();
    if rows.is_empty() && root.board.next_error().is_none() {
        out = out.child(ui::quiet("nothing queued", &p));
    }
    let fixed = 2 + NODE_W + 4;
    let rest = root.width - px(left_pane::WIDTH) - px(NEXT_W) - px(QUESTIONS_W) - px(48.0);
    let title_w = ((rest / adv) - fixed as f32 - 24.0).floor().max(4.0) as usize;
    for row in rows.iter() {
        let id = row["id"].as_str().unwrap_or_default();
        let title = row["title"].as_str().unwrap_or_default();
        let priority = row["priority"].as_str().unwrap_or_default();
        let line = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .h(px(20.0))
            .px_3()
            .child(
                div()
                    .w(adv * 2.0)
                    .flex_none()
                    .text_color(rgb(p.faint))
                    .child(lattice_glyph(LatticeState::Unmeasured).0.to_string()),
            )
            .child(col(id.to_string(), NODE_W, Role::Meta, root, adv))
            .child(col(priority.to_string(), 4, Role::Hint, root, adv))
            .child(col(ui::cut(title, title_w), 0, Role::Body, root, adv));
        out = out.child(line);
    }
    out
}

/// The HQ composer: a hairline-split row under the columns, a `to:` chip
/// naming the owning lead, the draft line with a caret, and the send state.
/// Enter routes the draft through the poll thread's mail door.
fn composer(root: &Root) -> Div {
    let p = root.palette;
    let target = super::hq::owning_lead(root.board.fleet())
        .unwrap_or_else(|| "no lead on this machine".into());
    let mut out = div()
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .pb_2()
        .pt_1()
        .border_t_1()
        .border_color(rgb(p.border));
    match &root.mail {
        Some(MailState::Sending) => {
            out = out.child(ui::text("sending", Role::Meta, &p).text_color(rgb(p.muted)))
        }
        Some(MailState::Sent(v)) => {
            out = out.child(ui::text(v.clone(), Role::Meta, &p).text_color(rgb(p.ok)))
        }
        Some(MailState::Refused(v)) => {
            out = out.child(ui::text(v.clone(), Role::Fact, &p).text_color(rgb(p.error)))
        }
        None => {}
    }
    let draft = root.board_draft.text();
    let (body, role) = if draft.is_empty() {
        ("message the lead".to_string(), Role::Meta)
    } else {
        (draft.to_string(), Role::Body)
    };
    out.child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(ui::tag(format!("to: {target}"), p.accent, &p))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(ui::text(body, role, &p))
                    .child(div().text_color(rgb(p.faint)).child("|")),
            ),
    )
}

/// A key in orchestration mode edits the mail draft. An IME draft takes the
/// keys.
pub(super) fn key(root: &mut Root, key: &Key, cx: &mut Context<Root>) {
    if root.board_draft.marked().is_some() {
        return;
    }
    let (shift, cmd) = (key.shift, key.cmd);
    match key.key {
        "enter" if !shift && !cmd && !key.ctrl => {
            // A whitespace draft stays put, as chat's submit does; a real
            // one goes out through the same action a click would take, so
            // the send records its usage event.
            if !root.board_draft.text().trim().is_empty() {
                let text = root.board_draft.take();
                root.act(Action::Mail(text), cx);
            }
        }
        "escape" if !root.board_draft.is_empty() => {
            root.board_draft = Draft::default();
            cx.stop_propagation();
            cx.notify();
        }
        "backspace" => root.board_draft.backspace(),
        "delete" => root.board_draft.delete(),
        "left" => root.board_draft.left(shift),
        "right" => root.board_draft.right(shift),
        "home" => root.board_draft.home(shift),
        "end" => root.board_draft.end(shift),
        _ => {
            // Only real text lands in the one-line draft: named keys kept
            // their control characters (shift-enter's `\r`, tab's `\t`),
            // which would ship to the mail door verbatim.
            if let Some(ch) = key
                .key_char
                .filter(|t| !cmd && !key.ctrl && !t.chars().any(char::is_control))
            {
                root.board_draft.replace_selection(ch);
                cx.stop_propagation();
            } else {
                return;
            }
        }
    }
    cx.notify();
}

/// Seats, Next and Questions side by side, each scrolling on its own, so a
/// long seat list never pushes the questions out of sight. The composer row
/// sits under the columns; when a read lands with everything empty, the
/// board area shows the designed empty state and the composer stays.
pub(super) fn render(root: &Root, cx: &mut Context<Root>) -> Div {
    let quiet = quiet_board(&root.board);
    let column = |id: &'static str| -> Stateful<Div> {
        div()
            .id(id)
            .h_full()
            .overflow_x_hidden()
            .overflow_y_scroll()
            .pb_8()
    };
    let table_min = widths(
        root,
        root.board
            .fleet()
            .map(|f| f.machines.as_slice())
            .unwrap_or_default(),
    )
    .min_w;
    let columns = div()
        .flex()
        .flex_row()
        .flex_1()
        .min_h_0()
        .w_full()
        .gap_6()
        .child(
            column("board-table")
                .flex_1()
                .min_w(table_min)
                .child(table(root)),
        )
        .child(
            column("board-next")
                .w(px(NEXT_W))
                .flex_shrink()
                .min_w(px(NEXT_MIN_W))
                .child(next_column(root)),
        )
        .child(
            column("board-feed")
                .w(px(QUESTIONS_W))
                .flex_none()
                .pr_4()
                .child(feed(root, cx)),
        );
    if quiet {
        return div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(empty_state(root))
            .child(composer(root));
    }
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .w_full()
        .child(columns)
        .child(composer(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fno::app_api::web_hq::Board;
    use fno::backlog_model::Inputs;
    use serde_json::json;

    fn empty_board_after_read() -> Board {
        let mut b = Board::default();
        b.record(
            Ok(json!({ "machines": [] })),
            Ok(fno::needs_overlay::QuestionsFold::default()),
            &Inputs::default(),
            1,
            &[],
        );
        b
    }

    #[test]
    fn the_empty_state_needs_a_read_and_three_empty_folds() {
        assert!(!quiet_board(&Board::default()));
        assert!(quiet_board(&empty_board_after_read()));
    }

    #[test]
    fn next_rows_block_the_empty_state() {
        let rows = vec![json!({"id": "x-1", "status": "ready", "title": "t"})];
        let mut inputs = Inputs::default();
        inputs.rows = fno::node_row::rows_from_values(rows);
        let mut b = Board::default();
        b.record(
            Ok(json!({ "machines": [] })),
            Ok(fno::needs_overlay::QuestionsFold::default()),
            &inputs,
            1,
            &[],
        );
        assert!(!quiet_board(&b));
    }

    #[test]
    fn a_failed_fleet_read_blocks_the_empty_state() {
        let mut b = Board::default();
        b.record(
            Err("org unreachable".into()),
            Ok(fno::needs_overlay::QuestionsFold::default()),
            &Inputs::default(),
            1,
            &[],
        );
        assert!(!quiet_board(&b));
    }

    #[test]
    fn the_composer_routes_to_the_local_machines_first_lead() {
        use fno::app_api::client::org_seats::{Fleet, Lead, Machine};
        let mut m = Machine::default();
        m.local = true;
        m.leads = vec![
            Lead {
                name: "philip".into(),
                seats: vec![],
            },
            Lead {
                name: "gus".into(),
                seats: vec![],
            },
        ];
        let fleet = Fleet {
            machines: vec![m],
            ..Default::default()
        };
        assert_eq!(
            super::super::hq::owning_lead(Some(&fleet)).as_deref(),
            Some("philip")
        );
        assert_eq!(super::super::hq::owning_lead(None), None);
    }
}
