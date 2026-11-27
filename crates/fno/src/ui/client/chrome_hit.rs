use super::*;

impl View {
    /// Map a left-click on chrome (the tab bar or the sideline) to what it does:
    /// switch tab/squad, focus an agent's pane, open a new tab, or a local hint
    /// for a row that isn't directly actionable (a work-only agent, a card).
    /// `None` = not a chrome cell (the caller falls through to [`hit_test`]), so
    /// clicking anywhere off the panel still reaches the pane underneath.
    pub(super) fn chrome_hit(&self, row: u16, col: u16) -> Option<ChromeHit> {
        let panel_w = self.panel_w();
        if let Some(hit) = self.chrome_hit_feed(row, col) {
            return Some(hit);
        }
        if let Some(hit) = bell::hit(self, row, col) {
            return Some(hit);
        }
        // The sideline's strip row (`Agents  Messages`, R15): the words
        // switch the view, the density button rides the same row, and the
        // rest of the row inside the sideline's columns is dead strip.
        // Checked before the Agents-only guard so every view's strip
        // answers. A click right of the divider falls through to the tab
        // strip, which shares this row.
        let pw = self.sideline_paint_w();
        if bell::button_at(self, row, col) {
            return Some(ChromeHit::Bell(bell::Hit::Toggle));
        }
        let top = self.sideline_top();
        if row as usize + 1 == top && pw > 0 && (col as usize) < pw.saturating_sub(1) {
            let need_w = self.need_input_label_cols();
            if need_w > 0 && (col as usize) < need_w {
                return Some(ChromeHit::NeedInput);
            }
            for (start, w, view) in self.top_row_spans() {
                if (col as usize) >= start && (col as usize) < start + w {
                    return Some(ChromeHit::TopRow(view));
                }
            }
            // The density button stays pinned to the strip row, where it
            // costs no display row; full-screen does not paint it.
            if !self.sideline_full {
                if let Some(range) = self.density_button_range(panel_w as usize) {
                    if range.contains(&(col as usize)) {
                        return Some(ChromeHit::CycleDensity);
                    }
                }
            }
            return None;
        }
        // Tab strip (row 0, scoped to the content columns since US1): it
        // begins at `panel_w`, walking the same spans the renderer paints (with
        // the same origin). A row-0 click LEFT of the divider (`col < panel_w`)
        // belongs to the sideline's reclaimed row 0 and falls through below.
        // `panel_w == 0` (no sideline) -> strip from col 0, unchanged.
        if row < TAB_BAR_ROWS && col >= panel_w {
            let col = col as usize;
            if let Some((start, text)) = self.notice_overlay(self.term.1 as usize) {
                if col >= start && col < start + text.chars().count() {
                    return None;
                }
            }
            let mut c = panel_w as usize;
            for span in self.tab_bar_window() {
                let w = tab_text_cols(&span.text);
                if col >= c && col < c + w {
                    return match span.hit? {
                        TabHit::Tab(tid) => Some(ChromeHit::Cmds(vec![Command::SelectTab(tid)])),
                        TabHit::NewTab => Some(ChromeHit::Cmds(vec![Command::NewTab])),
                    };
                }
                c += w;
            }
            return None;
        }
        // Sideline: the painted width minus its divider (the full terminal
        // in full-screen mode). Off/narrow => no panel. Under the docked
        // board the column is the board's own surface: no agents rows, no
        // footer, no density button - a click must resolve nothing here or
        // it acts on a phantom row.
        if self.sideline_view != crate::view_store::SidelineView::Agents {
            return None;
        }
        let paint_w = self.sideline_paint_w();
        if paint_w == 0 || col as usize >= paint_w - 1 {
            return None;
        }
        // Full-screen sideline paints below the strip; invert the same
        // offset the painter used.
        if (row as usize) < top {
            return None;
        }
        // The bottom row is overlaid by the status / which-key / search chrome
        // (draw_bottom_row paints last), so a click there belongs to that chrome,
        // not the sideline row drawn underneath it (codex P2).
        if row as usize == (self.term.0 as usize).saturating_sub(1) && self.bottom_row_is_chrome() {
            return None;
        }
        // Display row i is painted at `i - offset` (draw_sideline, since
        // the sideline owns the top painted row), so invert with the paint
        // offset - else a click on a scrolled row activates the wrong row.
        // The pinned footer answers ahead of the offset path, through the
        // same resolver hover and the row menus use: a click on the pinned
        // row is the footer's row even though its display row has scrolled
        // away.
        let i = self
            .pinned_footer_row_index(row, top)
            .unwrap_or_else(|| row as usize - top + self.sideline_offset());
        if let Some(hit) = self.table_header_hit(i, col) {
            return Some(hit);
        }
        // The footer's buttons: `+ session` opens the composer, `menu` the
        // sideline MENU popup; the rest of the row keeps its create action.
        if matches!(self.painted_rows().get(i), Some(DisplayRow::NewSquad)) {
            let buttons = self.footer_buttons(panel_w as usize);
            let at = |r: &Option<std::ops::Range<usize>>| {
                r.as_ref().is_some_and(|r| r.contains(&(col as usize)))
            };
            if at(&buttons.session) {
                return Some(ChromeHit::ToggleComposer);
            }
            if at(&buttons.host) {
                return Some(ChromeHit::AddRemoteHost);
            }
            if at(&buttons.menu) {
                return Some(ChromeHit::OpenSidelineMenu { row, col });
            }
        }
        if let Some(id) = self.card_node_hit(i, col) {
            return Some(ChromeHit::OpenNode(id));
        }
        if let Some(url) = self.card_pr_hit(i, col) {
            return Some(ChromeHit::OpenPr(url));
        }
        match self.painted_rows().get(i) {
            Some(DisplayRow::Card(k)) => {
                return self
                    .launcher
                    .as_ref()?
                    .card_lines(self, self.card_width())
                    .get(*k)?
                    .hit_at(col as usize)
                    .map(ChromeHit::Card);
            }
            Some(DisplayRow::Ghost(squad)) => return Some(ChromeHit::OpenDraft(*squad)),
            Some(DisplayRow::Sel(row))
                if row.tab.is_none()
                    && self
                        .header_plus_cols()
                        .is_some_and(|cols| cols.contains(&(col as usize))) =>
            {
                return Some(ChromeHit::OpenDraft(row.squad));
            }
            _ => {}
        }
        self.row_action(i)
    }

    /// The node on card line 1, when `col` falls on it: the tap that opens
    /// the node's plan. Reads the same layout the paint drew.
    fn card_node_hit(&self, i: usize, col: u16) -> Option<String> {
        if self.sideline_layout != sideline_color::SidelineLayout::Card {
            return None;
        }
        let rows = self.painted_rows();
        let DisplayRow::Agent(a) = rows.get(i)? else {
            return None;
        };
        // The span the last paint drew: a node cut or dropped beside a long
        // name must not answer a tap on the name.
        let painted = self.card_node_spans.borrow().get(&a.name).cloned();
        let span = match painted {
            Some(span) => span?,
            None => card_line::node_span(a, self.sideline_paint_w().checked_sub(1)?)?,
        };
        span.contains(&(col as usize))
            .then(|| a.node.clone())
            .flatten()
    }

    fn card_pr_hit(&self, i: usize, col: u16) -> Option<String> {
        if self.sideline_layout != sideline_color::SidelineLayout::Card {
            return None;
        }
        let rows = self.painted_rows();
        let DisplayRow::Agent(a) = rows.get(i)? else {
            return None;
        };
        let span = card_line::pr_span(a, self.sideline_paint_w().checked_sub(1)?)?;
        if !span.contains(&(col as usize)) {
            return None;
        }
        // The node's stored URL names its own repo. A node in another repo
        // must never open this repo's PR of the same number, so a missing
        // URL opens nothing.
        a.bindings
            .iter()
            .find(|b| Some(&b.node) == a.node.as_ref() && b.pr == a.pr)
            .and_then(|b| b.pr_url.clone())
    }

    fn table_header_hit(&self, row: usize, col: u16) -> Option<ChromeHit> {
        if (self.density != Density::Extended && !self.sideline_full)
            || !matches!(self.painted_rows().get(row), Some(DisplayRow::TableHead))
        {
            return None;
        }
        let text_w = self.sideline_paint_w().checked_sub(1)?;
        let card = self.sideline_layout == sideline_color::SidelineLayout::Card;
        // The card head names two columns and sorts nothing.
        if card {
            return None;
        }
        let rects = self.worker_column_rects(text_w as u16);
        let i = rects
            .iter()
            .take(5)
            .position(|r| col >= r.x && col < r.x + r.width)?;
        sideline::head_sorts(card)[i].map(ChromeHit::SortColumn)
    }
}

impl View {
    /// Map an outer-terminal cell (0-based) to `(pane, pane_row, pane_col)` when
    /// it falls inside a pane's content rect. `None` for a chrome cell (tab bar,
    /// sideline) or a content divider, so the caller swallows it - a mouse event
    /// on chrome never forwards to a pane (AC3-UI). Rects are content-area
    /// relative; the content origin is `(TAB_BAR_ROWS, left_chrome_w)`.
    pub(super) fn hit_test(&self, row: u16, col: u16) -> Option<(u64, u16, u16)> {
        let left_w = self.left_chrome_w();
        let (cr, cc) = (row.checked_sub(TAB_BAR_ROWS)?, col.checked_sub(left_w)?);
        // A pane cropped by a smaller window answers only inside it.
        if !self.in_visible(cr, cc) {
            return None;
        }
        for (pid, rect) in &self.layout.panes {
            // A framed pane answers for its CONTENT: the border ring is the
            // frame's own surface (it focuses, it never forwards), and the
            // pty grid the coordinates name is the content rect.
            let content = crate::pane_border::content_rect(*rect);
            if cr >= content.y
                && cr < content.y + content.rows
                && cc >= content.x
                && cc < content.x + content.cols
            {
                return Some((*pid, cr - content.y, cc - content.x));
            }
        }
        None
    }

    /// The pane covering a content-relative cell, if any. The shared primitive
    /// behind [`View::hit_test`] and [`View::seam_at`].
    pub(super) fn pane_covering(&self, cr: u16, cc: u16) -> Option<u64> {
        self.layout
            .panes
            .iter()
            .find(|(_, r)| cr >= r.y && cr < r.y + r.rows && cc >= r.x && cc < r.x + r.cols)
            .map(|(pid, _)| *pid)
    }

    /// The seam under an outer-terminal cell, addressed by the panes flanking it.
    ///
    /// The client never receives the pane tree - `Layout` carries a flat
    /// `Vec<(PaneId, Rect)>` - so a seam cannot be addressed by branch path here.
    /// A flanking pane pair is derivable from the rects and resolves to exactly
    /// one branch child pair server-side, which is why it is the wire address
    /// (`Command::ResizeSeam`) rather than the topological one the design
    /// originally assumed.
    ///
    /// `None` on a covered cell, on chrome, and on a `┼` crossing, where the two
    /// candidate seams are genuinely ambiguous and picking one would resize a
    /// divider the operator was not pointing at.
    pub(super) fn seam_at(&self, row: u16, col: u16) -> Option<Seam> {
        let left_w = self.left_chrome_w();
        if row < TAB_BAR_ROWS || col < left_w {
            return None;
        }
        let (cr, cc) = (row - TAB_BAR_ROWS, col - left_w);
        if self.pane_covering(cr, cc).is_some() {
            return None;
        }
        // Horizontal axis == children side by side == a vertical divider line.
        let across = cc
            .checked_sub(1)
            .and_then(|l| self.pane_covering(cr, l))
            .zip(cc.checked_add(1).and_then(|r| self.pane_covering(cr, r)))
            .map(|(a, b)| Seam {
                a,
                b,
                axis: Axis::Horizontal,
            });
        let down = cr
            .checked_sub(1)
            .and_then(|u| self.pane_covering(u, cc))
            .zip(cr.checked_add(1).and_then(|d| self.pane_covering(d, cc)))
            .map(|(a, b)| Seam {
                a,
                b,
                axis: Axis::Vertical,
            });
        match (across, down) {
            (Some(s), None) => Some(s),
            (None, Some(s)) => Some(s),
            _ => None,
        }
    }

    /// The rect of a pane by id, from the last layout.
    pub(super) fn pane_rect(&self, pid: u64) -> Option<Rect> {
        self.layout
            .panes
            .iter()
            .find(|(p, _)| *p == pid)
            .map(|(_, r)| *r)
    }

    /// Whether a seam still separates the exact pair that addressed it.
    ///
    /// Membership is deliberately not the test. A concurrent same-axis split
    /// can insert a pane between the two while keeping both ids alive, and a
    /// membership check would call that seam live: the drag stays latched,
    /// `set_seam_ratio` refuses every command for the now non-adjacent pair,
    /// and the divider looks dead until release with no notice ever shown.
    /// Geometry is what the address actually means, so geometry is what is
    /// checked - one divider cell between them, overlapping across it.
    pub(super) fn seam_is_live(&self, seam: Seam) -> bool {
        let (Some(ra), Some(rb)) = (self.pane_rect(seam.a), self.pane_rect(seam.b)) else {
            return false;
        };
        let abuts = |start: u16, len: u16, next: u16| start.saturating_add(len) + 1 == next;
        let overlaps = |a0: u16, a_len: u16, b0: u16, b_len: u16| {
            a0 < b0.saturating_add(b_len) && b0 < a0.saturating_add(a_len)
        };
        match seam.axis {
            Axis::Horizontal => {
                abuts(ra.x, ra.cols, rb.x) && overlaps(ra.y, ra.rows, rb.y, rb.rows)
            }
            Axis::Vertical => abuts(ra.y, ra.rows, rb.y) && overlaps(ra.x, ra.cols, rb.x, rb.cols),
        }
    }

    /// Where the seam sits now, as a content-area coordinate along its axis.
    /// Pair-relative, not branch-relative: the client cannot see the branch's
    /// other children, so the server rescales this against the pair's own total.
    /// Where the seam sits now, as a content-area coordinate along its axis.
    /// The divider is the cell immediately past pane `a`.
    pub(super) fn seam_pos(&self, seam: Seam) -> Option<u16> {
        let ra = self.pane_rect(seam.a)?;
        Some(match seam.axis {
            Axis::Horizontal => ra.x.saturating_add(ra.cols),
            Axis::Vertical => ra.y.saturating_add(ra.rows),
        })
    }

    /// Where an outer-terminal cell puts the divider, in the same content-area
    /// coordinates. Converting a position to a ratio is deliberately the
    /// server's job: it needs the branch child's extent, and a pane's rect is
    /// not that extent once axes alternate more than one level deep.
    pub(super) fn seam_pos_at(&self, seam: Seam, row: u16, col: u16) -> Option<u16> {
        let (cr, cc) = (
            row.checked_sub(TAB_BAR_ROWS)?,
            col.checked_sub(self.left_chrome_w())?,
        );
        Some(match seam.axis {
            Axis::Horizontal => cc,
            Axis::Vertical => cr,
        })
    }

    /// Recompute which grabbable chrome (seam, sideline border, pane grip) is
    /// accented, from the pointer position `(row, col)`.
    ///
    /// Terminals cannot change the cursor shape, so this accent is the entire
    /// draggability affordance. It must be refreshed on hover AND at the end of
    /// a drag: `Drag` events never touch hover state, so a drag that ends off
    /// the thing it grabbed would otherwise leave it lit until the next bare
    /// `Move` (the stale-highlight the pane relocation drag already avoids by
    /// re-hit-testing at its release coords).
    pub(super) fn refresh_hover_affordances(&mut self, row: u16, col: u16) {
        self.hover_seam = self.seam_at(row, col);
        self.hover_sideline_border = self.on_sideline_border(row, col);
        self.hover_feed_border = self.on_feed_border(row, col);
        self.hover_grip = self.grip_at(row, col);
    }

    /// The grip's row and column span for a pane rect, in OUTER terminal
    /// coordinates. Shared by the renderer and the hit test so a press always
    /// lands exactly where the glyphs drew.
    ///
    /// `None` when the pane cannot spare the cells. The grip is an affordance,
    /// never the only way in - the keyboard move-pane bind reaches the same
    /// operation - so a cramped pane loses the handle and keeps the gesture.
    ///
    /// Deliberately PERSISTENT rather than hover-only: a terminal cannot change
    /// the cursor shape, so a handle that appears only once the pointer is
    /// already on it is a handle nobody discovers. The cost is real and accepted
    /// - these three cells of each pane's top row always show `···` and always
    /// start a drag rather than reaching the inner app, so a program drawing
    /// there is overdrawn and unclickable at those columns. Bounded to
    /// multi-pane tabs, to three cells, and to panes wide enough to spare them,
    /// with the keyboard path as the escape hatch.
    pub(super) fn grip_span(&self, rect: Rect) -> Option<(u16, std::ops::Range<u16>)> {
        let w = GRIP.chars().count() as u16;
        // Two spare cells so the grip never abuts the pane's own borders, where
        // it would read as part of the divider lattice rather than as a handle.
        if rect.cols < w + 2 {
            return None;
        }
        let row = TAB_BAR_ROWS + rect.y;
        let c0 = self.left_chrome_w() + rect.x + (rect.cols - w) / 2;
        Some((row, c0..c0 + w))
    }

    /// The pane whose grip covers an outer cell.
    ///
    /// `None` on a single-pane tab: with nowhere to relocate to, a grip there
    /// would be an affordance for an operation that cannot succeed.
    pub(super) fn grip_at(&self, row: u16, col: u16) -> Option<u64> {
        if self.layout.panes.len() < 2 {
            return None;
        }
        self.layout.panes.iter().find_map(|(pid, rect)| {
            let (grow, gcols) = self.grip_span(*rect)?;
            (row == grow && gcols.contains(&col)).then_some(*pid)
        })
    }

    /// The drop zone under an outer cell during a relocation drag.
    ///
    /// Seams come straight from [`View::seam_at`], so the resize and
    /// relocate gestures cannot disagree about where a divider is - including
    /// its refusal on a `┼`, which stays "no zone here" rather than becoming a
    /// rejected drop. Outer edges are this node's own surface precisely because
    /// `seam_at` yields nothing off a live divider.
    pub(super) fn drop_zone_at(&self, row: u16, col: u16) -> Option<DropZone> {
        if let Some(seam) = self.seam_at(row, col) {
            // Landing "between the pair" is landing after the left/top flank.
            return Some(DropZone {
                target: seam.a,
                dir: match seam.axis {
                    Axis::Horizontal => Dir::Right,
                    Axis::Vertical => Dir::Down,
                },
            });
        }
        self.edge_zone_at(row, col)
    }

    /// The zone for a cell on the content area's outer rim.
    ///
    /// An edge drop lands beside the pane the operator actually pointed at,
    /// not spanning the full side of the layout. Predictability wins over
    /// power here: "it goes next to this one" is what the pointer already
    /// says, and a full-side insert would need a root-level tree op whose
    /// result the operator cannot see themselves asking for.
    pub(super) fn edge_zone_at(&self, row: u16, col: u16) -> Option<DropZone> {
        let left_w = self.left_chrome_w();
        if row < TAB_BAR_ROWS || col < left_w {
            return None;
        }
        let (cr, cc) = (row - TAB_BAR_ROWS, col - left_w);
        let (a_rows, a_cols) = self.visible_area();
        if a_rows == 0 || a_cols == 0 || cr >= a_rows || cc >= a_cols {
            return None;
        }
        let target = self.pane_covering(cr, cc)?;
        // Corners resolve left/right before up/down rather than being refused:
        // unlike a `┼` (where two seams genuinely compete), both answers at a
        // corner are the same drop from the operator's view - the pane ends up
        // in that corner either way - so a fixed order beats a dead cell.
        let dir = if cc == 0 {
            Dir::Left
        } else if cc + 1 == a_cols {
            Dir::Right
        } else if cr == 0 {
            Dir::Up
        } else if cr + 1 == a_rows {
            Dir::Down
        } else {
            // Interior cell: instead of going dark (the old `return None`),
            // resolve to the nearest edge of the pane UNDER the pointer, so the
            // destination band tracks the pointer live rather than only waking
            // at the content-area rim (shipped rim-only; a relocation
            // gave no preview until the pointer was dragged all the way to an
            // outer edge). Keyed off THIS pane's own centre so it stays
            // predictable: the pointer's dominant offset from centre picks the
            // axis, its sign the side. A dead-centre tie resolves horizontal,
            // matching the corner tie-break above. Rim/seam cells never reach
            // here (handled above and by `drop_zone_at`'s seam check), so their
            // semantics and every rim test are unchanged.
            let rect = self.pane_rect(target)?;
            let (lx, ly) = (
                cc.saturating_sub(rect.x) as i32,
                cr.saturating_sub(rect.y) as i32,
            );
            let (cols, rows) = (rect.cols as i32, rect.rows as i32);
            // Compare |nx-0.5| vs |ny-0.5| without floats: cross-multiply the
            // half-cell-centred offsets by the opposite dimension.
            let hx = 2 * lx + 1 - cols; // <0 left half, >0 right half
            let hy = 2 * ly + 1 - rows; // <0 top half,  >0 bottom half
            if hx.abs() * rows >= hy.abs() * cols {
                if hx < 0 {
                    Dir::Left
                } else {
                    Dir::Right
                }
            } else if hy < 0 {
                Dir::Up
            } else {
                Dir::Down
            }
        };
        Some(DropZone { target, dir })
    }

    /// (v43, G1) Whether a cell lies on the tab strip (the top bar right
    /// of the sideline panel) rather than the content area. The same region
    /// [`View::chrome_hit`] treats as the strip, so a pane dropped anywhere on it
    /// breaks into a new tab.
    pub(super) fn strip_at(&self, row: u16, col: u16) -> bool {
        row < TAB_BAR_ROWS && col >= self.panel_w()
    }

    /// (v43, G2) The stable tab id under a strip cell, if it names an
    /// existing tab (a drag SOURCE). Walks the same spans [`View::chrome_hit`]
    /// walks, so a drag pickup and a click resolve identically. The `+` (NewTab)
    /// is not a drag source, and a cell under the notice overlay is not the strip.
    pub(super) fn tab_cell_at(&self, row: u16, col: u16) -> Option<u64> {
        let panel_w = self.panel_w();
        if row >= TAB_BAR_ROWS || col < panel_w {
            return None;
        }
        let col = col as usize;
        if let Some((start, text)) = self.notice_overlay(self.term.1 as usize) {
            if col >= start && col < start + text.chars().count() {
                return None;
            }
        }
        let mut c = panel_w as usize;
        for span in self.tab_bar_window() {
            let w = span.text.chars().count();
            if col >= c && col < c + w {
                return match span.hit? {
                    TabHit::Tab(tid) => Some(tid),
                    TabHit::NewTab => None,
                };
            }
            c += w;
        }
        None
    }

    /// A content-derived identity for the sideline row at `i`, stable
    /// across a layout push. `None` for a row with no identity of its own (a
    /// spacer, a table head): those cannot be re-checked after a push, so a
    /// gesture must not latch onto one in the first place.
    ///
    /// An INDEX is not an identity. `display_rows()` is rebuilt on every layout,
    /// so a row that vanishes mid-gesture slides a different row under the same
    /// number - which is why `set_layout` re-anchors the selector by agent name
    /// and why `RowDrag` carries a `RowSource` rather than a position.
    pub(super) fn row_identity(&self, i: usize) -> Option<String> {
        match self.display_rows().get(i)? {
            DisplayRow::Agent(a) => Some(format!("agent:{}", a.name)),
            DisplayRow::Sel(s) => Some(format!("squad:{}:{:?}", s.squad, s.tab)),
            DisplayRow::Header { key, .. } => Some(format!("header:{key:?}")),
            DisplayRow::IdleFold { key, .. } => Some(format!("idlefold:{key:?}")),
            DisplayRow::NewSquad => Some("newsquad".into()),
            // Card lines carry their agent's identity, like the tap path.
            DisplayRow::CardDetail(a) | DisplayRow::CardMetrics(a) => {
                Some(format!("agent:{}", a.name))
            }
            // A card line or a ghost row is no row's identity: the card
            // rebuilds from the draft on every layout.
            DisplayRow::Blank
            | DisplayRow::CardRule
            | DisplayRow::TableHead
            | DisplayRow::TableEmpty
            | DisplayRow::Card(..)
            | DisplayRow::Ghost(..) => None,
        }
    }

    /// The density button is pinned chrome painted over row 0: a press
    /// there answers the strip, never the row drawn underneath it.
    pub(super) fn press_on_density_button(&self, row: u16, col: u16) -> bool {
        row == 0
            && self
                .density_button_range(self.panel_w() as usize)
                .is_some_and(|range| range.contains(&(col as usize)))
    }

    /// The sideline row a press at `(row, col)` should HOLD on, with the
    /// identity to re-check it by, for the rows `row_drag_source_at` declines.
    /// Inert rows qualify: a hold that opens no menu answers with a notice, and
    /// silence is the defect this fixes. A row with no stable identity does not,
    /// so its press keeps the pre-hold behavior rather than latching.
    pub(super) fn press_hold_row_at(&self, row: u16, col: u16) -> Option<(usize, String)> {
        if self.press_on_density_button(row, col) {
            return None;
        }
        let i = self.sideline_row_at(row, col)?;
        Some((i, self.row_identity(i)?))
    }

    /// (v43, G3) The drag source of a sideline agent row under a cell, if
    /// any. A pane-hosted row drags its pane; a paneless bg row drags its attach
    /// id; a row with neither (a dead external tombstone) is not draggable.
    pub(super) fn row_drag_source_at(&self, row: u16, col: u16) -> Option<RowSource> {
        if self.press_on_density_button(row, col) {
            return None;
        }
        let i = self.sideline_row_at(row, col)?;
        match self.display_rows().get(i)? {
            DisplayRow::Agent(a) => {
                if let Some(pid) = a.pane_id {
                    Some(RowSource::Pane(pid))
                } else {
                    a.attach_id.clone().map(RowSource::Attach)
                }
            }
            _ => None,
        }
    }

    /// The column range of the density button on the sideline's top
    /// row, shared by the renderer and the hit-test so a click lands where it
    /// draws. `None` when the panel is too narrow to reserve the button without
    /// eating the header's own label.
    ///
    /// The button is an affordance, never the only way in: Locked Decision 5
    /// puts the density cycle on a keybind too, so a too-narrow panel loses the
    /// button and keeps the gesture.
    pub(super) fn density_button_range(&self, panel_w: usize) -> Option<std::ops::Range<usize>> {
        let tw = panel_w.saturating_sub(1); // last column is the divider
        let start = tw.checked_sub(DENSITY_BTN_W)?;
        (tw >= DENSITY_BTN_W + 6).then_some(start..tw)
    }
}
