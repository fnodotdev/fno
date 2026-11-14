//! The chrome's one set of parts: the text roles, the one-line row, the
//! section label, the two buttons, the tag and the key hint. Every view
//! draws with these, so one look has one writer.
//!
//! The roles map `fno::theme::type_scale` onto two sizes in SF Mono: 12 for
//! everything a user reads, 11 for labels and hints, and weight for the
//! rest of the hierarchy, as in the TUI. Prose a user reads at length (chat
//! turns, a mod's "What it shows") takes the system font instead.

use gpui::{div, prelude::*, px, rgb, Div, ElementId, FontWeight, Pixels, SharedString, Stateful};

use super::palette::Palette;

/// The chrome's family: rows, ids, numbers, labels and the title bar.
pub const MONO: &str = "SF Mono";
/// Paragraph prose only.
pub const PROSE: &str = ".SystemUIFont";
/// The body size.
pub const BODY: f32 = 12.0;
/// The label and hint size.
pub const SMALL: f32 = 11.0;
/// One row: one 12pt line with air.
pub const ROW_H: f32 = 24.0;
/// The title bar's height. Fixed: the terminal font size never moves it.
pub const TITLE_H: f32 = 38.0;

/// A text level, by meaning. Each maps one `type_scale` name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// `HEADING`: a view or detail title.
    Heading,
    /// `NAME`: the name of one row.
    Name,
    /// `BODY`: plain text.
    Body,
    /// `META`: detail beside a name.
    Meta,
    /// `META` at the small size: a row's fact columns.
    Fact,
    /// A section or column label: small, quiet, sentence case.
    Label,
    /// A key hint or a column head: the quietest level.
    Hint,
}

impl Role {
    fn spec(self, p: &Palette) -> (f32, FontWeight, u32) {
        match self {
            Role::Heading => (BODY, FontWeight::SEMIBOLD, p.fg),
            Role::Name => (BODY, FontWeight::MEDIUM, p.fg),
            Role::Body => (BODY, FontWeight::NORMAL, p.fg),
            Role::Meta => (BODY, FontWeight::NORMAL, p.muted),
            Role::Fact => (SMALL, FontWeight::NORMAL, p.muted),
            Role::Label => (SMALL, FontWeight::SEMIBOLD, p.muted),
            Role::Hint => (SMALL, FontWeight::NORMAL, p.faint),
        }
    }
}

/// `e` in a role's size, weight and color.
pub fn role<E: Styled>(e: E, r: Role, p: &Palette) -> E {
    let (size, weight, color) = r.spec(p);
    e.text_size(px(size))
        .font_weight(weight)
        .text_color(rgb(color))
}

/// A line of text in a role.
pub fn text(t: impl Into<SharedString>, r: Role, p: &Palette) -> Div {
    role(div(), r, p).child(t.into())
}

/// One line that ends in an ellipsis when its box runs out, never a clip
/// mid-glyph. The box takes what the row leaves it.
pub fn clamp(t: impl Into<SharedString>, r: Role, p: &Palette) -> Div {
    div().flex_1().min_w_0().overflow_hidden().child(
        role(div(), r, p)
            .w_full()
            .line_clamp(1)
            .text_ellipsis()
            .child(t.into()),
    )
}

/// A fixed-width cell `chars` advances wide, in a role. Numbers align right,
/// so the mono figures line up down a column.
pub fn cell(
    t: impl Into<SharedString>,
    chars: usize,
    right: bool,
    r: Role,
    p: &Palette,
    adv: Pixels,
) -> Div {
    let d = role(div(), r, p)
        .flex_none()
        .w(adv * chars as f32)
        .overflow_hidden()
        .whitespace_nowrap();
    if right {
        d.flex().flex_row().justify_end()
    } else {
        d
    }
    .child(t.into())
}

/// One row: a single line at [`ROW_H`], full width, square, with the
/// selection band when `selected`. Callers add the cells.
pub fn row(id: impl Into<ElementId>, selected: bool, p: &Palette) -> Stateful<Div> {
    let p = *p;
    div()
        .id(id)
        .w_full()
        .h(px(ROW_H))
        .flex_none()
        .px_3()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .min_w_0()
        .when(selected, |d| d.bg(rgb(p.row_fill)))
}

/// A row that acts on click: hover and press fills, the pointer cursor.
pub fn clickable(d: Stateful<Div>, p: &Palette) -> Stateful<Div> {
    let p = *p;
    d.cursor_pointer()
        .hover(move |s| s.bg(rgb(p.hover)))
        .active(move |s| s.bg(rgb(p.pressed)))
}

/// A section label: smaller and quieter than the rows under it, with more
/// air above than below.
pub fn section(title: impl Into<SharedString>, p: &Palette) -> Div {
    role(div(), Role::Label, p)
        .pt(px(16.0))
        .pb(px(4.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .child(title.into())
}

/// A muted line in a list body: a wait, or an empty list.
pub fn quiet(t: impl Into<SharedString>, p: &Palette) -> Div {
    text(t, Role::Meta, p).py_1()
}

/// The two button kinds. One primary per view, in the accent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Primary,
    Quiet,
}

/// A filled, borderless button. The caller adds `on_click`; it acts on
/// click, so a press dragged off it does nothing.
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    kind: Button,
    p: &Palette,
) -> Stateful<Div> {
    let p = *p;
    let (bg, fg, hover, press) = match kind {
        Button::Primary => (p.accent, p.bg, p.accent, p.accent),
        Button::Quiet => (p.raised, p.fg, p.row_fill, p.pressed),
    };
    div()
        .id(id)
        .flex_none()
        .h(px(22.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .text_size(px(BODY))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(hover)).opacity(0.92))
        .active(move |s| s.bg(rgb(press)).opacity(0.8))
        .child(label.into())
}

/// A small filled label: "Built in", "Runs code", "Installed".
pub fn tag(t: impl Into<SharedString>, color: u32, p: &Palette) -> Div {
    div()
        .flex_none()
        .px(px(5.0))
        .rounded(px(3.0))
        .bg(rgb(p.raised))
        .text_size(px(SMALL))
        .text_color(rgb(color))
        .whitespace_nowrap()
        .child(t.into())
}

/// A key hint: the keys in the quietest level on a raised chip.
pub fn kbd(keys: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .flex_none()
        .px(px(4.0))
        .rounded(px(3.0))
        .bg(rgb(p.raised))
        .font_family(MONO)
        .text_size(px(SMALL))
        .text_color(rgb(p.faint))
        .whitespace_nowrap()
        .child(keys.into())
}

/// How many leading columns, `widths` chars each, fit whole in `width`
/// chars with `gap` chars between them. A column that does not fit drops,
/// and so does every column after it, so the order never changes.
pub fn fit(widths: &[usize], width: usize, gap: usize) -> usize {
    let mut used = 0;
    for (i, w) in widths.iter().enumerate() {
        let need = w + if i == 0 { 0 } else { gap };
        if used + need > width {
            return i;
        }
        used += need;
    }
    widths.len()
}

/// One SF Mono advance at `size`: the width of a cell in the chrome's grid.
pub fn advance(window: &gpui::Window, size: f32) -> Pixels {
    let text = window.text_system();
    let id = text.resolve_font(&gpui::font(MONO));
    text.advance(id, px(size), '0')
        .map(|s| s.width)
        .unwrap_or(px(size * 0.6))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_columns_drop_from_the_right_and_never_reorder() {
        // opus, ~$44.97, 41%, 12.3k tok, up 2h.
        let widths = [4, 7, 3, 9, 5];
        // 4 + 2 + 7 + 2 + 3 = 18 fits; the tokens would need 11 more.
        assert_eq!(fit(&widths, 18, 2), 3);
        assert_eq!(fit(&widths, 100, 2), 5);
        assert_eq!(fit(&widths, 3, 2), 0, "a fact never shows cut");
        // A short late column does not jump ahead of a long one that missed.
        assert_eq!(fit(&[12, 2], 11, 1), 0);
    }
}
