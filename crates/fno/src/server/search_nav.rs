//! The pane's shared-viewport search verbs (v12), moved here under the
//! file-budget ratchet: opening, stepping and clearing a free-text find
//! over a pane's server-side history, and the initiator-only counter
//! reply. The exchange picker's anchor jump (the other shared-viewport
//! verb) lives next door in pane_anchors.rs.

use super::*;

impl Core {
    /// Apply one in-scrollback search op to `pane` (v12). Open/step mutate
    /// the shared scroll + highlight (broadcast so every co-viewer tracks, tmux
    /// precedent) and reply a `SearchResult` counter to the initiator ONLY; clear
    /// (idempotent) drops the highlight for all. A missing pane, or a step with no
    /// active search, gets a one-line notice to the requester, never a silent
    /// no-op or a panic. Only match counts + coordinates ever leave the server.
    fn search_nav(&mut self, client_id: u64, pane: u64, op: SearchOp) {
        match op {
            SearchOp::Open(query) => {
                match self.panes.get_mut(&pane).map(|e| e.vt.search_open(&query)) {
                    Some((total, current)) => {
                        self.broadcast_pane(pane);
                        self.send_search_result(client_id, pane, total, current);
                    }
                    None => self.notice(client_id, "pane not found"),
                }
            }
            SearchOp::Step(dir) => match self.panes.get_mut(&pane).map(|e| e.vt.search_step(dir)) {
                Some(Some((total, current))) => {
                    self.broadcast_pane(pane);
                    self.send_search_result(client_id, pane, total, current);
                }
                Some(None) => self.notice(client_id, "no active search"),
                None => self.notice(client_id, "pane not found"),
            },
            // Idempotent: a no-match search_open already dropped the state while
            // the client still sends Clear on Esc, so guarding on has_search here
            // would misfire "no active search" on the common no-match-then-Esc.
            SearchOp::Clear => match self.panes.get_mut(&pane) {
                Some(e) => {
                    e.vt.search_clear();
                    self.broadcast_pane(pane);
                }
                None => self.notice(client_id, "pane not found"),
            },
        }
    }

    /// Reply the initiator-only `SearchResult` counter (v12). Reliable, like a
    /// `Copy`: the `[i/n]` chrome is the only signal the search landed, and a
    /// wedged reliable channel is a dead client (same teardown policy as
    /// [`Core::send_copy`]).
    fn send_search_result(&mut self, client_id: u64, pane_id: u64, total: u32, current: u32) {
        let Some(c) = self.clients.iter().find(|c| c.id == client_id) else {
            return;
        };
        if c.reliable_tx
            .try_send(ServerMsg::SearchResult {
                pane_id,
                total,
                current,
            })
            .is_err()
        {
            crate::mux_log!(error;
                "fno mux: client {client_id} reliable channel wedged on SearchResult; dropping it"
            );
            self.clients.retain(|c| c.id != client_id);
            self.push_layout(true);
        }
    }
}
