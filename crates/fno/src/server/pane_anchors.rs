//! Per-pane prompt anchors (v127): where a submitted prompt sits on the
//! pane's row space, stamped at submit time so the exchange picker can land
//! the pane on the exact row the prompt echoed on.
//!
//! The stamp is the [`crate::vt::Pane::cursor_abs_row`] plus the row's text,
//! taken the moment a submit-terminated burst is written to the PTY - the
//! grid still shows the pre-submit state, so the cursor row IS the echoed
//! prompt line. The jump joins a transcript turn to its anchor by submit
//! time (`ts_ms`, epoch milliseconds): the harness appends its user row at
//! submit, so the two stamps agree within seconds when the agent was idle
//! and within minutes when it queued the prompt behind other work - hence
//! the generous [`JOIN_TOLERANCE_MS`] and the nearest-anchor pick.
//!
//! Both failure modes of a frozen `abs_row` - eviction past the scrollback
//! cap and a TUI redraw that rebuilt the row space (resize, compaction,
//! clear) - refuse at jump time ([`crate::vt::Pane::anchor_jump`]) instead of
//! landing on a row that no longer holds the prompt; the client then opens
//! the session transcript, which never goes stale.

use super::*;
use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

/// One submitted prompt's landing row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneAnchor {
    /// Submit instant, epoch ms. The join key to the transcript row's
    /// `timestamp`.
    pub ts_ms: u64,
    /// The cursor's absolute row at submit (the [`anchor_row`](crate::vt::Pane)
    /// scheme: grid line + history size).
    pub abs_row: i64,
    /// The captured row's text, the jump's identity check.
    pub text: String,
}

/// Anchors kept per pane, oldest first. A ring, because a pane's scrollback
/// itself is a ring: past this many submits the oldest anchors point at rows
/// likely evicted anyway, and the join only ever asks for recent turns.
pub(crate) const MAX_PER_PANE: usize = 64;

/// How far a transcript turn's stamp and an anchor's stamp may drift and
/// still name the same submit. Covers a prompt the harness queued behind
/// other work before appending its row; a genuine jump never lands on a
/// prompt submitted this much earlier by accident, because the pick takes
/// the NEAREST anchor and a miss is answered, not guessed.
pub(crate) const JOIN_TOLERANCE_MS: u64 = 15 * 60 * 1000;

/// The stamp instant, epoch ms.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Whether a written burst ends in a submit the way prompts do: the human
/// path's CR rule, widened with a trailing LF for one-shot `pane send`
/// payloads, which arrive as `text + newline` in a single write.
pub(crate) fn ends_in_submit(bytes: &[u8]) -> bool {
    super::human_input::is_submit(bytes) || bytes.last() == Some(&b'\n')
}

impl Core {
    /// Stamp one anchor for `pane` when `bytes` ends in a submit. A no-op
    /// for every other burst (keystrokes, control sequences). The pane's
    /// grid is read BEFORE the child drains the write, so the cursor still
    /// sits on the echoed prompt line; a pane that is gone (a submit racing
    /// its exit) simply stamps nothing.
    pub(super) fn stamp_anchor(&mut self, pane: u64, bytes: &[u8]) {
        if !ends_in_submit(bytes) {
            return;
        }
        let Some(entry) = self.panes.get(&pane) else {
            return;
        };
        let abs_row = entry.vt.cursor_abs_row();
        let text = entry.vt.row_text(abs_row).unwrap_or_default();
        let ring = self.anchors.entry(pane).or_default();
        if ring.len() >= MAX_PER_PANE {
            ring.pop_front();
        }
        ring.push_back(PaneAnchor {
            ts_ms: now_ms(),
            abs_row,
            text,
        });
    }

    /// Land the exchange picker's jump: the anchor nearest `ts_ms` within
    /// [`JOIN_TOLERANCE_MS`], verified against the live grid, highlighted and
    /// scrolled to (shared, like a search jump). The initiator alone learns
    /// the verdict; a miss means the client falls back to the transcript.
    pub(super) fn anchor_jump(&mut self, client_id: u64, pane: u64, ts_ms: u64) {
        let jumped = self
            .anchors
            .get(&pane)
            // Nearest by |stamp - ts|, ties to the newer anchor (a re-send of
            // the same prompt stamps twice; the newer row is the one shown).
            .and_then(|ring| {
                ring.iter()
                    .rev()
                    .min_by_key(|a| a.ts_ms.abs_diff(ts_ms))
                    .cloned()
            })
            .filter(|a| a.ts_ms.abs_diff(ts_ms) <= JOIN_TOLERANCE_MS)
            .and_then(|a| {
                self.panes.get_mut(&pane).and_then(|entry| {
                    entry
                        .vt
                        .anchor_jump(a.abs_row, &a.text)
                        .then_some(())
                        .map(|_| a)
                })
            })
            .is_some();
        if jumped {
            self.broadcast_pane(pane);
        }
        self.send_anchor_result(client_id, pane, jumped);
    }

    /// Reply the initiator-only `AnchorJumped` verdict, with the same
    /// reliable-channel discipline as [`Core::send_search_result`]: a wedged
    /// receiver is a dead client and is dropped.
    fn send_anchor_result(&mut self, client_id: u64, pane_id: u64, jumped: bool) {
        let Some(c) = self.clients.iter().find(|c| c.id == client_id) else {
            return;
        };
        if c.reliable_tx
            .try_send(ServerMsg::AnchorJumped { pane_id, jumped })
            .is_err()
        {
            crate::mux_log!(error;
                "fno mux: client {client_id} reliable channel wedged on AnchorJumped; dropping it"
            );
            self.clients.retain(|c| c.id != client_id);
            self.push_layout(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The submit shape, both spellings; anything else stamps nothing.
    #[test]
    fn submit_shapes() {
        assert!(ends_in_submit(b"hello\r"));
        assert!(ends_in_submit(b"hello\n"));
        assert!(!ends_in_submit(b"hello"));
        assert!(!ends_in_submit(b""));
        assert!(!ends_in_submit(b"\x1b[200~pasted\r\x1b[201~"));
    }
}
