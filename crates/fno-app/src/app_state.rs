//! The machine-local state one quit leaves behind and the next launch
//! restores: window frame, last view, last streamed session, agents fold.
//!
//! The file lives under `~/Library/Application Support/fno-app/`, the
//! per-user home for desktop-app state on macOS, outside the fleet state
//! root (`docs/state-root-inventory.md` closes that root to new writers).
//! A missing or corrupt file reads as no state: a first run and a damaged
//! file both open the default window, never refuse to launch.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::window::MainView;

/// Bump when the shape changes; a file at any other version reads as absent.
pub const STATE_VERSION: u32 = 1;

/// Where this machine keeps the state file.
pub fn state_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("fno-app")
            .join("window-state.json"),
    )
}

/// What the Workspaces section groups its rows by: the workspace's own
/// nesting (the default), or flat bands by agent state or by machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupBy {
    #[default]
    Workspaces,
    State,
    Machine,
}

/// Which window mode the saved frame belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SavedMode {
    Windowed,
    Maximized,
    Fullscreen,
}

/// The saved frame, in points.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SavedBounds {
    pub mode: SavedMode,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// One quit's worth of UI state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppState {
    pub version: u32,
    pub view: MainView,
    pub bounds: SavedBounds,
    pub pinned: Option<fno_proto::chat_event::SessionKey>,
    /// The left pane's folded workspaces, by fold key (canonical cwd, else
    /// name); absence from the set means open, so an older file that
    /// carries none opens every workspace.
    #[serde(default)]
    pub folds: HashSet<String>,
    /// The Workspaces section's grouping; the default opens nested.
    #[serde(default)]
    pub group_by: GroupBy,
    /// The session panel: open, its card order and folded cards. A file
    /// from before the panel opens it with the default cards.
    #[serde(default)]
    pub right: crate::right_pane::Saved,
}

/// The ways a save can fail. Best-effort callers drop the error.
#[derive(Debug)]
pub enum StateError {
    Encode(serde_json::Error),
    Io(std::io::Error),
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateError::Encode(e) => write!(f, "encode window state: {e}"),
            StateError::Io(e) => write!(f, "write window state: {e}"),
        }
    }
}

/// Read the state file. A missing, corrupt or wrong-version file is None.
pub fn load(path: &Path) -> Option<AppState> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<AppState>(&text) {
        Ok(state) if state.version == STATE_VERSION => Some(state),
        _ => None,
    }
}

/// Write the state file. The write is temp-plus-rename, so a crash mid-save
/// leaves either the old file or the new one, never a half file.
pub fn save(path: &Path, state: &AppState) -> Result<(), StateError> {
    let text = serde_json::to_string(state).map_err(StateError::Encode)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(StateError::Io)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(StateError::Io)?;
    std::fs::rename(&tmp, path).map_err(StateError::Io)
}

/// Keep a saved frame only when it intersects one of the displays, so a
/// monitor unplugged since the last quit cannot park the window off-screen.
pub fn clamp_to_displays(
    bounds: &SavedBounds,
    displays: &[(f32, f32, f32, f32)],
) -> Option<SavedBounds> {
    let on_screen = displays.iter().any(|&(dx, dy, dw, dh)| {
        bounds.x < dx + dw
            && dx < bounds.x + bounds.w
            && bounds.y < dy + dh
            && dy < bounds.y + bounds.h
    });
    on_screen.then_some(*bounds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> AppState {
        AppState {
            version: STATE_VERSION,
            view: MainView::Orchestration,
            bounds: SavedBounds {
                mode: SavedMode::Windowed,
                x: 40.0,
                y: 60.0,
                w: 1200.0,
                h: 800.0,
            },
            pinned: Some(fno_proto::chat_event::SessionKey {
                harness: "claude".into(),
                session_id: "abc123".into(),
                subagent: None,
            }),
            folds: HashSet::new(),
            group_by: GroupBy::default(),
            // A folded panel, so the round trip proves the fold survives.
            right: crate::right_pane::Saved {
                open: false,
                ..crate::right_pane::Saved::default()
            },
        }
    }

    fn tmp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("fno-app-state-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn saved_state_round_trips_field_for_field() {
        let path = tmp_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        let state = sample();
        save(&path, &state).expect("save");
        assert_eq!(load(&path), Some(state));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupt_and_missing_files_read_as_none() {
        let missing = tmp_path("missing");
        let _ = std::fs::remove_file(&missing);
        assert_eq!(load(&missing), None);

        let corrupt = tmp_path("corrupt");
        std::fs::write(&corrupt, "{not json").unwrap();
        assert_eq!(load(&corrupt), None);
        let _ = std::fs::remove_file(&corrupt);
    }

    #[test]
    fn a_v1_file_without_folds_still_loads_and_opens_every_workspace() {
        // A state file written before the fold set existed carries no
        // folds field; serde's default fills an empty set, so every
        // workspace opens, and the agents_open field it does carry is
        // ignored.
        let path = tmp_path("pre-folds");
        let _ = std::fs::remove_file(&path);
        let text = format!(
            r#"{{"version":{},"view":"Chat","bounds":{{"mode":"windowed","x":0.0,"y":0.0,"w":800.0,"h":600.0}},"pinned":null,"agents_open":true}}"#,
            STATE_VERSION
        );
        std::fs::write(&path, text).unwrap();
        let state = load(&path).expect("a pre-folds file loads");
        assert_eq!(state.folds, HashSet::new());
        // Nor does it carry the session panel: it opens with the default cards.
        assert_eq!(state.right, crate::right_pane::Saved::default());
        assert!(state.right.open);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_future_version_reads_as_none() {
        let path = tmp_path("version");
        let mut state = sample();
        state.version = STATE_VERSION + 1;
        save(&path, &state).unwrap();
        assert_eq!(load(&path), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_off_screen_frame_is_dropped_and_an_on_screen_one_kept() {
        let displays = [(0.0, 0.0, 1440.0, 900.0)];
        let frame = SavedBounds {
            mode: SavedMode::Windowed,
            x: 0.0,
            y: 0.0,
            w: 800.0,
            h: 600.0,
        };
        assert_eq!(clamp_to_displays(&frame, &displays), Some(frame));

        let far = SavedBounds {
            mode: SavedMode::Windowed,
            x: 9000.0,
            y: 9000.0,
            w: 800.0,
            h: 600.0,
        };
        assert_eq!(clamp_to_displays(&far, &displays), None);
    }
}
