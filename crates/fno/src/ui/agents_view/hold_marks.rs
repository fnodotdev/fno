//! The hold marks: a row whose session has a live record in the fno-mail
//! hold store reads `[DND]`, and one the conversation rules armed reads
//! `[HELD]`, never the `[DND]` a lead reads as a hold it armed itself. A
//! read-time projection over the store the hold writers already use, so the
//! badge follows the hold's real lifecycle with nothing new to keep
//! consistent. The registry's `bus-only` value (the mail-only capability)
//! sets `dnd` upstream in `derive_rows`.

use super::RegistryAgent;
use fno_mail::hold::{HoldStore, SetBy};
use fno_mail::session::SessionId;
use std::path::{Path, PathBuf};

/// Stamp `dnd` and `held_conversation` from the hold store. Fail-quiet like
/// `overlay_truth_badges`: an unreadable record degrades to no mark.
pub(crate) fn overlay_hold_marks(rows: &mut [RegistryAgent]) {
    overlay_hold_marks_at(rows, &hold_dir());
}

/// The mail-hold dir, mirroring `mail_hold.rs`'s `state_root()`
/// (`FNO_HOME` > `$HOME/.fno`) so the overlay reads the root the writers
/// use. A global-config `paths.state_dir` root is not mirrored here; that
/// install degrades to no mark, never a wrong one.
fn hold_dir() -> PathBuf {
    let root = match std::env::var_os("FNO_HOME") {
        Some(home) => PathBuf::from(home),
        None => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".fno"))
            .unwrap_or_else(|| PathBuf::from(".fno")),
    };
    crate::state_layout::place_dir(&root, "mail-hold").unwrap_or_else(|| root.join("mail-hold"))
}

pub(crate) fn overlay_hold_marks_at(rows: &mut [RegistryAgent], hold_dir: &Path) {
    let now = chrono::Utc::now();
    // One directory read per render, never one file per row. `list`, not
    // `live`: a render never removes a record.
    let live: std::collections::HashMap<SessionId, SetBy> = HoldStore::new(hold_dir)
        .list()
        .into_iter()
        .filter(|h| h.live(now))
        .map(|h| (h.session().clone(), h.set_by()))
        .collect();
    if live.is_empty() {
        return;
    }
    for row in rows.iter_mut() {
        let Some(set_by) = row
            .harness_session_id
            .as_deref()
            .and_then(|sid| SessionId::parse(sid).ok())
            .and_then(|session| live.get(&session))
        else {
            continue;
        };
        row.dnd = true;
        row.held_conversation = *set_by == SetBy::Conversation;
    }
}
