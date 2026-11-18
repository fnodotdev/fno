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

/// Append one command row to the session's control file.
pub fn append(session: &str, command: &AppCommand) -> Result<(), String> {
    let Some(path) = control_path(session) else {
        return Err(format!(
            "control file: session spelling {session:?} names no file"
        ));
    };
    let line = serde_json::to_string(command).map_err(|e| format!("encode command: {e}"))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    writeln!(f, "{line}").map_err(|e| format!("append {:?} failed: {e}", path.display()))?;
    Ok(())
}

/// Read and clear the session's control file. A missing file answers an
/// empty list; a corrupt row is dropped and the rest kept, because one
/// bad line must never wedge the app's poll loop.
pub fn drain(session: &str) -> Vec<AppCommand> {
    let Some(path) = control_path(session) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        if let Ok(cmd) = serde_json::from_str::<AppCommand>(line) {
            out.push(cmd);
        }
    }
    let _ = std::fs::remove_file(&path);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_round_trips_and_tolerates_corrupt_rows() {
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
        let dir = std::env::temp_dir().join(format!("fno-appctl2-{}", std::process::pid()));
        std::fs::create_dir_all(dir.join("Library")).ok();
        unsafe { std::env::set_var("HOME", &dir) };
        append("a", &AppCommand::Quit).ok();
        assert!(drain("b").is_empty());
        assert_eq!(drain("a").len(), 1);
    }
}
