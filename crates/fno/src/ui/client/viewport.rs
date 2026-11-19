//! The content viewport the server tiles for: one announcer for the width
//! changes a terminal winch cannot see. A keyboard door that moves the
//! chrome (panel toggles, density presets, the board panel) changes
//! [`View::content_dims`] with no resize event; a stale area crops the
//! rightmost pane at the canvas edge with its border clipped.

use super::*;

impl View {
    /// The content dims when they drifted from `sent`, else `None`.
    pub(super) fn drifted(&self, sent: (u16, u16)) -> Option<(u16, u16)> {
        let dims = self.content_dims();
        (dims != sent).then_some(dims)
    }
}

/// Send the viewport the server should tile for. Absolute, so repeats are
/// idempotent and a missed one self-heals on the next.
pub(super) async fn announce(
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
    dims: (u16, u16),
) -> Result<(), String> {
    write_msg(
        sock_w,
        &ClientMsg::Resize {
            rows: dims.0,
            cols: dims.1,
        },
    )
    .await
    .map_err(|e| format!("resize send failed: {e}"))
}

/// The winch arm: re-read the terminal, re-clamp the scroll window,
/// announce, and answer the dims to remember as sent. A size read that
/// fails keeps the previous viewport.
pub(super) async fn winch_resize(
    view: &mut View,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
    sent: (u16, u16),
) -> Result<(u16, u16), String> {
    if let Ok((cols, rows)) = terminal::size() {
        view.term = (rows, cols);
        // A shorter terminal shrinks the scroll window; re-clamp so the
        // offset never scrolls past the last row.
        view.clamp_sideline_scroll();
        let dims = view.content_dims();
        announce(sock_w, dims).await?;
        return Ok(dims);
    }
    Ok(sent)
}
