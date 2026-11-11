//! One pane's cell grid. Each pane is its own gpui view, so a new `Frame`
//! notifies and repaints only that pane.

use std::sync::Arc;

use gpui::{
    canvas, fill, point, px, rgb, rgba, size, App, Bounds, Context, Font, FontStyle, FontWeight,
    Hsla, IntoElement, ParentElement, Pixels, Point, Render, SharedString, Size, Styled, TextRun,
    UnderlineStyle, Window,
};

use super::boxdraw;
use super::palette::Palette;
use crate::proto::{cell_flags, Cell, Frame};

/// The font and cell metrics every pane paints with.
#[derive(Clone)]
pub struct Metrics {
    pub font: Font,
    pub font_size: Pixels,
    pub cell: Size<Pixels>,
}

pub struct PaneView {
    pub frame: Option<Arc<Frame>>,
    pub focused: bool,
    /// Text the input handler is composing (a dead key, an IME draft),
    /// drawn at the cursor of the focused pane until it commits.
    pub preedit: Option<String>,
    pub metrics: Metrics,
    pub palette: Palette,
}

/// A cell's resolved colors and text attributes.
#[derive(Clone, Copy, PartialEq)]
struct Style {
    fg: u32,
    bg: u32,
    bold: bool,
    italic: bool,
    underline: bool,
    dim: bool,
    selected: bool,
}

fn style_of(cell: &Cell, p: &Palette) -> Style {
    let (mut fg, mut bg) = (p.fg(cell.fg), p.bg(cell.bg));
    if cell.flags & cell_flags::INVERSE != 0 {
        std::mem::swap(&mut fg, &mut bg);
    }
    Style {
        fg,
        bg,
        bold: cell.flags & cell_flags::BOLD != 0,
        italic: cell.flags & cell_flags::ITALIC != 0,
        underline: cell.flags & cell_flags::UNDERLINE != 0,
        dim: cell.flags & cell_flags::DIM != 0,
        selected: cell.flags & cell_flags::SELECTED != 0,
    }
}

fn color(hex: u32, dim: bool) -> Hsla {
    if dim {
        rgba(hex << 8 | 0x99).into()
    } else {
        rgb(hex).into()
    }
}

fn font_for(m: &Metrics, bold: bool, italic: bool) -> Font {
    Font {
        weight: if bold {
            FontWeight::BOLD
        } else {
            m.font.weight
        },
        style: if italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
        ..m.font.clone()
    }
}

/// True for the chars painted as quads, never through the font.
fn is_box(c: char) -> bool {
    matches!(c as u32, 0x2500..=0x259f)
}

/// A run of cells on one row that share a style and paint as one line.
struct Run {
    col: usize,
    text: String,
    style: Style,
    /// A wide glyph is its own run and is shaped at its natural width.
    wide: bool,
    /// Box drawing and block chars, painted as quads.
    boxed: bool,
}

fn row_runs(cells: &[Cell], p: &Palette) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for (col, cell) in cells.iter().enumerate() {
        if cell.flags & cell_flags::WIDE_SPACER != 0 {
            continue;
        }
        let style = style_of(cell, p);
        let wide = cells
            .get(col + 1)
            .is_some_and(|n| n.flags & cell_flags::WIDE_SPACER != 0);
        let boxed = is_box(cell.c);
        match runs.last_mut() {
            Some(last) if !wide && !last.wide && last.boxed == boxed && last.style == style => {
                last.text.push(cell.c)
            }
            _ => runs.push(Run {
                col,
                text: cell.c.to_string(),
                style,
                wide,
                boxed,
            }),
        }
    }
    runs
}

#[allow(clippy::too_many_arguments)]
fn paint_text(
    text: String,
    at: Point<Pixels>,
    font: Font,
    fg: Hsla,
    underline: bool,
    force: Option<Pixels>,
    m: &Metrics,
    window: &mut Window,
    cx: &mut App,
) {
    let run = TextRun {
        len: text.len(),
        font,
        color: fg,
        background_color: None,
        underline: underline.then_some(UnderlineStyle {
            thickness: px(1.0),
            color: Some(fg),
            wavy: false,
        }),
        strikethrough: None,
    };
    let line =
        window
            .text_system()
            .shape_line(SharedString::from(text), m.font_size, &[run], force);
    let _ = line.paint(at, m.cell.height, window, cx);
}

/// Paint box chars as quads; a char with no quads (a diagonal) falls back
/// to the font.
fn paint_box_run(run: &Run, at: Point<Pixels>, m: &Metrics, window: &mut Window, cx: &mut App) {
    let (cw, ch) = (m.cell.width, m.cell.height);
    let light = (f32::from(ch) / 16.0).round().max(1.0);
    let fg = color(run.style.fg, run.style.dim);
    for (i, c) in run.text.chars().enumerate() {
        let x = at.x + cw * i as f32;
        match boxdraw::quads(c, f32::from(cw), f32::from(ch), light) {
            Some(quads) => {
                for q in quads {
                    let mut c = fg;
                    c.a *= q.alpha;
                    window.paint_quad(fill(
                        Bounds::new(point(x + px(q.x), at.y + px(q.y)), size(px(q.w), px(q.h))),
                        c,
                    ));
                }
            }
            None => paint_text(
                c.to_string(),
                point(x, at.y),
                m.font.clone(),
                fg,
                false,
                Some(cw),
                m,
                window,
                cx,
            ),
        }
    }
}

/// A 1px outline: the cursor of a pane that does not take keys.
fn paint_outline(b: Bounds<Pixels>, c: Hsla, window: &mut Window) {
    let t = px(1.0);
    let (o, s) = (b.origin, b.size);
    for r in [
        Bounds::new(o, size(s.width, t)),
        Bounds::new(point(o.x, o.y + s.height - t), size(s.width, t)),
        Bounds::new(o, size(t, s.height)),
        Bounds::new(point(o.x + s.width - t, o.y), size(t, s.height)),
    ] {
        window.paint_quad(fill(r, c));
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_cursor(
    frame: &Frame,
    solid: bool,
    preedit: Option<&str>,
    m: &Metrics,
    p: &Palette,
    origin: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let (cw, ch) = (m.cell.width, m.cell.height);
    let cols = frame.cols as usize;
    let (r, c) = (frame.cursor_row as usize, frame.cursor_col as usize);
    let Some(cell) = frame.cells.get(r * cols + c) else {
        return;
    };
    let at = point(origin.x + cw * c as f32, origin.y + ch * r as f32);
    if let Some(text) = preedit.filter(|t| !t.is_empty()) {
        let width = cw * text.chars().count() as f32;
        window.paint_quad(fill(
            Bounds::new(at, size(width, ch)),
            color(p.panel, false),
        ));
        let font = m.font.clone();
        paint_text(
            text.to_string(),
            at,
            font,
            color(p.fg, false),
            true,
            None,
            m,
            window,
            cx,
        );
        return;
    }
    let s = style_of(cell, p);
    let wide = frame
        .cells
        .get(r * cols + c + 1)
        .is_some_and(|n| c + 1 < cols && n.flags & cell_flags::WIDE_SPACER != 0);
    let bounds = Bounds::new(at, size(cw * if wide { 2.0 } else { 1.0 }, ch));
    if !solid {
        paint_outline(bounds, color(s.fg, false), window);
        return;
    }
    window.paint_quad(fill(bounds, color(s.fg, false)));
    if cell.c != ' ' {
        let font = font_for(m, s.bold, s.italic);
        let force = (!wide).then_some(cw);
        paint_text(
            cell.c.to_string(),
            at,
            font,
            color(s.bg, false),
            false,
            force,
            m,
            window,
            cx,
        );
    }
}

/// `[+N]` at the pane's top right while it shows history N lines up.
fn paint_scroll_badge(
    frame: &Frame,
    m: &Metrics,
    p: &Palette,
    origin: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let cw = m.cell.width;
    let text = format!("[+{}]", frame.scroll_offset);
    // Half a cell of padding on each side, so the text never touches the
    // pane's border.
    let width = cw * (text.chars().count() as f32 + 1.0);
    let x = origin.x + cw * frame.cols as f32 - width;
    let at = point(x, origin.y);
    window.paint_quad(fill(
        Bounds::new(at, size(width, m.cell.height)),
        color(p.accent, false),
    ));
    paint_text(
        text,
        point(x + cw / 2.0, origin.y),
        m.font.clone(),
        color(p.bg, false),
        false,
        Some(cw),
        m,
        window,
        cx,
    );
}

#[allow(clippy::too_many_arguments)]
fn paint_frame(
    frame: &Frame,
    focused: bool,
    preedit: Option<&str>,
    m: &Metrics,
    p: &Palette,
    origin: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let (cw, ch) = (m.cell.width, m.cell.height);
    let cols = frame.cols as usize;
    for (r, cells) in frame.cells.chunks(cols.max(1)).enumerate() {
        let y = origin.y + ch * r as f32;
        for run in row_runs(cells, p) {
            let x = origin.x + cw * run.col as f32;
            let width = run.text.chars().count() * if run.wide { 2 } else { 1 };
            let s = run.style;
            if s.bg != p.bg || s.selected {
                let bg = if s.selected { p.accent } else { s.bg };
                let mut c = color(bg, false);
                if s.selected {
                    c.a = 0.35;
                }
                window.paint_quad(fill(
                    Bounds::new(point(x, y), size(cw * width as f32, ch)),
                    c,
                ));
            }
            if run.boxed {
                paint_box_run(&run, point(x, y), m, window, cx);
                continue;
            }
            if run.text.trim().is_empty() && !s.underline {
                continue;
            }
            let force = (!run.wide).then_some(cw);
            let font = font_for(m, s.bold, s.italic);
            let fg = color(s.fg, s.dim);
            paint_text(
                run.text,
                point(x, y),
                font,
                fg,
                s.underline,
                force,
                m,
                window,
                cx,
            );
        }
    }
    if frame.cursor_visible && frame.scroll_offset == 0 {
        // Solid only where keys land: the focused pane of the active window.
        let solid = focused && window.is_window_active();
        let preedit = preedit.filter(|_| focused);
        paint_cursor(frame, solid, preedit, m, p, origin, window, cx);
    }
    if frame.scroll_offset > 0 {
        paint_scroll_badge(frame, m, p, origin, window, cx);
    }
}

impl Render for PaneView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let frame = self.frame.clone();
        let focused = self.focused;
        let preedit = self.preedit.clone();
        let metrics = self.metrics.clone();
        let palette = self.palette;
        gpui::div().size_full().child(
            canvas(
                |_, _, _| (),
                move |bounds, (), window, cx| {
                    if let Some(frame) = frame {
                        paint_frame(
                            &frame,
                            focused,
                            preedit.as_deref(),
                            &metrics,
                            &palette,
                            bounds.origin,
                            window,
                            cx,
                        );
                    }
                },
            )
            .size_full(),
        )
    }
}
