//! The daemon arm that fires due scheduled-spawn rows through the normal
//! spawn front door, so the gate, overlays and naming stay one
//! implementation (the pr_watch fire_argv shape).

use crate::paths::AgentsHome;
use crate::spawn::spawn_schedule as sched;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const INTERVAL: Duration = Duration::from_secs(30);
const FIRE_TIMEOUT: f64 = 210.0;
const ON_POLL_TIMEOUT: f64 = 60.0;

pub(crate) const SEAM_ENV: &str = "FNO_SCHEDULE_FIRE_SEAM";
pub(crate) const KIND_FIRED: &str = "scheduled_spawn_fired";
pub(crate) const KIND_MISSED: &str = "scheduled_spawn_missed";
pub(crate) const KIND_SKIPPED: &str = "scheduled_spawn_skipped";
pub(crate) const KIND_REQUEUED: &str = "scheduled_spawn_requeued";

/// The idle-exit hold signal.
#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
    pending: Arc<AtomicBool>,
}

impl Arm {
    /// Whether the store held a pending row at the last arm pass.
    pub fn pending_signal(&self) -> bool {
        self.pending.load(Ordering::SeqCst)
    }
}

/// The config gate: config.schedule.enabled under config.autonomy.enabled.
/// Absence reads enabled, so a fresh install schedules.
pub(crate) fn schedule_enabled(project_root: Option<&Path>) -> bool {
    #[derive(Deserialize)]
    struct ArmBlock {
        #[serde(default)]
        enabled: Option<bool>,
    }
    #[derive(Deserialize)]
    struct Settings {
        #[serde(default)]
        schedule: Option<ArmBlock>,
        #[serde(default)]
        autonomy: Option<ArmBlock>,
    }
    let settings = crate::backlog::advance_settings::load_merged(project_root);
    let Ok(parsed) = serde_json::from_value::<Settings>(settings) else {
        return true;
    };
    let master = parsed.autonomy.and_then(|b| b.enabled).unwrap_or(true);
    parsed.schedule.and_then(|b| b.enabled).unwrap_or(true) && master
}

/// The fire decision for one due row at one instant.
pub(crate) enum FireDecision {
    Fire,
    Retry,
    Missed,
}

/// Fire, retry, missed, or skip, from the row and the instant alone.
pub(crate) fn decide(row: &sched::ScheduledSpawn, now: DateTime<Utc>) -> FireDecision {
    if let Some(fire_at) = row.fire_at_utc {
        if now > fire_at + chrono::Duration::seconds(row.grace_secs) {
            return FireDecision::Missed;
        }
        if fire_at > now {
            return FireDecision::Retry;
        }
        return FireDecision::Fire;
    }
    // An --on row: its grace counts from creation.
    if now > row.created_at_utc + chrono::Duration::seconds(row.grace_secs) {
        return FireDecision::Missed;
    }
    FireDecision::Retry
}

fn node_status_word(node: &str) -> Option<&'static str> {
    let row = crate::spawn::spawn_compose::graph_row_for(node)?;
    let facts = crate::backlog::row_facts::RowFacts::of(&row)?;
    let word = facts.status().as_str()?;
    match word {
        "done" | "superseded" | "deferred" => Some(word_static(word)),
        _ => None,
    }
}

fn word_static(word: &str) -> &'static str {
    match word {
        "done" => "done",
        "superseded" => "superseded",
        _ => "deferred",
    }
}

fn node_priority(row: &sched::ScheduledSpawn) -> u8 {
    // Plain body on purpose: a RowFacts view borrows the graph row it
    // reads, so threading it through and_then closures would return a
    // borrow of each closure's own parameter. Only the rank escapes.
    let Some(node) = row.node.as_deref() else {
        return 2;
    };
    let Some(graph_row) = crate::spawn::spawn_compose::graph_row_for(node) else {
        return 2;
    };
    let Some(facts) = crate::backlog::row_facts::RowFacts::of(&graph_row) else {
        return 2;
    };
    facts
        .priority()
        .as_str()
        .and_then(priority_rank)
        .unwrap_or(2)
}

fn priority_rank(word: &str) -> Option<u8> {
    match word {
        "p0" => Some(0),
        "p1" => Some(1),
        "p2" => Some(2),
        "p3" => Some(3),
        _ => Some(2),
    }
}

/// The argv the fire re-enters: the recorded argv plus --force when the
/// row recorded it, plus --spawned-by <scheduler> so lead share counts
/// against the scheduler while that session lives. A dead scheduler
/// passes the axis (spawn_gate_lanes share_reading), so a missed
/// scheduler never wedges the fire.
pub(crate) fn fire_argv(row: &sched::ScheduledSpawn) -> Vec<String> {
    let mut argv = row.argv.clone();
    let has_flag = |name: &str| {
        row.argv
            .iter()
            .any(|t| t == name || t.starts_with(&format!("{name}=")))
    };
    if row.force && !has_flag("--force") && !has_flag("-F") {
        argv.push("--force".into());
    }
    if !row.scheduler_session.is_empty() && !has_flag("--spawned-by") {
        argv.push("--spawned-by".into());
        argv.push(row.scheduler_session.clone());
    }
    if let Ok(seam) = std::env::var(SEAM_ENV) {
        if !seam.is_empty() {
            // The seam receives the recorded argv, so a probe asserts the
            // exact re-entry shape (AC3-HP).
            let mut seam_argv = vec![seam];
            seam_argv.extend(argv);
            return seam_argv;
        }
    }
    let mut full = vec![
        crate::scrape::fno_bin().to_string_lossy().into_owned(),
        "agents".into(),
        "spawn".into(),
    ];
    full.extend(argv);
    full
}

/// The reboot backstop: the pr-watch launchd tick (every 600s) calls this
/// so a reboot restarts the daemon inside one interval while the store
/// holds a pending row. The tick is sync and daemon-free, so the ensure
/// rides this binary's own `restart --if-down` subprocess: the idempotent
/// arm starts only a measured-down daemon and exits 0 when one serves. A
/// pinned lane is skipped; it never forks its own daemon.
pub(crate) fn reboot_backstop(home: &AgentsHome) {
    if std::env::var_os("FNO_SUPERVISOR_SOCKET").is_some_and(|v| !v.is_empty()) {
        return;
    }
    let Ok(rows) = sched::pending(home) else {
        return;
    };
    if rows.is_empty() {
        return;
    }
    if std::os::unix::net::UnixStream::connect(home.supervisor_sock()).is_ok() {
        return;
    }
    let Ok(exe) = crate::client_exe::current() else {
        return;
    };
    let args = vec!["restart".to_string(), "--if-down".to_string()];
    // R9: the backstop is best-effort; the next 600s tick retries.
    crate::gate::pr_watch::clock::run(crate::gate::pr_watch::clock::Call {
        program: exe.as_os_str(),
        args: &args,
        stdin: None,
        cwd: None,
        env: &[],
        timeout_s: 30.0,
    })
    .ok();
}

/// The arm entry: cadence 30s, one pass in flight, body off-loop.
pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    {
        let mut last = arm
            .last_tick
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if last.is_some_and(|tick| tick.elapsed() < INTERVAL)
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    let pending = Arc::clone(&arm.pending);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        run_pass(&home, &pending);
    });
}

fn run_pass(home: &AgentsHome, pending: &AtomicBool) {
    let now = Utc::now();
    let rows = sched::pending(home).unwrap_or_default();
    pending.store(!rows.is_empty(), Ordering::SeqCst);
    if !schedule_enabled(None) {
        // AC5-ERR: a due row under a disabled gate journals skipped with
        // reason disabled and fires nothing; the rows stay pending so
        // re-enabling the gate lets them fire later.
        for row in rows.iter().filter(|r| is_due(r, now)) {
            journal(home, KIND_SKIPPED, row, Some("disabled"));
        }
        return;
    }
    for row in rows
        .iter()
        .filter(|r| matches!(r.trigger, sched::Trigger::On { .. }))
    {
        poll_on_row(home, row, now);
    }
    let due_rows: Vec<sched::ScheduledSpawn> =
        rows.into_iter().filter(|r| is_due(r, now)).collect();
    // Rows due in one minute fire in node priority order (AC3-HP): the
    // live graph read decides, p2 for an absent node, then fire time,
    // then id.
    let mut keyed: Vec<(
        u8,
        Option<chrono::DateTime<Utc>>,
        String,
        sched::ScheduledSpawn,
    )> = due_rows
        .into_iter()
        .map(|row| (node_priority(&row), row.fire_at_utc, row.id.clone(), row))
        .collect();
    keyed.sort_by(|a, b| {
        // Tuples carry no Index; compare the three key fields directly.
        (&a.0, &a.1, &a.2).cmp(&(&b.0, &b.1, &b.2))
    });
    for (_prio, _fire, _id, row) in keyed {
        // The gate never refuses a done, superseded or deferred node
        // (premise P6), so the fire path reads the status itself.
        if row
            .node
            .as_deref()
            .is_some_and(|n| node_status_word(n).is_some())
        {
            // R9: a lost write costs nothing; the journal row below is the
            // durable record.
            sched::set_state(
                home,
                &row.id,
                sched::RowState::Skipped,
                Some("node done, superseded or deferred"),
            )
            .ok();
            journal(home, KIND_SKIPPED, &row, Some("node status"));
            continue;
        }
        match decide(&row, now) {
            FireDecision::Fire => fire_row(home, &row, now),
            FireDecision::Retry => {}
            FireDecision::Missed => {
                // R9: a lost state write is read by the next pass; the
                // journal row below is the durable record either way.
                sched::set_state(home, &row.id, sched::RowState::Missed, None).ok();
                journal(home, KIND_MISSED, &row, Some("past grace"));
                mail_scheduler(&row, "past its grace window");
            }
        }
    }
}

/// Whether a timed row is due: past fire time, outside the retry backoff.
fn is_due(row: &sched::ScheduledSpawn, now: DateTime<Utc>) -> bool {
    let Some(fire_at) = row.fire_at_utc else {
        return false;
    };
    if fire_at > now {
        return false;
    }
    if let Some(attempt) = row.last_attempt_utc {
        if now < attempt + chrono::Duration::seconds(sched::RETRY_SECS) {
            return false;
        }
    }
    true
}

/// One fired spawn: runs the recorded argv through the front door under
/// the 210s bound, journals, and re-queues an --every row.
fn fire_row(home: &AgentsHome, row: &sched::ScheduledSpawn, now: DateTime<Utc>) {
    let argv = fire_argv(row);
    if argv.is_empty() {
        return;
    }
    let Some((program, args)) = argv.split_first() else {
        return;
    };
    let output = crate::gate::pr_watch::clock::run(crate::gate::pr_watch::clock::Call {
        program: std::ffi::OsStr::new(program),
        args,
        stdin: None,
        // The re-entry runs in the row's cwd: a daemon fires rows for any
        // project, and the daemon's own cwd is never the row's.
        cwd: Some(row.cwd.as_path()),
        env: &[],
        timeout_s: FIRE_TIMEOUT,
    });
    let reading = match &output {
        Ok(out) => {
            let tail = out
                .stderr
                .lines()
                .rev()
                .take(3)
                .collect::<Vec<_>>()
                .join("; ");
            if tail.is_empty() {
                format!("spawn exited {}", out.code)
            } else {
                tail
            }
        }
        Err(_) => "fire subprocess failed".to_string(),
    };
    let fired = matches!(&output, Ok(out) if out.code == 0);
    if fired {
        // R9: the journal row below is the durable record of the fire.
        sched::set_state(home, &row.id, sched::RowState::Fired, None).ok();
        journal(home, KIND_FIRED, row, Some(reading.as_str()));
        if let sched::Trigger::Every { interval_secs } = &row.trigger {
            let next = row.fire_at_utc.unwrap_or(now) + chrono::Duration::seconds(*interval_secs);
            if sched::requeue(home, &row.id, next).is_ok() {
                journal(home, KIND_REQUEUED, row, None);
            }
        }
    } else {
        // R9: a lost attempt write only costs one extra 5m retry.
        sched::record_attempt(home, &row.id, Some(reading.as_str()), now).ok();
        // The retry consumed the grace: close the row missed now.
        if matches!(decide(row, now), FireDecision::Missed) {
            // R9: the journal row below is the durable record either way.
            sched::set_state(
                home,
                &row.id,
                sched::RowState::Missed,
                Some(reading.as_str()),
            )
            .ok();
            journal(home, KIND_MISSED, row, Some(reading.as_str()));
            mail_scheduler(row, reading.as_str());
        }
    }
}

/// One --on poll: runs the row's command under sh -c with the row's cwd
/// under the 60s bound with the process-group kill; exit 0 fires the
/// row, anything else leaves it pending for the next pass.
fn poll_on_row(home: &AgentsHome, row: &sched::ScheduledSpawn, now: DateTime<Utc>) {
    let sched::Trigger::On { command } = &row.trigger else {
        return;
    };
    let args = vec!["-c".to_string(), command.clone()];
    let output = crate::gate::pr_watch::clock::run(crate::gate::pr_watch::clock::Call {
        program: std::ffi::OsStr::new("/bin/sh"),
        args: &args,
        stdin: None,
        cwd: Some(row.cwd.as_path()),
        env: &[],
        timeout_s: ON_POLL_TIMEOUT,
    });
    let exited_zero = matches!(&output, Ok(out) if out.code == 0);
    if exited_zero {
        fire_row(home, row, now);
    }
}

/// One journal row plus the scheduler mail, addressed to the row's
/// scheduler session. Mail carries no session-id lane, so the send
/// addresses the registry name that resolves today and journals the
/// session id either way.
fn mail_scheduler(row: &sched::ScheduledSpawn, reason: &str) {
    if row.scheduler_session.is_empty() {
        return;
    }
    let body = format!(
        "scheduled spawn {} missed: {}. Re-schedule with: fno agent spawn --when <time> <name> <prompt>",
        row.id, reason
    );
    // R9: mail is best-effort; the journal row is the durable record.
    crate::lead_mail::send_to_name("fno-scheduler", &row.scheduler_session, &body).ok();
}

/// One agents journal row for the fired/missed/skipped/requeued kinds.
fn journal(home: &AgentsHome, kind: &str, row: &sched::ScheduledSpawn, detail: Option<&str>) {
    let _ = crate::runtime::agents_event::append_agents_event(
        &home.events_jsonl(),
        kind,
        &[
            ("row", serde_json::json!(row.id)),
            ("node", serde_json::json!(row.node)),
            (
                "scheduler_session",
                serde_json::json!(row.scheduler_session),
            ),
            ("argv", serde_json::json!(row.argv)),
            ("detail", serde_json::json!(detail)),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn::spawn_schedule::{RowState, Trigger};
    use chrono::TimeZone;

    fn now_utc() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 22, 12, 0, 0).unwrap()
    }

    fn sample_row(trigger: Trigger) -> sched::ScheduledSpawn {
        sched::ScheduledSpawn {
            id: "ss-test".into(),
            fire_at_utc: None,
            trigger,
            grace_secs: 3600,
            force: false,
            node: None,
            argv: vec!["w1".into(), "say hi".into()],
            cwd: std::path::PathBuf::from("/tmp"),
            scheduler_session: String::new(),
            state: RowState::Pending,
            last_gate_reading: None,
            created_at_utc: now_utc(),
            last_attempt_utc: None,
        }
    }

    #[test]
    fn a_timed_row_due_now_decides_fire() {
        let mut row = sample_row(Trigger::At);
        row.fire_at_utc = Some(now_utc() - chrono::Duration::minutes(1));
        assert!(matches!(decide(&row, now_utc()), FireDecision::Fire));
    }

    #[test]
    fn a_row_past_grace_decides_missed() {
        let mut row = sample_row(Trigger::At);
        row.fire_at_utc = Some(now_utc() - chrono::Duration::hours(2));
        assert!(matches!(decide(&row, now_utc()), FireDecision::Missed));
    }

    #[test]
    fn a_future_row_waits() {
        let mut row = sample_row(Trigger::At);
        row.fire_at_utc = Some(now_utc() + chrono::Duration::hours(1));
        assert!(matches!(decide(&row, now_utc()), FireDecision::Retry));
    }

    #[test]
    fn an_on_row_counts_its_grace_from_creation() {
        let mut row = sample_row(Trigger::On {
            command: "true".into(),
        });
        row.created_at_utc = now_utc() - chrono::Duration::hours(2);
        assert!(matches!(decide(&row, now_utc()), FireDecision::Missed));
        row.created_at_utc = now_utc();
        assert!(matches!(decide(&row, now_utc()), FireDecision::Retry));
    }

    #[test]
    fn the_fire_argv_reenters_with_force_and_scheduler() {
        let mut row = sample_row(Trigger::At);
        row.force = true;
        row.scheduler_session = "t-sched".into();
        let argv = fire_argv(&row);
        assert!(argv.contains(&"--force".to_string()));
        assert!(argv.contains(&"--spawned-by".to_string()));
        assert!(argv.contains(&"t-sched".to_string()));
        assert!(argv
            .windows(2)
            .any(|w| w == ["agents".to_string(), "spawn".to_string()]));
    }
}
