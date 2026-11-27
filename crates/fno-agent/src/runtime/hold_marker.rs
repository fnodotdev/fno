//! The daemon hold: a marker file that keeps the daemon down.
//!
//! A plain SIGTERM does not hold. Every client command calls `ensure_daemon`,
//! which lazy-starts a daemon when none answers, so a stopped daemon is back in
//! seconds. `fno agent restart --hold "<reason>"` writes this marker first,
//! then stops the daemon. While the marker exists, `ensure_daemon` refuses to
//! fork and the daemon refuses to bind, so neither a client nor a launchd
//! relaunch revives it. `fno agent restart --release` removes the marker and
//! starts a daemon.
//!
//! The marker is fail-closed: a file that exists but does not parse still
//! holds. Only a release removes it. There is no expiry.

use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::paths::AgentsHome;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonHold {
    pub reason: String,
    pub held_at_unix: u64,
    pub held_by_pid: u32,
}

impl DaemonHold {
    /// The one line a refused start prints.
    pub fn refusal(&self) -> String {
        format!(
            "the daemon is held: {} (since unix {}, by pid {}). \
             `fno agent restart --release` lifts the hold and starts it",
            self.reason, self.held_at_unix, self.held_by_pid
        )
    }
}

/// The hold in force, if any. A marker that exists but is unreadable or
/// malformed still reads as a hold with an explanatory reason.
pub fn read_hold(home: &AgentsHome) -> Option<DaemonHold> {
    read_hold_at(&home.daemon_hold())
}

fn read_hold_at(path: &Path) -> Option<DaemonHold> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            return Some(DaemonHold {
                reason: format!("hold marker {} is unreadable: {e}", path.display()),
                held_at_unix: 0,
                held_by_pid: 0,
            })
        }
    };
    Some(serde_json::from_str(&raw).unwrap_or_else(|e| DaemonHold {
        reason: format!("hold marker {} does not parse: {e}", path.display()),
        held_at_unix: 0,
        held_by_pid: 0,
    }))
}

/// Write the marker atomically (temp sibling, then rename). Placing a hold
/// over an existing one replaces the reason.
pub fn place_hold(home: &AgentsHome, reason: &str) -> std::io::Result<DaemonHold> {
    let hold = DaemonHold {
        reason: reason.to_string(),
        held_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        held_by_pid: std::process::id(),
    };
    let path = home.daemon_hold();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("hold.tmp{}", std::process::id()));
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(serde_json::to_string(&hold)?.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&tmp, &path)?;
    Ok(hold)
}

/// Remove the marker. Returns the hold that was lifted, `None` when none stood.
pub fn release_hold(home: &AgentsHome) -> std::io::Result<Option<DaemonHold>> {
    let prior = read_hold(home);
    match std::fs::remove_file(home.daemon_hold()) {
        Ok(()) => Ok(prior),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> (tempfile::TempDir, AgentsHome) {
        let dir = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(dir.path().to_path_buf());
        (dir, home)
    }

    #[test]
    fn place_read_release_round_trip() {
        let (_dir, home) = home();
        assert!(read_hold(&home).is_none());
        let placed = place_hold(&home, "r2d2 datadump").unwrap();
        assert_eq!(read_hold(&home), Some(placed.clone()));
        assert!(placed.refusal().contains("r2d2 datadump"));
        assert_eq!(release_hold(&home).unwrap(), Some(placed));
        assert!(read_hold(&home).is_none());
        assert_eq!(release_hold(&home).unwrap(), None);
    }

    #[tokio::test]
    async fn a_hold_stops_the_on_demand_start() {
        let (dir, home) = home();
        place_hold(&home, "r2d2 datadump").unwrap();
        let bin = dir.path().join("fno-agent-daemon");
        std::fs::write(&bin, "").unwrap();
        let err = crate::client::ensure_daemon(&home, &bin).await.unwrap_err();
        assert!(matches!(err, crate::client::ClientError::DaemonHeld(_)));
        assert!(err.to_string().contains("r2d2 datadump"));
    }

    #[test]
    fn a_malformed_marker_still_holds() {
        let (_dir, home) = home();
        std::fs::write(home.daemon_hold(), "not json").unwrap();
        let hold = read_hold(&home).expect("a garbled marker must hold, never release");
        assert!(hold.reason.contains("does not parse"));
    }
}
