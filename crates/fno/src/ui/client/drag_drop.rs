//! Pane, tab and row drags: what a press-and-drag carries and where it drops.
//! Moved out of client.rs (file budget).

use super::*;

/// A draggable seam between two panes, addressed by the panes flanking it:
/// `a` is the left/top pane, `b` the right/bottom one. `axis` is the branch's
/// axis, so `Horizontal` (children side by side) means a vertical divider line.
///
/// The pair addresses one branch child pair, not one pane pair: a seam can run
/// past several panes on either side, and naming any pane from each side picks
/// out the same two branch children (a same-axis branch never nests, so every
/// descendant of a child shares that child's extent along the branch axis).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Seam {
    pub(super) a: u64,
    pub(super) b: u64,
    pub(super) axis: Axis,
}

/// A seam drag in flight. `start_pos` is where the divider sat when the drag
/// began, kept for the Esc revert; `last_pos` suppresses duplicate commands
/// between cell-boundary crossings.
#[derive(Clone, Copy, Debug)]
pub(super) struct SeamDrag {
    pub(super) seam: Seam,
    pub(super) start_pos: u16,
    pub(super) last_pos: u16,
    pub(super) last_at: Instant,
}

/// A sideline right-border drag in flight. `start_width` is the width
/// at grab, so a bare Esc reverts to it (mirroring [`SeamDrag::start_pos`]);
/// `last_at` refreshes on every column of motion so a swallowed mouse-up expires
/// via the same stuck-drag timeout seam and pane drags use (Locked 9).
#[derive(Clone, Copy, Debug)]
pub(super) struct SidelineDrag {
    pub(super) start_width: u16,
    pub(super) last_at: Instant,
}

/// Where a drop would put the dragged pane: adjacent to `target` on
/// its `dir` side.
///
/// A seam and the outer edge beside it collapse to the same shape on purpose -
/// "between C and D" is "after C", and "along the left edge" is "left of the
/// leftmost pane there" - so the drop path has one vocabulary and `move_leaf`
/// has one entry point, which is what keeps drag and keyboard genuinely
/// identical rather than merely similar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct DropZone {
    pub(super) target: u64,
    pub(super) dir: Dir,
}

/// A pane relocation drag in flight.
///
/// `zone` is the candidate under the pointer, recomputed per motion report and
/// kept here so the renderer can light it without re-hit-testing. Purely
/// client-local: nothing is sent until the release, so a drag that dies mid-air
/// leaves the server's tree untouched.
#[derive(Clone, Copy, Debug)]
pub(super) struct PaneDrag {
    pub(super) mover: u64,
    pub(super) zone: Option<DropZone>,
    /// (v43, G1) The pointer is over the tab strip: the drop breaks the
    /// pane into its own new tab (`Command::BreakPane`) instead of relocating it
    /// within the tab (`Command::MovePane`). Mutually exclusive with `zone` - a
    /// cell is either the strip or the content area, never both.
    pub(super) on_strip: bool,
    pub(super) last_at: Instant,
}

/// (v43, G2) A tab-cell relocation drag in flight: picking a whole tab up
/// from the strip to graft it into the current tab's content as a split
/// (`Command::JoinTab`). Mirrors [`PaneDrag`]'s lifetime (ghost, timeout reaper,
/// esc-cancel); `zone` is the content-edge candidate under the pointer.
#[derive(Clone, Copy, Debug)]
pub(super) struct TabDrag {
    pub(super) src_tab: u64,
    pub(super) zone: Option<DropZone>,
    pub(super) last_at: Instant,
    /// When the press began - the long-press clock, unlike `last_at`
    /// which motion refreshes.
    pub(super) start_at: Instant,
    /// Whether any drag report arrived: a long-press needs a hold
    /// with NO motion, and the clock alone cannot tell a hold from a slow
    /// drag that ends zone-less.
    pub(super) moved: bool,
}

/// (v43, G3) The drag source of a sideline agent row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RowSource {
    /// A pane-hosted row: the drop moves its pane into the current tab's content,
    /// a cross-tab `Command::MovePane` (the pane lives in another tab).
    Pane(u64),
    /// A paneless bg row: the drop attaches it at the drop slot
    /// (`Command::AttachAgent` with a placement).
    Attach(String),
}

/// (v43, G3) A sideline-row placement drag in flight. Reuses the
/// content-area zone vocabulary; `RowSource` carries whether the drop moves a
/// live pane or attaches a paneless session. Not `Copy` (the attach id is owned).
#[derive(Clone, Debug)]
pub(super) struct RowDrag {
    pub(super) src: RowSource,
    pub(super) zone: Option<DropZone>,
    pub(super) last_at: Instant,
    /// When the press began - the long-press clock, unlike `last_at`
    /// which motion refreshes.
    pub(super) start_at: Instant,
    /// Whether any drag report arrived: a long-press needs a hold
    /// with NO motion, and the clock alone cannot tell a hold from a slow
    /// drag that ends zone-less.
    pub(super) moved: bool,
}

impl View {
    pub(super) fn begin_pane_drag(&mut self, mover: u64, now: Instant) {
        self.pane_drag = Some(PaneDrag {
            mover,
            zone: None,
            on_strip: false,
            last_at: now,
        });
    }

    /// Track the pointer to a new candidate zone. Returns whether the highlight
    /// changed, so a drag across a pane's interior costs no redraws.
    pub(super) fn pane_drag_to(&mut self, row: u16, col: u16, now: Instant) -> bool {
        let Some(mover) = self.pane_drag.map(|d| d.mover) else {
            return false;
        };
        // Over the strip (G1): the drop breaks the pane into its own tab, so the
        // content zone is cleared - a cell is the strip XOR a content zone.
        let on_strip = self.strip_at(row, col);
        // A zone the dragged pane already occupies is the origin: no highlight,
        // so the gesture reads as "this drop does nothing" before the release.
        // BOTH flanks count - a seam is addressed by its left/top pane, so the
        // divider a pane already abuts on its far side names the NEIGHBOUR and
        // would otherwise light up as a real destination.
        let abuts = |s: Seam| s.a == mover || s.b == mover;
        let zone = if on_strip {
            None
        } else {
            self.drop_zone_at(row, col)
                .filter(|z| z.target != mover && self.pane_rect(mover).is_some())
                .filter(|_| !self.seam_at(row, col).is_some_and(abuts))
        };
        let drag = self.pane_drag.as_mut().expect("checked above");
        drag.last_at = now;
        let changed = drag.zone != zone || drag.on_strip != on_strip;
        drag.zone = zone;
        drag.on_strip = on_strip;
        changed
    }

    /// End the drag and return the command that applies it.
    ///
    /// `None` on every cancel path - released off any zone, on the origin, or
    /// after the dragged pane went away - so a cancelled drag provably puts
    /// nothing on the wire rather than sending a command the server will refuse.
    /// Over the strip it breaks the pane into its own tab instead (G1).
    pub(super) fn commit_pane_drag(&mut self) -> Option<Command> {
        let drag = self.pane_drag.take()?;
        if drag.on_strip {
            return Some(Command::BreakPane { pane: drag.mover });
        }
        let zone = drag.zone?;
        Some(Command::MovePane {
            mover: Some(drag.mover),
            target: Some(zone.target),
            dir: zone.dir,
        })
    }

    pub(super) fn begin_tab_drag(&mut self, src_tab: u64, now: Instant) {
        self.tab_drag = Some(TabDrag {
            src_tab,
            zone: None,
            last_at: now,
            start_at: now,
            moved: false,
        });
    }

    /// Track a tab-cell drag to a content-edge zone. Returns whether the
    /// highlight changed. A drag of the CURRENT tab lights nothing: any zone in
    /// its own content would be a self-join, suppressed exactly like an origin
    /// drop (grammar).
    pub(super) fn tab_drag_to(&mut self, row: u16, col: u16, now: Instant) -> bool {
        let Some(src_tab) = self.tab_drag.map(|d| d.src_tab) else {
            return false;
        };
        let self_join = self.active_squad_active_tab_id() == Some(src_tab);
        let zone = if self_join {
            None
        } else {
            self.drop_zone_at(row, col)
        };
        let drag = self.tab_drag.as_mut().expect("checked above");
        drag.last_at = now;
        let changed = drag.zone != zone;
        drag.zone = zone;
        changed
    }

    /// End the tab-cell drag and return the join command, or `None` on any cancel
    /// path (off-zone, or a self-join onto the source tab's own content - the
    /// server also refuses BAD_REQUEST, belt and braces).
    pub(super) fn commit_tab_drag(&mut self) -> Option<Command> {
        let drag = self.tab_drag.take()?;
        if self.active_squad_active_tab_id() == Some(drag.src_tab) {
            return None;
        }
        let zone = drag.zone?;
        Some(Command::JoinTab {
            src_tab: drag.src_tab,
            anchor_pane: zone.target,
            dir: zone.dir,
        })
    }

    pub(super) fn cancel_tab_drag(&mut self) -> bool {
        self.tab_drag.take().is_some()
    }

    pub(super) fn begin_row_drag(&mut self, src: RowSource, now: Instant) {
        self.row_drag = Some(RowDrag {
            src,
            zone: None,
            last_at: now,
            start_at: now,
            moved: false,
        });
    }

    /// Track a sideline-row drag to a content-edge zone. Unlike a pane drag this
    /// does NOT gate on `pane_rect(mover)`: a pane-hosted row names a pane living
    /// in ANOTHER tab, off the current layout by design (the server resolves it).
    pub(super) fn row_drag_to(&mut self, row: u16, col: u16, now: Instant) -> bool {
        let mover = match self.row_drag.as_ref().map(|d| &d.src) {
            None => return false,
            Some(RowSource::Pane(pid)) => Some(*pid),
            Some(RowSource::Attach(_)) => None,
        };
        // Suppress an origin drop the same way `pane_drag_to` does, but only for
        // a mover that is ON this layout: the server discards an origin move in
        // silence (it reads as a deliberate cancel), so a zone that lights up
        // and then moves nothing is the whole defect. An off-layout mover cannot
        // be an origin - it is not in this tree at all - and must keep every
        // zone, which is why the `pane_rect` gate stays out of this path.
        let zone = match mover.filter(|m| self.pane_rect(*m).is_some()) {
            Some(mover) => {
                let abuts = |s: Seam| s.a == mover || s.b == mover;
                self.drop_zone_at(row, col)
                    .filter(|z| z.target != mover)
                    .filter(|_| !self.seam_at(row, col).is_some_and(abuts))
            }
            None => self.drop_zone_at(row, col),
        };
        let drag = self.row_drag.as_mut().expect("checked above");
        drag.last_at = now;
        let changed = drag.zone != zone;
        drag.zone = zone;
        changed
    }

    /// End the sideline-row drag and return its placement command, or `None` off
    /// any zone. A pane-hosted row moves its pane cross-tab (`MovePane`); a
    /// paneless bg row attaches at the drop slot (`AttachAgent` with a placement
    /// anchored to the dropped-on pane in the current tab).
    pub(super) fn commit_row_drag(&mut self) -> Option<Command> {
        let drag = self.row_drag.take()?;
        let zone = drag.zone?;
        Some(match drag.src {
            RowSource::Pane(pid) => Command::MovePane {
                mover: Some(pid),
                target: Some(zone.target),
                dir: zone.dir,
            },
            // The anchor (`at`) alone names the drop slot: the server resolves
            // its squad and tab from the anchor pane's live location (overriding
            // the agent's owner-squad routing), so the client leaves `tab`
            // unset rather than guess "the active tab".
            RowSource::Attach(id) => Command::AttachAgent {
                id,
                placement: PanePlacement {
                    at: Some(zone.target),
                    split: Some(zone.dir),
                    here: false,
                    ..PanePlacement::default()
                },
            },
        })
    }

    pub(super) fn cancel_row_drag(&mut self) -> bool {
        self.row_drag.take().is_some()
    }

    /// Abandon a drag with no command. Esc, the timeout, and the dragged pane
    /// exiting mid-gesture all land here. Returns whether one was in flight.
    pub(super) fn cancel_pane_drag(&mut self) -> bool {
        self.pane_drag.take().is_some()
    }

    /// The outer cells to light for a candidate zone: the one-cell band along
    /// the `dir` side of the target's rect.
    ///
    /// One formula serves both zone kinds because a zone always names the gap
    /// the pane will land in - beside a seam that band IS the divider between
    /// the pair. On the layout's outer rim there is no gap yet, so the band
    /// clamps onto the target's own edge column/row: still honest ("it lands on
    /// this side of this pane") without reaching into the sideline or the tab
    /// bar, which own those cells.
    pub(super) fn drop_band(
        &self,
        zone: DropZone,
    ) -> Option<(std::ops::Range<u16>, std::ops::Range<u16>)> {
        let rect = self.pane_rect(zone.target)?;
        let left_w = self.left_chrome_w();
        // Saturating throughout: `rect` comes from the last Layout, which can
        // lag a sideline toggle or a resize, so these sums are not guaranteed to
        // stay inside the terminal. Overflow here would panic a debug build for
        // a highlight; clamping just draws the band at the edge instead.
        let (r0, c0) = (
            TAB_BAR_ROWS.saturating_add(rect.y),
            left_w.saturating_add(rect.x),
        );
        let (r1, c1) = (r0.saturating_add(rect.rows), c0.saturating_add(rect.cols));
        // The band sits one cell outside the rect, except on the rim where that
        // cell belongs to the sideline or the tab bar - there it clamps back
        // onto the rect's own edge.
        // BOTH sides must be bounded. Guarding only the low side left the right
        // and bottom rims resolving to a cell one past the terminal, which
        // compose() then clamped away to an empty range: the zone rendered
        // nothing while a release there still relocated the pane. In this UI's
        // own vocabulary an unlit zone means "this drop does nothing" (an origin
        // drop deliberately blanks the zone to say exactly that), so the
        // asymmetry taught the wrong thing on half the rim.
        let (term_rows, term_cols) = self.term;
        // `outside` is None when there is no cell on that side at all (the
        // rect already starts at the edge). Checked rather than wrapping: the
        // old version leaned on wrapping to u16::MAX to fail the range test,
        // which worked but read as an accident.
        let band_col = |outside: Option<u16>, own: u16| {
            let c = outside
                .filter(|c| (left_w..term_cols).contains(c))
                .unwrap_or(own);
            c..c.saturating_add(1)
        };
        let band_row = |outside: Option<u16>, own: u16| {
            let r = outside
                .filter(|r| (TAB_BAR_ROWS..term_rows).contains(r))
                .unwrap_or(own);
            r..r.saturating_add(1)
        };
        // saturating: a zero-column rect from the server would otherwise
        // underflow `c1 - 1` and panic in debug.
        Some(match zone.dir {
            Dir::Left => (r0..r1, band_col(c0.checked_sub(1), c0)),
            Dir::Right => (r0..r1, band_col(Some(c1), c1.saturating_sub(1))),
            Dir::Up => (band_row(r0.checked_sub(1), r0), c0..c1),
            Dir::Down => (band_row(Some(r1), r1.saturating_sub(1)), c0..c1),
        })
    }
}
