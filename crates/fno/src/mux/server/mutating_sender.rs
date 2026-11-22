//! Which sender of a `CoreMsg` mutates shared state? The read-only
//! enforcement at the server drops any PTY/tree-mutating message from an
//! observer (passive) client, whatever sends it. The web bridge holds no
//! write half so it never sends these; this makes the guarantee hold for
//! ANY (0,0) attacher, not by the bridge's discipline alone. Resize is
//! already neutralized (its dims are ignored for a passive client);
//! Detach/Gone/Query/etc. pass. A link client passes for its own pane.

use super::CoreMsg;

/// The client whose message mutates PTY or shared state, `None` when the
/// message carries no client mutation (or no client at all).
pub(super) fn mutating_sender(msg: &CoreMsg) -> Option<u64> {
    match msg {
        CoreMsg::Input { id, .. } => Some(*id),
        CoreMsg::Command { id, .. } => Some(*id),
        CoreMsg::Mouse { id, .. } => Some(*id),
        CoreMsg::BlockNav { id, .. } => Some(*id),
        // DraftHint opens the pane's draft latch, exactly the shared
        // guard state a keystroke moves; a read-only observer must
        // never hold deliveries off a pane.
        CoreMsg::DraftHint { id, .. } => Some(*id),
        // Search and PaneScroll/PaneScrollEdge move the shared scroll for
        // every co-viewer, the same shared-state mutation as BlockNav; a
        // read-only observer must never move anyone's viewport.
        CoreMsg::Search { id, .. } => Some(*id),
        CoreMsg::PaneScroll { id, .. } => Some(*id),
        CoreMsg::PaneScrollEdge { id, .. } => Some(*id),
        // AnchorJump moves the shared scroll + selection exactly like
        // Search does; same read-only gate.
        CoreMsg::AnchorJump { id, .. } => Some(*id),
        // PaneAnswer injects a keystroke into a pane PTY; a
        // read-only observer must never reach it (same class as Input).
        CoreMsg::PaneAnswer { id, .. } => Some(*id),
        // DispatchNext spawns a real worker pane; a passive
        // web-bridge observer must never start work (Invariant).
        // DispatchResult is NOT gated here - it originates from the trusted
        // off-loop task, not a client. AgentLaunch (v83) spawns a real
        // worker the same way, so it gates identically.
        CoreMsg::DispatchNext { id, .. } => Some(*id),
        CoreMsg::AgentLaunch { id, .. } => Some(*id),
        // ShellRun starts a shell command and types into it, so a
        // read-only observer must never send one.
        CoreMsg::ShellRun(id, _) => Some(*id),
        CoreMsg::PaneInput(request) => Some(request.id),
        _ => None,
    }
}
