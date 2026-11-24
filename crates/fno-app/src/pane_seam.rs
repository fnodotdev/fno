//! The app shell's pane seams: the drag math, the clamp bounds and the
//! seam element shared by the left pane's edge and the Orchestration
//! board's two column seams. A grab starts a drag, a move points a width,
//! a release commits it to Root, and a double-click resets to the default.

use gpui::{div, prelude::*, px, rgb, CursorStyle, Div, Stateful};

use super::palette::Palette;

/// Which seam a drag started on: the kind decides the drag direction and
/// the default the double-click resets to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeamKind {
    /// The left pane's right edge.
    LeftPane,
    /// The board's Next column's left edge.
    BoardNext,
    /// The board's Questions column's left edge.
    BoardFeed,
}

/// A pane's floor: narrower than this a pane stops being readable.
pub const MIN_PANE_W: f32 = 240.0;

/// A drag's live record: which seam, where the grab happened, and the
/// width the pane carried at grab.
#[derive(Debug, Clone, Copy)]
pub struct EdgeDrag {
    pub kind: SeamKind,
    pub grab_x: f32,
    pub start_w: f32,
}

impl EdgeDrag {
    /// The pane width the drag points at now: the left pane follows the
    /// cursor, and a board column widens against it (dragging its seam
    /// left widens the column to the seam's right).
    pub fn width_now(&self, now_x: f32, root_w: f32) -> f32 {
        pane_width_after_drag(self.kind, self.start_w, self.grab_x, now_x, root_w)
    }
}

/// The pane width a drag points at, clamped to the pane's bounds: at least
/// `MIN_PANE_W` and at most half the window. The max floors at the min, so
/// a window narrower than twice the floor never inverts the bounds.
pub fn pane_width_after_drag(
    kind: SeamKind,
    start_w: f32,
    grab_x: f32,
    now_x: f32,
    root_w: f32,
) -> f32 {
    let delta = match kind {
        SeamKind::LeftPane => now_x - grab_x,
        SeamKind::BoardNext | SeamKind::BoardFeed => grab_x - now_x,
    };
    let min = MIN_PANE_W;
    (start_w + delta).clamp(min, (root_w * 0.5).max(min))
}

/// The width a double-click resets the pane to: the built-in default.
pub fn default_width(kind: SeamKind) -> f32 {
    match kind {
        SeamKind::LeftPane => crate::left_pane::WIDTH,
        SeamKind::BoardNext => crate::orchestration::NEXT_W,
        SeamKind::BoardFeed => crate::orchestration::QUESTIONS_W,
    }
}

/// The 6px hit zone on a pane's dragging edge, painted as a 1px hairline
/// in the border color (a plain hairline, never an accent card), with the
/// ew-resize cursor. The caller attaches the mouse-down listener, which
/// starts the drag or, on the second click, resets the width.
pub fn seam(kind: SeamKind, p: &Palette) -> Stateful<Div> {
    let id = match kind {
        SeamKind::LeftPane => "left-pane-seam",
        SeamKind::BoardNext => "board-next-seam",
        SeamKind::BoardFeed => "board-feed-seam",
    };
    div()
        .id(id)
        .w(px(6.0))
        .h_full()
        .flex_none()
        .cursor(CursorStyle::ResizeLeftRight)
        .border_l_1()
        .border_color(rgb(p.border))
        .hover(|s| s.bg(rgb(p.hover)))
        .active(|s| s.bg(rgb(p.row_fill)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The drag math a paint cannot lie about: direction, the clamp bounds,
    // and the defaults a double-click resets to.

    #[test]
    fn a_left_pane_drag_follows_the_cursor() {
        let w = pane_width_after_drag(SeamKind::LeftPane, 320.0, 320.0, 420.0, 1440.0);
        assert_eq!(w, 420.0);
    }

    #[test]
    fn a_board_seam_widens_against_the_cursor() {
        let w = pane_width_after_drag(SeamKind::BoardNext, 300.0, 900.0, 850.0, 1440.0);
        assert_eq!(w, 350.0);
    }

    #[test]
    fn both_bounds_clamp() {
        // Past half the window: the pane holds at 720 of 1440.
        assert_eq!(
            pane_width_after_drag(SeamKind::LeftPane, 320.0, 320.0, 800.0, 1440.0),
            720.0
        );
        // Under the floor: 240.
        assert_eq!(
            pane_width_after_drag(SeamKind::LeftPane, 320.0, 320.0, 100.0, 1440.0),
            240.0
        );
        // A board column clamps at half the window too.
        assert_eq!(
            pane_width_after_drag(SeamKind::BoardFeed, 440.0, 900.0, 0.0, 1440.0),
            720.0
        );
    }
}
