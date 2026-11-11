//! The machine-armed hold mark: a bus-only row whose mail-hold
//! sidecar carries a conversation-sourced clock is `[HELD]`, never the
//! `[DND]` a lead reads as a hold it armed itself. Read-time projection over
//! the sidecars the conversation arm already writes - no registry field, so
//! the badge follows the hold's real lifecycle (release clears the policy and
//! the clock together) with nothing new to keep consistent.

use super::RegistryAgent;
use std::path::{Path, PathBuf};

/// Stamp `held_conversation` onto rows the mail-hold sidecars mark
/// machine-armed. Fail-quiet like `overlay_truth_badges`: an unreadable
/// sidecar degrades to no mark - the row shows `[DND]`, today's label,
/// never a wrong one.
pub(crate) fn overlay_hold_marks(rows: &mut [RegistryAgent]) {
    overlay_hold_marks_at(rows, &hold_dir());
}

/// The mail-hold dir, mirroring `mail_hold.rs`'s `state_root()`
/// (`FNO_HOME` > `$HOME/.fno`) so the overlay reads the root the arm
/// wrote. A global-config `paths.state_dir` root is not mirrored here;
/// that install degrades to no mark, never a wrong one.
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
    for row in rows.iter_mut() {
        if !row.dnd {
            continue;
        }
        let Some(sid) = row.harness_session_id.as_deref() else {
            continue;
        };
        // mail_hold.rs `identity_key`: ses_ ids verbatim, else lowercased.
        let key = if sid.starts_with("ses_") {
            sid.to_string()
        } else {
            sid.to_lowercase()
        };
        let Ok(text) = std::fs::read_to_string(hold_dir.join(format!("{key}.json"))) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        // A lapsed conversation clock still marks the row: its stamp waits
        // on the release timer or the lapsed-hold sweep, and a [DND] in that
        // window reads as a hold nobody set.
        if v.get("source").and_then(|s| s.as_str()) == Some("conversation") {
            row.held_conversation = true;
        }
    }
}
