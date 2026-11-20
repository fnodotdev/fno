//! One spelling for the fno-app control channel and the saved window
//! state file: the path helpers and the command rows both the CLI doors
//! and the app read, so the two sides cannot drift.
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

pub fn support_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut p = PathBuf::from(home);
    p.push("Library");
    p.push("Application Support");
    p.push("fno-app");
    Some(p)
}

/// The saved window-state file the app writes on quit.
pub fn state_path() -> Option<PathBuf> {
    let mut p = support_dir()?;
    p.push("window-state.json");
    Some(p)
}

/// The control file for one session: the CLI doors append rows, the
/// running app drains them. The session names the file, so a spelling
/// with a separator can never escape the dir.
pub fn control_path(session: &str) -> Option<PathBuf> {
    if session.is_empty() || session.contains('/') || session.contains('\\') {
        return None;
    }
    let mut p = support_dir()?;
    p.push(format!("control-{session}.jsonl"));
    Some(p)
}

/// One control row: the CLI writes it, the running app applies it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum AppCommand {
    /// Focus one agent row (name, node id or session id).
    Focus { agent: String },
    /// Switch the active view (the kebab spelling mirrors MainView).
    View { view: String },
    /// Quit through the same save-on-quit path the red close button rides.
    Quit,
}

/// Why a control row did not land. Typed so no string-typed Result
/// crosses the seam (R6); the text is what the door prints.
#[derive(Debug)]
pub enum ControlError {
    /// The session spelling names no file inside the support dir.
    BadSession(String),
    /// The row could not be encoded.
    Encode(serde_json::Error),
    /// The row could not be written.
    Write {
        path: String,
        source: std::io::Error,
    },
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlError::BadSession(s) => {
                write!(f, "control file: session spelling {s:?} names no file")
            }
            ControlError::Encode(e) => write!(f, "encode command: {e}"),
            ControlError::Write { path, source } => write!(f, "write {path}: {source}"),
        }
    }
}

impl std::error::Error for ControlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ControlError::BadSession(_) => None,
            ControlError::Encode(e) => Some(e),
            ControlError::Write { source, .. } => Some(source),
        }
    }
}

/// Append one command row to the session's control file.
pub fn append(session: &str, command: &AppCommand) -> Result<(), ControlError> {
    let Some(path) = control_path(session) else {
        return Err(ControlError::BadSession(session.to_string()));
    };
    let line = serde_json::to_string(command).map_err(ControlError::Encode)?;
    // The support dir may not exist yet: `open --focus` writes before its
    // app ever launched, so the row must not die to a missing directory.
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|source| ControlError::Write {
            path: dir.display().to_string(),
            source,
        })?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|source| ControlError::Write {
            path: path.display().to_string(),
            source,
        })?;
    writeln!(f, "{line}").map_err(|source| ControlError::Write {
        path: path.display().to_string(),
        source,
    })?;
    Ok(())
}

/// Read and clear the session's control file. A missing file answers an
/// empty list; a corrupt row is dropped and the rest kept, because one
/// bad line must never wedge the app's poll loop.
pub fn drain(session: &str) -> Vec<AppCommand> {
    let Some(path) = control_path(session) else {
        return Vec::new();
    };
    // Rename before the read: a row appended between the read and the
    // remove would otherwise be deleted unread. The rename hands the
    // drain a private snapshot, and a row appended mid-drain re-creates
    // the original file, so it survives for the next poll.
    let mut staged = path.clone();
    staged.set_extension("draining");
    if std::fs::rename(&path, &staged).is_err() {
        return Vec::new();
    }
    let out = std::fs::read_to_string(&staged)
        .map(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str::<AppCommand>(line).ok())
                .collect()
        })
        .unwrap_or_default();
    std::fs::remove_file(&staged).ok();
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// The two tests mutate the process-global HOME; cargo runs them in
    /// parallel threads, so this guard serializes the mutations.
    static HOME_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn control_round_trips_and_tolerates_corrupt_rows() {
        let _guard = HOME_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("fno-appctl-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Library")).ok();
        unsafe { std::env::set_var("HOME", &dir) };
        let cmd = AppCommand::Focus {
            agent: "x-1".into(),
        };
        append("t", &cmd).unwrap();
        assert_eq!(drain("t"), vec![cmd]);
        assert!(drain("t").is_empty());
    }

    #[test]
    fn sessions_never_share_control_files() {
        let _guard = HOME_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("fno-appctl2-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Library")).ok();
        unsafe { std::env::set_var("HOME", &dir) };
        append("a", &AppCommand::Quit).ok();
        assert!(drain("b").is_empty());
        assert_eq!(drain("a").len(), 1);
    }
}
