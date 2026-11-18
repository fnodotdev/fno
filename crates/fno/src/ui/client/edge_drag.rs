//! One edge-drag helper for every split the client paints itself.
//!
//! A surface hands over a [`Grab`] when the pointer presses its edge. The
//! drag turns each pointer report into a clamped size, and the size is held
//! in [`EdgeSizes`] for the session. The keyboard nudge reads the same table,
//! so mouse and keys cannot disagree. Tiled pane seams, the sideline, the
//! bell panel and the file viewer keep their own state: the server or the
//! disk owns those sizes.

use std::collections::HashMap;

use super::*;
use crate::view_store::SidelineView;

/// A split the client lays out itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Edge {
    /// Backlog board | node detail.
    BacklogSeam,
    /// Messages: tree | participants.
    MessagesTree,
    /// Messages: participants | thread.
    MessagesPart,
    /// Org board: explorer | right pane.
    OrgExplorer,
    /// Sideline list above, reply dock below.
    ReplyDock,
    /// Backlog host | persona drill column.
    PersonaSeam,
    /// Drilled record: board | persona column.
    DrillBoardSeam,
    /// Drilled record: persona column | record.
    DrillPersonaSeam,
}

impl Edge {
    /// The name a snapshot's `--edge` flag gives an edge.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "backlog" => Self::BacklogSeam,
            "messages-tree" => Self::MessagesTree,
            "messages-part" => Self::MessagesPart,
            "org" => Self::OrgExplorer,
            "dock" => Self::ReplyDock,
            "persona" => Self::PersonaSeam,
            "drill-board" => Self::DrillBoardSeam,
            "drill-persona" => Self::DrillPersonaSeam,
            _ => return None,
        })
    }
}

/// Which way the pointer measures the size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Along {
    /// Size is `col - origin`: the pane left of the edge.
    Col,
    /// Size is `origin - row`: the pane below the edge.
    RowFromBottom,
}

/// Where one edge sits and how far it may move, read at the press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Grab {
    pub(crate) edge: Edge,
    pub(crate) axis: Along,
    pub(crate) origin: u16,
    pub(crate) lo: usize,
    pub(crate) hi: usize,
}

impl Grab {
    /// The size the pointer asks for, clamped to this edge's bounds.
    pub(crate) fn size_at(&self, row: u16, col: u16) -> usize {
        let raw = match self.axis {
            Along::Col => col.saturating_sub(self.origin),
            Along::RowFromBottom => self.origin.saturating_sub(row),
        };
        clamp(usize::from(raw), self.lo, self.hi)
    }
}

/// A drag in flight. Esc puts the edge back at `start`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EdgeDrag {
    pub(crate) grab: Grab,
    pub(crate) start: Option<usize>,
}

/// Columns one key press moves an edge.
pub(crate) const STEP: isize = 2;

/// `value` inside `[lo, hi]`. Never panics when `lo > hi`: a narrow terminal
/// can leave no room, and `lo` wins.
pub(crate) fn clamp(value: usize, lo: usize, hi: usize) -> usize {
    value.min(hi).max(lo)
}

/// The sizes the operator has set this session, one per edge.
#[derive(Default, Debug)]
pub(crate) struct EdgeSizes(HashMap<Edge, usize>);

impl EdgeSizes {
    pub(crate) fn get(&self, edge: Edge) -> Option<usize> {
        self.0.get(&edge).copied()
    }

    pub(crate) fn set(&mut self, edge: Edge, size: usize) {
        self.0.insert(edge, size);
    }

    pub(crate) fn clear(&mut self, edge: Edge) {
        self.0.remove(&edge);
    }

    /// The held size, or `default`, clamped to what the terminal has room for
    /// now. A terminal that shrank never leaves a stale size past the bounds.
    pub(crate) fn resolve(&self, edge: Edge, default: usize, lo: usize, hi: usize) -> usize {
        clamp(self.get(edge).unwrap_or(default), lo, hi)
    }

    /// Move an edge by `delta` from its current size. Returns whether the
    /// size changed.
    pub(crate) fn nudge(
        &mut self,
        edge: Edge,
        current: usize,
        delta: isize,
        lo: usize,
        hi: usize,
    ) -> bool {
        let want = clamp(current.saturating_add_signed(delta), lo, hi);
        if want == current {
            return false;
        }
        self.set(edge, want);
        true
    }
}

/// The strip of cells that grabs one edge.
#[derive(Clone, Copy, Debug)]
enum Line {
    Col { col: u16, rows: (u16, u16) },
    Row { row: u16, cols: (u16, u16) },
}

/// One edge as it is painted right now.
#[derive(Clone, Copy, Debug)]
struct Geom {
    grab: Grab,
    /// The size the edge holds now, in the unit `grab` measures.
    cur: usize,
    line: Line,
}

impl Geom {
    fn hits(&self, row: u16, col: u16) -> bool {
        match self.line {
            Line::Col { col: c, rows } => col == c && row >= rows.0 && row < rows.1,
            Line::Row { row: r, cols } => row == r && col >= cols.0 && col < cols.1,
        }
    }
}

impl View {
    fn col_geom(
        edge: Edge,
        origin: usize,
        cur: usize,
        (lo, hi): (usize, usize),
        rows: (usize, usize),
    ) -> Geom {
        let col = (origin + cur) as u16;
        Geom {
            grab: Grab {
                edge,
                axis: Along::Col,
                origin: origin as u16,
                lo,
                hi,
            },
            cur,
            line: Line::Col {
                col,
                rows: (rows.0 as u16, rows.1 as u16),
            },
        }
    }

    /// Every persona seam the surface now on screen lets the operator
    /// move. The dock seam (host | persona) covers the plain dock and
    /// the editor fold, which paints host | persona with the same held
    /// size. A drilled record offers its own two seams instead. The
    /// strip fold (editor sibling, no board) paints no seam at all.
    fn persona_seam_geoms(&self) -> Vec<Geom> {
        let Some(o) = self.persona.as_ref() else {
            return Vec::new();
        };
        let folded = super::persona_overlay::sibling_pane(self).is_some();
        if folded && self.backlog_board.is_none() {
            return Vec::new();
        }
        if o.drill && !folded {
            return self.drill_seam_geoms();
        }
        let docked = self.backlog_board.is_some()
            || matches!(
                o.home,
                super::persona_overlay::Home::Bell
                    | super::persona_overlay::Home::Decision
                    | super::persona_overlay::Home::Question
            );
        if !docked {
            return Vec::new();
        }
        let Some((top, left, h, w)) = super::persona_overlay::persona_pane_rect(self) else {
            return Vec::new();
        };
        let Some(host_w) = super::persona_overlay::dock_columns(self, w).map(|(host, _)| host)
        else {
            return Vec::new();
        };
        let bounds = super::persona_overlay::seam_bounds(w);
        vec![Self::col_geom(
            Edge::PersonaSeam,
            left,
            host_w,
            bounds,
            (top, top + h),
        )]
    }

    /// The drilled layout's two seams, from the same held sizes
    /// [`super::persona_overlay::drill_widths`] feeds the paint: the
    /// board seam measures the board column, the persona seam the
    /// persona column. The record takes the remainder. The strip arm
    /// (record crowded the board out) offers the persona seam alone.
    fn drill_seam_geoms(&self) -> Vec<Geom> {
        let Some((top, left, h, w)) = super::persona_overlay::persona_pane_rect(self) else {
            return Vec::new();
        };
        let (per_w, board_w, _) = super::persona_overlay::drill_widths(self, w);
        let span = (top, top + h);
        let Some(board_w) = board_w else {
            return vec![Self::col_geom(
                Edge::DrillPersonaSeam,
                left,
                per_w,
                (30, w.saturating_sub(31)),
                span,
            )];
        };
        vec![
            Self::col_geom(
                Edge::DrillPersonaSeam,
                left + board_w + 1,
                per_w,
                (30, w.saturating_sub(board_w + 32)),
                span,
            ),
            Self::col_geom(
                Edge::DrillBoardSeam,
                left,
                board_w,
                (24, w.saturating_sub(per_w + 32)),
                span,
            ),
        ]
    }

    /// The messages tree and participant seams while the list claims the
    /// surface: no detail open, no reply dock active.
    fn messages_seam_geoms(&self) -> Option<Vec<Geom>> {
        if !self
            .messages_board
            .as_ref()
            .is_some_and(|b| b.detail.is_none())
            || super::messages_reply::active(self)
        {
            return None;
        }
        let (rows, cols) = (self.term.0 as usize, self.term.1 as usize);
        let (tree_w, part_w) = super::messages_view::split(cols, &self.edge_sizes);
        let body = (1, rows.saturating_sub(1));
        Some(vec![
            Self::col_geom(
                Edge::MessagesTree,
                0,
                tree_w,
                super::messages_view::tree_bounds(cols),
                body,
            ),
            Self::col_geom(
                Edge::MessagesPart,
                tree_w,
                part_w,
                super::messages_view::part_bounds(cols, tree_w),
                body,
            ),
        ])
    }

    fn edge_geoms(&self) -> Vec<Geom> {
        let (rows, cols) = (self.term.0 as usize, self.term.1 as usize);
        let chrome = self.bottom_row_is_chrome() as usize;
        if let Some(seams) = self.messages_seam_geoms() {
            return seams;
        }
        let org_here = self.org_board.is_some()
            && (self.board_full || self.sideline_view == SidelineView::Org);
        if org_here {
            let (width, top, bottom) = if self.board_full {
                (cols, 0, rows.saturating_sub(1))
            } else {
                let w = (self.panel_w() as usize).saturating_sub(1);
                (w, 1, rows.saturating_sub(1 + chrome))
            };
            return super::org_board::explorer_bounds(width)
                .map(|bounds| {
                    let cur = super::org_board::explorer_w(width, &self.edge_sizes);
                    Self::col_geom(Edge::OrgExplorer, 0, cur, bounds, (top, bottom))
                })
                .into_iter()
                .collect();
        }
        let persona_seams = self.persona_seam_geoms();
        if !persona_seams.is_empty() {
            return persona_seams;
        }
        if let Some(b) = &self.backlog_board {
            let popup = b.detail.is_some() || b.popup_open();
            let host = if self.board_full {
                Some((0, cols, rows))
            } else if self.sideline_view == SidelineView::Backlog && self.panel_w() > 0 {
                let w = (self.panel_w() as usize).saturating_sub(1);
                Some((1, w, rows.saturating_sub(chrome + 1)))
            } else {
                None
            };
            if let (false, Some((left, w, h))) = (popup, host) {
                // A board still loaded behind another sideline view paints
                // nothing here, so only a hosted board claims the edges.
                if w >= 100 {
                    let bounds = super::backlog_board::backlog_panes::seam_bounds(w);
                    let cur = self.edge_sizes.resolve(
                        Edge::BacklogSeam,
                        w * 45 / 100,
                        bounds.0,
                        bounds.1,
                    );
                    // The filter bar's two rows sit above the seam.
                    return vec![Self::col_geom(Edge::BacklogSeam, left, cur, bounds, (2, h))];
                }
            }
            if host.is_some() {
                return Vec::new();
            }
        }
        if self.peek_is_docked() {
            let Some(layout) = self.active_overlay_layout() else {
                return Vec::new();
            };
            let avail = rows.saturating_sub(TAB_BAR_ROWS as usize + self.status_rows() as usize);
            let floor = TAB_BAR_ROWS as usize + avail;
            let top = layout.origin.0;
            // A dock that took the column's top (over the answered row) has no
            // floor-anchored edge to grab.
            if top + layout.framed.lines.len() != floor {
                return Vec::new();
            }
            let (lo, hi) = super::reply_dock::dock_bounds(avail);
            return vec![Geom {
                grab: Grab {
                    edge: Edge::ReplyDock,
                    axis: Along::RowFromBottom,
                    origin: floor as u16,
                    lo,
                    hi,
                },
                cur: self.edge_sizes.resolve(Edge::ReplyDock, avail / 2, lo, hi),
                line: Line::Row {
                    row: top as u16,
                    cols: (0, self.panel_w().saturating_sub(1)),
                },
            }];
        }
        Vec::new()
    }

    /// Grab the edge under a press at `(row, col)`. Returns whether one was.
    pub(super) fn begin_edge_drag(&mut self, row: u16, col: u16) -> bool {
        let Some(geom) = self.edge_geoms().into_iter().find(|g| g.hits(row, col)) else {
            return false;
        };
        self.edge_drag = Some(EdgeDrag {
            grab: geom.grab,
            start: self.edge_sizes.get(geom.grab.edge),
        });
        true
    }

    /// Esc: drop the drag and put the edge back where it started.
    pub(super) fn revert_edge_drag(&mut self) -> bool {
        let Some(drag) = self.edge_drag.take() else {
            return false;
        };
        match drag.start {
            Some(size) => self.edge_sizes.set(drag.grab.edge, size),
            None => self.edge_sizes.clear(drag.grab.edge),
        }
        true
    }

    /// The resize chord (prefix + Ctrl+arrow) moves the edge of the surface
    /// on screen: the seam follows the arrow. Returns whether a client
    /// surface took it; otherwise the chord goes on to the server's panes.
    pub(super) fn nudge_edge(&mut self, dir: Dir) -> bool {
        let geoms = self.edge_geoms();
        // Messages lists the tree seam first. The tree column moves it; the
        // other two columns move the seam on their right, or on their left
        // for the thread column, which has none on its right. A drilled
        // record lists the persona seam first: the keyboard moves the seam
        // on the focused column's left, so the board column reaches its
        // own seam.
        let drill_board = self.persona.as_ref().is_some_and(|o| o.drill)
            && self.input_owner() == super::region_focus::RegionOwner::Board;
        let pick = match self.messages_board.as_ref().map(|b| b.col) {
            None if drill_board => 1,
            Some(super::messages_view::Col::Tree) | None => 0,
            Some(_) => 1,
        };
        let Some(geom) = geoms.get(pick).copied() else {
            return false;
        };
        let delta = match (geom.grab.axis, dir) {
            (Along::Col, Dir::Left) => -STEP,
            (Along::Col, Dir::Right) => STEP,
            (Along::RowFromBottom, Dir::Up) => STEP,
            (Along::RowFromBottom, Dir::Down) => -STEP,
            _ => return true,
        };
        self.edge_sizes
            .nudge(geom.grab.edge, geom.cur, delta, geom.grab.lo, geom.grab.hi);
        true
    }
}

/// Pointer reports while an edge drag may be in flight, and the press that
/// starts one. Returns whether the report is spent.
pub(super) fn mouse(view: &mut View, rep: crate::mouse::MouseReport) -> bool {
    use crate::proto::{MouseButton, MouseKind};
    if let Some(drag) = view.edge_drag {
        match rep.kind {
            MouseKind::Drag(MouseButton::Left) => view
                .edge_sizes
                .set(drag.grab.edge, drag.grab.size_at(rep.row, rep.col)),
            _ => view.edge_drag = None,
        }
        return true;
    }
    matches!(rep.kind, MouseKind::Press(MouseButton::Left))
        && view.begin_edge_drag(rep.row, rep.col)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grab(axis: Along, origin: u16) -> Grab {
        Grab {
            edge: Edge::BacklogSeam,
            axis,
            origin,
            lo: 20,
            hi: 60,
        }
    }

    #[test]
    fn pointer_maps_to_a_clamped_size_on_both_axes() {
        let col = grab(Along::Col, 1);
        assert_eq!(col.size_at(5, 41), 40);
        assert_eq!(col.size_at(5, 2), 20, "dragged past the floor");
        assert_eq!(col.size_at(5, 200), 60, "dragged past the ceiling");
        let row = grab(Along::RowFromBottom, 40);
        assert_eq!(
            row.size_at(15, 0),
            25,
            "a pointer above the origin grows it"
        );
        assert_eq!(row.size_at(50, 0), 20, "a pointer below the origin floors");
    }
}
