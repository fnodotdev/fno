//! The exchange picker's jump verdict: one `AnchorJumped` reply, filtered
//! against the pending jump, with the session transcript (the peek overlay)
//! as the miss fallback - the transcript is the anchor that never goes
//! stale. Lives outside client.rs because that file is over the line
//! budget and this change would grow it.

use super::*;

/// Consume one reply. Returns true when a pending jump matched and the
/// view changed - the caller redraws. A reply for a pane this client never
/// jumped (or that arrived after the pending jump cleared) is dropped
/// silently: the picker is the only sender, and it sends one jump per arm.
pub(super) fn reply(view: &mut View, pane_id: u64, jumped: bool) -> bool {
    // The pending jump (session id, pane, ts) filters the reply to the
    // pane it named; anything else is dropped. A miss never jumps: the
    // notice says why and the session transcript opens instead.
    let pending = view
        .messages_board
        .as_ref()
        .and_then(|b| b.pending_jump.as_ref())
        .map(|(sid, pane, _)| (sid.clone(), *pane));
    if let Some((sid, _pane)) = pending.filter(|(_, p)| *p == pane_id) {
        if let Some(b) = view.messages_board.as_mut() {
            b.pending_jump = None;
        }
        if !jumped {
            let idx = view.display_rows().iter().position(|r| {
                matches!(
                    r,
                    DisplayRow::Agent(a)
                        if a.harness_session_id.as_deref() == Some(sid.as_str())
                )
            });
            if let Some(idx) = idx {
                let name = view
                    .layout
                    .agents
                    .iter()
                    .find(|a| a.harness_session_id.as_deref() == Some(sid.as_str()))
                    .map(|a| a.name.clone())
                    .unwrap_or_else(|| sid.get(..8).unwrap_or(&sid).to_string());
                view.open_peek(idx, name);
                view.set_notice(
                    "the pane no longer holds that turn - opened the session transcript"
                        .to_string(),
                );
            } else {
                view.set_notice(
                    "the pane no longer holds that turn - open the session from the sideline to read the reply"
                        .to_string(),
                );
            }
        }
        return true;
    }
    false
}
