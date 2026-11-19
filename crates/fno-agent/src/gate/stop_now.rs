//! `fleet-incident stop --now`: the interrupt level on top of the loops hold.
//!
//! The loops hold stops a loop at its next turn end, so a turn in progress
//! runs on. `--now` also presses Esc in every running turn, through one of two
//! lanes: a claude background session's control socket, or the mux pane a
//! registered pane worker runs in. It writes the workers it reached to a
//! sidecar beside the breaker. The all-clear types a resume turn into exactly
//! those workers and removes the sidecar: an interrupted worker is idle, so
//! no hook delivers the bus all-clear to it.
//!
//! The stopper's own session is skipped, and so is any worker not mid-turn
//! (Esc on a permission prompt would deny it). The background listing never
//! carries interactive rows, so the user's own terminal is never pressed; a
//! portal pane mirrors a background session and is not a pane worker.

use crate::claude_roster::{self, ClaudeAgentRow, ClaudeAgentsSnapshot};
use crate::paths::AgentsHome;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The resume turn the all-clear types into each interrupted worker.
pub const RESUME_TEXT: &str = "All clear: the fleet stop is lifted. The stop interrupted your turn. Pick up the work where it stopped.";

/// One worker `--now` pressed Esc in. A background session names its claude
/// `account` root (`None` is the ambient `~/.claude`); a pane worker names
/// its `pane` address (`server:pane_id`) and hosted `harness`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Interrupted {
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
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

fn is_own(skip: &[String], short_id: &str, session_id: Option<&str>) -> bool {
    // An empty prefix would match every id and skip the whole fleet.
    !short_id.is_empty()
        && skip
            .iter()
            .any(|id| id.starts_with(short_id) || session_id == Some(id.as_str()))
}

/// The background rows a `--now` stop interrupts: a live session mid-turn
/// that is not one of `skip`.
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
        .filter(|row| !is_own(skip, &row.short_id, row.session_id.as_deref()))
        .collect()
}

/// Every registered pane worker that is not one of `skip`, as an
/// [`Interrupted`] row ready to press: session, pane address, harness.
pub(crate) fn pane_workers(registry: &crate::state::Registry, skip: &[String]) -> Vec<Interrupted> {
    registry
        .entries
        .iter()
        .filter(|row| row.substrate.as_deref() == Some("pane"))
        .filter_map(|row| {
            let mux = row.mux.as_ref().filter(|m| !m.session.is_empty())?;
            let session = row
                .harness_session_id
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| row.name.clone());
            let short = session.split('-').next().unwrap_or(&session).to_string();
            (!is_own(skip, &short, Some(&session))).then(|| Interrupted {
                session,
                account: None,
                pane: Some(format!("{}:{}", mux.session, mux.pane_id)),
                harness: Some(row.harness.clone().unwrap_or_else(|| "claude".into())),
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

/// Press Esc in one worker's running turn through its lane. A pane worker
/// whose screen does not read `working` is left alone.
fn press(row: &Interrupted) -> Result<bool, &'static str> {
    match (&row.pane, &row.harness) {
        (Some(address), Some(harness)) => {
            let (server, pane) = address.split_once(':').ok_or("bad-pane-address")?;
            let pane = pane.parse::<u64>().map_err(|_| "bad-pane-address")?;
            match crate::mail_cli::pane_prepare::screen_state(server, pane, harness) {
                Ok(state) if state == "working" => {}
                _ => return Ok(false),
            }
            crate::mail_inject::interrupt_via_pane(&row.session, address).map(|()| true)
        }
        _ => {
            let (dir, _) = account_dirs(row.account.as_deref())?;
            crate::mail_inject::interrupt_via_control_sock_in(&dir, &row.session).map(|()| true)
        }
    }
}

/// Press Esc in every running turn and record who took it. The sidecar
/// keeps workers an earlier `--now` reached, so one all-clear resumes all.
pub fn interrupt_running(home: &AgentsHome) -> std::io::Result<Sweep> {
    let skip = own_sessions();
    let snapshot = claude_roster::read_all_agents_union();
    let mut candidates: Vec<Interrupted> = running_turns(&snapshot, &skip)
        .into_iter()
        .map(|row| Interrupted {
            session: row.short_id.clone(),
            account: row.account.clone(),
            pane: None,
            harness: None,
        })
        .collect();
    if let Ok(registry) = crate::state::load_registry(&home.registry_json()) {
        candidates.extend(pane_workers(&registry, &skip));
    }
    let mut sweep = Sweep::default();
    for row in candidates {
        match press(&row) {
            Ok(true) => sweep.interrupted.push(row),
            Ok(false) => {}
            Err(reason) => sweep.failed.push((row.session, reason)),
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

/// Type the resume turn into every worker `--now` interrupted, then drop
/// the sidecar. Returns the workers the turn reached and those it missed.
pub fn resume_interrupted(home: &AgentsHome) -> (Vec<String>, Vec<(String, &'static str)>) {
    let path = interrupted_path(home);
    let (mut resumed, mut missed) = (Vec::new(), Vec::new());
    for row in read_interrupted(&path) {
        let harness = row.harness.as_deref().unwrap_or("claude");
        let (attempts, interval, delay) = (
            crate::mail_inject::DEFAULT_ATTEMPTS,
            crate::mail_inject::DEFAULT_INTERVAL_MS,
            crate::mail_inject::enter_delay_for_harness(harness),
        );
        let sent = match &row.pane {
            Some(address) => crate::mail_inject::deliver_via_pane(
                &row.session,
                address,
                Some(harness),
                RESUME_TEXT,
                attempts,
                interval,
                delay,
            ),
            None => account_dirs(row.account.as_deref()).and_then(|(dir, projects)| {
                crate::mail_inject::deliver_via_control_sock_in(
                    &dir,
                    &projects,
                    &row.session,
                    RESUME_TEXT,
                    attempts,
                    interval,
                    delay,
                )
            }),
        };
        match sent {
            Ok(()) => resumed.push(row.session),
            Err(reason) => missed.push((row.session, reason)),
        }
    }
    // A sidecar left behind would resume the same workers at the next clear.
    if let Err(error) = std::fs::remove_file(&path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            eprintln!("fleet-incident: {}: {error}", path.display());
        }
    }
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
    fn now_picks_running_turns_and_pane_workers_but_never_the_stopper() {
        let snapshot = ClaudeAgentsSnapshot::known(vec![
            row("aaaa1111", "busy", Some(1)),
            row("bbbb2222", "idle", Some(2)),
            row("cccc3333", "blocked", Some(3)),
            row("dddd4444", "busy", None),
            row("eeee5555", "busy", Some(5)),
        ]);
        let own = "eeee5555-0000-0000-0000-000000000000";
        let skip = vec![own.to_string()];
        let picked: Vec<&str> = running_turns(&snapshot, &skip)
            .iter()
            .map(|r| r.short_id.as_str())
            .collect();
        assert_eq!(picked, vec!["aaaa1111"]);

        let entry = |sid: &str, substrate: &str, pane: Option<u64>| crate::state::RegistryEntry {
            name: sid.into(),
            harness: Some("codex".into()),
            harness_session_id: Some(sid.into()),
            substrate: Some(substrate.into()),
            mux: pane.map(|pane_id| crate::state::MuxRef {
                session: "main".into(),
                pane_id,
            }),
            ..Default::default()
        };
        let registry = crate::state::Registry {
            entries: vec![
                entry("ffff6666-1", "pane", Some(7)),
                entry(own, "pane", Some(8)),
                entry("abab0000-1", "thread", None),
            ],
            ..Default::default()
        };
        let panes = pane_workers(&registry, &skip);
        let [pane] = panes.as_slice() else {
            panic!("one pane worker, not the stopper or the thread: {panes:?}");
        };
        assert_eq!(pane.pane.as_deref(), Some("main:7"));
        assert_eq!(pane.harness.as_deref(), Some("codex"));
    }
}
