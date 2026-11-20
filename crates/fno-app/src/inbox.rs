//! The Inbox view: the navigation for questions and decisions. Open lists
//! what waits on the user, each row one click from its answer sheet;
//! Answered lists what the fold recently settled, the answer at the row's
//! right edge. Hovering an open row swaps its fact columns for the Answer
//! button, the comp's hover state.

use gpui::{div, prelude::*, px, rgb, Context, Div, ElementId};

use super::ui::{self, Role};
use super::window::{Action, Root};
use fno::needs_overlay::QuestionItem;

/// The fact columns at an open row's right edge: asker, node, age, in
/// small advances.
const COLS: [usize; 3] = [10, 8, 4];

/// The Inbox body: the Open and Answered sections over one scroll, and the
/// hint line under them.
pub(super) fn render(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let now = fno::app_api::digest_overlay::now_secs();
    let mut out = div()
        .id("inbox-body")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .w_full()
        .overflow_y_scroll()
        .pb_8();
    if let Some(e) = root.board.questions_error() {
        out = out.child(
            ui::text(e.to_string(), Role::Fact, &p)
                .text_color(rgb(p.error))
                .px_3()
                .py_1(),
        );
    }
    if !root.board.read_yet() {
        out = out.child(ui::quiet("reading…", &p).px_3());
        return out;
    }
    let questions = root.board.questions_newest_first();
    out = out.child(open_section(&questions, &p, now, root, cx));
    let answered = root.board.answered();
    out = out.child(answered_section(answered, &p, now, root, cx));
    out.child(ui::quiet(
        "Click a question to answer it in place. The asking agent keeps working.",
        &p,
    ))
}

/// The Open section: one row per question the fold lists, the count in the
/// header. A row opens its answer sheet; hover swaps the facts for the
/// Answer button.
fn open_section(
    questions: &[&fno::app_api::web_hq::OpenQuestion],
    p: &super::palette::Palette,
    now: u64,
    root: &Root,
    cx: &mut Context<Root>,
) -> Div {
    let mut out = div().flex().flex_col().child(
        ui::section("Open", p)
            .px_3()
            .justify_between()
            .child(warn_count(questions.len(), p)),
    );
    if questions.is_empty() {
        out = out.child(ui::quiet("nothing waits on you", p).px_3());
        return out;
    }
    for (i, q) in questions.iter().enumerate() {
        out = out.child(question_row(&q.item, i, p, now, root, cx));
    }
    out
}

/// The section's right edge: the filled diamond and the open count, in the
/// warn color, shape carrying the meaning.
fn warn_count(n: usize, p: &super::palette::Palette) -> Div {
    ui::role(div(), Role::Fact, p)
        .flex()
        .flex_row()
        .gap(px(3.0))
        .child(div().text_color(rgb(p.warn)).child("◆"))
        .child(n.to_string())
}

/// One open row: the diamond, the title, and the fact columns that hover
/// swaps for the Answer button.
fn question_row(
    q: &QuestionItem,
    i: usize,
    p: &super::palette::Palette,
    now: u64,
    root: &Root,
    cx: &mut Context<Root>,
) -> Div {
    let small = root.adv * (ui::SMALL / ui::BODY);
    let action = Action::OpenAnswer(q.id.clone());
    let id = format!("inbox-q-{i}");
    let answered = matches!(
        root.answers.get(&q.id),
        Some(super::window::AnswerState::Sending(_))
            | Some(super::window::AnswerState::SendingWords)
    );
    let facts = div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        // The comp's hover swap: the facts yield to the Answer button.
        .group_hover("inbox-row", |s| s.opacity(0.0))
        .child(fact_cell(
            q.asker.as_ref().map(|a| a.handle.as_str()),
            COLS[0],
            p,
            small,
        ))
        .child(fact_cell(q.node.as_deref(), COLS[1], p, small))
        .child(fact_cell(
            Some(ui::age(&q.created_at, now).as_str()),
            COLS[2],
            p,
            small,
        ));
    let answer_btn = ui::button("inbox-answer", "Answer", ui::Button::Primary, p)
        .opacity(0.0)
        .group_hover("inbox-row", |s| s.opacity(1.0))
        .when(!answered, |d| {
            d.on_click(
                cx.listener(move |root, _, _, cx| root.act(Action::OpenAnswer(q.id.clone()), cx)),
            )
        });
    let open_click = Action::OpenAnswer(q.id.clone());
    ui::row(ElementId::Name(id.into()), false, p)
        .group("inbox-row")
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(p.row_fill)))
        .on_click(cx.listener(move |root, _, _, cx| root.act(open_click.clone(), cx)))
        .child(
            div()
                .w(root.adv * 2.0)
                .flex_none()
                .text_color(rgb(p.warn))
                .child("◆"),
        )
        .child(ui::clamp(q.title.clone(), Role::Name, p))
        .child(facts)
        .child(answer_btn)
}

/// One right-aligned fact cell in a small advance, empty when absent.
fn fact_cell(
    v: Option<&str>,
    chars: usize,
    p: &super::palette::Palette,
    small: gpui::Pixels,
) -> Div {
    ui::cell(v.unwrap_or_default(), chars, true, Role::Fact, p, small)
}

/// The Answered section: what the fold recently settled, the answer at
/// the row's right edge, the count on the header.
fn answered_section(
    answered: &[fno::needs_overlay::AnsweredItem],
    p: &super::palette::Palette,
    now: u64,
    root: &Root,
) -> Div {
    let mut out = div().flex().flex_col().child(
        ui::section("Answered", p)
            .px_3()
            .justify_between()
            .child(ok_count(answered.len(), p)),
    );
    if answered.is_empty() {
        out = out.child(ui::quiet("nothing answered yet", p).px_3());
        return out;
    }
    for (i, a) in answered.iter().enumerate() {
        let id = format!("inbox-a-{i}");
        let small = root.adv * (ui::SMALL / ui::BODY);
        out = out.child(
            ui::row(ElementId::Name(id.into()), false, p)
                .child(
                    div()
                        .w(root.adv * 2.0)
                        .flex_none()
                        .text_color(rgb(p.ok))
                        .child("✓"),
                )
                .child(ui::clamp(a.title.clone(), Role::Name, p))
                .child(ui::clamp(a.answer.clone(), Role::Meta, p).max_w(px(220.0)))
                .child(fact_cell(
                    Some(ui::age(&a.at, now).as_str()),
                    COLS[2],
                    p,
                    small,
                )),
        );
    }
    out
}

/// The Answered header's right edge: the check and the count, in the ok
/// color.
fn ok_count(n: usize, p: &super::palette::Palette) -> Div {
    ui::role(div(), Role::Fact, p)
        .flex()
        .flex_row()
        .gap(px(3.0))
        .child(div().text_color(rgb(p.ok)).child("✓"))
        .child(n.to_string())
}
