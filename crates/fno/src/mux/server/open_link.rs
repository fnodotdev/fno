//! What does a click or a hover on a pane cell mean, and who gets the answer?
//!
//! One lookup ([`Core::click_target`]) answers both, so they cannot disagree.
//! The lookup reads the grid and stats. Anything that reads a file runs off
//! the core loop and answers over the asking client's reliable channel.

use super::image_refs::{self, ImageSource};
use super::pasted_images::{PaneSession, Stores};
use super::{route_mouse, Core, MouseAction, ServerMsg};
use crate::proto::{MouseButton, MouseEvent, MouseKind};

/// What one cell opens.
#[derive(Debug)]
enum ClickTarget {
    /// An fno token (`@handle`, a mail id). The mux's on every pane.
    Token(String),
    /// A local image reference. The mux's on every pane too.
    Image(ImageSource, Option<PaneSession>),
    /// Any other link. An Alt-click on a pane whose app owns the mouse.
    Link(String),
}

impl ClickTarget {
    /// Whether the mux takes this click from an app that owns the mouse.
    fn claims(&self, alt: bool) -> bool {
        match self {
            Self::Token(_) | Self::Image(..) => true,
            Self::Link(_) => alt,
        }
    }
}

/// A claimed left press, waiting for its release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ClickClaim {
    pub(super) row: u16,
    pub(super) col: u16,
    pub(super) client: u64,
    pub(super) alt: bool,
}

impl Core {
    /// The target under viewport cell `(row, col)` of `pane`, with its visible
    /// cells. The pane's LIVE cwd (kernel cwd while the child pid resolves,
    /// else its spawn cwd) is what relative candidates resolve against.
    fn click_target(
        &self,
        pane: u64,
        row: u16,
        col: u16,
    ) -> Option<(ClickTarget, Vec<(u16, u16)>)> {
        let entry = self.panes.get(&pane)?;
        let cwd = crate::pane_cwd::live_or_spawn(entry.pty.child_pid(), &entry.cwd);
        let link = entry.vt.link_span(row, col, &cwd);
        if let Some(span) = &link {
            if crate::link::is_fno_uri(&span.uri) {
                return Some((ClickTarget::Token(span.uri.clone()), span.cells.clone()));
            }
            if crate::link::is_openable(&span.uri) {
                return Some((ClickTarget::Link(span.uri.clone()), span.cells.clone()));
            }
        }
        // A remote portal prints another machine's paths.
        if !self.is_remote_portal(pane) {
            let image = entry
                .vt
                .link_window(row, col)
                .and_then(|window| image_refs::find_here(&window, &cwd));
            if let Some(image) = image {
                let session = match image.source {
                    ImageSource::Pasted(_) => Some(self.pane_session(pane)?),
                    ImageSource::File(_) => None,
                };
                return Some((ClickTarget::Image(image.source, session), image.cells));
            }
        }
        let span = link?;
        // An image name links only through the resolver's checks.
        let unverified = crate::link::file_uri_parts(&span.uri)
            .is_some_and(|(path, _)| image_refs::has_image_ext(&path));
        (!unverified).then_some((ClickTarget::Link(span.uri), span.cells))
    }

    /// Send a validated link only to the client that clicked it.
    pub(super) fn send_open_link(&mut self, client_id: u64, url: String) {
        if !(crate::link::is_openable(&url)
            || crate::link::is_sender_uri(&url)
            || crate::link::is_message_uri(&url)
            || crate::link::is_handle_uri(&url)
            || crate::link::is_file_uri(&url))
        {
            return;
        }
        let Some(c) = self.clients.iter().find(|c| c.id == client_id) else {
            return;
        };
        if c.reliable_tx.try_send(ServerMsg::OpenLink { url }).is_err() {
            crate::mux_log!(error;
                "fno mux: client {client_id} reliable channel wedged on OpenLink; dropping it"
            );
            self.clients.retain(|c| c.id != client_id);
            self.push_layout(true);
        }
    }

    fn open_target(&mut self, client_id: u64, target: ClickTarget) {
        let (source, session) = match target {
            ClickTarget::Token(url) | ClickTarget::Link(url) => {
                return self.send_open_link(client_id, url)
            }
            ClickTarget::Image(source, session) => (source, session),
        };
        let Some(client) = self.clients.iter().find(|c| c.id == client_id) else {
            return;
        };
        let (tx, notify) = (client.reliable_tx.clone(), client.notify.clone());
        let pasted = match &source {
            ImageSource::Pasted(paste) => Some(paste.number),
            ImageSource::File(_) => None,
        };
        tokio::spawn(async move {
            let found = tokio::task::spawn_blocking(move || {
                image_refs::resolve(&source, session.as_ref(), &Stores::host())
            })
            .await;
            let msg = match found {
                Ok(Ok(path)) => ServerMsg::OpenLink {
                    url: match pasted {
                        Some(number) => crate::link::pasted_uri(&path, number),
                        None => crate::link::file_uri(&path, None),
                    },
                },
                Ok(Err(error)) => ServerMsg::Notice {
                    text: error.to_string(),
                },
                Err(error) => ServerMsg::Notice {
                    text: format!("image lookup stopped: {error}"),
                },
            };
            // A send fails only for a client that has already gone.
            if tx.send(msg).await.is_ok() {
                notify.notify_one();
            }
        });
    }

    /// A plain click in a pane the mux owns the mouse of.
    pub(super) fn open_click(&mut self, client_id: u64, pane: u64, row: u16, col: u16) {
        if let Some((target, _)) = self.click_target(pane, row, col) {
            self.open_target(client_id, target);
        }
    }

    /// The claim half of a click in a pane whose app owns the mouse.
    ///
    /// A plain LEFT press on an fno token or an image reference claims the
    /// pair, and so does an ALT press on any link: alt is the gesture because
    /// SGR reports it and cmd has no SGR bit. The matching release consumes
    /// the claim and opens on the same cell. A release on any other cell is
    /// swallowed with it (the app saw no press, so it gets no torn tail). A
    /// drag off a claimed press replays the press ahead of the drag, so an
    /// app-side selection starting on a link still begins whole. Claims are
    /// PER PANE, so one client's events never consume another client's pair.
    /// Every unclaimed event forwards byte-identical (return `false`).
    pub(super) fn claim_click(&mut self, client_id: u64, pane: u64, event: &MouseEvent) -> bool {
        match event.kind {
            MouseKind::Press(MouseButton::Left) => {
                // A second press means the previous gesture's release never
                // came (dragged off-pane, or dropped client-side on chrome).
                self.click_claims.remove(&pane);
                let claimed = self
                    .click_target(pane, event.row, event.col)
                    .is_some_and(|(target, _)| target.claims(event.alt));
                if claimed {
                    let claim = ClickClaim {
                        row: event.row,
                        col: event.col,
                        client: client_id,
                        alt: event.alt,
                    };
                    self.click_claims.insert(pane, claim);
                }
                claimed
            }
            MouseKind::Release(MouseButton::Left) => {
                let Some(claim) = self.click_claims.get(&pane).copied() else {
                    return false;
                };
                if claim.client != client_id {
                    return false;
                }
                self.click_claims.remove(&pane);
                if (claim.row, claim.col) == (event.row, event.col) {
                    if let Some((target, _)) = self.click_target(pane, event.row, event.col) {
                        // The grid may have changed under a held button.
                        if target.claims(claim.alt) {
                            self.open_target(client_id, target);
                        }
                    }
                }
                true
            }
            MouseKind::Drag(MouseButton::Left) => {
                let Some(claim) = self.click_claims.get(&pane).copied() else {
                    return false;
                };
                if claim.client == client_id {
                    self.click_claims.remove(&pane);
                    // The replay carries the CLAIMED press's cell and alt bit:
                    // a selection must begin where it began.
                    let press = MouseEvent {
                        row: claim.row,
                        col: claim.col,
                        kind: MouseKind::Press(MouseButton::Left),
                        alt: claim.alt,
                    };
                    let bytes = super::sgr_mouse_bytes(&press);
                    if let Some(entry) = self.panes.get(&pane) {
                        // A failed write is a closing pty, whose selection is moot.
                        let _ = entry.pty.write_input(&bytes);
                    }
                }
                false
            }
            _ => {
                // A non-press event on the pane breaks its own pending pair.
                self.click_claims.remove(&pane);
                false
            }
        }
    }

    /// (v56, hover affordance) One link-span lookup for the requesting client
    /// only. The guards mirror the click path: the pane must be in the
    /// requester's live view, and the click must be the mux's. That is any
    /// link where the mux owns the mouse, and an image reference everywhere.
    /// Coordinates only, never text. Every miss answers an EMPTY cell list so
    /// the client clears the underline. An image file answers after its bytes
    /// are checked, and the client drops a late answer by `seq`. No pane
    /// state changes and no broadcast: co-viewers never see another's hover.
    pub(super) fn link_hover(&mut self, client_id: u64, pane: u64, row: u16, col: u16, seq: u64) {
        let wedged = {
            let Some(client) = self.clients.iter().find(|c| c.id == client_id) else {
                return;
            };
            let modes = self.panes.get(&pane).map(|entry| entry.vt.modes());
            let hit = match modes {
                Some(modes) if client.visible.contains(&pane) => {
                    let mux_owns = route_mouse(modes, MouseKind::Release(MouseButton::Left))
                        == MouseAction::SelectRelease;
                    self.click_target(pane, row, col)
                        .filter(|(target, _)| mux_owns || matches!(target, ClickTarget::Image(..)))
                }
                _ => None,
            };
            let reply = move |cells| ServerMsg::LinkHover {
                pane_id: pane,
                seq,
                cells,
            };
            match hit {
                Some((ClickTarget::Image(ImageSource::File(path), _), cells)) => {
                    let tx = client.reliable_tx.clone();
                    tokio::spawn(async move {
                        let readable = matches!(
                            tokio::task::spawn_blocking(move || image_refs::verify(&path)).await,
                            Ok(Ok(()))
                        );
                        let cells = if readable { cells } else { Vec::new() };
                        // A send fails only for a client that has already gone.
                        tx.send(reply(cells)).await.ok();
                    });
                    false
                }
                hit => {
                    let cells = hit.map(|(_, cells)| cells).unwrap_or_default();
                    client.reliable_tx.try_send(reply(cells)).is_err()
                }
            }
        };
        if wedged {
            crate::mux_log!(error;
                "fno mux: client {client_id} reliable channel wedged on LinkHover; dropping it"
            );
            self.clients.retain(|c| c.id != client_id);
            self.push_layout(true);
        }
    }
}
