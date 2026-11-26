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
use fno::proto::{cell_flags, Cell, Frame};

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
    /// The live query tinting this pane's visible rows while the find bar
    /// targets it. The window sets it and clears it through `sync_panes`.
    pub find: Option<String>,
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

/// The visible-row match spans `(start cell, cell count)` of `needle`:
/// case-insensitive, one rendered row (the server's scan rule). Wide-spacer
/// cells are skipped as scan text and folded into the span they follow, so
/// a wide glyph inside or ending a match paints its full two columns.
fn row_matches(cells: &[Cell], needle: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let needle: Vec<char> = needle.to_lowercase().chars().collect();
    if needle.is_empty() {
        return out;
    }
    let n = needle.len();
    // The scan chars and the cell slots they paint into, spacers excluded.
    let slots: Vec<usize> = cells
        .iter()
        .enumerate()
        .filter(|(_, c)| c.flags & cell_flags::WIDE_SPACER == 0)
        .map(|(i, _)| i)
        .collect();
    let lower: Vec<char> = cells
        .iter()
        .filter(|c| c.flags & cell_flags::WIDE_SPACER == 0)
        .map(|c| c.c.to_lowercase().next().unwrap_or(c.c))
        .collect();
    let mut at = 0;
    while at + n <= lower.len() {
        let (Some(&start), Some(&last)) = (slots.get(at), slots.get(at + n - 1)) else {
            break;
        };
        if lower.get(at..at + n) == Some(needle.as_slice()) {
            let mut end = last + 1;
            // A match ending on a wide glyph paints its spacer too.
            while cells
                .get(end)
                .is_some_and(|c| c.flags & cell_flags::WIDE_SPACER != 0)
            {
                end += 1;
            }
            out.push((start, end - start));
            at += n;
        } else {
            at += 1;
        }
    }
    out
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

/// The IME draft at the cursor cell, over a ground as wide as its display
/// width: a CJK draft takes two cells a char. It paints whether or not the
/// app shows its cursor, since Claude Code hides it.
fn paint_preedit(
    frame: &Frame,
    text: &str,
    m: &Metrics,
    p: &Palette,
    origin: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let (cw, ch) = (m.cell.width, m.cell.height);
    let (r, c) = (frame.cursor_row as f32, frame.cursor_col as f32);
    let at = point(origin.x + cw * c, origin.y + ch * r);
    let width = cw * unicode_width::UnicodeWidthStr::width(text) as f32;
    window.paint_quad(fill(
        Bounds::new(at, size(width, ch)),
        color(p.panel, false),
    ));
    paint_text(
        text.to_string(),
        at,
        m.font.clone(),
        color(p.fg, false),
        true,
        None,
        m,
        window,
        cx,
    );
}

fn paint_cursor(
    frame: &Frame,
    solid: bool,
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
    // A pane narrower than the badge starts it at the pane's own edge.
    let x = (origin.x + cw * frame.cols as f32 - width).max(origin.x);
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
    find: Option<&str>,
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
        // The other visible matches tint first, under the text; the server's
        // SELECTED current match paints over it in the run loop below.
        if let Some(needle) = find {
            for (start, len) in row_matches(cells, needle) {
                let mut c = color(p.accent, false);
                c.a = 0.18;
                window.paint_quad(fill(
                    Bounds::new(
                        point(origin.x + cw * start as f32, y),
                        size(cw * len as f32, ch),
                    ),
                    c,
                ));
            }
        }
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
    let preedit = preedit.filter(|t| focused && !t.is_empty());
    if let (Some(text), 0) = (preedit, frame.scroll_offset) {
        paint_preedit(frame, text, m, p, origin, window, cx);
    } else if frame.cursor_visible && frame.scroll_offset == 0 {
        // Solid only where keys land: the focused pane of the active window.
        let solid = focused && window.is_window_active();
        paint_cursor(frame, solid, m, p, origin, window, cx);
    }
    paint_block_gutter(frame, m, p, origin, window);
    if frame.scroll_offset > 0 {
        paint_scroll_badge(frame, m, p, origin, window, cx);
    }
}

/// One thin bar per visible command block at the pane's left edge, the width
/// of the click target in `mouse::gutter_hit`. The selected block takes the
/// accent, a failed one the error hue, and the rest sit quiet.
fn paint_block_gutter(
    frame: &Frame,
    m: &Metrics,
    p: &Palette,
    origin: Point<Pixels>,
    window: &mut Window,
) {
    let width = m.cell.width * crate::mouse::GUTTER_WIDTH;
    let left = origin.x - m.cell.width * crate::mouse::GUTTER_LEFT;
    for b in &frame.blocks {
        // A quiet block is dimmed, so the failed and the selected one stand
        // out in either theme.
        let (hue, dim) = match (b.selected, b.exit) {
            (true, _) => (p.accent, false),
            (false, Some(code)) if code != 0 => (p.error_mark, false),
            _ => (p.faint, true),
        };
        // A quarter cell of gap above each bar marks where a block starts. A
        // block cut off by the viewport top keeps its full bar.
        let gap = if b.top > 0 {
            m.cell.height * 0.25
        } else {
            px(0.0)
        };
        let rows = (b.bottom.saturating_sub(b.top) + 1) as f32;
        let at = point(left, origin.y + m.cell.height * b.top as f32 + gap);
        window.paint_quad(fill(
            Bounds::new(at, size(width, m.cell.height * rows - gap)),
            color(hue, dim),
        ));
    }
}

impl Render for PaneView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let frame = self.frame.clone();
        let focused = self.focused;
        let preedit = self.preedit.clone();
        let find = self.find.clone();
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
                            find.as_deref(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row_of(text: &str) -> Vec<Cell> {
        text.chars()
            .map(|c| Cell {
                c,
                ..Cell::default()
            })
            .collect()
    }

    #[test]
    fn row_matches_finds_every_case_insensitive_occurrence() {
        let cells = row_of("error: x error");
        assert_eq!(row_matches(&cells, "ERROR"), vec![(0, 5), (9, 5)]);
        assert_eq!(row_matches(&cells, "error"), vec![(0, 5), (9, 5)]);
        assert_eq!(row_matches(&cells, "x"), vec![(7, 1)]);
        assert!(row_matches(&cells, "").is_empty());
        assert!(row_matches(&cells, "zzz").is_empty());
    }

    #[test]
    fn wide_glyphs_stay_aligned_with_the_painted_cells() {
        // '字' is one glyph over two cells: cell 1 paints it, cell 2 is its
        // WIDE_SPACER, and the 'b' lands at cell 3.
        let mut cells = row_of("a字b");
        cells.insert(
            2,
            Cell {
                c: ' ',
                flags: cell_flags::WIDE_SPACER,
                ..Cell::default()
            },
        );
        // A match before it: plain cell indices.
        assert_eq!(row_matches(&cells, "a"), vec![(0, 1)]);
        // After it: the start slot skips the spacer.
        assert_eq!(row_matches(&cells, "b"), vec![(3, 1)]);
        // Spanning it: the span covers both glyph cells.
        assert_eq!(row_matches(&cells, "a字b"), vec![(0, 4)]);
        // Ending on it: the span extends into the spacer.
        let mut tail = row_of("a字");
        tail.insert(
            2,
            Cell {
                c: ' ',
                flags: cell_flags::WIDE_SPACER,
                ..Cell::default()
            },
        );
        assert_eq!(row_matches(&tail, "a字"), vec![(0, 3)]);
    }
}
