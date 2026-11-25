//! Drawing chat mode: a virtual list of rows in a centred column, the tool
//! sheet, the pending turns and the composer.

use std::ops::Range;

use gpui::{
    canvas, div, font, img, list, prelude::*, px, relative, rgb, rgba, AnyElement, Context,
    CursorStyle, Div, ElementId, Font, FontStyle, FontWeight, Hsla, InteractiveText, MouseButton,
    ObjectFit, SharedString, Stateful, StrikethroughStyle, StyledImage, StyledText, TextRun,
    UnderlineStyle, Window,
};

use super::field::Field;
use super::markdown::{Block, Inline, Runs};
use super::model::{Ask, Choice, Content, Item, Picture};
use super::{Closed, EMPTY_AFTER};
use crate::find;
use crate::palette::Palette;
use crate::ui::{self, Role};
use crate::window::{Action, Root};
use fno::app_api::client::agent_lattice_state;
use fno::app_api::client::card_line::card_facts;
use fno::app_api::lattice::{lattice_glyph, status_long};
use fno::proto::AgentRow;

/// The reading column's width.
const COLUMN: f32 = 720.0;
/// The most output a tool sheet shows.
const OUTPUT_MAX: usize = 20_000;
const UI_FONT: &str = ui::PROSE;

/// One clickable row: padded, with a hover and a pressed background. It acts
/// on click, so a press dragged off it does nothing.
fn clickable(id: ElementId, action: Action, p: Palette, cx: &mut Context<Root>) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded(px(4.0))
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(p.hover)))
        .active(move |s| s.bg(rgb(p.pressed)))
        .on_click(cx.listener(move |root, _, _, cx| root.act(action.clone(), cx)))
}

fn button(
    id: &'static str,
    label: &'static str,
    action: Action,
    p: Palette,
    cx: &mut Context<Root>,
) -> Stateful<Div> {
    ui::button(id, label, ui::Button::Quiet, &p)
        .on_click(cx.listener(move |root, _, _, cx| root.act(action.clone(), cx)))
}

fn muted(text: impl Into<SharedString>, p: Palette) -> Div {
    div().text_color(rgb(p.muted)).child(text.into())
}

/// Text runs for `runs`: bold, italic, struck, mono code and accent links.
/// `hl` carries the find-highlight ranges: cuts inside one paint accent,
/// the current match at the grid's selected alpha, the rest lighter.
fn rich(
    hl: &[(Range<usize>, bool)],
    id: ElementId,
    runs: &Runs,
    base: Font,
    color: u32,
    mono: &SharedString,
    p: Palette,
) -> AnyElement {
    let mut cuts = vec![0, runs.text.len()];
    for (r, _) in &runs.spans {
        cuts.extend([r.start, r.end]);
    }
    for (r, _) in hl {
        cuts.extend([r.start, r.end]);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let mut run = TextRun {
            len: b - a,
            font: base.clone(),
            color: rgb(color).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        for (_, style) in runs
            .spans
            .iter()
            .filter(|(r, _)| r.start <= a && b <= r.end)
        {
            match style {
                Inline::Bold => run.font.weight = FontWeight::BOLD,
                Inline::Italic => run.font.style = FontStyle::Italic,
                Inline::Code => {
                    run.font.family = mono.clone();
                    run.background_color = Some(rgb(p.raised).into());
                }
                Inline::Strike => {
                    run.strikethrough = Some(StrikethroughStyle {
                        thickness: px(1.0),
                        color: Some(rgb(color).into()),
                    })
                }
                Inline::Link(_) => {
                    run.color = rgb(p.accent).into();
                    run.underline = Some(UnderlineStyle {
                        thickness: px(1.0),
                        color: Some(rgb(p.accent).into()),
                        wavy: false,
                    });
                }
            }
        }
        // The find tint wins over the span styles, so a hit inside code
        // text stays visible: the current match at the grid's selected
        // alpha, the rest lighter.
        for (r, cur) in hl {
            if r.start <= a && b <= r.end {
                let mut c: Hsla = rgb(p.accent).into();
                c.a = if *cur { 0.35 } else { 0.18 };
                run.background_color = Some(c);
            }
        }
        out.push(run);
    }
    let text = StyledText::new(SharedString::from(runs.text.clone())).with_runs(out);
    let links: Vec<(Range<usize>, String)> = runs
        .spans
        .iter()
        .filter_map(|(r, s)| match s {
            Inline::Link(url) => Some((r.clone(), url.clone())),
            _ => None,
        })
        .collect();
    if links.is_empty() {
        return text.into_any_element();
    }
    let ranges = links.iter().map(|(r, _)| r.clone()).collect();
    let urls: Vec<String> = links.into_iter().map(|(_, u)| u).collect();
    InteractiveText::new(id, text)
        .on_click(ranges, move |ix, _, cx| {
            if let Some(url) = urls.get(ix).filter(|u| fno::link::is_openable(u)) {
                fno::ui_usage::click("app", "chat_link");
                cx.open_url(url);
            }
        })
        .into_any_element()
}

/// `text` as styled runs with the find highlights painted under the glyphs:
/// the current match at the grid's selected alpha, the rest lighter. Plain
/// rows have no `rich()` runs to thread, so the highlights go straight on.
fn painted(
    text: &str,
    base: Font,
    color: u32,
    hl: &[(Range<usize>, bool)],
    p: Palette,
) -> StyledText {
    let mut cuts = vec![0, text.len()];
    for (r, _) in hl {
        cuts.extend([r.start, r.end]);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let runs = cuts
        .windows(2)
        .map(|w| {
            let (a, b) = (w[0], w[1]);
            let mut run = TextRun {
                len: b - a,
                font: base.clone(),
                color: rgb(color).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            for (r, cur) in hl {
                if r.start <= a && b <= r.end {
                    let mut c: Hsla = rgb(p.accent).into();
                    c.a = if *cur { 0.35 } else { 0.18 };
                    run.background_color = Some(c);
                }
            }
            run
        })
        .collect::<Vec<_>>();
    StyledText::new(SharedString::from(text.to_owned())).with_runs(runs)
}

fn picture(pic: &Picture, p: Palette) -> AnyElement {
    let shown = match pic {
        Picture::Bytes(bytes) => img(bytes.clone()),
        Picture::Path(path) => img(path.clone()),
        Picture::Url(url) => img(SharedString::from(url.clone())),
        Picture::Missing { bytes_len } => {
            return muted(format!("[image, {} KB, not sent]", bytes_len / 1024), p)
                .into_any_element()
        }
    };
    let fallback = p.muted;
    shown
        .max_w(px(480.0))
        .max_h(px(360.0))
        .rounded_md()
        .object_fit(ObjectFit::Contain)
        .with_fallback(move || {
            div()
                .text_color(rgb(fallback))
                .child("[image did not load]")
                .into_any_element()
        })
        .into_any_element()
}

/// One row's markdown. `ix` keys the link targets.
fn markdown(ix: usize, blocks: &[Block], color: u32, root: &Root) -> Div {
    let p = root.palette;
    let mono = root.metrics.font.family.clone();
    let id = |b: usize, n: usize| ElementId::Name(format!("md-{ix}-{b}-{n}").into());
    let ui = font(UI_FONT);
    let bold = Font {
        weight: FontWeight::BOLD,
        ..font(UI_FONT)
    };
    let mut out = div().flex().flex_col().gap_2();
    for (b, block) in blocks.iter().enumerate() {
        let text = |runs: &Runs, base: Font| {
            let hl = find::chat_ranges(root.find.as_ref(), ix, b, runs.text.len());
            rich(&hl, id(b, 0), runs, base, color, &mono, p)
        };
        out = out.child(match block {
            Block::Para(runs) => div().child(text(runs, ui.clone())),
            Block::Heading(level, runs) => {
                let d = div().pt_1().child(text(runs, bold.clone()));
                match *level {
                    1 => d.text_2xl(),
                    2 => d.text_xl(),
                    3 => d.text_lg(),
                    _ => d.text_base(),
                }
            }
            Block::Item {
                depth,
                marker,
                runs,
            } => div()
                .flex()
                .flex_row()
                .pl(px(20.0 * depth.saturating_sub(1) as f32))
                .child(
                    div()
                        .w(px(24.0))
                        .flex_none()
                        .text_color(rgb(p.muted))
                        .child(marker.clone()),
                )
                .child(div().flex_1().min_w_0().child(text(runs, ui.clone()))),
            Block::Code(code) => {
                let hl = find::chat_ranges(root.find.as_ref(), ix, b, code.len());
                let code_font = Font {
                    family: mono.clone(),
                    ..font(UI_FONT)
                };
                let body = if hl.is_empty() {
                    div().child(code.clone()).into_any_element()
                } else {
                    painted(code, code_font, p.fg, &hl, p).into_any_element()
                };
                div()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(p.raised))
                    .font_family(mono.clone())
                    .text_color(rgb(p.fg))
                    .child(body)
            }
            Block::Quote(runs) => {
                let hl = find::chat_ranges(root.find.as_ref(), ix, b, runs.text.len());
                div()
                    .pl_3()
                    .border_l_2()
                    .border_color(rgb(p.border))
                    .child(rich(&hl, id(b, 0), runs, ui.clone(), p.muted, &mono, p))
            }
            Block::Rule => div().h(px(1.0)).bg(rgb(p.border)),
            Block::Table { head, rows } => {
                let cols = rows
                    .iter()
                    .map(Vec::len)
                    .chain([head.len()])
                    .max()
                    .unwrap_or(0);
                let line = |r: usize, cells: &[Runs], base: Font, header: bool| {
                    let mut d = div().flex().flex_row();
                    if header {
                        d = d.bg(rgb(p.raised));
                    } else {
                        d = d.border_t_1().border_color(rgb(p.border));
                    }
                    for c in 0..cols {
                        let cell = cells.get(c).map(|runs| {
                            let hl = find::chat_ranges(root.find.as_ref(), ix, b, runs.text.len());
                            rich(
                                &hl,
                                id(b, r * cols + c + 1),
                                runs,
                                base.clone(),
                                color,
                                &mono,
                                p,
                            )
                        });
                        d = d.child(div().flex_1().min_w_0().px_2().py_1().children(cell));
                    }
                    d
                };
                let mut grid = div()
                    .flex()
                    .flex_col()
                    .bg(rgb(p.panel))
                    .rounded(px(4.0))
                    .overflow_hidden()
                    .child(line(0, head, bold.clone(), true));
                for (r, row) in rows.iter().enumerate() {
                    grid = grid.child(line(r + 1, row, ui.clone(), false));
                }
                grid
            }
            Block::Image { url, alt } => {
                let pic = if url.contains("://") {
                    Picture::Url(url.clone())
                } else {
                    Picture::Path(url.into())
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(picture(&pic, p))
                    .children((!alt.is_empty()).then(|| muted(alt.clone(), p).text_xs()))
            }
        });
    }
    out
}

fn bubble(content: &Content, pending: bool, p: Palette, hl: &[(Range<usize>, bool)]) -> Div {
    let body = div()
        .max_w(px(560.0))
        .px_3()
        .py_2()
        .rounded_lg()
        .bg(rgb(p.raised))
        .text_color(rgb(p.fg))
        .flex()
        .flex_col()
        .gap_2()
        .children((!content.text.is_empty()).then(|| {
            let text: AnyElement = if hl.is_empty() {
                div().child(content.text.clone()).into_any_element()
            } else {
                painted(content.text.as_str(), font(UI_FONT), p.fg, hl, p).into_any_element()
            };
            // The text carries its own width cap: with only the bubble box
            // capped, the flex measure runs the text at max-content, the box
            // sizes to one line, and the wrapped paint spills onto the row
            // below.
            div()
                .max_w(px(536.0))
                .min_w_0()
                .flex_shrink()
                .child(text)
                .into_any_element()
        }))
        .children(content.pictures.iter().map(|pic| picture(pic, p)))
        .children(pending.then(|| muted("sending…", p).text_xs()));
    // Measure at the list's full width: a shrink-wrapped flex row sizes its
    // wrapped text on one line and the taller paint overruns the next row.
    div().w_full().flex().flex_row().justify_end().child(body)
}

/// The folded head of a system-injected row. A task-notification block
/// names itself in its `<summary>` (HTML-escaped, all fleet blocks carry
/// one); any other injected row keeps its raw first line.
fn notice_head(text: &str) -> String {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("<task-notification>") {
        let first = trimmed.lines().next().unwrap_or_default();
        if first.is_empty() {
            return "System notice".to_string();
        }
        return first.to_string();
    }
    let at = trimmed
        .find("<summary>")
        .map(|at| &trimmed[at + "<summary>".len()..])
        .and_then(|rest| rest.find("</summary>").map(|end| rest[..end].trim()));
    match at {
        Some(s) if !s.is_empty() => s
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&"),
        _ => "Task notification".to_string(),
    }
}

/// A folded system-injected row: the caret head, and the full text open
/// under it when the row is expanded.
fn notice_fold(
    root: &Root,
    ix: usize,
    content: &Content,
    p: Palette,
    cx: &mut Context<Root>,
) -> AnyElement {
    let id = ElementId::NamedInteger(SharedString::from("notice".to_string()), ix as u64);
    let open = root.chat.conv.expanded.contains(&ix);
    let caret = if open { "▾" } else { "▸" };
    // The caret takes the sideline fold's treatment (default size,
    // secondary color) so the fold reads first; the label stays
    // small under it.
    let head = clickable(id, Action::ToggleChat(ix), p, cx).child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(div().text_color(rgb(p.muted)).child(caret))
            .child(
                div()
                    .text_color(rgb(p.muted))
                    .text_xs()
                    .line_clamp(1)
                    .text_ellipsis()
                    .child(notice_head(&content.text)),
            ),
    );
    let mut d = div().flex().flex_col().child(head);
    if open {
        let hl = find::chat_ranges(root.find.as_ref(), ix, 0, content.text.len());
        let full: AnyElement = if hl.is_empty() {
            muted(content.text.clone(), p).into_any_element()
        } else {
            painted(content.text.as_str(), font(UI_FONT), p.muted, &hl, p).into_any_element()
        };
        d = d.child(div().ml_2().mt_1().child(full));
    }
    d.into_any_element()
}

/// One list row, inside the centred column.
fn row(root: &mut Root, ix: usize, _window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let p = root.palette;
    let Some(item) = root.chat.conv.items.get(ix) else {
        return div().into_any_element();
    };
    let id = |name: &str| ElementId::NamedInteger(SharedString::from(name.to_string()), ix as u64);
    let body: AnyElement = match item {
        Item::User { content, .. } if item.folds_notice() => notice_fold(root, ix, content, p, cx),
        Item::User { content, .. } => {
            let hl = find::chat_ranges(root.find.as_ref(), ix, 0, content.text.len());
            bubble(content, false, p, &hl).into_any_element()
        }
        Item::Text { .. } => {
            markdown(ix, item.blocks().unwrap_or_default(), p.fg, root).into_any_element()
        }
        // A redacted block carries no text: nothing to open, so no fold.
        Item::Thinking { text, .. } if text.is_empty() => div().into_any_element(),
        Item::Thinking { .. } => {
            let open = root.chat.conv.expanded.contains(&ix);
            let caret = if open { "▾" } else { "▸" };
            let head = clickable(id("think"), Action::ToggleChat(ix), p, cx)
                .text_color(rgb(p.muted))
                .child(format!("{caret} Thinking"));
            let mut d = div().flex().flex_col().child(head);
            if open {
                d = d.child(
                    div()
                        .ml_2()
                        .pl_3()
                        .border_l_2()
                        .border_color(rgb(p.border))
                        .child(markdown(
                            ix,
                            item.blocks().unwrap_or_default(),
                            p.muted,
                            root,
                        )),
                );
            }
            d.into_any_element()
        }
        Item::Tool {
            name,
            summary,
            result,
            ..
        } => {
            let (mark, color) = match result {
                None => ("…", p.muted),
                Some((true, _)) => ("✓", p.ok),
                Some((false, _)) => ("✗", p.error),
            };
            tool_row(id("tool"), name, summary, mark, color, root, ix, cx).into_any_element()
        }
        Item::Question { ask, answer, .. } => {
            let said = answer
                .as_deref()
                .map(|a| format!(" · answered: {a}"))
                .unwrap_or_default();
            muted(format!("asked: {}{said}", ask.prompt), p)
                .line_clamp(2)
                .text_ellipsis()
                .into_any_element()
        }
        Item::Line(text) => {
            let hl = find::chat_ranges(root.find.as_ref(), ix, 0, text.len());
            let d = if hl.is_empty() {
                muted(text.clone(), p)
            } else {
                div()
                    .text_color(rgb(p.muted))
                    .child(painted(text, font(UI_FONT), p.muted, &hl, p))
            };
            d.line_clamp(1).text_ellipsis().into_any_element()
        }
    };
    column(
        div()
            .flex()
            .flex_col()
            // A definite width basis: bubble() measures itself at w_full,
            // and under a shrink-wrapped parent that resolve lands on
            // max-content, the one-line box the comments in bubble() warn
            // about.
            .w_full()
            .min_w_0()
            .children(author(root, ix))
            .child(body),
    )
    .py_1()
    .into_any_element()
}

/// One tool row: its mark, the tool name, and the find-tinted summary.
fn tool_row(
    id: ElementId,
    name: &str,
    summary: &str,
    mark: &'static str,
    color: u32,
    root: &Root,
    ix: usize,
    cx: &mut Context<Root>,
) -> AnyElement {
    let p = root.palette;
    let first = summary.lines().next().unwrap_or_default().to_string();
    let hl = find::chat_ranges(root.find.as_ref(), ix, 0, first.len());
    clickable(id, Action::OpenSheet(ix), p, cx)
        .flex()
        .flex_row()
        .gap_2()
        .font_family(ui::MONO)
        .text_size(px(ui::BODY))
        .text_color(rgb(p.muted))
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(rgb(color))
                .child(mark),
        )
        .child(
            div()
                .flex_none()
                .text_color(rgb(p.fg))
                .child(name.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .line_clamp(1)
                .text_ellipsis()
                .child(if hl.is_empty() {
                    first.clone().into_any_element()
                } else {
                    painted(&first, font(UI_FONT), p.muted, &hl, p).into_any_element()
                }),
        )
        .into_any_element()
}

/// The line that opens a turn: its author and the time it began.
fn author(root: &Root, ix: usize) -> Option<Div> {
    let you = speaker(&root.chat.conv.items, ix)?;
    let who = if you {
        "You".to_string()
    } else {
        agent(root).map_or_else(|| "Agent".to_string(), |a| a.name.clone())
    };
    let at = root.chat.conv.began.get(&ix).and_then(|ms| clock(*ms));
    let line = [Some(who), at].into_iter().flatten().collect::<Vec<_>>();
    Some(
        ui::text(line.join(" · "), Role::Fact, &root.palette)
            .font_family(ui::MONO)
            .pt_3()
            .pb_1()
            .when(you, |d| d.flex().flex_row().justify_end()),
    )
}

/// Whether row `ix` opens a turn, and whose: `Some(true)` for the user's,
/// `Some(false)` for the agent's, `None` when it continues the last one.
fn speaker(items: &[Item], ix: usize) -> Option<bool> {
    let side = |it: &Item| match it {
        Item::User { meta: false, .. } => Some(true),
        Item::Text { .. } | Item::Thinking { .. } | Item::Tool { .. } | Item::Question { .. } => {
            Some(false)
        }
        Item::User { meta: true, .. } | Item::Line(_) => None,
    };
    let me = side(items.get(ix)?)?;
    let before = items.get(..ix)?.iter().rev().find_map(side);
    (before != Some(me)).then_some(me)
}

/// A unix-ms stamp as the local wall clock.
fn clock(ms: i64) -> Option<String> {
    let t = chrono::DateTime::from_timestamp_millis(ms)?;
    Some(t.with_timezone(&chrono::Local).format("%H:%M").to_string())
}

/// The agent whose conversation shows: the one whose session the chat reads.
fn agent(root: &Root) -> Option<&AgentRow> {
    let key = root.chat.key.as_ref()?;
    root.layout
        .agents
        .iter()
        .find(|a| a.harness_session_id.as_deref() == Some(key.session_id.as_str()))
}

/// One line over the conversation: whose it is, the node, the state and
/// the cost so far.
fn header(root: &Root) -> Option<Div> {
    let p = root.palette;
    let key = root.chat.key.as_ref()?;
    let row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .h(px(32.0))
        .font_family(ui::MONO);
    let Some(a) = agent(root) else {
        let short: String = key.session_id.chars().take(8).collect();
        return Some(column(
            row.child(ui::text(key.harness.clone(), Role::Heading, &p))
                .child(ui::text(short, Role::Meta, &p)),
        ));
    };
    let state = agent_lattice_state(a);
    let now = fno::app_api::digest_overlay::now_secs();
    let facts = card_facts(a, now);
    Some(column(
        row.child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(
                    div()
                        .text_color(rgb(p.status(status_long(state))))
                        .child(lattice_glyph(state).0.to_string()),
                )
                .child(ui::text(a.name.clone(), Role::Heading, &p)),
        )
        .children(a.node.clone().map(|n| ui::text(n, Role::Meta, &p)))
        .child(ui::text(
            a.result
                .clone()
                .unwrap_or_else(|| status_long(state).to_string()),
            Role::Meta,
            &p,
        ))
        .child(div().flex_1())
        .children(facts.cost.map(|c| ui::text(c, Role::Fact, &p))),
    ))
}

fn column(child: impl IntoElement) -> Div {
    div()
        .w_full()
        .flex()
        .flex_row()
        .justify_center()
        .px_6()
        .child(div().w_full().max_w(px(COLUMN)).child(child))
}

/// A text cut at `max` chars and how many chars it dropped.
fn cut(text: &str, max: usize) -> (&str, usize) {
    match text.char_indices().nth(max) {
        Some((at, _)) => (&text[..at], text[at..].chars().count()),
        None => (text, 0),
    }
}

fn section(
    title: &'static str,
    copy: &'static str,
    scroll: &'static str,
    text: &str,
    root: &Root,
    cx: &mut Context<Root>,
) -> Div {
    let p = root.palette;
    let (shown, more) = cut(text, OUTPUT_MAX);
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .child(ui::text(title, Role::Label, &p).flex_1())
                .child(button(copy, "Copy", Action::Copy(text.to_string()), p, cx)),
        )
        .child(
            div()
                .id(scroll)
                .max_h(px(260.0))
                .overflow_y_scroll()
                .p_2()
                .rounded_md()
                .bg(rgb(p.bg))
                .font_family(root.metrics.font.family.clone())
                .text_xs()
                .text_color(rgb(p.fg))
                .child(shown.to_string()),
        )
        .children((more > 0).then(|| muted(format!("+{more} chars"), p).text_xs()))
}

/// The open tool call's sheet over the chat, with its input and output.
fn sheet(root: &Root, cx: &mut Context<Root>) -> Option<Stateful<Div>> {
    let p = root.palette;
    let Item::Tool {
        name,
        input,
        result,
        ..
    } = root.chat.conv.items.get(root.chat.sheet?)?
    else {
        return None;
    };
    let (status, color) = match result {
        None => ("running", p.muted),
        Some((true, _)) => ("succeeded", p.ok),
        Some((false, _)) => ("failed", p.error),
    };
    let mut panel = div()
        .w(px(760.0))
        .max_w(relative(0.9))
        .max_h(relative(0.85))
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded(px(6.0))
        .bg(rgb(p.panel))
        .font_family(ui::MONO)
        .text_size(px(ui::BODY))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(ui::text(name.clone(), Role::Heading, &p))
                .child(
                    ui::text(status, Role::Meta, &p)
                        .flex_1()
                        .text_color(rgb(color)),
                )
                .child(button("sheet-close", "Close", Action::CloseSheet, p, cx)),
        )
        .child(section(
            "Input",
            "sheet-copy-in",
            "sheet-in",
            input,
            root,
            cx,
        ));
    if let Some((_, out)) = result {
        panel = panel.child(section(
            "Output",
            "sheet-copy-out",
            "sheet-out",
            &out.text,
            root,
            cx,
        ));
        panel = panel.children(out.pictures.iter().map(|pic| picture(pic, p)));
    }
    Some(
        div()
            .id("sheet-backdrop")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .bg(rgba(0x0000_0099))
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|root, _, _, cx| root.act(Action::CloseSheet, cx)))
            .child(panel),
    )
}

fn status(root: &Root) -> Option<Div> {
    let p = root.palette;
    let empty = root.chat.conv.items.is_empty() && root.chat.closed.is_none();
    let text = match &root.chat.key {
        None => return None,
        Some(_) if empty && root.chat.opened_at.elapsed() < EMPTY_AFTER => {
            "Reading the conversation…"
        }
        Some(_) if empty => "No messages in this session yet.",
        Some(_) => return None,
    };
    Some(column(muted(text, p)).py_2())
}

/// The composer, or in its place the closed stream's reason and Reconnect.
/// The bar that replaces the composer when the chat stream is over: the end
/// reason and a reconnect button. The link's own exit reason folds in here;
/// the corner toast for it is suppressed while this bar stands (window
/// render).
fn closed_bar(root: &Root, p: Palette, cx: &mut Context<Root>) -> Option<Div> {
    let closed = root.chat.closed.as_ref()?;
    let (text, color) = match closed {
        Closed::Ended => match &root.closed {
            Some(reason) => (format!("The chat stream ended: {reason}."), p.error),
            None => ("The session's chat stream ended.".to_string(), p.muted),
        },
        Closed::Failed(why) => (why.clone(), p.error),
    };
    let bar = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .p_2()
        .rounded(px(6.0))
        .bg(rgb(p.raised))
        .child(div().flex_1().min_w_0().text_color(rgb(color)).child(text))
        .child(button(
            "chat-reconnect",
            "Reconnect",
            Action::Reconnect,
            p,
            cx,
        ));
    Some(column(bar).pb_3())
}

/// The strip that holds the composer's slot while the agent waits on an
/// answer: a Needs-you stamp and the question, one row per option with the
/// recommended one chipped, and under them the picked option's description.
/// A click picks, a click on the picked row answers; so do the arrows and
/// Enter. The chip is filled with the theme ink, never a hue of its own.
fn question_strip(root: &Root, ask: &Ask, p: Palette, cx: &mut Context<Root>) -> Div {
    let pick = root.chat.pick.min(ask.choices.len().saturating_sub(1));
    let mut strip = div()
        .flex()
        .flex_col()
        .px_3()
        .py_2()
        .gap_1()
        .rounded(px(8.0))
        .bg(rgb(p.raised))
        .child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::BOLD)
                        .text_color(rgb(p.warn))
                        .child("Needs you"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(rgb(p.fg))
                        .child(ask.prompt.clone()),
                ),
        );
    for (i, choice) in ask.choices.iter().enumerate() {
        strip = strip.child(choice_row(i, choice, i == pick, p, cx));
    }
    strip.child(choice_detail(ask.choices.get(pick), p))
}

/// One option row: the dot, the label and, for the agent's pick, the chip.
fn choice_row(
    i: usize,
    choice: &Choice,
    picked: bool,
    p: Palette,
    cx: &mut Context<Root>,
) -> Stateful<Div> {
    let mut row = div()
        .id(ElementId::NamedInteger("choice".into(), i as u64))
        .flex()
        .flex_row()
        .items_start()
        .gap_2()
        .px_2()
        .py_1()
        .rounded(px(4.0))
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(p.hover)))
        .on_click(cx.listener(move |root, _, _, cx| root.act(Action::PickChoice(i), cx)))
        .child(
            div()
                .flex_none()
                .text_color(rgb(if picked { p.fg } else { p.muted }))
                .child(if picked { "●" } else { "○" }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(rgb(p.fg))
                .child(choice.label.clone()),
        );
    if picked {
        row = row.bg(rgb(p.row_fill));
    }
    if choice.recommended {
        row = row.child(
            div()
                .flex_none()
                .px_1()
                .rounded(px(3.0))
                .bg(rgb(p.fg))
                .text_color(rgb(p.bg))
                .child("recommended"),
        );
    }
    row
}

/// The picked option's short name and description, under the rows.
fn choice_detail(picked: Option<&Choice>, p: Palette) -> Div {
    div()
        .flex()
        .flex_row()
        .gap_2()
        .mt_1()
        .pt_2()
        .border_t_1()
        .border_color(rgb(p.border))
        .px_2()
        .child(
            div()
                .flex_none()
                .max_w(relative(0.4))
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(p.fg))
                .child(
                    picked
                        .map(|c| c.label.split(':').next().unwrap_or(&c.label).to_string())
                        .unwrap_or_default(),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(rgb(p.muted))
                .child(picked.map(|c| c.detail.clone()).unwrap_or_default()),
        )
}

/// A filled field, no outline. The send hint answers the key question the
/// placeholder no longer carries, and it sends on click too. Per the badge
/// rule, the hint shows only while cmd is held.
fn send_button(root: &Root, p: Palette, cx: &mut Context<Root>) -> Stateful<Div> {
    clickable("chat-send".into(), Action::SendChat, p, cx)
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .flex_none()
        .when(root.picking, |s| {
            s.child(ui::kbd("⏎", &p))
                .child(ui::text("send", Role::Hint, &p))
                .child(ui::kbd("⇧⏎", &p))
                .child(ui::text("new line", Role::Hint, &p))
        })
}

fn composer(root: &Root, window: &Window, cx: &mut Context<Root>) -> Option<Div> {
    let p = root.palette;
    root.chat.key.as_ref()?;
    if root.chat.closed.is_some() {
        return closed_bar(root, p, cx);
    }
    if let Some((_, ask)) = root.chat.conv.open_question() {
        return Some(column(question_strip(root, ask, p, cx)).pb_3());
    }
    let at = root.chat.composer.clone();
    let state = root.chat.field.clone();
    let text = Field {
        draft: root.chat.draft.clone(),
        placeholder: "Message the agent".into(),
        fg: rgb(p.fg).into(),
        muted: rgb(p.muted).into(),
        selection: (p.accent << 8) | 0x55,
        focused: root.has_focus(window),
        state: state.clone(),
    };
    let field = div()
        .id("composer")
        .relative()
        .flex_1()
        .min_w_0()
        .max_h(px(160.0))
        .overflow_y_scroll()
        .track_scroll(&state.scroll)
        .py_1()
        .cursor(CursorStyle::IBeam)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|root, ev, window, cx| root.composer_down(ev, window, cx)),
        )
        .on_mouse_move(cx.listener(|root, ev, window, cx| root.composer_drag(ev, window, cx)))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|root, ev, window, cx| root.composer_up(ev, window, cx)),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|root, ev, window, cx| root.composer_up(ev, window, cx)),
        )
        .child(
            canvas(move |bounds, _, _| at.set(bounds), |_, _, _, _| {})
                .absolute()
                .size_full(),
        )
        .child(text);
    let send = send_button(root, p, cx);
    let mut bar = div()
        .relative()
        .flex()
        .flex_col()
        .px_3()
        .py_2()
        .rounded(px(8.0))
        .bg(rgb(p.raised));
    if let Some(att) = root.chat.draft.attachment() {
        bar = bar.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .text_color(rgb(p.muted))
                .child(format!("[img {} KB]", att.bytes.len() / 1024)),
        );
    }
    if let Some(pop) = root.chat.skill_view().and_then(|v| {
        crate::launch::skill_popover(
            v,
            root.adv,
            root.chat.composer.get().size.width,
            px(700.0),
            true,
            &p,
        )
    }) {
        bar = bar.child(pop);
    }
    bar = bar.child(
        div()
            .flex()
            .flex_row()
            .items_end()
            .gap_2()
            .child(field)
            .child(send),
    );
    Some(column(bar).pb_3())
}

pub(crate) fn render(root: &Root, window: &Window, cx: &mut Context<Root>) -> Div {
    // The start view: an empty pane holds the launch composer, the recent
    // workspaces and the hint row, not a bare "no session" line.
    if crate::start::visible(root) {
        let mut out = crate::start::render(root, window, cx);
        if let Some(s) = sheet(root, cx) {
            out = out.child(s);
        }
        return out;
    }
    let p = root.palette;
    let rows = list(root.chat.list.clone(), cx.processor(row))
        .flex_1()
        .w_full();
    let pending = root.chat.pending.iter().map(|text| {
        let content = Content {
            text: text.clone(),
            pictures: Vec::new(),
        };
        column(bubble(&content, true, p, &[])).py_1()
    });
    let mut out = div()
        .relative()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .children(header(root))
        .child(rows)
        .children(pending.collect::<Vec<_>>())
        .children(status(root))
        // A block gap keeps the last turn a full block off the composer.
        .children(composer(root, window, cx).map(|c| c.mt_3()));
    if let Some(s) = sheet(root, cx) {
        out = out.child(s);
    }
    out
}
