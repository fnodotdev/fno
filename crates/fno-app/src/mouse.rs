//! Window points to pane cells. The server sends each pane's rect in cells
//! of the content area, and the window draws that area half a cell in (the
//! borders sit in the gap), so area cell (0, 0) starts at (cw/2, ch/2) of the
//! terminal area.

use fno::tree::{Axis, Rect};

/// The cells each pane keeps empty, in total per axis (rows, cols). The window
/// tells the server so every PTY is this much smaller than its rect, then
/// draws the grid half this far in from each side: text stays a full cell from
/// a split line, which the one-cell gap alone cannot give. The same value
/// goes to the server and to every point-to-cell conversion below.
pub const INSET: (u16, u16) = (1, 1);

/// The pixel shift from a rect's corner to its inset grid.
pub fn pad(cell: (f32, f32)) -> (f32, f32) {
    (
        cell.0 * f32::from(INSET.1) / 2.0,
        cell.1 * f32::from(INSET.0) / 2.0,
    )
}

/// The rect of the grid the server sizes the pane's PTY to.
pub fn inset(r: Rect) -> Rect {
    Rect {
        rows: r.rows.saturating_sub(INSET.0).max(1),
        cols: r.cols.saturating_sub(INSET.1).max(1),
        ..r
    }
}

/// A pane and a pane-local, 0-based cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub pane: u64,
    pub row: u16,
    pub col: u16,
}

/// The area cell under a point given relative to the terminal area's origin.
/// The point is first moved by the inset shift, and the rects handed to
/// [`hit`] and [`clamped`] are the inset ones.
fn area_cell(x: f32, y: f32, cell: (f32, f32)) -> (f32, f32) {
    let (px, py) = pad(cell);
    (
        ((y - py) / cell.1 - 0.5).floor(),
        ((x - px) / cell.0 - 0.5).floor(),
    )
}

/// The pane and cell under the point. `None` on a border, in the gap between
/// panes, or outside every pane: such a click sends nothing.
pub fn hit(x: f32, y: f32, cell: (f32, f32), panes: &[(u64, Rect)]) -> Option<Hit> {
    let (row, col) = area_cell(x, y, cell);
    if row < 0.0 || col < 0.0 {
        return None;
    }
    let (row, col) = (row as u32, col as u32);
    panes.iter().find_map(|&(pane, r)| {
        let inside = (r.y as u32..r.y as u32 + r.rows as u32).contains(&row)
            && (r.x as u32..r.x as u32 + r.cols as u32).contains(&col);
        inside.then(|| Hit {
            pane,
            row: (row - r.y as u32) as u16,
            col: (col - r.x as u32) as u16,
        })
    })
}

/// The share of a pane's first cell, from its left edge, that is the command
/// block gutter. The gutter paints over the first cell and clicks there pick a
/// block, so a drag that starts at column 0 further right still selects text.
pub const GUTTER: f32 = 1.0 / 3.0;

/// The pane cell under the point when the point lies in a pane's gutter: the
/// left [`GUTTER`] share of its first column. Anything else is `None`.
pub fn gutter_hit(x: f32, y: f32, cell: (f32, f32), panes: &[(u64, Rect)]) -> Option<Hit> {
    let hit = hit(x, y, cell, panes)?;
    if hit.col != 0 {
        return None;
    }
    let (_, col) = area_cell(x, y, cell);
    let (px, _) = pad(cell);
    let across = (x - px) / cell.0 - 0.5 - col;
    (across < GUTTER).then_some(hit)
}

/// The cell under the point clamped into one pane, for a drag that leaves
/// the pane it started in: the selection stops at the pane's edge.
pub fn clamped(x: f32, y: f32, cell: (f32, f32), pane: u64, r: Rect) -> Hit {
    let (row, col) = area_cell(x, y, cell);
    let clamp = |v: f32, start: u16, len: u16| {
        (v - start as f32).clamp(0.0, len.saturating_sub(1) as f32) as u16
    };
    Hit {
        pane,
        row: clamp(row, r.y, r.rows),
        col: clamp(col, r.x, r.cols),
    }
}

/// Whole wheel lines from a pixel delta, keeping the remainder in `acc` so a
/// trackpad's small deltas add up. Positive is up, into the history.
pub fn wheel_lines(acc: &mut f32, dy: f32, line: f32) -> i32 {
    *acc += dy / line;
    let lines = acc.trunc();
    *acc -= lines;
    lines as i32
}

/// One draggable split: the gap cell between panes `a` and `b`. The pair
/// names the seam because the layout carries rects, never the tree, and the
/// server resolves the pair to the branch child pair it flanks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seam {
    pub a: u64,
    pub b: u64,
    /// Horizontal: panes side by side, the divider stands vertical, a drag
    /// moves along cols.
    pub axis: Axis,
}

/// A seam drag in progress: the position at grab, so Esc can put the seam
/// back, and the last position sent, so a move sends only on a cell crossing.
#[derive(Debug, Clone, Copy)]
pub struct SeamDrag {
    pub seam: Seam,
    pub start_pos: u16,
    pub last_pos: u16,
}

/// The pane covering an area cell, by the raw rects: the grids lose the
/// padding, and the seam's flank test must count that padding as covered.
fn pane_at(row: i32, col: i32, panes: &[(u64, Rect)]) -> Option<u64> {
    panes.iter().find_map(|&(pane, r)| {
        let inside = row >= i32::from(r.y)
            && row < i32::from(r.y + r.rows)
            && col >= i32::from(r.x)
            && col < i32::from(r.x + r.cols);
        inside.then_some(pane)
    })
}

/// The seam under a point in the terminal area: a cell no pane covers,
/// flanked by panes along one axis. A crossing flanked on both axes (`┼`)
/// names two seams and refuses, as the TUI's hit test does.
pub fn seam_at(x: f32, y: f32, cell: (f32, f32), panes: &[(u64, Rect)]) -> Option<Seam> {
    let (row, col) = area_cell(x, y, cell);
    if row < 0.0 || col < 0.0 {
        return None;
    }
    seam_at_cell(row as i32, col as i32, panes)
}

fn seam_at_cell(row: i32, col: i32, panes: &[(u64, Rect)]) -> Option<Seam> {
    if pane_at(row, col, panes).is_some() {
        return None;
    }
    let across = pane_at(row, col - 1, panes).zip(pane_at(row, col + 1, panes));
    let down = pane_at(row - 1, col, panes).zip(pane_at(row + 1, col, panes));
    match (across, down) {
        (Some((a, b)), None) => Some(Seam {
            a,
            b,
            axis: Axis::Horizontal,
        }),
        (None, Some((a, b))) => Some(Seam {
            a,
            b,
            axis: Axis::Vertical,
        }),
        _ => None,
    }
}

/// Where the seam sits now: the divider cell just past pane `a`, in the
/// content-area coordinates the server rescales against the pair.
pub fn seam_pos(seam: &Seam, panes: &[(u64, Rect)]) -> Option<u16> {
    let r = panes.iter().find(|(id, _)| *id == seam.a).map(|p| p.1)?;
    Some(match seam.axis {
        Axis::Horizontal => r.x.saturating_add(r.cols),
        Axis::Vertical => r.y.saturating_add(r.rows),
    })
}

/// The divider position a drag points at: the cursor's own coordinate along
/// the seam's axis. `None` off the terminal area.
pub fn seam_pos_at(axis: Axis, x: f32, y: f32, cell: (f32, f32)) -> Option<u16> {
    let (row, col) = area_cell(x, y, cell);
    if row < 0.0 || col < 0.0 {
        return None;
    }
    Some(match axis {
        Axis::Horizontal => col as u16,
        Axis::Vertical => row as u16,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inset_grid_sits_half_the_inset_in_from_its_rect() {
        let cell = (10.0, 20.0);
        let rect = Rect {
            x: 21,
            y: 0,
            rows: 10,
            cols: 20,
        };
        let grid = inset(rect);
        assert_eq!((grid.rows, grid.cols), (9, 19));
        let panes = [(2, grid)];
        // The rect's own first cell is now padding; the grid starts a half
        // inset (5px, 10px) further in.
        let (x, y) = ((21.0 + 0.5) * cell.0 + 1.0, 0.5 * cell.1 + 1.0);
        assert_eq!(hit(x, y, cell, &panes), None);
        let (px, py) = pad(cell);
        assert_eq!(
            hit(x + px, y + py, cell, &panes),
            Some(Hit {
                pane: 2,
                row: 0,
                col: 0
            })
        );
    }

    #[test]
    fn only_the_left_third_of_the_first_column_is_the_gutter() {
        let cell = (30.0, 20.0);
        let panes = [(
            2,
            Rect {
                x: 4,
                y: 0,
                rows: 5,
                cols: 8,
            },
        )];
        let (px, py) = pad(cell);
        let at = |col: f32| (px + (4.0 + 0.5 + col) * cell.0, py + 0.5 * cell.1 + 1.0);
        let (x, y) = at(0.1);
        assert_eq!(gutter_hit(x, y, cell, &panes).map(|h| h.col), Some(0));
        let (x, y) = at(0.6);
        assert_eq!(gutter_hit(x, y, cell, &panes), None);
        let (x, y) = at(1.1);
        assert_eq!(gutter_hit(x, y, cell, &panes), None);
    }

    #[test]
    fn points_map_to_pane_cells_and_borders_map_to_nothing() {
        let cell = (10.0, 20.0);
        // Two side-by-side panes with the one-column gap between them.
        let panes = [
            (
                1,
                Rect {
                    x: 0,
                    y: 0,
                    rows: 10,
                    cols: 20,
                },
            ),
            (
                2,
                Rect {
                    x: 21,
                    y: 0,
                    rows: 10,
                    cols: 20,
                },
            ),
        ];
        // Area cell (3, 25) is pane 2's local (3, 4).
        let (px, py) = pad(cell);
        let at = |row: f32, col: f32| {
            (
                (col + 0.5) * cell.0 + 1.0 + px,
                (row + 0.5) * cell.1 + 1.0 + py,
            )
        };
        let (x, y) = at(3.0, 25.0);
        assert_eq!(
            hit(x, y, cell, &panes),
            Some(Hit {
                pane: 2,
                row: 3,
                col: 4
            })
        );
        let (x, y) = at(0.0, 0.0);
        assert_eq!(hit(x, y, cell, &panes).map(|h| h.pane), Some(1));
        // The gap column, the outer border's half cell and below the panes.
        let (x, y) = at(2.0, 20.0);
        assert_eq!(hit(x, y, cell, &panes), None);
        assert_eq!(hit(2.0, 2.0, cell, &panes), None);
        let (x, y) = at(10.0, 2.0);
        assert_eq!(hit(x, y, cell, &panes), None);
        // A drag past the right edge stops at the pane's last column.
        let (x, y) = at(1.0, 30.0);
        assert_eq!(clamped(x, y, cell, 1, panes[0].1).col, 19);
        // Two half-line trackpad deltas make one line.
        let mut acc = 0.0;
        assert_eq!(wheel_lines(&mut acc, 10.0, 20.0), 0);
        assert_eq!(wheel_lines(&mut acc, 10.0, 20.0), 1);
        assert_eq!(wheel_lines(&mut acc, -45.0, 20.0), -2);
    }

    #[test]
    fn seams_live_in_gap_cells_and_crossings_refuse() {
        let cell = (10.0, 20.0);
        // Four panes in a grid: a vertical divider between the columns (gap
        // col 20), a horizontal one between the rows (gap row 5).
        let panes = [
            (
                1,
                Rect {
                    x: 0,
                    y: 0,
                    rows: 5,
                    cols: 20,
                },
            ),
            (
                2,
                Rect {
                    x: 21,
                    y: 0,
                    rows: 5,
                    cols: 20,
                },
            ),
            (
                3,
                Rect {
                    x: 0,
                    y: 6,
                    rows: 5,
                    cols: 20,
                },
            ),
            (
                4,
                Rect {
                    x: 21,
                    y: 6,
                    rows: 5,
                    cols: 20,
                },
            ),
        ];
        // Points land in the named area cell: the pad shift plus a 1px
        // nudge, as the window maps them.
        let (px, py) = pad(cell);
        let at = |row: f32, col: f32| {
            (
                (col + 0.5) * cell.0 + 1.0 + px,
                (row + 0.5) * cell.1 + 1.0 + py,
            )
        };
        // The gap cell between the top pair is one vertical seam.
        let (x, y) = at(2.0, 20.0);
        assert_eq!(
            seam_at(x, y, cell, &panes),
            Some(Seam {
                a: 1,
                b: 2,
                axis: Axis::Horizontal
            })
        );
        let seam = seam_at(x, y, cell, &panes).unwrap();
        // The divider sits just past pane a, and a drag one cell left asks
        // for pos 19.
        assert_eq!(seam_pos(&seam, &panes), Some(20));
        let (lx, ly) = at(2.0, 19.0);
        assert_eq!(seam_pos_at(seam.axis, lx, ly, cell), Some(19));
        // The gap row between the stacked halves is one horizontal seam,
        // named top pane first.
        let (x, y) = at(5.0, 10.0);
        assert_eq!(
            seam_at(x, y, cell, &panes),
            Some(Seam {
                a: 1,
                b: 3,
                axis: Axis::Vertical
            })
        );
        // Where the two dividers cross, the cell flanks no seam cleanly
        // (here neither axis matches both sides): refused, as the TUI
        // refuses a `┼`.
        let (x, y) = at(5.0, 20.0);
        assert_eq!(seam_at(x, y, cell, &panes), None);
        // A covered cell and a point off the area refuse too.
        let (x, y) = at(2.0, 10.0);
        assert_eq!(seam_at(x, y, cell, &panes), None);
        let (x, y) = at(-1.0, 20.0);
        assert_eq!(seam_at(x, y, cell, &panes), None);
    }
}
