//! Pane PTY sizes. A pane's PTY is its layout rect, minus the border ring a
//! framed pane wears, minus the inset the clients viewing its tab ask for
//! (`ClientMsg::PaneInset`): a client that pads its panes in pixels keeps the
//! cells that padding covers out of the grid. The largest inset wins, the same
//! way the smallest viewer wins the whole-area size.

use super::*;

impl Core {
    /// The cells every pane of a tab keeps empty: the largest inset among the
    /// non-passive clients viewing it.
    fn tab_inset(&self, tid: TabId) -> (u16, u16) {
        self.clients
            .iter()
            .filter(|c| c.view.1 == tid && !c.passive)
            .filter_map(|c| self.sizing.insets.get(&c.id))
            .fold((0, 0), |a, i| (a.0.max(i.0), a.1.max(i.1)))
    }

    pub(super) fn set_pane_inset(&mut self, id: u64, rows: u16, cols: u16) -> Flow {
        let live = self.clients.iter().any(|c| c.id == id && !c.passive);
        if live && self.sizing.insets.insert(id, (rows, cols)) != Some((rows, cols)) {
            self.push_layout(true);
        }
        Flow::Continue
    }

    /// Resize each pane of `tid` to its rect: only real geometry changes hit
    /// the PTY. A pane under a remote drive (`link_drive`) takes the
    /// driver's terminal area instead of its tile; the client clamps the
    /// bigger grid into the tile until the drive clears.
    pub(super) fn apply_pane_sizes(&mut self, tid: TabId, rects: &[(u64, Rect)]) {
        let inset = self.tab_inset(tid);
        for (pid, r) in rects {
            let driven = self.links.drive(*pid).map(|(_, area)| area);
            if let Some(area) = driven {
                // The driver's own terminal carries no inset, so the raw
                // area applies; the tab's inset fold waits for the reflow.
                self.set_pane_size(*pid, area.0, area.1);
                continue;
            }
            if let Some(entry) = self.panes.get_mut(pid) {
                let content = crate::pane_border::content_rect(*r);
                let content = Rect {
                    rows: content.rows.saturating_sub(inset.0).max(1),
                    cols: content.cols.saturating_sub(inset.1).max(1),
                    ..content
                };
                Self::resize_pane_entry(entry, (content.rows, content.cols));
            }
        }
    }

    /// Resize one pane to `(rows, cols)` by id, drive area or tile alike.
    fn set_pane_size(&mut self, pid: u64, rows: u16, cols: u16) {
        if let Some(entry) = self.panes.get_mut(&pid) {
            Self::resize_pane_entry(entry, (rows, cols));
        }
    }

    /// The one pane-resize body: only a real change hits the PTY, a
    /// keeper-hosted pane flips its grid on the keeper's ack, and the child
    /// is nudged to repaint once the resize dust settles.
    fn resize_pane_entry(entry: &mut PaneEntry, size: (u16, u16)) {
        if entry.requested_size == size {
            return;
        }
        entry.requested_size = size;
        if let Err(e) = entry.pty.resize(size.0, size.1, 0, 0) {
            // Grid and kernel winsize would disagree: log it.
            crate::mux_log!(error; "fno mux: pty resize failed: {e}");
        }
        if entry.pty.is_keeper_hosted() {
            // Applied later, in `drain_pty_output`, once the
            // keeper's ack for THIS resize round-trips
            // through the pane's own ordered output channel
            // (`PaneChunk::Resized`). Flipping `vt` here,
            // before the round trip lands, would let output
            // the child already produced under the OLD size
            // - still ahead of this resize on the wire -
            // arrive after the flip and get fed into the
            // wrong-size grid (the byte-exact reattach
            // race this branch's keeper hop introduced).
        } else {
            entry.vt.resize(size.0, size.1);
        }
        // Ask the child to repaint once the resize dust
        // settles: arm a deferred nudge the 1s core tick fires.
        // An immediate re-signal would coalesce into the burst
        // the resize itself raised and change nothing.
        entry.nudge_due = Some(Instant::now() + Self::NUDGE_DELAY);
    }
}

#[cfg(test)]
#[path = "pane_inset_tests.rs"]
mod tests;
