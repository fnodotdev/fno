//! `fleet-incident stop --now`: the interrupt level on top of the loops hold.
//!
//! The loops hold stops a loop at its next turn end, so a turn in progress
//! runs on. `--now` also presses Esc in every claude background session whose
//! turn is running, over that session's control socket, and writes the
//! sessions it reached to a sidecar beside the breaker. The all-clear types a
//! resume turn into exactly those sessions and removes the sidecar: an
//! interrupted session is idle, so no hook delivers the bus all-clear to it.
//!
//! The stopper's own session is skipped, and so are interactive sessions
//! (the user's own terminal) and sessions not mid-turn (Esc on a permission
//! prompt would deny it). A pane or codex-thread worker is not reached yet;
//! the receipt counts only what this lane pressed.

use crate::claude_roster::{self, ClaudeAgentRow, ClaudeAgentsSnapshot};
use crate::paths::AgentsHome;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The resume turn the all-clear types into each interrupted session.
pub const RESUME_TEXT: &str = "All clear: the fleet stop is lifted. The stop interrupted your turn. Pick up the work where it stopped.";

/// One session `--now` pressed Esc in. `account` names the isolated claude
/// account root it lives under; `None` is the ambient `~/.claude`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Interrupted {
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

/// What one sweep did: who took the Esc, and who refused it with why.
#[derive(Debug, Default)]
pub struct Sweep {
    pub interrupted: Vec<Interrupted>,
    pub failed: Vec<(String, &'static str)>,
}

/// The sidecar beside the machine-wide breaker.
pub fn interrupted_path(home: &AgentsHome) -> PathBuf {
    home.fleet_stop_json()
        .with_file_name("fleet-stop-interrupted.json")
}

/// The rows a `--now` stop interrupts: a live background session mid-turn
/// that is not one of `skip`. The listing never carries interactive rows.
pub(crate) fn running_turns<'a>(
    snapshot: &'a ClaudeAgentsSnapshot,
    skip: &[String],
) -> Vec<&'a ClaudeAgentRow> {
    let (ClaudeAgentsSnapshot::Known { rows, .. } | ClaudeAgentsSnapshot::Unknown { rows, .. }) =
        snapshot;
    let carries_pids = snapshot.carries_pids();
    rows.iter()
        .filter(|row| matches!(row.state.as_deref(), Some("busy" | "working")))
        .filter(|row| row.has_live_process(carries_pids))
        .filter(|row| {
            !skip.iter().any(|id| {
                id.starts_with(&row.short_id) || row.session_id.as_deref() == Some(id.as_str())
            })
        })
        .collect()
}

/// The stopper's own session ids, so a lead that runs `--now` keeps its turn.
fn own_sessions() -> Vec<String> {
    ["CLAUDE_CODE_SESSION_ID", "FNO_SESSION_ID"]
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .filter(|id| !id.trim().is_empty())
        .collect()
}

/// The daemon dir and transcript dir of the account root a session lives in.
fn account_dirs(account: Option<&str>) -> Result<(PathBuf, PathBuf), &'static str> {
    match account {
        None => Ok((
            claude_roster::daemon_dir(),
            crate::claude_drive::claude_projects_dir(),
        )),
        Some(id) => claude_roster::isolated_account_dirs()
            .into_iter()
            .find(|(name, _)| name == id)
            .map(|(_, root)| {
                (
                    claude_roster::daemon_dir_in(Some(&root)),
                    root.join("projects"),
                )
            })
            .ok_or("account-unconfigured"),
    }
}

/// Press Esc in every running turn and record who took it. The sidecar
/// keeps sessions an earlier `--now` reached, so one all-clear resumes all.
pub fn interrupt_running(home: &AgentsHome) -> std::io::Result<Sweep> {
    let snapshot = claude_roster::read_all_agents_union();
    let mut sweep = Sweep::default();
    for row in running_turns(&snapshot, &own_sessions()) {
        let pressed = account_dirs(row.account.as_deref()).and_then(|(dir, _)| {
            crate::mail_inject::interrupt_via_control_sock_in(&dir, &row.short_id)
        });
        match pressed {
            Ok(()) => sweep.interrupted.push(Interrupted {
                session: row.short_id.clone(),
                account: row.account.clone(),
            }),
            Err(reason) => sweep.failed.push((row.short_id.clone(), reason)),
        }
    }
    let path = interrupted_path(home);
    let mut all = read_interrupted(&path);
    for row in &sweep.interrupted {
        if !all.contains(row) {
            all.push(row.clone());
        }
    }
    if !all.is_empty() {
        write_interrupted(&path, &all)?;
    }
    Ok(sweep)
}

/// Type the resume turn into every session `--now` interrupted, then drop
/// the sidecar. Returns the sessions the turn reached and those it missed.
pub fn resume_interrupted(home: &AgentsHome) -> (Vec<String>, Vec<(String, &'static str)>) {
    let path = interrupted_path(home);
    let rows = read_interrupted(&path);
    let (mut resumed, mut missed) = (Vec::new(), Vec::new());
    for row in rows {
        let sent = account_dirs(row.account.as_deref()).and_then(|(dir, projects)| {
            crate::mail_inject::deliver_via_control_sock_in(
                &dir,
                &projects,
                &row.session,
                RESUME_TEXT,
                crate::mail_inject::DEFAULT_ATTEMPTS,
                crate::mail_inject::DEFAULT_INTERVAL_MS,
                crate::mail_inject::enter_delay_for_harness("claude"),
            )
        });
        match sent {
            Ok(()) => resumed.push(row.session),
            Err(reason) => missed.push((row.session, reason)),
        }
    }
    let _ = std::fs::remove_file(&path);
    (resumed, missed)
}

fn read_interrupted(path: &Path) -> Vec<Interrupted> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_interrupted(path: &Path, rows: &[Interrupted]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string(rows)?)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(short: &str, state: &str, pid: Option<u32>) -> ClaudeAgentRow {
        ClaudeAgentRow::new(short, Some(state)).with_pid(pid)
    }

    #[test]
    fn now_interrupts_only_live_running_turns_and_never_the_stopper() {
        let snapshot = ClaudeAgentsSnapshot::known(vec![
            row("aaaa1111", "busy", Some(1)),
            row("bbbb2222", "idle", Some(2)),
            row("cccc3333", "blocked", Some(3)),
            row("dddd4444", "busy", None),
            row("eeee5555", "busy", Some(5)),
        ]);
        let skip = vec!["eeee5555-0000-0000-0000-000000000000".to_string()];
        let picked: Vec<&str> = running_turns(&snapshot, &skip)
            .iter()
            .map(|r| r.short_id.as_str())
            .collect();
        assert_eq!(picked, vec!["aaaa1111"]);
    }
}
