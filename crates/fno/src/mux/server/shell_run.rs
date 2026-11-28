//! The composer's one-shot `!` runs. A run is a pane with no tab, drawn in
//! the card of the client that started it. It moves to a tab on request or
//! when that client leaves, and it is the same process there: a run is never
//! killed for leaving the card and never started twice.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::pane_spawn::{SpawnOrder, KEEPER_ROAD};
use super::*;
use crate::proto::shell_run::{ShellRunRequest, ShellRunState, ShellRunUpdate};

/// The script of a run. It records the exit code for the server. A run that
/// moved to a tab waits for Enter, so its output does not close with it.
const SCRIPT: &str = r#"trap : INT; "$0" -lc "$1"; s=$?; printf %s "$s" > "$2/exit"; if [ -e "$2/tab" ]; then printf '\n[exit %s] enter closes' "$s"; read -r _; fi; exit "$s""#;

/// The log lines a finished run keeps.
const LOG_LINES: u16 = 1000;

/// Why a run did not start or move.
#[derive(Debug, thiserror::Error)]
enum RunError {
    #[error("the project folder {0} is not there")]
    NoFolder(String),
    #[error("the run's state folder could not be made: {0}")]
    State(#[from] std::io::Error),
    #[error("the command did not start: {0}")]
    Spawn(String),
}

struct Run {
    id: u64,
    /// The client whose card shows it. `None` once it is in a tab.
    client: Option<u64>,
    squad: u64,
    cwd: String,
    dir: PathBuf,
}

/// Pane -> the run it hosts.
#[derive(Default)]
pub(crate) struct ShellRuns {
    by_pane: BTreeMap<u64, Run>,
}

impl ShellRuns {
    fn pane_of(&self, client: u64, id: u64) -> Option<u64> {
        self.by_pane
            .iter()
            .find(|(_, run)| run.client == Some(client) && run.id == id)
            .map(|(pane, _)| *pane)
    }
}

impl Core {
    pub(super) fn shell_run(&mut self, client: u64, request: ShellRunRequest) -> Flow {
        match request {
            ShellRunRequest::Start {
                run,
                squad,
                cwd,
                shell,
                line,
                rows,
                cols,
            } => {
                let dir = crate::proto::mux_dir()
                    .join("runs")
                    .join(format!("{}-{client}-{run}", std::process::id()));
                let held = Run {
                    id: run,
                    client: Some(client),
                    squad,
                    cwd,
                    dir,
                };
                return self.shell_run_start(client, held, [shell, line], (rows, cols));
            }
            ShellRunRequest::Input { run, bytes } => {
                if let Some(pane) = self.launch_run_pane(client, run) {
                    self.write_user_burst(pane, &bytes, Some(client));
                }
            }
            ShellRunRequest::ToTab { run } => {
                if let Some(pane) = self.launch_run_pane(client, run) {
                    if let Some((sid, tid)) = self.shell_run_to_tab(pane) {
                        self.set_view(client, sid, tid);
                        self.shell_run_tell(client, run, ShellRunState::Moved);
                    }
                    self.push_layout(true);
                } else {
                    // It ended and the card missed that update: let the card go.
                    let state = ShellRunState::Done {
                        exit: None,
                        output: String::new(),
                    };
                    self.shell_run_tell(client, run, state);
                }
            }
        }
        Flow::Continue
    }

    fn launch_run_pane(&self, client: u64, run: u64) -> Option<u64> {
        self.shell_runs
            .pane_of(client, run)
            .filter(|pane| self.panes.contains_key(pane))
    }

    fn shell_run_start(
        &mut self,
        client: u64,
        run: Run,
        [shell, line]: [String; 2],
        (rows, cols): (u16, u16),
    ) -> Flow {
        let id = run.id;
        let ready = if std::path::Path::new(&run.cwd).is_dir() {
            std::fs::create_dir_all(&run.dir)
                .map_err(RunError::from)
                .and_then(|()| {
                    crate::process_admission::admit_tab(0, None)
                        .map_err(|e| RunError::Spawn(e.to_string()))
                })
        } else {
            Err(RunError::NoFolder(run.cwd.clone()))
        };
        let permit = match ready {
            Ok(permit) => permit,
            Err(e) => {
                let reason = e.to_string();
                self.shell_run_tell(client, id, ShellRunState::Refused { reason });
                return Flow::Continue;
            }
        };
        let order = SpawnOrder {
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                SCRIPT.to_string(),
                shell,
                line,
                run.dir.to_string_lossy().into_owned(),
            ],
            rows: rows.max(1),
            cols: cols.max(1),
            cwd: run.cwd.clone(),
            env: Vec::new(),
            permit,
            keeper: KEEPER_ROAD,
        };
        self.spawn_then(order, Some(client), move |core, spawned| {
            match spawned {
                Ok(pane) => {
                    core.shell_runs.by_pane.insert(pane, run);
                    core.shell_run_tell(client, id, ShellRunState::Running { pane });
                    if core.clients.iter().any(|c| c.id == client) {
                        core.shell_run_output(pane);
                    } else {
                        // The card left while the command started.
                        core.shell_runs_client_gone(client);
                        core.push_layout(true);
                    }
                }
                Err(e) => {
                    let reason = RunError::Spawn(e).to_string();
                    core.shell_run_tell(client, id, ShellRunState::Refused { reason });
                }
            }
            Flow::Continue
        })
    }

    /// Give a run's pane a tab of its workspace. Answers where it landed.
    fn shell_run_to_tab(&mut self, pane: u64) -> Option<(u64, TabId)> {
        let run = self.shell_runs.by_pane.get_mut(&pane)?;
        run.client.take()?;
        if let Err(e) = std::fs::write(run.dir.join("tab"), "") {
            crate::mux_log!(warn; "fno mux: run in pane {pane} closes when it ends: {e}");
        }
        let (squad, cwd) = (run.squad, run.cwd.clone());
        let dest = match self.session.squad(squad) {
            Some(_) => squad,
            None => observers::passive_anchor(&self.session).0,
        };
        let placement = PanePlacement {
            tab: Some(crate::proto::TabSel::New),
            ..Default::default()
        };
        match self.place_with(Some(dest), &cwd, pane, &placement) {
            Ok((sid, tid, _)) => Some((sid, tid)),
            Err((_, e)) => {
                self.notice_all(format!("the ! run could not move to a tab: {e}"));
                None
            }
        }
    }

    /// The client left: each run its card showed moves to a tab.
    pub(super) fn shell_runs_client_gone(&mut self, client: u64) {
        let held: Vec<u64> = self
            .shell_runs
            .by_pane
            .iter()
            .filter(|(_, run)| run.client == Some(client))
            .map(|(pane, _)| *pane)
            .collect();
        for pane in held {
            self.shell_run_to_tab(pane);
        }
    }

    /// A workspace that closed took the card along: its runs move to a tab,
    /// so none keeps running out of sight.
    pub(super) fn shell_runs_rehome(&mut self) {
        let stray: Vec<(u64, u64, u64)> = self
            .shell_runs
            .by_pane
            .iter()
            .filter(|(_, run)| self.session.squad(run.squad).is_none())
            .filter_map(|(pane, run)| Some((*pane, run.client?, run.id)))
            .collect();
        for (pane, client, id) in stray {
            if self.shell_run_to_tab(pane).is_some() {
                self.shell_run_tell(client, id, ShellRunState::Moved);
            }
        }
    }

    /// Send a run's screen to its card, through that client's frame slot.
    pub(super) fn shell_run_output(&self, pane: u64) {
        let Some(client) = self
            .shell_runs
            .by_pane
            .get(&pane)
            .and_then(|run| run.client)
        else {
            return;
        };
        let (Some(entry), Some(to)) = (
            self.panes.get(&pane),
            self.clients.iter().find(|c| c.id == client),
        ) else {
            return;
        };
        if let Ok(mut slots) = to.dirty.lock() {
            slots.insert(pane, entry.vt.frame());
        }
        to.notify.notify_one();
    }

    /// A layout pass flushes the frame slots, so each run's screen goes back.
    pub(super) fn shell_runs_reseed(&self) {
        for pane in self.shell_runs.by_pane.keys() {
            self.shell_run_output(*pane);
        }
    }

    /// The pane is leaving the map: answer the card and drop the run's folder.
    pub(super) fn shell_run_settle(&mut self, pane: u64) {
        let Some(run) = self.shell_runs.by_pane.remove(&pane) else {
            return;
        };
        if let Some(client) = run.client {
            let exit = std::fs::read_to_string(run.dir.join("exit"))
                .ok()
                .and_then(|code| code.trim().parse().ok());
            let output = self
                .panes
                .get(&pane)
                .map(|entry| entry.vt.read_tail(LOG_LINES))
                .unwrap_or_default();
            self.shell_run_tell(client, run.id, ShellRunState::Done { exit, output });
        }
        if let Err(e) = std::fs::remove_dir_all(&run.dir) {
            crate::mux_log!(warn; "fno mux: run folder {} stays: {e}", run.dir.display());
        }
    }

    fn shell_run_tell(&self, client: u64, run: u64, state: ShellRunState) {
        let Some(to) = self.clients.iter().find(|c| c.id == client) else {
            return;
        };
        let update = ShellRunUpdate { run, state };
        if to
            .reliable_tx
            .try_send(ServerMsg::ShellRun(update))
            .is_err()
        {
            crate::mux_log!(warn; "fno mux: client {client} missed an update of run {run}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{empty_core, leaf_tab};
    use super::*;

    #[test]
    fn a_run_leaves_the_card_for_a_tab_and_is_the_same_pane() {
        let mut core = empty_core();
        core.shells = vec!["/bin/cat".into()];
        let shell = core.spawn_pane(24, 80, "/tmp").expect("a shell");
        core.session
            .add_squad(1, vec!["/tmp".into()], None, leaf_tab(5, shell));
        let pane = core.spawn_pane(10, 60, "/tmp").expect("the run");
        let dir = tempfile::tempdir().expect("a folder");
        let run = Run {
            id: 3,
            client: Some(9),
            squad: 1,
            cwd: "/tmp".into(),
            dir: dir.path().join("run"),
        };
        std::fs::create_dir_all(&run.dir).expect("the run's folder");
        let flag = run.dir.join("tab");
        core.shell_runs.by_pane.insert(pane, run);
        assert!(
            core.session.find_pane(pane).is_none(),
            "no tab while carded"
        );

        core.shell_runs_client_gone(9);
        assert!(core.session.find_pane(pane).is_some(), "the same pane");
        assert!(flag.exists(), "it waits for Enter at its end");
        assert_eq!(core.shell_runs.pane_of(9, 3), None, "the card let it go");

        core.shell_run_settle(pane);
        assert!(!flag.exists(), "its folder goes with it");
        core.reap_pane(pane);
        core.reap_pane(shell);
    }
}
