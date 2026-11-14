//! Focus recall: the (v124) tmux parity commands' runtime state and
//! handlers. Parent helpers resolve through the glob. Nothing here is
//! persisted: a restart forgets recall state.
//!
//! The zoom is level-triggered rather than editing the seven `tab.focus =`
//! sites: [`FocusRecall::tab_rects`] runs on every layout pass (the same
//! per-pass sweep the `seen` eviction uses), and drops the zoom the moment
//! its invariant breaks: the zoomed pane is no longer focus, no longer a
//! leaf, or the tab collapsed to one pane. Every focus change, split and
//! close ends the zoom with no edit at the mutation sites.
use super::*;
use crate::tree::PaneId;

/// Per-tab and per-client recall state for the (v124) commands.
#[derive(Default)]
pub(crate) struct FocusRecall {
    /// Per tab: (current, previous) focus, recorded level-triggered.
    pub(super) focus: HashMap<TabId, (PaneId, PaneId)>,
    /// Per client: the view held before the current one.
    pub(super) view: HashMap<u64, (u64, TabId)>,
    /// Per tab: the zoomed pane; dropped when its invariant breaks.
    pub(super) zoomed: HashMap<TabId, PaneId>,
}

impl FocusRecall {
    pub(super) fn tab_rects(&mut self, tab: &Tab, area: Rect) -> Vec<(PaneId, Rect)> {
        // Level-triggered focus record: when the live focus left the stored
        // current, the stored current becomes the previous.
        let entry = self.focus.entry(tab.id).or_insert((tab.focus, tab.focus));
        if entry.0 != tab.focus {
            *entry = (tab.focus, entry.0);
        }
        // Drop the zoom the moment its invariant breaks: the pane is no
        // longer focus, no longer a leaf, or the tab collapsed to one pane.
        let live = tree::leaves(&tab.root);
        let holds = self
            .zoomed
            .get(&tab.id)
            .is_some_and(|&z| z == tab.focus && live.contains(&z) && live.len() > 1);
        if !holds {
            self.zoomed.remove(&tab.id);
        }
        match self.zoomed.get(&tab.id) {
            Some(&z) => vec![(z, area)],
            None => tree::layout(&tab.root, area),
        }
    }
}
impl Core {
    /// (v124) `prefix z`: zoom the focused pane over the tab, or restore.
    pub(super) fn zoom_pane(&mut self, client_id: u64, view: (u64, TabId), _vp: Rect) {
        let Some(tab) = self.viewed_tab(view) else {
            return;
        };
        let focus = tab.focus;
        let tab_id = tab.id;
        let live = tree::leaves(&tab.root);
        let zoomed_now = self.recall.zoomed.get(&tab_id).copied();
        if zoomed_now == Some(focus) {
            self.recall.zoomed.remove(&tab_id);
            self.push_layout(true);
            return;
        }
        if live.len() < 2 {
            self.notice(client_id, "only one pane");
            return;
        }
        self.recall.zoomed.insert(tab_id, focus);
        self.push_layout(true);
    }

    /// (v124) `prefix o`: focus the next pane in tree order, wrapping.
    pub(super) fn next_pane(&mut self, client_id: u64, view: (u64, TabId), _vp: Rect) {
        let Some(tab) = self.viewed_tab_mut(view) else {
            return;
        };
        let leaves = tree::leaves(&tab.root);
        if leaves.len() < 2 {
            self.notice(client_id, "only one pane");
            return;
        }
        let idx = leaves
            .iter()
            .position(|&p| p == tab.focus)
            .expect("focus is a live leaf");
        let at = (idx + 1) % leaves.len();
        let Some(&next) = leaves.get(at) else {
            return;
        };
        tab.focus = next;
        self.push_layout(false);
    }

    /// (v124) `prefix ;`: focus the pane focused before the current one.
    pub(super) fn last_pane(&mut self, client_id: u64, view: (u64, TabId), _vp: Rect) {
        let Some(tab) = self.viewed_tab_mut(view) else {
            return;
        };
        let tab_id = tab.id;
        if tree::leaves(&tab.root).len() < 2 {
            self.notice(client_id, "only one pane");
            return;
        }
        let prev = match self.recall.focus.get(&tab_id) {
            Some(&(cur, prev)) if cur != prev => prev,
            _ => {
                self.notice(client_id, "no last pane");
                return;
            }
        };
        if let Some(tab) = self.viewed_tab_mut(view) {
            tab.focus = prev;
            self.push_layout(false);
        }
    }

    /// (v124) `prefix l`: return this client to its previous view.
    pub(super) fn last_tab(&mut self, client_id: u64, view: (u64, TabId), _vp: Rect) {
        let Some(&(prev_sid, prev_tid)) = self.recall.view.get(&client_id) else {
            self.notice(client_id, "no last tab");
            return;
        };
        let live = prev_sid != view.0 || prev_tid != view.1;
        let found = self
            .session
            .squad(prev_sid)
            .is_some_and(|sq| sq.tabs.iter().any(|t| t.id == prev_tid));
        if !live || !found {
            self.notice(client_id, "no last tab");
            return;
        }
        self.set_view(client_id, prev_sid, prev_tid);
        // A tab switch replaces the visible pane set, so the stale frame
        // slots must flush: the same reemit=true SelectTab passes.
        self.push_layout(true);
    }

    /// (v124) `prefix {`/`}`: exchange the focused pane with its neighbour
    /// in tree order, wrapping like tmux `swap-pane -U`/`-D`.
    pub(super) fn swap_pane(
        &mut self,
        client_id: u64,
        view: (u64, TabId),
        _vp: Rect,
        forward: bool,
    ) {
        let Some(tab) = self.viewed_tab_mut(view) else {
            return;
        };
        let leaves = tree::leaves(&tab.root);
        if leaves.len() < 2 {
            self.notice(client_id, "only one pane");
            return;
        }
        let idx = leaves
            .iter()
            .position(|&p| p == tab.focus)
            .expect("focus is a live leaf");
        let n = leaves.len();
        let at = if forward {
            (idx + 1) % n
        } else {
            (idx + n - 1) % n
        };
        let Some(&neighbor) = leaves.get(at) else {
            return;
        };
        if !tree::swap_leaves(tab, tab.focus, neighbor) {
            self.notice(client_id, "cannot swap panes");
            return;
        }
        self.push_layout(true);
    }

    /// `prefix` arrows: focus `dir`-ward of the focused pane. Moved verbatim
    /// from server.rs (file budget).
    pub(super) fn focus_dir(&mut self, client_id: u64, view: (u64, TabId), vp: Rect, dir: Dir) {
        let Some(tab) = self.viewed_tab_mut(view) else {
            return;
        };
        match tree::navigate(&tab.root, vp, tab.focus, dir) {
            Some(next) => {
                // Focus is per-tab, shared by co-viewers (Locked 4).
                tab.focus = next;
                self.push_layout(false);
            }
            None => self.notice(client_id, "no pane in that direction"),
        }
    }

    /// `prefix H/J/K/L`: resize toward `dir`, ending any zoom first (tmux
    /// resizes inside a zoom the same way). Moved from server.rs.
    pub(super) fn resize_dir(&mut self, client_id: u64, view: (u64, TabId), vp: Rect, dir: Dir) {
        let Some(tab) = self.viewed_tab(view) else {
            return;
        };
        let tab_id = tab.id;
        self.recall.zoomed.remove(&tab_id);
        let Some(tab) = self.viewed_tab_mut(view) else {
            return;
        };
        if tree::resize(tab, vp, dir, tree::RESIZE_STEP) {
            self.push_layout(true);
        } else {
            // BEL only when nothing changed.
            self.notice(client_id, "cannot resize further");
        }
    }

    /// A mouse drag on a divider. Silent on refusal: a drag streams commands
    /// and clamps constantly, so a notice per rejected cell would be noise.
    /// Ends any zoom first, so a drag never moves a seam the operator cannot
    /// see. Moved from server.rs.
    pub(super) fn resize_seam(
        &mut self,
        _client_id: u64,
        view: (u64, TabId),
        vp: Rect,
        a: u64,
        b: u64,
        pos: u16,
    ) {
        let Some(tab) = self.viewed_tab(view) else {
            return;
        };
        let tab_id = tab.id;
        self.recall.zoomed.remove(&tab_id);
        let Some(tab) = self.viewed_tab_mut(view) else {
            return;
        };
        if tree::set_seam_pos(tab, vp, a, b, pos) {
            self.push_layout(true);
        }
    }

    /// Relocate a pane beside a target: the grip-drop and the keyboard
    /// move-pane bind. Moved verbatim from server.rs (file budget), comment
    /// included: the resolve-both-ends-against-the-viewed-tab block and the
    /// cross-tab composition are the part a future editor must not lose.
    pub(super) fn move_pane(
        &mut self,
        client_id: u64,
        view: (u64, TabId),
        vp: Rect,
        mover: Option<u64>,
        target: Option<u64>,
        dir: Dir,
    ) {
        // Resolve both ends against the VIEWED tab first: that owns the
        // keyboard-bind defaults (mover = focus; target = the pane the
        // same geometry FocusDir uses lies `dir`-ward). A drop names both
        // and skips the navigate.
        let (mover, target) = {
            let Some(tab) = self.viewed_tab(view) else {
                return;
            };
            let mover = mover.unwrap_or(tab.focus);
            let Some(target) = target.or_else(|| tree::navigate(&tab.root, vp, mover, dir)) else {
                self.notice(client_id, "no pane in that direction");
                return;
            };
            (mover, target)
        };
        // A sideline-row drop can name a `mover` living in ANOTHER tab
        // (the row carries the pane id, not the rendered layout). When the
        // two ends live in different tabs, compose the cross-tab move
        // (detach + graft, all-or-nothing); otherwise the within-tab
        // move_leaf, unchanged. Loud on refusal, unlike ResizeSeam: a
        // relocation is ONE deliberate gesture, so a rejected drop that
        // said nothing would read as the feature being broken.
        let src = self.session.find_pane(mover);
        let dst = self.session.find_pane(target);
        match (src, dst) {
            (Some(s), Some(d)) if s != d => {
                // move_pane_cross_tab propagates only PaneGone / TooSmall
                // (from detach_leaf / graft_subtree); Origin is a
                // within-tab move_leaf verdict and cannot arise here, so
                // every real Err is a named notice.
                match self.move_pane_cross_tab(mover, s, target, d, dir) {
                    Ok(()) => {}
                    Err(e) => self.notice(client_id, e.to_string()),
                }
            }
            _ => {
                let Some(tab) = self.viewed_tab_mut(view) else {
                    return;
                };
                match tree::move_leaf(tab, vp, mover, target, dir) {
                    Ok(()) => self.push_layout(true),
                    // An origin drop is a cancel the client should not have
                    // sent; silently accept it rather than scold the
                    // operator for a gesture that, from their side, did
                    // nothing on purpose.
                    Err(tree::MoveError::Origin) => {}
                    Err(e) => self.notice(client_id, e.to_string()),
                }
            }
        }
    }
}
