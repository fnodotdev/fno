//! Text reads over a rio-vt grid that the mux and the agent screen seam
//! share. Both walk a `Crosswords` grid into plain text, and both must fold a
//! cell to the same char, or a pane reads differently in the two places.

use rio_vt::crosswords::square::{ContentTag, Square};
use rio_vt::crosswords::style::{Style, StyleFlags};

/// A cell's visible char for text walks: bg-only fills and hidden text read
/// as spaces.
pub fn square_text_char(square: &Square, styles: &[Style]) -> char {
    let ch = square.c();
    if !matches!(square.content_tag(), ContentTag::Codepoint) || ch == '\0' {
        return ' ';
    }
    let hidden = styles
        .get(square.style_id() as usize)
        .is_some_and(|s| s.flags.contains(StyleFlags::HIDDEN));
    if hidden {
        ' '
    } else {
        ch
    }
}
