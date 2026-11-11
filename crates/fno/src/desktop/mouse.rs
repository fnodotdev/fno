//! Window points to pane cells. The server sends each pane's rect in cells
//! of the content area, and the window draws that area half a cell in (the
//! borders sit in the gap), so area cell (0, 0) starts at (cw/2, ch/2) of the
//! terminal area.

use crate::tree::Rect;

/// A pane and a pane-local, 0-based cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub pane: u64,
    pub row: u16,
    pub col: u16,
}

/// The area cell under a point given relative to the terminal area's origin.
fn area_cell(x: f32, y: f32, cell: (f32, f32)) -> (f32, f32) {
    ((y / cell.1 - 0.5).floor(), (x / cell.0 - 0.5).floor())
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let at = |row: f32, col: f32| ((col + 0.5) * cell.0 + 1.0, (row + 0.5) * cell.1 + 1.0);
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
}
