//! The link role: one passive client that may drive one pane.
//!
//! `fno mux serve --link` runs on the pane's own machine, and ssh carries only
//! terminal bytes, so this is the same person at another keyboard. It stays
//! `passive`: it births no squad, stays out of the tab size pick, and takes
//! frames through the observer tag beside this one. The observer tag grants no
//! write path. Only this table does, and only for the one pane it names.

use std::collections::BTreeMap;

use super::observers::clean_label;
use super::{Core, CoreMsg, Flow};
use crate::proto::{LinkHello, ObserverHello, ObserverVia};

/// Client id -> the pane it may drive.
#[derive(Debug, Default)]
pub(crate) struct Links {
    pane_of: BTreeMap<u64, u64>,
    /// Client id -> whether the link crossed a machine boundary. The far
    /// door's hello name carries `user from <ip>` exactly when ssh ran it
    /// (`default_name` reads SSH_CONNECTION), so this is the cross-machine
    /// fact the hold gate reads: a remote viewer's typed send into a held
    /// pane refuses like a local `pane send` does, while the person at the
    /// machine keeps typing.
    remote: BTreeMap<u64, bool>,
    /// Client id -> the cell area its terminal reported (hello `rows`/`cols`,
    /// then `ClientMsg::Resize`). Read only by the remote-drive pick
    /// (`link_drive`): the client row's own `dims` stay `(0, 0)` so the local
    /// tab pick and the inset fold never see a link viewer.
    dims: BTreeMap<u64, (u16, u16)>,
    /// Pane -> (driver client, its terminal's area): the remote-drive
    /// override `link_drive` applies over the pane's tile. Cleared when a
    /// local client acts on the tab, when the driver leaves, or when the
    /// drive area changes. A pane id is never reused, so an entry whose pane
    /// died is inert and rides out with its driver's cleanup.
    drive_area: BTreeMap<u64, (u64, (u16, u16))>,
}

impl Links {
    pub(super) fn pane(&self, client: u64) -> Option<u64> {
        self.pane_of.get(&client).copied()
    }

    /// Whether client `id`'s link crossed a machine boundary (its far door
    /// ran over ssh). A local link is the same person at another terminal
    /// on this machine and is never remote.
    pub(super) fn is_remote(&self, client: u64) -> bool {
        self.remote.get(&client).copied().unwrap_or(false)
    }

    /// Record a link client's terminal area. `ClientMsg::Resize` from a
    /// passive client is ignored for geometry (`Client.dims` stays `(0, 0)`),
    /// so this map is the only place a link viewer's size lives.
    pub(super) fn set_dims(&mut self, client: u64, area: (u16, u16)) {
        self.dims.insert(client, area);
    }

    /// The terminal area client `id` reported, when sane. A `(0, 0)` hello
    /// (an older far door, or no terminal) drives nothing.
    pub(super) fn drive_dims(&self, client: u64) -> Option<(u16, u16)> {
        self.dims
            .get(&client)
            .copied()
            .filter(|(rows, cols)| *rows > 0 && *cols > 0)
    }

    /// Take `pane` for driver `client` at `area`. True when this changed the
    /// pane's drive, so the caller re-applies geometry.
    pub(super) fn take_drive(&mut self, client: u64, pane: u64, area: (u16, u16)) -> bool {
        if self.drive_area.get(&pane) == Some(&(client, area)) {
            return false;
        }
        self.drive_area.insert(pane, (client, area));
        true
    }

    /// The pane's active drive: its driver and the area the pane's pty
    /// takes while the drive holds.
    pub(super) fn drive(&self, pane: u64) -> Option<(u64, (u16, u16))> {
        self.drive_area.get(&pane).copied()
    }

    /// The panes currently under a drive.
    pub(super) fn driven_panes(&self) -> Vec<u64> {
        self.drive_area.keys().copied().collect()
    }

    /// The client's reported area changed: store it, and move any drive the
    /// client holds to the new area. True when a drive moved, so the
    /// caller's layout pass applies it.
    pub(super) fn resize_drive(&mut self, client: u64, area: (u16, u16)) -> bool {
        self.dims.insert(client, area);
        let mut moved = false;
        for (_, (driver, stored)) in self.drive_area.iter_mut() {
            if *driver == client && *stored != area {
                *stored = area;
                moved = true;
            }
        }
        moved
    }

    /// Drop every drive in `panes`. True when any existed, so the caller
    /// re-applies geometry.
    pub(super) fn clear_drives(&mut self, panes: &[u64]) -> bool {
        let any = panes.iter().any(|p| self.drive_area.remove(p).is_some());
        any
    }

    /// The client left: drop its dims and every drive it held. Returns the
    /// panes it drove, whose geometry the caller re-applies.
    pub(super) fn forget_client(&mut self, client: u64) -> Vec<u64> {
        self.dims.remove(&client);
        let driven: Vec<u64> = self
            .drive_area
            .iter()
            .filter(|(_, (driver, _))| *driver == client)
            .map(|(pane, _)| *pane)
            .collect();
        for pane in &driven {
            self.drive_area.remove(pane);
        }
        driven
    }

    /// Whether a mutating message from passive client `id` passes the
    /// read-only gate. `Input` carries no pane because `handle_input` sends it
    /// to this client's own pane; every other message names its pane.
    pub(super) fn may_send(&self, id: u64, msg: &CoreMsg) -> bool {
        let Some(own) = self.pane(id) else {
            return false;
        };
        match msg {
            CoreMsg::Input { .. } => true,
            CoreMsg::Mouse { pane, .. }
            | CoreMsg::PaneScroll { pane, .. }
            | CoreMsg::PaneScrollEdge { pane, .. } => *pane == own,
            _ => false,
        }
    }
}

impl Core {
    /// The pane whose terminal modes this client's terminal follows: a link
    /// client's own pane, which its view's focus says nothing about.
    pub(super) fn modes_pane(&self, id: u64, view: (u64, super::TabId)) -> Option<u64> {
        let focus = self.viewed_tab(view).map(|tab| tab.focus);
        self.links.pane(id).or(focus)
    }

    /// Mail's fail-closed target check. A `name@host` address is another
    /// machine's row: this registry cannot answer for it, and the mail command
    /// routes it.
    pub(super) fn mail_target(&self, name: &str) -> Result<(), String> {
        if name.contains('@') {
            return Ok(());
        }
        self.resolve_lifecycle_target(name, None).map(|_| ())
    }

    /// Tag passive client `id` as the driver of one pane, after checking the
    /// pane still holds the child the far door read. A refused hello leaves the
    /// client an untagged observer that can write nothing, so the far door
    /// names the refusal from its own check rather than a reply here.
    pub(super) fn link(&mut self, id: u64, hello: LinkHello) -> Flow {
        let child = self
            .panes
            .get(&hello.pane)
            .and_then(|entry| entry.pty.child_pid());
        if !self.is_passive(id) || child.map(|pid| format!("pid:{pid}")) != Some(hello.identity) {
            crate::mux_log!(warn; "fno mux: client {id} link to pane {} refused", hello.pane);
            return Flow::Continue;
        }
        let live: Vec<u64> = self.clients.iter().map(|c| c.id).collect();
        self.links.pane_of.retain(|cid, _| live.contains(cid));
        self.links.remote.retain(|cid, _| live.contains(cid));
        self.links.dims.retain(|cid, _| live.contains(cid));
        let remote = hello.name.contains(" from ");
        self.links.pane_of.insert(id, hello.pane);
        self.links.remote.insert(id, remote);
        self.links.set_dims(id, (hello.rows, hello.cols));
        self.observe(
            id,
            ObserverHello {
                name: clean_label(&hello.name),
                pane: hello.pane,
                via: ObserverVia::Ssh,
                frames: true,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::empty_core;

    #[test]
    fn mail_target_passes_a_remote_address_and_refuses_an_unknown_local_name() {
        let core = empty_core();
        assert_eq!(core.mail_target("ines@u"), Ok(()));
        assert!(core.mail_target("ines").is_err());
    }
}
