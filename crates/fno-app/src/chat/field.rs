//! The composer's text element: wrapped lines, the selection, a caret and the
//! marked-text underline, plus the layout kept for hit testing.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    fill, point, px, relative, rgba, size, App, Bounds, Element, ElementId, Font, GlobalElementId,
    Hsla, IntoElement, LayoutId, Pixels, Point, ScrollHandle, SharedString, Style, TextRun,
    UnderlineStyle, Window, WrappedLine,
};
use unicode_segmentation::UnicodeSegmentation;

use super::composer::Draft;

/// What a paint leaves behind for the key and input-handler paths.
pub struct Layout {
    revision: u64,
    lines: Vec<WrappedLine>,
    starts: Vec<usize>,
    line_height: Pixels,
    pub origin: Point<Pixels>,
    width: Pixels,
    font: Font,
    font_size: Pixels,
}

impl Layout {
    fn shape(
        text: &str,
        runs: &[TextRun],
        revision: u64,
        font: Font,
        font_size: Pixels,
        line_height: Pixels,
        width: Pixels,
        window: &Window,
    ) -> Layout {
        let lines: Vec<WrappedLine> = window
            .text_system()
            .shape_text(
                SharedString::from(text.to_string()),
                font_size,
                runs,
                Some(width),
                None,
            )
            .map(|l| l.into_iter().collect())
            .unwrap_or_default();
        let mut starts = Vec::with_capacity(lines.len());
        let mut at = 0;
        for l in &lines {
            starts.push(at);
            at += l.len() + 1;
        }
        Layout {
            revision,
            lines,
            starts,
            line_height,
            origin: Point::default(),
            width,
            font,
            font_size,
        }
    }

    /// The same layout re-shaped for new text, at the stored width and font.
    pub fn reshaped(&self, draft: &Draft, window: &Window) -> Layout {
        let runs = [TextRun {
            len: draft.text().len(),
            font: self.font.clone(),
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }];
        let mut fresh = Layout::shape(
            draft.text(),
            &runs,
            draft.revision(),
            self.font.clone(),
            self.font_size,
            self.line_height,
            self.width,
            window,
        );
        fresh.origin = self.origin;
        fresh
    }

    pub fn is_current(&self, draft: &Draft) -> bool {
        self.revision == draft.revision()
    }

    pub fn line_height(&self) -> Pixels {
        self.line_height
    }

    fn line_top(&self, ix: usize) -> Pixels {
        self.lines[..ix]
            .iter()
            .map(|l| l.size(self.line_height).height)
            .fold(px(0.), |a, h| a + h)
    }

    pub fn height(&self) -> Pixels {
        self.line_top(self.lines.len()).max(self.line_height)
    }

    fn line_of(&self, offset: usize) -> usize {
        self.starts
            .iter()
            .rposition(|&s| s <= offset)
            .unwrap_or_default()
    }

    /// A byte offset's top-left, relative to the layout origin.
    pub fn position_for_index(&self, offset: usize) -> Point<Pixels> {
        if self.lines.is_empty() {
            return Point::default();
        }
        let ix = self.line_of(offset);
        let local = (offset - self.starts[ix]).min(self.lines[ix].len());
        let at = self.lines[ix]
            .position_for_index(local, self.line_height)
            .unwrap_or_default();
        point(at.x, at.y + self.line_top(ix))
    }

    /// The offset nearest a point relative to the layout origin.
    pub fn index_for_point(&self, at: Point<Pixels>, text_len: usize) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        let mut top = px(0.);
        for (ix, line) in self.lines.iter().enumerate() {
            let h = line.size(self.line_height).height;
            if at.y < top + h || ix + 1 == self.lines.len() {
                let local = point(at.x, (at.y - top).max(px(0.)));
                let found = line
                    .closest_index_for_position(local, self.line_height)
                    .unwrap_or_else(|e| e);
                return (self.starts[ix] + found).min(text_len);
            }
            top += h;
        }
        text_len
    }

    /// One visual row up or down from `from`, holding the goal column.
    pub fn vertical(&self, from: usize, up: bool, goal_x: Pixels, text_len: usize) -> usize {
        let at = self.position_for_index(from);
        let target = if up {
            at.y - self.line_height
        } else {
            at.y + self.line_height
        };
        if target < px(0.) {
            return 0;
        }
        if target >= self.height() {
            return text_len;
        }
        self.index_for_point(point(goal_x, target), text_len)
    }

    fn row_quads(&self, sel: &Range<usize>, text: &str) -> Vec<Bounds<Pixels>> {
        let mut rows: Vec<(Pixels, Pixels, Pixels)> = Vec::new();
        for (i, g) in text[sel.clone()].grapheme_indices(true) {
            let from = sel.start + i;
            let a = self.position_for_index(from);
            let b = self.position_for_index(from + g.len());
            let right = if b.y == a.y {
                b.x
            } else {
                self.width.max(a.x + px(4.))
            };
            let right = if g == "\n" { a.x + px(6.) } else { right };
            match rows.last_mut() {
                Some(r) if r.0 == a.y => r.2 = r.2.max(right),
                _ => rows.push((a.y, a.x, right)),
            }
        }
        rows.into_iter()
            .map(|(y, x0, x1)| Bounds::new(point(x0, y), size(x1 - x0, self.line_height)))
            .collect()
    }
}

/// Shared between the element and the window's key and mouse paths.
pub struct FieldState {
    pub layout: RefCell<Option<Layout>>,
    pub scroll: ScrollHandle,
    /// Set by a caret move; the next paint scrolls the caret into view.
    pub reveal: Cell<bool>,
    /// The column Up and Down hold across rows.
    pub goal_x: Cell<Option<Pixels>>,
    pub dragging: Cell<bool>,
}

impl Default for FieldState {
    fn default() -> Self {
        FieldState {
            layout: RefCell::new(None),
            scroll: ScrollHandle::new(),
            reveal: Cell::new(false),
            goal_x: Cell::new(None),
            dragging: Cell::new(false),
        }
    }
}

pub struct Field {
    pub draft: Draft,
    pub placeholder: &'static str,
    pub fg: Hsla,
    pub muted: Hsla,
    pub selection: u32,
    pub focused: bool,
    pub state: Rc<FieldState>,
}

impl Field {
    fn runs(&self, len: usize, font: &Font) -> Vec<TextRun> {
        let base = TextRun {
            len,
            font: font.clone(),
            color: self.fg,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let Some(m) = self.draft.marked() else {
            return vec![base];
        };
        let mark = UnderlineStyle {
            color: Some(self.fg),
            thickness: px(1.0),
            wavy: false,
        };
        [
            TextRun {
                len: m.start,
                ..base.clone()
            },
            TextRun {
                len: m.end - m.start,
                underline: Some(mark),
                ..base.clone()
            },
            TextRun {
                len: len - m.end,
                ..base
            },
        ]
        .into_iter()
        .filter(|r| r.len > 0)
        .collect()
    }
}

impl IntoElement for Field {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

pub struct Prepaint {
    layout: Layout,
    selection: Vec<Bounds<Pixels>>,
    caret: Bounds<Pixels>,
}

impl Element for Field {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let text = if self.draft.is_empty() {
            self.placeholder.to_string()
        } else {
            self.draft.text().to_string()
        };
        let ts = window.text_style();
        let font = ts.font();
        let font_size = ts.font_size.to_pixels(window.rem_size());
        let lh = ts.line_height_in_pixels(window.rem_size());
        let id = window.request_measured_layout(style, move |known, avail, window, _| {
            let width = known.width.unwrap_or(match avail.width {
                gpui::AvailableSpace::Definite(w) => w,
                _ => px(400.),
            });
            let run = TextRun {
                len: text.len(),
                font: font.clone(),
                color: Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let rows = Layout::shape(&text, &[run], 0, font.clone(), font_size, lh, width, window);
            size(width, rows.height())
        });
        let _ = cx;
        (id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut (),
        window: &mut Window,
        _cx: &mut App,
    ) -> Prepaint {
        let ts = window.text_style();
        let font = ts.font();
        let font_size = ts.font_size.to_pixels(window.rem_size());
        let lh = ts.line_height_in_pixels(window.rem_size());
        let placeholder = self.draft.is_empty();
        let (text, runs) = if placeholder {
            let run = TextRun {
                len: self.placeholder.len(),
                font: font.clone(),
                color: self.muted,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            (self.placeholder, vec![run])
        } else {
            (self.draft.text(), self.runs(self.draft.text().len(), &font))
        };
        let mut layout = Layout::shape(
            text,
            &runs,
            self.draft.revision(),
            font,
            font_size,
            lh,
            bounds.size.width,
            window,
        );
        layout.origin = bounds.origin;
        let caret_at = if placeholder {
            Point::default()
        } else {
            layout.position_for_index(self.draft.caret())
        };
        let caret = Bounds::new(caret_at, size(px(1.5), lh));
        let selection = if placeholder || !self.draft.has_selection() {
            Vec::new()
        } else {
            layout.row_quads(&self.draft.selection(), self.draft.text())
        };
        Prepaint {
            layout,
            selection,
            caret,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut (),
        pre: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let origin = bounds.origin;
        for q in &pre.selection {
            window.paint_quad(fill(
                Bounds::new(origin + q.origin, q.size),
                rgba(self.selection),
            ));
        }
        let mut top = px(0.);
        for line in &pre.layout.lines {
            let _ = line.paint(
                origin + point(px(0.), top),
                pre.layout.line_height,
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            );
            top += line.size(pre.layout.line_height).height;
        }
        if self.focused && !self.draft.has_selection() {
            window.paint_quad(fill(
                Bounds::new(origin + pre.caret.origin, pre.caret.size),
                self.fg,
            ));
        }
        if self.state.reveal.replace(false) {
            self.reveal(origin + pre.caret.origin, pre.layout.line_height, window);
        }
        let mut layout = std::mem::replace(&mut pre.layout, empty());
        layout.origin = origin;
        *self.state.layout.borrow_mut() = Some(layout);
    }
}

impl Field {
    /// Scroll the box so the caret row shows.
    fn reveal(&self, caret: Point<Pixels>, lh: Pixels, window: &mut Window) {
        let view = self.state.scroll.bounds();
        let mut offset = self.state.scroll.offset();
        if caret.y < view.top() {
            offset.y += view.top() - caret.y;
        } else if caret.y + lh > view.bottom() {
            offset.y -= caret.y + lh - view.bottom();
        } else {
            return;
        }
        self.state.scroll.set_offset(offset);
        window.refresh();
    }
}

fn empty() -> Layout {
    Layout {
        revision: 0,
        lines: Vec::new(),
        starts: Vec::new(),
        line_height: px(0.),
        origin: Point::default(),
        width: px(0.),
        font: gpui::font(".SystemUIFont"),
        font_size: px(0.),
    }
}
