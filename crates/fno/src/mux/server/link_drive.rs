//! The remote-drive pane area: a cross-machine link viewer's action takes
//! the pane's pty to their terminal's cell area, and the owning machine's
//! next action reflows it back.
//!
//! The tab's own size pick (`window_size.rs`) stays local-only: a phone-sized
//! observer can never shrink a real client's PTY. The drive is one pane's
//! override instead, applied where tiles are applied (`apply_pane_sizes`),
//! and only under the `latest` policy - the same "who acted last decides"
//! rule local co-viewers share. Under `smallest` / `largest` a driven pane
//! keeps its tile, as before. Only a REMOTE link drives; a local link is the
//! same person at another terminal on this machine and never resizes a pane.

use std::collections::HashSet;

use super::{Core, TabId};

impl Core {
    /// The tabs a layout pass must size beyond its viewers: a driven pane's
    /// tab (the drive holds the pane; the link client's passive anchor need
    /// not name its tab) and a clearing drive's queued tab.
    pub(super) fn drive_tabs(&mut self, viewed: &mut HashSet<TabId>) {
        for pane in self.links.driven_panes() {
            if let Some(tid) = self.tab_of_pane(pane) {
                viewed.insert(tid);
            }
        }
        for tid in self.links.drain_reflow_tabs() {
            viewed.insert(tid);
        }
    }

    /// The acting client's drive side: a cross-machine link drive takes its
    /// pane to the driver's own cell area; a local actor reflows driven
    /// panes in their tab back to the tiles.
    pub(super) fn acting_drive_hook(&mut self, id: u64, tab: Option<TabId>) {
        if self.is_passive(id) {
            self.drive_pane_from_link(id);
        } else if let Some(tid) = tab {
            self.reflow_driven_panes(tid);
        }
    }

    /// A passive client's Resize feeds the remote drive, never the local
    /// pick: the caller's trailing push_layout applies a moved drive. A
    /// non-link observer's resize stays ignored.
    pub(super) fn link_resize(&mut self, id: u64, rows: u16, cols: u16) {
        if self.links.pane(id).is_some() {
            self.links.resize_drive(id, (rows, cols));
        }
    }

    /// The client left: drop its dims and drives, and queue the tabs it
    /// drove for the restoring pass.
    pub(super) fn link_gone(&mut self, id: u64) {
        let driven = self.links.forget_client(id);
        for pane in &driven {
            if let Some(tid) = self.tab_of_pane(*pane) {
                self.links.queue_reflow_tab(tid);
            }
        }
    }

    /// Client `id` - a remote link viewer - acted in its pane: take the
    /// pane's pty to the area their terminal reported. A repeat with the
    /// same area changes nothing, so every keystroke after the first is
    /// free. Returns nothing: the re-applied geometry is the outcome.
    pub(super) fn drive_pane_from_link(&mut self, id: u64) {
        if !self.sizing.is_latest() {
            return;
        }
        let Some(pane) = self.links.pane(id) else {
            return;
        };
        if !self.links.is_remote(id) {
            return;
        }
        let Some(area) = self.links.drive_dims(id) else {
            return;
        };
        if self.links.take_drive(id, pane, area) {
            self.push_layout(true);
        }
    }

    /// A local client acted on their tab: every drive on a pane of that tab
    /// clears, and `push_layout` re-tiles the panes back. A drive on a pane
    /// of another tab stays: its driver is still in it.
    pub(super) fn reflow_driven_panes(&mut self, tid: TabId) {
        let driven = self.links.driven_panes();
        let in_tab: Vec<u64> = driven
            .into_iter()
            .filter(|pane| self.tab_of_pane(*pane) == Some(tid))
            .collect();
        if self.links.clear_drives(&in_tab) {
            // The panes re-tile with this pass even if the actor is the
            // only viewer - queueing is redundant there but keeps a drive
            // cleared by the driver's own departure covered.
            self.links.queue_reflow_tab(tid);
            self.push_layout(true);
        }
    }

    /// The tab whose pane tree names `pane`.
    pub(super) fn tab_of_pane(&self, pane: u64) -> Option<TabId> {
        for squad in &self.session.squads {
            for tab in &squad.tabs {
                if super::tree::leaves(&tab.root).contains(&pane) {
                    return Some(tab.id);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use tokio::sync::{mpsc, Notify};

    use super::super::tests::{empty_core, leaf_tab};
    use super::super::window_size::WindowSize;
    use super::super::{Client, CoreMsg, Modes, TabSizing};
    use super::*;
    use crate::proto::LinkHello;

    fn client(id: u64, view_tab: TabId, dims: (u16, u16), passive: bool) -> Client {
        Client {
            id,
            reliable_tx: mpsc::channel(1).0,
            dirty: Arc::default(),
            notify: Arc::new(Notify::new()),
            synced_modes: Modes::default(),
            view: (1, view_tab),
            visible: HashSet::new(),
            dims,
            passive,
            last_press: None,
        }
    }

    /// A one-tab squad (id 1) whose single tab (id 5) holds one real shell
    /// pane, sized 24x80, under the `latest` window-size policy.
    fn drive_core() -> (Core, u64) {
        let mut core = empty_core();
        core.sizing = TabSizing::with_policy(WindowSize::Latest);
        core.shells = vec!["/bin/cat".into()];
        core.next_pane_id = 100;
        let p = core.spawn_pane(24, 80, "/a").unwrap();
        core.session
            .add_squad(1, vec!["/a".into()], Some("sq".into()), leaf_tab(5, p));
        core.sizing.areas.insert(5, (24, 80));
        (core, p)
    }

    fn link_remote(core: &mut Core, id: u64, pane: u64, area: (u16, u16)) {
        link_remote_on(core, id, pane, 5, area);
    }

    fn link_remote_on(core: &mut Core, id: u64, pane: u64, view_tab: TabId, area: (u16, u16)) {
        core.clients.push(client(id, view_tab, (0, 0), true));
        let child = core.panes[&pane].pty.child_pid().unwrap();
        let hello = LinkHello {
            name: "me from 10.0.0.9".into(),
            pane,
            identity: format!("pid:{child}"),
            rows: area.0,
            cols: area.1,
        };
        core.handle(CoreMsg::Link(id, hello));
    }

    fn typed(core: &mut Core, id: u64) {
        core.handle(CoreMsg::Input {
            id,
            bytes: b"x".to_vec(),
        });
    }

    fn pane_size(core: &Core, pane: u64) -> (u16, u16) {
        core.panes[&pane].requested_size
    }

    /// The geometry pass only covers viewed tabs; a drive on a pane whose
    /// tab nobody views (the link's passive anchor names another tab) still
    /// applies.
    #[test]
    fn a_drive_applies_when_the_driven_pane_is_not_on_the_anchor_tab() {
        let (mut core, p) = drive_core();
        let q = core.spawn_pane(24, 80, "/a").unwrap();
        core.session.squad_mut(1).unwrap().tabs.push(leaf_tab(6, q));
        core.sizing.areas.insert(6, (24, 80));
        link_remote_on(&mut core, 2, p, 6, (40, 120));
        typed(&mut core, 2);
        assert_eq!(
            pane_size(&core, p),
            (40, 120),
            "the drive reaches the pane's own tab"
        );
        core.reap_pane(p);
        core.reap_pane(q);
    }

    #[test]
    fn a_remote_drive_takes_the_pane_to_the_link_terminal() {
        let (mut core, p) = drive_core();
        let tile = pane_size(&core, p);
        link_remote(&mut core, 2, p, (40, 120));
        typed(&mut core, 2);
        assert_eq!(pane_size(&core, p), (40, 120), "the drive applies");
        assert_ne!(tile, (40, 120));
        typed(&mut core, 2);
        assert_eq!(
            pane_size(&core, p),
            (40, 120),
            "a repeat keystroke changes nothing"
        );
        core.reap_pane(p);
    }

    #[test]
    fn the_owner_action_reflows_the_pane_back_to_its_tile() {
        let (mut core, p) = drive_core();
        let tile = pane_size(&core, p);
        link_remote(&mut core, 2, p, (40, 120));
        typed(&mut core, 2);
        assert_eq!(pane_size(&core, p), (40, 120));
        core.clients.push(client(3, 5, (24, 80), false));
        typed(&mut core, 3);
        assert_eq!(
            pane_size(&core, p),
            tile,
            "the owning machine's action reflows the tile"
        );
        typed(&mut core, 2);
        assert_eq!(
            pane_size(&core, p),
            (40, 120),
            "the remote viewer drives again"
        );
        core.reap_pane(p);
    }

    #[test]
    fn a_local_link_never_drives() {
        let (mut core, p) = drive_core();
        let tile = pane_size(&core, p);
        core.clients.push(client(2, 5, (0, 0), true));
        let child = core.panes[&p].pty.child_pid().unwrap();
        let hello = LinkHello {
            name: "me".into(),
            pane: p,
            identity: format!("pid:{child}"),
            rows: 40,
            cols: 120,
        };
        core.handle(CoreMsg::Link(2, hello));
        typed(&mut core, 2);
        assert_eq!(pane_size(&core, p), tile, "same machine, no takeover");
        core.reap_pane(p);
    }

    #[test]
    fn a_drive_survives_an_unrelated_layout_push() {
        let (mut core, p) = drive_core();
        link_remote(&mut core, 2, p, (40, 120));
        typed(&mut core, 2);
        core.clients.push(client(3, 5, (30, 100), false));
        core.handle(CoreMsg::Resize {
            id: 3,
            rows: 30,
            cols: 100,
        });
        assert_eq!(
            pane_size(&core, p),
            (40, 120),
            "another client's resize does not stomp the drive"
        );
        core.reap_pane(p);
    }

    #[test]
    fn a_link_resize_moves_the_drive() {
        let (mut core, p) = drive_core();
        link_remote(&mut core, 2, p, (40, 120));
        typed(&mut core, 2);
        core.handle(CoreMsg::Resize {
            id: 2,
            rows: 50,
            cols: 140,
        });
        assert_eq!(
            pane_size(&core, p),
            (50, 140),
            "the driver's new terminal area applies"
        );
        core.reap_pane(p);
    }

    #[test]
    fn the_driver_leaving_reflows() {
        let (mut core, p) = drive_core();
        let tile = pane_size(&core, p);
        link_remote(&mut core, 2, p, (40, 120));
        typed(&mut core, 2);
        assert_eq!(pane_size(&core, p), (40, 120));
        core.handle(CoreMsg::Gone(2));
        assert_eq!(pane_size(&core, p), tile, "a vanished driver holds nothing");
        core.reap_pane(p);
    }

    #[test]
    fn a_zero_area_hello_drives_nothing() {
        let (mut core, p) = drive_core();
        let tile = pane_size(&core, p);
        // An older far door sends no rows/cols: serde defaults to (0, 0).
        link_remote(&mut core, 2, p, (0, 0));
        typed(&mut core, 2);
        assert_eq!(pane_size(&core, p), tile, "no reported area, no takeover");
        core.reap_pane(p);
    }

    #[test]
    fn a_non_latest_policy_never_drives() {
        let mut core = empty_core();
        core.shells = vec!["/bin/cat".into()];
        core.next_pane_id = 100;
        let p = core.spawn_pane(24, 80, "/a").unwrap();
        core.session
            .add_squad(1, vec!["/a".into()], Some("sq".into()), leaf_tab(5, p));
        core.sizing.areas.insert(5, (24, 80));
        let tile = pane_size(&core, p);
        link_remote(&mut core, 2, p, (40, 120));
        typed(&mut core, 2);
        assert_eq!(
            pane_size(&core, p),
            tile,
            "smallest/largest keep the tile, as before"
        );
        core.reap_pane(p);
    }
}
