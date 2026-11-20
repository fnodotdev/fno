//! The answer sheet: one question, its choices, a free-text answer, over a
//! dimmed window. The key sheet's overlay look: a centered panel, the title
//! and an esc hint on top, the choices headed in the accent, the recommended
//! one marked with the accent chevron and its tag, a field for the user's
//! own words, and Answer/Later. Esc, Later or a click outside closes it;
//! Answer submits the typed words, AnswerPick::Words, or the picked choice,
//! the option path the board rows already ride.

use gpui::{div, prelude::*, px, rgb, rgba, Context, Div, ElementId, FontWeight, MouseButton};

use super::chat::composer::Draft;
use super::keys::Key;
use super::ui::{self, Role};
use super::window::{Action, AnswerState, Root};
use fno::needs_overlay::{QuestionItem, QuestionOption};

/// The sheet panel's width.
const SHEET_W: f32 = 560.0;

/// The sheet's held state: which question, which choice the user picked,
/// and what they typed. `pick` feeds Action::AnswerChoice; the draft feeds
/// the words path.
pub(super) struct Sheet {
    pub(super) id: String,
    pub(super) pick: Option<u32>,
    pub(super) draft: Draft,
}

impl Sheet {
    /// A fresh sheet for one question: nothing picked, nothing typed.
    pub(super) fn new(id: String) -> Self {
        Self {
            id,
            pick: None,
            draft: Draft::default(),
        }
    }
}

/// The sheet over a dimmed window. The body reads the board's open list; a
/// question that left it renders the gone state, so a sheet opened on a
/// stale row explains itself instead of dying quietly.
pub(super) fn render(root: &Root, cx: &mut Context<Root>) -> Div {
    let q = root
        .answer
        .as_ref()
        .and_then(|s| root.board.questions().iter().find(|q| q.item.id == s.id));
    let body = match q {
        Some(q) => sheet(&q.item, root, cx),
        None => gone(root, cx),
    };
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x0000_0066))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|root, _, _, cx| root.act(Action::CloseAnswer, cx)),
        )
        .child(body)
}

/// The sheet's gone state: the board no longer lists the question, so the
/// panel explains itself instead of dying quietly.
fn gone(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    div()
        .id("answer-sheet")
        .flex()
        .flex_col()
        .gap_2()
        .w(px(SHEET_W))
        .px_4()
        .py_3()
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(p.border))
        .bg(rgb(p.panel))
        .font_family(ui::MONO)
        .text_size(px(ui::BODY))
        .child(ui::text("the question left the board", Role::Meta, &p))
        .child(
            ui::button("answer-later-gone", "Later", ui::Button::Quiet, &p)
                .on_click(cx.listener(|root, _, _, cx| root.act(Action::CloseAnswer, cx))),
        )
}

/// The sheet body for one open question.
fn sheet(q: &QuestionItem, root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let state = root.answers.get(&q.id);
    let sending = matches!(
        state,
        Some(AnswerState::Sending(_)) | Some(AnswerState::SendingWords)
    );
    let now = fno::app_api::digest_overlay::now_secs();
    let mut out = div()
        .id("answer-sheet")
        .flex()
        .flex_col()
        .gap_1()
        .w(px(SHEET_W))
        .px_4()
        .py_3()
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(p.border))
        .bg(rgb(p.panel))
        .font_family(ui::MONO)
        .text_size(px(ui::BODY))
        // A click on the panel stays on it: the scrim behind closes, this
        // panel does not.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .flex_row()
                .items_baseline()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(ui::cut(&q.title, 56)),
                )
                .child(ui::kbd("esc close", &p)),
        );
    if let Some(line) = subtitle(q, root, now) {
        out = out.child(ui::text(line, Role::Meta, &p));
    }
    if let Some(body) = q.body.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        out = out.child(
            div()
                .font_family(ui::PROSE)
                .text_size(px(13.0))
                .text_color(rgb(p.fg))
                .child(body.to_string()),
        );
    }
    if !q.options.is_empty() {
        out = out.child(
            ui::role(div(), Role::Label, &p)
                .pt(px(10.0))
                .text_color(rgb(p.accent))
                .child("Choices"),
        );
    }
    for o in &q.options {
        out = out.child(choice_row(o, q, root, cx));
    }
    if let Some(AnswerState::Refused(why)) = state {
        let line = ui::text(format!("answer refused: {why}"), Role::Fact, &p);
        out = out.child(line.text_color(rgb(p.error)));
    }
    if !sending {
        out = out.child(field(root)).child(buttons(root, cx));
    }
    out
}

/// One choice row: the picked dot, the recommended chevron, the option's
/// text, and the recommended tag. A click picks; Answer sends.
fn choice_row(o: &QuestionOption, q: &QuestionItem, root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let sheet_pick = root.answer.as_ref().and_then(|s| s.pick);
    let recommended = q.recommendation.as_ref().map(|r| r.option);
    let picked = sheet_pick == Some(o.n);
    let is_rec = recommended == Some(o.n);
    let (glyph, glyph_color) = if picked {
        ("●".to_string(), p.accent)
    } else if is_rec {
        ("▸".to_string(), p.accent)
    } else {
        (String::new(), p.faint)
    };
    let action = Action::AnswerChoice(o.n);
    let id = format!("answer-choice-{}", o.n);
    div()
        .id(ElementId::Name(id.into()))
        .flex()
        .flex_row()
        .items_start()
        .gap_2()
        .px_2()
        .py(px(5.0))
        .rounded(px(4.0))
        .cursor_pointer()
        .when(picked, |d| d.bg(rgb(p.row_fill)))
        .hover(move |s| s.bg(rgb(p.row_fill)))
        .active(move |s| s.bg(rgb(p.pressed)))
        .on_click(cx.listener(move |root, _, _, cx| root.act(action.clone(), cx)))
        .child(
            div()
                .flex_none()
                .w(root.adv * 2.0)
                .text_color(rgb(glyph_color))
                .child(glyph),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .font_family(ui::PROSE)
                        .text_size(px(13.0))
                        .child(o.text.clone()),
                )
                .when_some(o.next.clone().filter(|n| !n.is_empty()), |d, next| {
                    d.child(ui::text(next, Role::Meta, &p))
                }),
        )
        .when(is_rec && !picked, |d| {
            d.child(ui::tag("Recommended", p.accent, &p))
        })
}

/// The free-text line: a raised field with the typed words or the
/// placeholder and a caret.
fn field(root: &Root) -> Div {
    let p = root.palette;
    let draft = root
        .answer
        .as_ref()
        .map(|s| s.draft.text().trim().to_string())
        .unwrap_or_default();
    let (text, role) = if draft.is_empty() {
        ("or type your own answer".to_string(), Role::Hint)
    } else {
        (draft, Role::Body)
    };
    div()
        .mt_2()
        .px_2()
        .py(px(5.0))
        .rounded(px(4.0))
        .bg(rgb(p.raised))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(ui::role(div().flex_1().min_w_0(), role, &p).child(text))
                .child(div().text_color(rgb(p.faint)).child("|")),
        )
}

/// Answer and Later.
fn buttons(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    div()
        .flex()
        .flex_row()
        .gap_2()
        .pt_1()
        .child(
            ui::button("answer-submit", "Answer", ui::Button::Primary, &p)
                .on_click(cx.listener(|root, _, _, cx| root.act(Action::SubmitAnswer, cx))),
        )
        .child(
            ui::button("answer-later", "Later", ui::Button::Quiet, &p)
                .on_click(cx.listener(|root, _, _, cx| root.act(Action::CloseAnswer, cx))),
        )
}

/// The sheet's subtitle: `handle · model · age`, the asker's model joined
/// off the live roster. A piece with no source drops out of the line.
fn subtitle(q: &QuestionItem, root: &Root, now: u64) -> Option<String> {
    let asker = q.asker.as_ref()?;
    let model = asker
        .session_id
        .as_deref()
        .and_then(|sid| {
            root.layout
                .agents
                .iter()
                .find(|a| a.harness_session_id.as_deref() == Some(sid))
        })
        .map(|a| {
            fno::app_api::client::card_line::card_facts(a, now)
                .model
                .unwrap_or_else(|| a.harness.clone().unwrap_or_default())
        })
        .filter(|m| !m.is_empty());
    let mut parts: Vec<String> = Vec::new();
    parts.push(asker.handle.clone());
    if let Some(m) = model {
        parts.push(m);
    }
    if !q.created_at.is_empty() {
        parts.push(format!("{} ago", ui::age(&q.created_at, now)));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The sheet's keys. True when the sheet took the key: escape closes, enter
/// submits, the editing keys and clean text land in the draft, the grammar
/// the board composer edits with. A cmd or ctrl key falls through, so the
/// shortcuts keep running under the sheet.
pub(super) fn key(root: &mut Root, key: &Key, cx: &mut Context<Root>) -> bool {
    let sheet = match root.answer.as_mut() {
        Some(s) => s,
        None => return false,
    };
    if sheet.draft.marked().is_some() {
        return true;
    }
    let (shift, cmd) = (key.shift, key.cmd);
    let mut action = None;
    match key.key {
        "escape" => action = Some(Action::CloseAnswer),
        "enter" if !shift && !cmd && !key.ctrl => action = Some(Action::SubmitAnswer),
        "backspace" => sheet.draft.backspace(),
        "delete" => sheet.draft.delete(),
        "left" => sheet.draft.left(shift),
        "right" => sheet.draft.right(shift),
        "home" => sheet.draft.home(shift),
        "end" => sheet.draft.end(shift),
        _ => {
            let clean = key
                .key_char
                .filter(|t| !cmd && !key.ctrl && !t.chars().any(char::is_control));
            match clean {
                Some(ch) => sheet.draft.replace_selection(ch),
                None => return false,
            }
        }
    }
    if let Some(a) = action {
        root.act(a, cx);
    }
    cx.notify();
    true
}

/// The Answer button and Enter: the typed words win over the picked
/// choice. The in-flight and answered states land in the same map the
/// board rows read, so every surface stops double-answering together.
pub(super) fn submit(root: &mut Root, cx: &mut Context<Root>) {
    let Some(sheet) = root.answer.as_ref() else {
        return;
    };
    let (id, pick) = (sheet.id.clone(), sheet.pick);
    let words = sheet.draft.text().trim().to_string();
    if root.poller.is_none() {
        root.say("no board poll: cannot answer".into(), cx);
        return;
    }
    if root.answers.contains_key(&id) {
        return;
    }
    if !words.is_empty() {
        if let Some(poller) = root.poller.as_ref() {
            poller.answer_words(id.clone(), words);
        }
        root.answers.insert(id, AnswerState::SendingWords);
    } else if let Some(option) = pick {
        if let Some(poller) = root.poller.as_ref() {
            poller.answer(id.clone(), option);
        }
        root.answers.insert(id, AnswerState::Sending(option));
    } else {
        root.say("pick a choice or type your answer".into(), cx);
    }
    cx.notify();
}
