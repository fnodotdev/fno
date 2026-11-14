//! Landing the search counter reply: the pane filter, the result counter,
//! and the empty-result BEL. Moved out of client.rs, which is over the
//! line budget; this is the code the exchange-picker jump PR touched (the
//! same navigation-reply family as client/anchor_jump.rs).

use super::*;

/// Consume one `SearchResult` reply. Returns true when the counter landed
/// and the view changed - the caller redraws. A reply for a different
/// pane's search, or for a query still being typed (not submitted), is
/// dropped so a stale counter never paints onto the wrong pane.
pub(super) fn result(view: &mut View, pane_id: u64, total: u32, current: u32) -> bool {
    if let Some(sv) = view
        .search
        .as_mut()
        .filter(|sv| sv.pane == pane_id && sv.submitted)
    {
        sv.result = Some((total, current));
        // BEL only while the search is still open: a late zero-result
        // reply arriving after a local Esc must not sound a bell.
        if total == 0 {
            let _ = raw_out(b"\x07");
        }
        return true;
    }
    false
}
