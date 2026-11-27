//! Which registry rows are live for the worker-slot cap. Split from
//! spawn_gate under the file budget: the cap's one liveness filter, the
//! report-freshness source that reaches rows no probe can, and the tests
//! that pin it. `spawn_gate` re-exports [`live_rows`] so every existing
//! reader keeps its path.

use crate::harness::Harness;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

use crate::claude_roster::ClaudeRoster;
use crate::daemon::pid_is_ours;
use crate::spawn_gate::NOTE;
use crate::state::{load_registry, Registry, RegistryEntry};

/// The liveness-filtered registry rows behind [`slot_count`], exposed so the
/// per-territory cap can read the rows' worked NODES without a second
/// liveness implementation.
pub(crate) fn live_rows(registry_path: &Path, warnings: &mut Vec<String>) -> Vec<RegistryEntry> {
    match live_rows_checked(registry_path, warnings) {
        LiveRead::Rows { rows, .. } => rows,
        // The slot cap degrades to 0 on an unreadable registry; callers that
        // must not act on a degraded read use [`live_rows_checked`].
        LiveRead::RegistryUnreadable(_) => Vec::new(),
    }
}

/// A liveness read that keeps its failure. Zero rows from an unreadable
/// registry is a degraded answer, not an empty fleet: a caller whose next
/// step would erase state it failed to see (a scope sweep dropping ladders)
/// holds on [`LiveRead::RegistryUnreadable`] instead. A read that holds rows
/// also carries the live-status worker rows it dropped, counted by reason.
pub(crate) enum LiveRead {
    Rows {
        rows: Vec<RegistryEntry>,
        dropped: Dropped,
    },
    RegistryUnreadable(String),
}

/// Why one live-status row took no worker slot.
#[derive(Debug, Clone, Copy)]
enum DropReason {
    /// Its inside-leg report reads done: a finished worker frees its slot.
    ReportedDone,
    /// Its session holds a terminal-run receipt.
    TerminalRun,
    /// Its recorded pid, with its start time, is gone.
    DeadPid,
    /// A claude fleet mint the roster does not list.
    RosterMiss,
    /// No roster answers for it, and its last session report is absent or
    /// older than a day.
    StaleReport,
    /// The harness reports its claude session usage-blocked.
    UsageBlocked,
}

/// The live-status rows one read dropped, counted by reason. The six counts
/// cover worker rows only, so their sum plus the worker rows kept is the
/// live-status non-lead row count of the registry read.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Dropped {
    reported_done: usize,
    terminal_run: usize,
    dead_pid: usize,
    roster_miss: usize,
    stale_report: usize,
    usage_blocked: usize,
    /// Lead rows dropped. A lead takes no slot, so each sits outside the six.
    leads: usize,
}

impl Dropped {
    fn note(&mut self, reason: DropReason) {
        match reason {
            DropReason::ReportedDone => self.reported_done += 1,
            DropReason::TerminalRun => self.terminal_run += 1,
            DropReason::DeadPid => self.dead_pid += 1,
            DropReason::RosterMiss => self.roster_miss += 1,
            DropReason::StaleReport => self.stale_report += 1,
            DropReason::UsageBlocked => self.usage_blocked += 1,
        }
    }

    /// Every worker row dropped.
    fn total(&self) -> usize {
        self.reported_done
            + self.terminal_run
            + self.dead_pid
            + self.roster_miss
            + self.stale_report
            + self.usage_blocked
    }

    /// The probe's `dropped` object. Every reason is present, a zero count
    /// too, so a reader sees each way a row can drop.
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "total": self.total(),
            "reported_done": self.reported_done,
            "terminal_run": self.terminal_run,
            "dead_pid": self.dead_pid,
            "roster_miss": self.roster_miss,
            "stale_report": self.stale_report,
            "usage_blocked": self.usage_blocked,
            "leads": self.leads,
        })
    }

    /// One note line with the same counts, for the readers that print notes.
    /// `None` when the read dropped nothing.
    pub(crate) fn note_line(&self) -> Option<String> {
        if self.total() == 0 && self.leads == 0 {
            return None;
        }
        let mut line = format!(
            "{NOTE} {} live worker row(s) dropped from the slot count: reported done {}, \
             terminal run {}, dead pid {}, roster miss {}, stale report {}, usage blocked {}",
            self.total(),
            self.reported_done,
            self.terminal_run,
            self.dead_pid,
            self.roster_miss,
            self.stale_report,
            self.usage_blocked
        );
        if self.leads > 0 {
            line.push_str(&format!("; {} lead row(s) dropped", self.leads));
        }
        Some(line)
    }
}

pub(crate) fn live_rows_checked(registry_path: &Path, warnings: &mut Vec<String>) -> LiveRead {
    live_rows_with_blocked(
        registry_path,
        &crate::claude_roster::read_blocked_short_ids,
        warnings,
    )
}

/// [`live_rows`] with the harness blocked-set read injected: tests pin the
/// set instead of shelling out to `claude agents`.
fn live_rows_with_blocked(
    registry_path: &Path,
    read_blocked: &dyn Fn() -> std::collections::HashSet<String>,
    warnings: &mut Vec<String>,
) -> LiveRead {
    let live_roster_short_ids: std::collections::HashSet<String> = match ClaudeRoster::load_default(
    ) {
        Ok(roster) => roster
            .workers_deduped()
            .iter()
            .filter(|w| w.pid.map(|p| pid_is_ours(p, w.proc_start)).unwrap_or(false))
            .map(|w| w.short_id().to_string())
            .collect(),
        Err(e) => {
            warnings.push(format!(
                    "{NOTE} claude roster unreadable ({e}); pid-less roster-backed rows uncounted, report-stamped rows still counted"
                ));
            Default::default()
        }
    };
    let now_secs = crate::daemon::now_epoch_secs();
    let home = crate::paths::AgentsHome::at(registry_path.parent().unwrap_or(Path::new(".")));
    let terminal = crate::terminal_run::read_keys(&home.terminal_runs_dir());
    let mut rows = Vec::new();
    let mut dropped = Dropped::default();
    let mut blocked: Option<std::collections::HashSet<String>> = None;
    let mut dropped_blocked: Vec<String> = Vec::new();
    match load_registry(registry_path) {
        Ok(Registry { entries, .. }) => {
            for e in entries {
                if !crate::spawn_gate::status_is_liveish(&e.status) {
                    continue;
                }
                let mut reason = drop_reason(&e, &terminal, &live_roster_short_ids, now_secs);
                if reason.is_none() && e.harness_kind() == Harness::Claude {
                    // A usage-blocked session holds no working slot: the
                    // harness's own state is the witness transcript recency
                    // cannot give. Positive readings only - a row absent
                    // from the agents list, or a root that failed to parse,
                    // answers nothing here; the roster polices death.
                    let blocked = blocked.get_or_insert_with(|| read_blocked());
                    if e.transport_short().is_some_and(|sid| blocked.contains(sid)) {
                        reason = Some(DropReason::UsageBlocked);
                        dropped_blocked.push(e.name.clone());
                    }
                }
                match reason {
                    None => rows.push(e),
                    // A lead takes no slot, so its drop sits outside the worker reasons.
                    Some(_) if e.role_level.is_some() => dropped.leads += 1,
                    Some(reason) => dropped.note(reason),
                }
            }
        }
        Err(e) => {
            warnings.push(format!(
                "{NOTE} fno registry unreadable ({e}); slot count degraded to 0"
            ));
            return LiveRead::RegistryUnreadable(e.to_string());
        }
    }
    if !dropped_blocked.is_empty() {
        warnings.push(format!(
            "{NOTE} {} claude row(s) usage-blocked per claude agents, slot(s) freed: {}",
            dropped_blocked.len(),
            dropped_blocked.join(", ")
        ))
    }
    LiveRead::Rows { rows, dropped }
}

/// The evidence that a row released its capacity, or `None` while it holds.
/// [`holds_capacity`] is this read answering nothing.
fn capacity_release(
    row: &RegistryEntry,
    terminal: &std::collections::HashSet<String>,
) -> Option<DropReason> {
    if crate::reap_evidence::reported_done(row, crate::daemon::now_epoch_secs().max(0) as u64) {
        Some(DropReason::ReportedDone)
    } else if row
        .harness_session_id
        .as_deref()
        .is_some_and(|sid| terminal.contains(sid))
    {
        Some(DropReason::TerminalRun)
    } else {
        None
    }
}

pub(crate) fn holds_capacity(
    row: &RegistryEntry,
    terminal: &std::collections::HashSet<String>,
) -> bool {
    capacity_release(row, terminal).is_none()
}

/// Why a live-status row takes no slot, or `None` while it stays. The first
/// reason that holds wins, so the reasons split the dropped rows: each row
/// lands in one count.
fn drop_reason(
    row: &RegistryEntry,
    terminal: &std::collections::HashSet<String>,
    live_roster_short_ids: &std::collections::HashSet<String>,
    now_secs: i64,
) -> Option<DropReason> {
    // A lead takes no worker slot, so capacity evidence keeps its count.
    // Only death or the harness's blocked state drops it.
    let released = match row.role_level {
        Some(_) => None,
        None => capacity_release(row, terminal),
    };
    released.or_else(|| death_reason(row, live_roster_short_ids, now_secs))
}

/// Why a live-status row is gone, or `None` while it lives.
fn death_reason(
    row: &RegistryEntry,
    live_roster_short_ids: &std::collections::HashSet<String>,
    now_secs: i64,
) -> Option<DropReason> {
    match row.pid {
        Some(p) if !pid_is_ours(p, row.pid_start_time) => Some(DropReason::DeadPid),
        Some(_) => None,
        None => {
            let roster_live = row
                .transport_short()
                .map(|sid| live_roster_short_ids.contains(sid));
            match roster_live {
                Some(true) => None,
                // The roster polices this row (a claude fleet mint) and
                // answers no: dead, at once. A dead bg worker frees its
                // slot.
                Some(false) if is_roster_policed(row) => Some(DropReason::RosterMiss),
                // No oracle answers for this row: the user's own session, a
                // non-claude native, or an unreadable roster. The last
                // session report's freshness stands in.
                _ => (!report_is_fresh(row, now_secs)).then_some(DropReason::StaleReport),
            }
        }
    }
}

/// How long one session report asserts occupancy for a pid-less row the
/// roster cannot answer. SessionStart re-fires on resume, clear and compact,
/// so a working session refreshes the stamp; a vanished session occupies a
/// slot at most this long after its last report. 24h: a user chat spans a
/// workday, and no shorter bound may drop a live chat mid-session
/// (d-d0c63de7). Bounded, never terminal (d-10a72d88): no exit is modeled.
const REPORT_FRESHNESS: Duration = Duration::from_secs(24 * 60 * 60);

/// The row shapes the claude roster POLICES: a claude bg worker minted by
/// the ask lane (origin `spawn`, substrate `thread`, no pane ref, no
/// unproven-lineage receipt), whose liveness the daemon roster settles. A
/// roster miss on one of these is a definitive death. Every other pid-less
/// row - the user's own session (origin `operator`, d-d0c63de7), an adopted
/// row, a non-claude native, a hosted pane row whose pid lookup failed, a
/// spawned row re-minted at its session report - is outside the roster's
/// jurisdiction, and its report freshness is the evidence.
fn is_roster_policed(e: &RegistryEntry) -> bool {
    if e.harness_kind() != Harness::Claude || e.origin.as_deref() != Some("spawn") {
        return false;
    }
    // A hosted pane row is not a daemon job: pid None is a supported
    // outcome there (the pane pid lookup can fail) and the roster never
    // lists it. The mux ref the pane restamp heals, or an explicit
    // interactive host mode, names the shape.
    if e.mux.is_some() || e.host_mode.as_deref() == Some(crate::state::HOST_MODE_INTERACTIVE) {
        return false;
    }
    // A spawned row whose original went missing before its first report
    // re-mints at that report under an unproven lineage; the receipt names
    // it, and freshness is its evidence too.
    if e.lineage_reason.as_deref().is_some_and(|r| !r.is_empty()) {
        return false;
    }
    true
}

/// Whether the row's last session report is inside [`REPORT_FRESHNESS`].
/// Absent or unparseable fails closed (never counted) - the discipline of
/// `InsideLegReport::is_live_at`.
fn report_is_fresh(e: &RegistryEntry, now_secs: i64) -> bool {
    match e
        .last_report_at
        .as_deref()
        .and_then(crate::state::rfc3339_like_to_secs)
    {
        Some(at) => now_secs.saturating_sub(at as i64) <= REPORT_FRESHNESS.as_secs() as i64,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RegistryEntry;
    use crate::{claims, AgentStatus};

    /// A pid-less session-report row: the native-adopt shape. `short_id` is
    /// set (every mint derives one) but absent from the pinned empty roster,
    /// which is exactly the population `live_rows` cannot answer today.
    fn pidless_reported_row(name: &str, origin: &str, harness: &str) -> RegistryEntry {
        let mut e = RegistryEntry::default();
        e.name = name.into();
        e.short_id = format!("{name}-transport");
        e.status = AgentStatus::Busy;
        e.origin = Some(origin.into());
        e.harness = Some(harness.into());
        e
    }

    /// The wall clock `live_rows` reads, rendered as a registry stamp.
    fn stamp_ago(secs: u64) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let t = chrono::DateTime::from_timestamp((now - secs) as i64, 0).unwrap();
        t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
    }

    /// The empty-roster pin every live_rows liveness test needs: an empty
    /// daemon dir reads as a roster with zero workers, so no real worker on
    /// this machine can answer a row's transport lookup. The caller holds
    /// the env lock.
    fn empty_roster_dir() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("daemon")).unwrap();
        std::env::set_var("FNO_CLAUDE_DAEMON_DIR", dir.path().join("daemon"));
        std::env::set_var("FNO_CLAIMS_ROOT", dir.path().join("claims-root"));
        dir
    }

    fn write_reg(dir: &tempfile::TempDir, entries: &[RegistryEntry]) -> std::path::PathBuf {
        let reg = dir.path().join("registry.json");
        let rows: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| serde_json::to_value(e).unwrap())
            .collect();
        std::fs::write(
            &reg,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "entries": rows
            }))
            .unwrap(),
        )
        .unwrap();
        reg
    }

    #[test]
    fn a_fresh_session_report_makes_a_pidless_native_row_an_occupant() {
        let _g = claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = empty_roster_dir();
        let mut op = pidless_reported_row("user-chat", "operator", "claude");
        op.last_report_at = Some(stamp_ago(3600));
        // A native launch child: origin spawn, but the roster is claude-only,
        // so a codex row is outside its jurisdiction.
        let mut codex_native = pidless_reported_row("codex-native", "spawn", "codex");
        codex_native.last_report_at = Some(stamp_ago(3600));
        let mut done = codex_native.clone();
        done.name = "finished-child".into();
        done.inside_leg = Some(crate::state::InsideLegReport {
            state: crate::state::InsideLegState::Done,
            received_at: stamp_ago(60),
            ..Default::default()
        });
        let mut parked = codex_native.clone();
        parked.name = "parked-run".into();
        parked.harness_session_id = Some("parked-run".into());
        crate::terminal_run::write_receipt(
            &crate::paths::AgentsHome::at(dir.path()),
            "parked-run",
            "DonePRGreen",
        )
        .unwrap();
        let reg = write_reg(&dir, &[op, codex_native, done, parked]);
        let mut warnings = Vec::new();
        let rows = live_rows(&reg, &mut warnings);
        assert_eq!(rows.len(), 2, "{warnings:?}");
        assert_eq!(crate::spawn_gate::slot_count(&reg, &mut warnings), 2);
        std::env::remove_var("FNO_CLAUDE_DAEMON_DIR");
        std::env::remove_var("FNO_CLAIMS_ROOT");
    }

    #[test]
    fn a_stale_report_drops_the_occupant() {
        let _g = claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = empty_roster_dir();
        let mut op = pidless_reported_row("user-chat", "operator", "claude");
        op.last_report_at = Some(stamp_ago(25 * 3600));
        let reg = write_reg(&dir, &[op]);
        let mut warnings = Vec::new();
        let rows = live_rows(&reg, &mut warnings);
        assert_eq!(rows.len(), 0, "{warnings:?}");
        std::env::remove_var("FNO_CLAUDE_DAEMON_DIR");
        std::env::remove_var("FNO_CLAIMS_ROOT");
    }

    /// Every way a live-status row drops lands in its own count, a dropped
    /// lead row in its own, and the kept rows plus the tally are every
    /// live-status row read. Both roster-backed rows ride our own live pid,
    /// so the roster answers live for both: the harness's blocked reading is
    /// the only witness that drops one, and the other is the control.
    #[test]
    fn the_tally_counts_each_dropped_row_under_one_reason() {
        let _g = claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = empty_roster_dir();
        let start = crate::daemon::process_start_time(std::process::id())
            .expect("our own start time reads");
        let worker = |short: &str| {
            serde_json::json!({
                "sessionId": format!("{short}-0000-0000-0000-000000000000"),
                "pid": std::process::id(),
                "procStart": start,
            })
        };
        std::fs::write(
            dir.path().join("daemon/roster.json"),
            serde_json::to_vec(&serde_json::json!({
                "proto": 1,
                "workers": {"deadbeef": worker("deadbeef"), "aaaa1111": worker("aaaa1111")}
            }))
            .unwrap(),
        )
        .unwrap();
        let fleet = |name: &str, short: &str| {
            let mut e = pidless_reported_row(name, "spawn", "claude");
            e.short_id = short.into();
            e
        };
        let mut lead = pidless_reported_row("lead", "operator", "codex");
        lead.role_level = Some(1);
        lead.last_report_at = Some(stamp_ago(60));
        let mut reported_done = pidless_reported_row("reported-done", "spawn", "codex");
        reported_done.inside_leg = Some(crate::state::InsideLegReport {
            state: crate::state::InsideLegState::Done,
            received_at: stamp_ago(60),
            ..Default::default()
        });
        let mut terminal_run = pidless_reported_row("terminal-run", "spawn", "codex");
        terminal_run.harness_session_id = Some("terminal-run".into());
        crate::terminal_run::write_receipt(
            &crate::paths::AgentsHome::at(dir.path()),
            "terminal-run",
            "DonePRGreen",
        )
        .unwrap();
        // 2^22+17: realistically never a live pid.
        let mut dead_pid = pidless_reported_row("dead-pid", "spawn", "codex");
        dead_pid.pid = Some(4_194_321);
        let mut dropped_lead = pidless_reported_row("dropped-lead", "operator", "codex");
        dropped_lead.role_level = Some(1);
        let mut exited = pidless_reported_row("exited", "spawn", "codex");
        exited.status = AgentStatus::Exited;
        let reg = write_reg(
            &dir,
            &[
                fleet("kept", "aaaa1111"),
                lead,
                reported_done,
                terminal_run,
                dead_pid,
                fleet("roster-miss", "miss0001"),
                pidless_reported_row("stale-report", "operator", "codex"),
                dropped_lead,
                fleet("usage-blocked", "deadbeef"),
                exited,
            ],
        );
        let mut warnings = Vec::new();
        let LiveRead::Rows { rows, dropped } = live_rows_with_blocked(
            &reg,
            &|| std::collections::HashSet::from(["deadbeef".to_string()]),
            &mut warnings,
        ) else {
            panic!("registry readable, so the read must carry rows")
        };
        let kept_names: Vec<&str> = rows.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(kept_names, ["kept", "lead"]);
        assert_eq!(
            dropped.to_json(),
            json!({
                "total": 6,
                "reported_done": 1,
                "terminal_run": 1,
                "dead_pid": 1,
                "roster_miss": 1,
                "stale_report": 1,
                "usage_blocked": 1,
                "leads": 1,
            })
        );
        // Nine of the ten rows carry a live status. Each one is kept, dropped
        // as a worker, or dropped as a lead.
        assert_eq!(rows.len() + dropped.total() + dropped.leads, 9);
        assert_eq!(
            dropped.note_line(),
            Some(format!(
                "{NOTE} 6 live worker row(s) dropped from the slot count: reported done 1, \
                 terminal run 1, dead pid 1, roster miss 1, stale report 1, usage blocked 1; \
                 1 lead row(s) dropped"
            ))
        );
        assert_eq!(Dropped::default().note_line(), None);
        assert!(
            warnings.iter().any(|w| w.contains("usage-blocked")),
            "{warnings:?}"
        );
        std::env::remove_var("FNO_CLAUDE_DAEMON_DIR");
        std::env::remove_var("FNO_CLAIMS_ROOT");
    }

    #[test]
    fn an_unreadable_registry_reads_unreadable_not_empty() {
        let dir = tempfile::tempdir().unwrap();
        let reg = dir.path().join("registry.json");
        std::fs::write(&reg, "not json at all").unwrap();
        let mut warnings = Vec::new();
        let read = live_rows_checked(&reg, &mut warnings);
        assert!(
            matches!(read, LiveRead::RegistryUnreadable(_)),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("registry unreadable")),
            "{warnings:?}"
        );
    }
}
