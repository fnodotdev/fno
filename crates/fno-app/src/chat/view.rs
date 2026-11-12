//! Drawing chat mode: a virtual list of rows in a centred column, the tool
//! sheet, the pending turns and the composer.

use std::ops::Range;

use gpui::{
    canvas, div, font, img, list, prelude::*, px, relative, rgb, rgba, AnyElement, Context, Div,
    ElementId, Font, FontStyle, FontWeight, InteractiveText, MouseButton, ObjectFit, SharedString,
    Stateful, StrikethroughStyle, StyledImage, StyledText, TextRun, UnderlineStyle, Window,
};

use super::markdown::{Block, Inline, Runs};
use super::model::{Content, Item, Picture};
use super::{Closed, EMPTY_AFTER};
use crate::palette::Palette;
use crate::window::{Action, Root};

/// The reading column's width.
const COLUMN: f32 = 720.0;
/// The most output a tool sheet shows.
const OUTPUT_MAX: usize = 20_000;
const UI_FONT: &str = ".SystemUIFont";

/// One clickable row: padded, with a hover and a pressed background. It acts
/// on click, so a press dragged off it does nothing.
fn clickable(id: ElementId, action: Action, p: Palette, cx: &mut Context<Root>) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(p.panel)))
        .active(move |s| s.bg(rgb(p.border)))
        .on_click(cx.listener(move |root, _, _, cx| root.act(action.clone(), cx)))
}

fn button(
    id: &'static str,
    label: &'static str,
    action: Action,
    p: Palette,
    cx: &mut Context<Root>,
) -> Stateful<Div> {
    clickable(id.into(), action, p, cx)
        .flex_none()
        .px_3()
        .border_1()
        .border_color(rgb(p.border))
        .text_color(rgb(p.fg))
        .child(label)
}

fn muted(text: impl Into<SharedString>, p: Palette) -> Div {
    div().text_color(rgb(p.muted)).child(text.into())
}

/// Text runs for `runs`: bold, italic, struck, mono code and accent links.
fn rich(
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
                    run.background_color = Some(rgb(p.panel).into());
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
                cx.open_url(url);
            }
        })
        .into_any_element()
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
        let text = |runs: &Runs, base: Font| rich(id(b, 0), runs, base, color, &mono, p);
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
            Block::Code(code) => div()
                .p_2()
                .rounded_md()
                .bg(rgb(p.panel))
                .font_family(mono.clone())
                .text_color(rgb(p.fg))
                .child(code.clone()),
            Block::Quote(runs) => div()
                .pl_3()
                .border_l_2()
                .border_color(rgb(p.border))
                .child(rich(id(b, 0), runs, ui.clone(), p.muted, &mono, p)),
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
                        d = d.bg(rgb(p.panel));
                    } else {
                        d = d.border_t_1().border_color(rgb(p.border));
                    }
                    for c in 0..cols {
                        let cell = cells.get(c).map(|runs| {
                            rich(id(b, r * cols + c + 1), runs, base.clone(), color, &mono, p)
                        });
                        d = d.child(div().flex_1().min_w_0().px_2().py_1().children(cell));
                    }
                    d
                };
                let mut grid = div()
                    .flex()
                    .flex_col()
                    .border_1()
                    .border_color(rgb(p.border))
                    .rounded_md()
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

fn bubble(content: &Content, pending: bool, p: Palette) -> Div {
    let body = div()
        .max_w(px(560.0))
        .px_3()
        .py_2()
        .rounded_lg()
        .bg(rgb(p.bubble))
        .text_color(rgb(p.fg))
        .flex()
        .flex_col()
        .gap_2()
        .children((!content.text.is_empty()).then(|| div().child(content.text.clone())))
        .children(content.pictures.iter().map(|pic| picture(pic, p)))
        .children(pending.then(|| muted("sending…", p).text_xs()));
    div().flex().flex_row().justify_end().child(body)
}

/// One list row, inside the centred column.
fn row(root: &mut Root, ix: usize, _window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let p = root.palette;
    let Some(item) = root.chat.conv.items.get(ix) else {
        return div().into_any_element();
    };
    let id = |name: &str| ElementId::NamedInteger(SharedString::from(name.to_string()), ix as u64);
    let body: AnyElement = match item {
        Item::User {
            content,
            meta: true,
        } => muted(content.text.clone(), p)
            .text_xs()
            .line_clamp(1)
            .text_ellipsis()
            .into_any_element(),
        Item::User { content, .. } => bubble(content, false, p).into_any_element(),
        Item::Text { .. } => {
            markdown(ix, item.blocks().unwrap_or_default(), p.fg, root).into_any_element()
        }
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
            let first = summary.lines().next().unwrap_or_default().to_string();
            clickable(id("tool"), Action::OpenSheet(ix), p, cx)
                .flex()
                .flex_row()
                .gap_2()
                .text_color(rgb(p.muted))
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(rgb(color))
                        .child(mark),
                )
                .child(div().flex_none().text_color(rgb(p.fg)).child(name.clone()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .line_clamp(1)
                        .text_ellipsis()
                        .child(first),
                )
                .into_any_element()
        }
        Item::Line(text) => muted(text.clone(), p)
            .line_clamp(1)
            .text_ellipsis()
            .into_any_element(),
    };
    column(body).py_1().into_any_element()
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
                .child(muted(title, p).flex_1().text_xs())
                .child(button(copy, "Copy", Action::Copy(text.to_string()), p, cx).text_xs()),
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
        .rounded_lg()
        .border_1()
        .border_color(rgb(p.border))
        .bg(rgb(p.panel))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(div().text_color(rgb(p.fg)).child(name.clone()))
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(rgb(color))
                        .child(status),
                )
                .child(button("sheet-close", "Close", Action::CloseSheet, p, cx).text_xs()),
        )
        .child(section(
            "INPUT",
            "sheet-copy-in",
            "sheet-in",
            input,
            root,
            cx,
        ));
    if let Some((_, out)) = result {
        panel = panel.child(section(
            "OUTPUT",
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
        None => "The focused pane hosts no agent session. Pick an agent on the left.",
        Some(_) if empty && root.chat.opened_at.elapsed() < EMPTY_AFTER => {
            "Reading the conversation…"
        }
        Some(_) if empty => "No messages in this session yet.",
        Some(_) => return None,
    };
    Some(column(muted(text, p)).py_2())
}

/// The composer, or in its place the closed stream's reason and Reconnect.
fn composer(root: &Root, window: &Window, cx: &mut Context<Root>) -> Option<Div> {
    let p = root.palette;
    root.chat.key.as_ref()?;
    if let Some(closed) = &root.chat.closed {
        let (text, color) = match closed {
            // The link's own exit reason folds in here; the corner toast
            // for it is suppressed while this bar stands (window render).
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
            .rounded_lg()
            .border_1()
            .border_color(rgb(p.border))
            .child(div().flex_1().min_w_0().text_color(rgb(color)).child(text))
            .child(button(
                "chat-reconnect",
                "Reconnect",
                Action::Reconnect,
                p,
                cx,
            ));
        return Some(column(bar).pb_3());
    }
    let preedit = root.preedit_text();
    let (text, color) = if root.chat.draft.is_empty() && preedit.is_empty() {
        (
            "Message the agent. Enter sends; Shift-Enter adds a line.".to_string(),
            p.muted,
        )
    } else {
        (format!("{}{preedit}▏", root.chat.draft), p.fg)
    };
    let border = if root.has_focus(window) {
        p.focus
    } else {
        p.border
    };
    let at = root.chat.composer.clone();
    let field = div()
        .id("composer")
        .relative()
        .flex_1()
        .min_w_0()
        .max_h(px(160.0))
        .overflow_y_scroll()
        .py_1()
        .text_color(rgb(color))
        .child(
            canvas(move |bounds, _, _| at.set(bounds), |_, _, _, _| {})
                .absolute()
                .size_full(),
        )
        .child(text);
    let bar = div()
        .flex()
        .flex_row()
        .items_end()
        .gap_2()
        .p_2()
        .rounded_lg()
        .border_1()
        .border_color(rgb(border))
        .bg(rgb(p.panel))
        .child(field)
        .child(button("chat-send", "Send", Action::SendChat, p, cx));
    Some(column(bar).pb_3())
}

pub(crate) fn render(root: &Root, window: &Window, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let rows = list(root.chat.list.clone(), cx.processor(row))
        .flex_1()
        .w_full();
    let pending = root.chat.pending.iter().map(|text| {
        let content = Content {
            text: text.clone(),
            pictures: Vec::new(),
        };
        column(bubble(&content, true, p)).py_1()
    });
    let mut out = div()
        .relative()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .child(rows)
        .children(pending.collect::<Vec<_>>())
        .children(status(root))
        .children(composer(root, window, cx));
    if let Some(s) = sheet(root, cx) {
        out = out.child(s);
    }
    out
}
