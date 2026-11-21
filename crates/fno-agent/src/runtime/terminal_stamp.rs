//! The operator's last terminal, stamped by the hook door and read by the desk toast. A launchd daemon fires toasts with an empty environment, so the only app name it can offer the toast comes from the stamp: the raw `TERM_PROGRAM` value the last hook call saw. Machine-scoped, one small file; a failed stamp is silent, because a toast is never load-bearing.

use std::path::{Path, PathBuf};

/// `~/.fno/state/terminal-app`, folded under `state/` like `attention/`. The state root's top level is closed (`docs/state-root-inventory.md`), so a new machine-scoped file lands in the existing `state/` folder.
pub fn terminal_stamp_path() -> Result<PathBuf, std::io::Error> {
    let home = crate::paths::AgentsHome::from_env();
    let parent = home
        .root()
        .parent()
        .ok_or_else(|| std::io::Error::other("no agents home parent"))?;
    Ok(parent.join("state").join("terminal-app"))
}

/// Stamp `TERM_PROGRAM` when it is set and changed. Best effort: a failed stamp never fails the hook, and a hook fire costs one stat plus a rare tiny write, so the PreToolUse latency budget holds. A no-op under `cfg!(test)`: unit tests must never write the ambient stamp, and the tests that exercise stamping pass a path to [`stamp_terminal`] instead.
pub fn stamp_from_env() {
    let Ok(term) = std::env::var("TERM_PROGRAM") else {
        return;
    };
    if term.trim().is_empty() {
        return;
    }
    if cfg!(test) {
        return;
    }
    if let Some(path) = terminal_path() {
        stamp_terminal(&path, &term);
    }
}

/// Read the raw stamped `TERM_PROGRAM` value, if any. Best effort.
pub fn read_stamp() -> Option<String> {
    let path = terminal_path()?;
    read_raw(&path)
}

fn terminal_path() -> Option<PathBuf> {
    terminal_stamp_path().ok()
}

/// The one write: create the parent, write to a temp name, rename into place, 0600. Every leg is best effort by construction: a failed leg returns and this fire simply carries no stamp, so the dispatch falls back to its candidate list. A failed write or permission step leaves the tmp file behind; the next fire's write truncates it. A caller on the hook path wraps it in the TERM_PROGRAM guard.
pub fn stamp_terminal(path: &Path, term_program: &str) {
    if term_program.trim().is_empty() {
        return;
    }
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, term_program.as_bytes()).is_err() {
        return;
    }
    if std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o600)).is_err()
    {
        return;
    }
    if std::fs::rename(&tmp, path).is_err() {
        return;
    }
}

/// The one read: the trimmed first line.
pub fn read_raw(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.lines().next().unwrap_or_default().trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The stamped terminal, mapped through the known-terminal table. `None`
/// when no stamp exists or the value maps to nothing.
pub fn stamped_terminal_app() -> Option<&'static str> {
    let term = read_stamp()?;
    crate::runtime::notify_dispatch::terminal_app(&term, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_and_read_round_trip_through_a_pinned_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state").join("terminal-app");
        stamp_terminal(&path, "ghostty");
        assert_eq!(read_raw(&path).as_deref(), Some("ghostty"));
    }

    #[test]
    fn an_empty_term_program_never_overwrites_the_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terminal-app");
        stamp_terminal(&path, "  ");
        assert_eq!(read_raw(&path), None);
        stamp_terminal(&path, "iTerm.app");
        assert_eq!(read_raw(&path).as_deref(), Some("iTerm.app"));
    }
}
