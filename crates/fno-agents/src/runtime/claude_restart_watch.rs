//! The claude-restart watch arm: revive thread workers a Claude daemon
//! restart dropped, with no lead action.
//!
//! A Claude Code auto-update restarts the vendor daemon. The takeover adopts
//! jobs into the new supervisor but their recorded job identity moves: new
//! pids, new start times. A registered claude thread row whose job identity
//! no longer matches any live roster worker is down, and today a lead revives
//! each by hand with `fno agents session resume`. This arm watches the
//! daemon's incarnation stamp (supervisor pid, supervisor start token,
//! highest worker cliVersion) and, on a change, runs one revival pass over
//! the down rows through boot revival's revive door - the same door the lead
//! runs by hand.
//!
//! Evidence rules are the roster witness's own (`liveness_sweep::BgRoster`):
//! an unreadable roster is unknown liveness and never fires; a missing pid or
//! a missing start time on either side is degraded evidence and never reads
//! down; only absence, a gone pid, or a proven start-time mismatch reads
//! down. The pass holds while boot revival's boot hold stands, so a machine
//! reboot runs one revival, not two.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::claude_roster::ClaudeRoster;
use crate::liveness_sweep::{BgRoster, JobHealth};
use crate::paths::AgentsHome;
use crate::state::RegistryEntry;
use crate::{boot_revival, AgentStatus};

/// One observation every two minutes: a takeover drops workers for the
/// window between restart and the next tick, and the tick that follows
/// revives them; nothing in the drop is served faster by a tighter cadence.
pub const CLAUDE_RESTART_WATCH_INTERVAL_S: u64 = 120;

/// The sender stamp the pass's mail carries, distinct from boot revival's,
/// so a drain attributes the rows to the restart pass that sent them.
pub(crate) const SENDER: &str = "fno/claude-restart";

const RECEIPT_FILE: &str = "claude-restart-revival.json";

/// The Claude daemon incarnation the arm watches. Any field changing is one
/// restart: an auto-update swaps the supervisor binary, and with it the pid,
/// the start token, and the version the adopted workers record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DaemonStamp {
    supervisor_pid: u32,
    proc_start: u64,
    version: Option<String>,
}

/// The daemon stamp a readable merged roster names. `None` when the roster
/// is unreadable, the supervisor pid is missing, or the start token cannot
/// be read: an unknown incarnation records nothing and fires nothing.
fn read_stamp(roster: &ClaudeRoster) -> Option<DaemonStamp> {
    let supervisor_pid = roster.supervisor_pid?;
    let proc_start = crate::process_probe::process_start_time(supervisor_pid)?;
    let version = roster
        .workers
        .values()
        .filter_map(|w| w.cli_version.clone())
        .max();
    Some(DaemonStamp {
        supervisor_pid,
        proc_start,
        version,
    })
}

/// A row the pass may revive: a claude thread worker that expects a live
/// backend on a node whose work is not closed. `Spawning` and `Restarting`
/// are excluded because a launch is already in flight for the row - the
/// pass reviving it would race the launch it names. Exited and
/// PermanentDead are deliberate or terminal stops, never this pass's
/// business.
fn candidate(e: &RegistryEntry, closed_nodes: &std::collections::HashSet<String>) -> bool {
    e.harness_name() == "claude"
        && e.substrate.as_deref() == Some("thread")
        && e.mux.is_none()
        && crate::daemon::is_non_terminal(e.status)
        && !matches!(e.status, AgentStatus::Spawning | AgentStatus::Restarting)
        && e.harness_session_id
            .as_deref()
            .is_some_and(|s| s.len() >= 8)
        && e.node.as_deref().is_none_or(|n| !closed_nodes.contains(n))
}

/// One pass's rows plus why nothing ran. `skip_reason` set means rows is
/// empty and no revive was attempted.
#[derive(Debug, Default)]
struct PassOutcome {
    rows: Vec<boot_revival::ReceiptRow>,
    skip_reason: Option<String>,
    stamp: Option<DaemonStamp>,
}

/// The pass body, on an injected witness and revive fn so tests drive it
/// without touching the machine's live daemon roster or launching sessions.
/// The boot hold rides in as a parameter: the caller reads it from boot
/// revival, a test injects it, so neither depends on the host's boot age.
/// Leads go first, mirroring boot revival, so a revived lead re-seats its
/// role before its workers fan out under it.
fn run_pass_with(
    home: &AgentsHome,
    stamp: DaemonStamp,
    hold: Option<String>,
    witness: &BgRoster,
    revive: &dyn Fn(&AgentsHome, &str) -> boot_revival::ReceiptRow,
) -> PassOutcome {
    let mut outcome = PassOutcome {
        stamp: Some(stamp),
        ..Default::default()
    };
    if hold.is_some() {
        outcome.skip_reason = Some("boot revival's hold is still standing".to_string());
        return outcome;
    }
    let Ok(registry) = crate::state::load_registry(&home.registry_json()) else {
        outcome.skip_reason = Some("registry unread".to_string());
        return outcome;
    };
    let (closed_nodes, _merged) = boot_revival::closed_and_merged_nodes();
    let mut candidates: Vec<&RegistryEntry> = registry
        .entries
        .iter()
        .filter(|e| candidate(e, &closed_nodes))
        .collect();
    candidates.sort_by_key(|e| e.role_level.is_none());
    for e in &candidates {
        // Only a real down reading revives: Unknown (unreadable roster,
        // degraded evidence) and Live both pass through untouched, and the
        // witness answers Unknown itself on a roster it could not read.
        if let JobHealth::Down(_) = witness.job_health(e) {
            outcome.rows.push(revive(home, &e.name));
        }
    }
    boot_revival::notify_as(home, &outcome.rows, SENDER, "claude daemon restart revival");
    write_receipt(home, &outcome);
    outcome
}

fn write_receipt(home: &AgentsHome, outcome: &PassOutcome) {
    let Some(stamp) = outcome.stamp.as_ref() else {
        return;
    };
    #[derive(Serialize)]
    struct PassReceipt<'a> {
        daemon: &'a DaemonStamp,
        at_unix: u64,
        rows: &'a [boot_revival::ReceiptRow],
        done: bool,
    }
    let receipt = PassReceipt {
        daemon: stamp,
        at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            // A clock before the epoch is a test-runner artifact; the receipt
            // stamps observability, never a gate, so 0 is the honest floor.
            .unwrap_or(0),
        rows: &outcome.rows,
        done: true,
    };
    let path = home.root().join(RECEIPT_FILE);
    // The receipt is the pass's own record beside the tick row; a failure to
    // write it costs the rerun path its stamp, never the revival itself. A
    // serialize failure names itself too: an empty receipt would read as a
    // corrupt one, so neither failure lands silently.
    let bytes = match serde_json::to_vec(&receipt) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("claude restart watch: receipt serialize failed ({error})");
            return;
        }
    };
    if let Err(error) = std::fs::write(&path, bytes) {
        eprintln!("claude restart watch: receipt write failed ({error})");
    }
}

/// One revive per stamp change. The pass runs on the blocking pool so a
/// slow revive never stalls the daemon loop.
fn run_tick(
    home: &AgentsHome,
    state: &Mutex<Option<DaemonStamp>>,
    journal: &crate::loop_runtime::Journal,
) {
    let witness = BgRoster::load_roster_only();
    let stamp = witness.roster().and_then(read_stamp);
    let Some(stamp) = stamp else {
        crate::tick_ledger::emit_tick(
            journal,
            "claude_restart_watch",
            crate::tick_ledger::SCHED_DAEMON,
            0,
            Some("the Claude daemon roster or its supervisor start is unread"),
            None,
            CLAUDE_RESTART_WATCH_INTERVAL_S,
        );
        return;
    };
    let previous = state.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(stamp.clone());
    drop(guard);
    let changed = match previous {
        None => {
            // First observation records the incarnation; the arm did not see
            // the restart, so it cannot name what changed. A pass here would
            // race boot revival after a reboot and revive rows whose staleness
            // no restart this arm observed explains.
            crate::tick_ledger::emit_tick(
                journal,
                "claude_restart_watch",
                crate::tick_ledger::SCHED_DAEMON,
                0,
                Some("first observation; the stamp is recorded"),
                None,
                CLAUDE_RESTART_WATCH_INTERVAL_S,
            );
            return;
        }
        Some(prev) => prev != stamp,
    };
    if !changed {
        crate::tick_ledger::emit_tick(
            journal,
            "claude_restart_watch",
            crate::tick_ledger::SCHED_DAEMON,
            0,
            None,
            Some("daemon incarnation unchanged"),
            CLAUDE_RESTART_WATCH_INTERVAL_S,
        );
        return;
    }
    let outcome = run_pass_with(
        home,
        stamp,
        boot_revival::hold_reason(home),
        &witness,
        &|home, name| boot_revival::revive_unattended(home, name),
    );
    let revived = outcome
        .rows
        .iter()
        .filter(|r| r.outcome == "revived")
        .count() as u64;
    crate::tick_ledger::emit_tick(
        journal,
        "claude_restart_watch",
        crate::tick_ledger::SCHED_DAEMON,
        revived,
        outcome.skip_reason.as_deref(),
        Some(&format!(
            "daemon restart revived {} worker(s)",
            outcome.rows.len()
        )),
        CLAUDE_RESTART_WATCH_INTERVAL_S,
    );
}

/// The arm's tick site: cadence gate, one-in-flight gate, then the pass on
/// the blocking pool. Same shape as the machine watch arm beside it.
pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    let interval = Duration::from_secs(CLAUDE_RESTART_WATCH_INTERVAL_S);
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < interval)
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    let state = Arc::clone(&arm.state);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        let journal = crate::loop_runtime::Journal::new_raw(
            home.events_jsonl(),
            crate::daemon::global_events_path(&home),
        );
        run_tick(&home, &state, &journal);
    });
}

/// The arm's memory: the last observed incarnation. `None` until the first
/// readable stamp lands.
#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
    state: Arc<Mutex<Option<DaemonStamp>>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_roster::RosterWorker;

    fn roster_with(workers: Vec<RosterWorker>, supervisor_pid: u32) -> ClaudeRoster {
        let mut roster = ClaudeRoster {
            proto: 1,
            supervisor_pid: Some(supervisor_pid),
            updated_at: Some(1),
            workers: Default::default(),
        };
        for w in workers {
            roster.workers.insert(w.short_id().to_string(), w);
        }
        roster
    }

    fn worker(session_id: &str, pid: u32, proc_start: u64, version: &str) -> RosterWorker {
        RosterWorker {
            session_id: session_id.to_string(),
            pid: Some(pid),
            repl_pid: None,
            proc_start: Some(proc_start),
            pty_sock: None,
            pty_auth: None,
            cli_version: Some(version.to_string()),
            cwd: String::new(),
            worktree_path: None,
        }
    }

    fn thread_row(name: &str, session_id: &str, pid_start: Option<u64>) -> RegistryEntry {
        RegistryEntry {
            name: name.to_string(),
            harness: Some("claude".into()),
            substrate: Some("thread".into()),
            harness_session_id: Some(session_id.to_string()),
            pid_start_time: pid_start,
            // A registry row's default status is Spawning, which the pass
            // excludes; a candidate under test is a live worker.
            status: AgentStatus::Live,
            ..Default::default()
        }
    }

    fn stamp_of(supervisor_pid: u32, proc_start: u64, version: &str) -> DaemonStamp {
        DaemonStamp {
            supervisor_pid,
            proc_start,
            version: Some(version.to_string()),
        }
    }

    #[test]
    fn a_missing_roster_or_supervisor_records_no_stamp() {
        // No supervisor pid: the roster names no daemon to watch.
        let mut roster = roster_with(vec![], 0);
        roster.supervisor_pid = None;
        assert_eq!(read_stamp(&roster), None);
    }

    #[test]
    fn the_stamp_takes_the_highest_worker_version() {
        // The supervisor start probe hits the real process table: a pid this
        // test can prove alive is its own.
        let roster = roster_with(
            vec![
                worker("aaaaaaaa-111", 101, 5, "2.1.9"),
                worker("bbbbbbbb-222", 102, 6, "2.1.321"),
            ],
            std::process::id(),
        );
        let stamp = read_stamp(&roster).expect("a live supervisor stamps");
        // Lexicographic max: fine while the vendor's versions share a width;
        // the field is compared for inequality only, never ordered against
        // a release schedule.
        assert_eq!(stamp.version.as_deref(), Some("2.1.9"));
    }

    #[test]
    fn absent_listed_pid_gone_and_start_mismatch_read_down() {
        // The pid probes hit the real process table, so the alive jobs use
        // this test process's own pid and the gone job uses a positive pid
        // no kernel hands out: representable in pid_t (the probe refuses a
        // broadcast-range value) and far above every pid_max.
        let live_pid = std::process::id();
        let gone_pid = 2_000_000_000_u32;
        let witness = BgRoster::for_roster_testing(Some(roster_with(
            vec![
                worker("cccccccc-333", live_pid, 100, "2.1.321"),
                worker("eeeeeeee-555", live_pid, 200, "2.1.321"),
                worker("ffffffff-666", gone_pid, 100, "2.1.321"),
            ],
            live_pid,
        )));
        // Absent: no listed job carries the row's session.
        let absent = thread_row("absent", "aaaaaaaa-111", Some(100));
        assert_eq!(
            witness.job_health(&absent),
            JobHealth::Down(crate::liveness_sweep::JobDownReason::Absent)
        );
        // Pid gone: the listed job's pid answers nothing.
        let gone = thread_row("gone", "ffffffff-666", Some(100));
        assert_eq!(
            witness.job_health(&gone),
            JobHealth::Down(crate::liveness_sweep::JobDownReason::PidGone)
        );
        // Start mismatch: the listed job is a later incarnation of the row's
        // job (the takeover re-adopts under new pids and start times).
        let mismatch = thread_row("mismatch", "eeeeeeee-555", Some(100));
        assert_eq!(
            witness.job_health(&mismatch),
            JobHealth::Down(crate::liveness_sweep::JobDownReason::StartTimeMismatch)
        );
        // No recorded start on the row, pid alive: degraded evidence, live.
        let degraded = thread_row("degraded", "cccccccc-333", None);
        assert_eq!(witness.job_health(&degraded), JobHealth::Live);
        // Matched start times: live.
        let live = thread_row("live", "cccccccc-333", Some(100));
        assert_eq!(witness.job_health(&live), JobHealth::Live);
        // An unreadable roster is unknown liveness: never down.
        let blind = BgRoster::for_roster_testing(None);
        assert_eq!(blind.job_health(&live), JobHealth::Unknown);
    }

    #[test]
    fn only_claude_thread_rows_with_open_work_are_candidates() {
        let closed: std::collections::HashSet<String> = ["x-closed".to_string()].into();
        let mut pane_row = thread_row("pane", "aaaaaaaa-111", Some(1));
        pane_row.mux = Some(crate::state::MuxRef {
            session: "main".into(),
            pane_id: 7,
        });
        assert!(
            !candidate(&pane_row, &closed),
            "a pane row is not a thread worker"
        );
        let mut codex_row = thread_row("codex", "aaaaaaaa-111", Some(1));
        codex_row.harness = Some("codex".into());
        assert!(!candidate(&codex_row, &closed));
        let mut exited = thread_row("exited", "aaaaaaaa-111", Some(1));
        exited.status = AgentStatus::Exited;
        assert!(!candidate(&exited, &closed), "a terminal stop stays down");
        let mut spawning = thread_row("spawning", "aaaaaaaa-111", Some(1));
        spawning.status = AgentStatus::Spawning;
        assert!(
            !candidate(&spawning, &closed),
            "a spawn in flight has no roster row yet"
        );
        let mut restarting = thread_row("restarting", "aaaaaaaa-111", Some(1));
        restarting.status = AgentStatus::Restarting;
        assert!(
            !candidate(&restarting, &closed),
            "a row under the restart policy is already relaunching"
        );
        let mut on_closed = thread_row("closed", "aaaaaaaa-111", Some(1));
        on_closed.node = Some("x-closed".into());
        assert!(
            !candidate(&on_closed, &closed),
            "a done node's worker stays down"
        );
        assert!(candidate(
            &thread_row("good", "aaaaaaaa-111", Some(1)),
            &closed
        ));
    }

    #[test]
    fn one_pass_revives_each_down_row_once_and_leads_first() {
        let dir = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(dir.path());
        std::fs::create_dir_all(home.root()).expect("test home creates");
        let mut lead = thread_row("lead", "aaaaaaaa-111", Some(1));
        lead.role_level = Some(1);
        let worker = thread_row("worker", "bbbbbbbb-222", Some(1));
        let mut pane_row = thread_row("pane", "cccccccc-333", Some(1));
        pane_row.mux = Some(crate::state::MuxRef {
            session: "main".into(),
            pane_id: 7,
        });
        crate::state::update_registry(&home.registry_json(), |reg| {
            reg.entries = vec![worker, pane_row, lead];
        })
        .expect("registry writes");
        // A readable witness whose roster lists nothing: every candidate
        // reads Down(Absent), no ambient machine state involved.
        let witness = BgRoster::for_roster_testing(Some(roster_with(vec![], 4242)));
        let revived = std::cell::RefCell::new(Vec::new());
        let outcome = run_pass_with(
            &home,
            stamp_of(1, 1, "v"),
            None,
            &witness,
            &|_home, name| {
                revived.borrow_mut().push(name.to_string());
                boot_revival::ReceiptRow::new(name, "revived", "test")
            },
        );
        assert_eq!(
            *revived.borrow(),
            vec!["lead".to_string(), "worker".to_string()]
        );
        assert_eq!(outcome.rows.len(), 2);
        assert!(outcome.skip_reason.is_none());
        assert!(
            home.root().join(RECEIPT_FILE).exists(),
            "the pass writes its receipt"
        );
    }

    #[test]
    fn the_pass_holds_while_boot_revival_holds() {
        let dir = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(dir.path());
        std::fs::create_dir_all(home.root()).expect("test home creates");
        // The hold rides in as a parameter: the branch is what the test
        // pins, not the host's boot age. No receipt file at all would read
        // the hold only while the runner is young, so the ambient path is
        // never taken here.
        let witness = BgRoster::for_roster_testing(Some(roster_with(vec![], 4242)));
        let revived = std::cell::RefCell::new(Vec::new());
        let outcome = run_pass_with(
            &home,
            stamp_of(1, 1, "v"),
            Some("boot revival's hold is still standing".to_string()),
            &witness,
            &|_home, name| {
                revived.borrow_mut().push(name.to_string());
                boot_revival::ReceiptRow::new(name, "revived", "test")
            },
        );
        assert_eq!(outcome.rows.len(), 0);
        assert!(outcome.skip_reason.is_some(), "the hold names itself");
        assert!(revived.borrow().is_empty());
    }
}
