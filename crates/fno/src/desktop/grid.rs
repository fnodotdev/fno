//! One pane's cell grid. Each pane is its own gpui view, so a new `Frame`
//! notifies and repaints only that pane.

use std::sync::Arc;

use gpui::{
    canvas, fill, point, px, rgb, rgba, size, App, Bounds, Context, Font, FontStyle, FontWeight,
    Hsla, IntoElement, ParentElement, Pixels, Render, SharedString, Size, Styled, TextRun,
    UnderlineStyle, Window,
};

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

/// A run of cells on one row that share a style and paint as one line.
struct Run {
    col: usize,
    text: String,
    style: Style,
    /// A wide glyph is its own run and is shaped at its natural width.
    wide: bool,
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
        match runs.last_mut() {
            Some(last) if !wide && !last.wide && last.style == style => last.text.push(cell.c),
            _ => runs.push(Run {
                col,
                text: cell.c.to_string(),
                style,
                wide,
            }),
        }
    }
    runs
}

fn paint_frame(
    frame: &Frame,
    focused: bool,
    m: &Metrics,
    p: &Palette,
    origin: gpui::Point<Pixels>,
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
            if run.text.trim().is_empty() && !s.underline {
                continue;
            }
            let fg = color(s.fg, s.dim);
            let font = Font {
                weight: if s.bold {
                    FontWeight::BOLD
                } else {
                    m.font.weight
                },
                style: if s.italic {
                    FontStyle::Italic
                } else {
                    FontStyle::Normal
                },
                ..m.font.clone()
            };
            let text_run = TextRun {
                len: run.text.len(),
                font,
                color: fg,
                background_color: None,
                underline: s.underline.then_some(UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(fg),
                    wavy: false,
                }),
                strikethrough: None,
            };
            let force = (!run.wide).then_some(cw);
            let line = window.text_system().shape_line(
                SharedString::from(run.text),
                m.font_size,
                &[text_run],
                force,
            );
            let _ = line.paint(point(x, y), ch, window, cx);
        }
    }
    if focused && frame.cursor_visible && frame.scroll_offset == 0 {
        let (r, c) = (frame.cursor_row as usize, frame.cursor_col as usize);
        if let Some(cell) = frame.cells.get(r * cols + c) {
            let s = style_of(cell, p);
            let at = point(origin.x + cw * c as f32, origin.y + ch * r as f32);
            window.paint_quad(fill(Bounds::new(at, m.cell), color(s.fg, false)));
            if cell.c != ' ' {
                let run = TextRun {
                    len: cell.c.len_utf8(),
                    font: m.font.clone(),
                    color: color(s.bg, false),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let line = window.text_system().shape_line(
                    SharedString::from(cell.c.to_string()),
                    m.font_size,
                    &[run],
                    Some(cw),
                );
                let _ = line.paint(at, ch, window, cx);
            }
        }
    }
}

impl Render for PaneView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let frame = self.frame.clone();
        let focused = self.focused;
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
