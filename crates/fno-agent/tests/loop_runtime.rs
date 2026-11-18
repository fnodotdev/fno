#![allow(dead_code)]

/// Integration tests for `fno-agent` loop_runtime module.
///
/// Each test drives `run_loop` directly via in-process mock impls of the
/// Queue / Dispatcher / Session traits. All file I/O uses temporary
/// directories; no network or real sessions are involved.
///
/// Test naming mirrors the acceptance criteria in the task spec:
///   1. empty_queue_terminates_nowork_without_dispatch
///   2. unit_closes_on_termination_event
///   3. no_event_exit_emits_node_failed_then_redispatches
///   4. iteration_ceiling_terminates_budget
///   5. preexisting_termination_skips_dispatch
///   6. cancel_returns_interrupted
///   7. zero_max_iterations_rejected
///   8. journal_write_failure_is_fatal
///   9. envelope_shape
use fno_agent::loop_runtime::{
    run_loop, Cancelled, CloseOutcome, DispatchCtx, Dispatcher, Evidence, Journal, LoopBudget,
    LoopError, Queue, Session, Unit,
};
// Note: tests use Journal::new_raw (cfg(test) convenience constructor) to avoid
// importing the newtype wrappers in every test.
use fno_agent::loopcheck::TerminationReason;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Build a minimal Unit for tests.
fn make_unit(id: &str, session_key: &str) -> Unit {
    Unit {
        id: id.to_string(),
        title: format!("Test unit {id}"),
        session_key: session_key.to_string(),
        plan_path: None,
    }
}

/// Write a valid termination event line into `journal_path` for `session_key`.
fn seed_termination_event(journal_path: &Path, session_key: &str, reason: &str) {
    let line = format!(
        "{{\"ts\":\"2026-06-06T00:00:00Z\",\"type\":\"termination\",\"source\":\"hook\",\
         \"data\":{{\"session_id\":\"{session_key}\",\"reason\":\"{reason}\",\"message\":\"pre-seeded\"}}}}\n"
    );
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal_path)
        .expect("seed termination event");
    f.write_all(line.as_bytes()).expect("write seed");
}

/// Read all lines from a JSONL file and parse them as JSON values.
fn read_jsonl(path: &Path) -> Vec<serde_json::Value> {
    if !path.exists() && !fno_agent::event_store::store_path(path).exists() {
        return vec![];
    }
    let content = fno_agent::event_store::journal_text(path, &[]);
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Count events of a given type in a JSONL file.
fn count_events(path: &Path, event_type: &str) -> usize {
    read_jsonl(path)
        .into_iter()
        .filter(|v| v["type"].as_str() == Some(event_type))
        .count()
}

fn hold_store_write_lock(journal: &Path) -> rusqlite::Connection {
    fno_agent::event_store::append_envelope(
        journal,
        r#"{"ts":"2026-06-06T00:00:00Z","type":"lock_seed","source":"test","data":{}}"#,
        Some("evt:lock-seed"),
    )
    .unwrap();
    let store = fno_agent::event_store::store_path(journal);
    let conn = rusqlite::Connection::open(&store).unwrap();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    conn
}

// ── mock impls ────────────────────────────────────────────────────────────────

/// A Queue that serves a fixed list of units in order, then returns None.
struct FixedQueue {
    units: Mutex<Vec<Unit>>,
    /// Records each unit.id passed to close().
    closed: Mutex<Vec<String>>,
    close_outcome: CloseOutcome,
}

impl FixedQueue {
    fn new(units: Vec<Unit>) -> Self {
        Self {
            units: Mutex::new(units),
            closed: Mutex::new(vec![]),
            close_outcome: CloseOutcome::Closed,
        }
    }
}

impl Queue for FixedQueue {
    fn next(&mut self) -> Result<Option<Unit>, LoopError> {
        let mut units = self.units.lock().unwrap();
        if units.is_empty() {
            Ok(None)
        } else {
            Ok(Some(units.remove(0)))
        }
    }

    fn has_pending(&mut self) -> Result<bool, LoopError> {
        Ok(!self.units.lock().unwrap().is_empty())
    }

    fn close(&mut self, unit: &Unit, _evidence: &Evidence) -> Result<CloseOutcome, LoopError> {
        self.closed.lock().unwrap().push(unit.id.clone());
        // Return a clone of the stored outcome.
        Ok(match &self.close_outcome {
            CloseOutcome::Closed => CloseOutcome::Closed,
            CloseOutcome::Refused(s) => CloseOutcome::Refused(s.clone()),
            CloseOutcome::Parked(s) => CloseOutcome::Parked(s.clone()),
            CloseOutcome::AwaitingMerge => CloseOutcome::AwaitingMerge,
        })
    }
}

/// A Dispatcher that panics if called (for "no dispatch expected" tests).
struct PanicDispatcher;

impl Dispatcher for PanicDispatcher {
    fn run(&self, _unit: &Unit, _ctx: &DispatchCtx) -> Result<Box<dyn Session>, LoopError> {
        panic!("PanicDispatcher: dispatch must not be called in this test");
    }
}

/// A Session that writes an optional termination event to a journal file, then
/// returns a fixed exit code. Used by MockDispatcher below.
struct MockSession {
    journal_path: PathBuf,
    session_key: String,
    termination_reason: Option<String>, // None -> write nothing
    exit_code: i32,
}

impl Session for MockSession {
    fn wait(&mut self) -> Result<i32, LoopError> {
        if let Some(reason) = &self.termination_reason {
            seed_termination_event(&self.journal_path, &self.session_key, reason);
        }
        Ok(self.exit_code)
    }
}

/// A Dispatcher whose sessions are configured by a list of responses (in order).
/// Each response specifies whether the session writes a termination event and
/// what exit code to return.
struct MockDispatcher {
    journal_path: PathBuf,
    /// Per-dispatch config: (termination_reason, exit_code). Consumed in order;
    /// last element is reused if the list is exhausted.
    responses: Mutex<Vec<(Option<String>, i32)>>,
    dispatch_count: AtomicU64,
}

impl MockDispatcher {
    fn new(journal_path: PathBuf, responses: Vec<(Option<String>, i32)>) -> Self {
        Self {
            journal_path,
            responses: Mutex::new(responses),
            dispatch_count: AtomicU64::new(0),
        }
    }

    fn count(&self) -> u64 {
        self.dispatch_count.load(Ordering::SeqCst)
    }
}

impl Dispatcher for MockDispatcher {
    fn run(&self, unit: &Unit, _ctx: &DispatchCtx) -> Result<Box<dyn Session>, LoopError> {
        self.dispatch_count.fetch_add(1, Ordering::SeqCst);
        let mut responses = self.responses.lock().unwrap();
        let (reason, exit_code) = if responses.len() > 1 {
            responses.remove(0)
        } else {
            // Reuse last response indefinitely.
            responses[0].clone()
        };
        Ok(Box::new(MockSession {
            journal_path: self.journal_path.clone(),
            session_key: unit.session_key.clone(),
            termination_reason: reason,
            exit_code,
        }))
    }
}

// ── test 1: empty queue terminates with NoWork, dispatcher not called ─────────

#[test]
fn empty_queue_terminates_nowork_without_dispatch() {
    let dir = TempDir::new().unwrap();
    let project_events = dir.path().join("events.jsonl");
    let global_events = dir.path().join("global-events.jsonl");

    let mut queue = FixedQueue::new(vec![]);
    let dispatcher = PanicDispatcher;
    let budget = LoopBudget::new(10).unwrap();
    let journal = Journal::new_raw(project_events.clone(), global_events);

    let outcome = run_loop(&mut queue, &dispatcher, &budget, &journal, &|| None, None).unwrap();

    assert_eq!(outcome.reason, TerminationReason::NoWork);
    assert_eq!(outcome.iterations_used, 0);
    assert!(outcome.units.is_empty());

    // loop_terminated event must be present in the project journal.
    let events = read_jsonl(&project_events);
    let terminated: Vec<_> = events
        .iter()
        .filter(|v| v["type"].as_str() == Some("loop_terminated"))
        .collect();
    assert_eq!(
        terminated.len(),
        1,
        "expected exactly one loop_terminated event"
    );
    assert_eq!(
        terminated[0]["data"]["reason"].as_str(),
        Some("NoWork"),
        "loop_terminated reason must be NoWork"
    );
}

// ── test 3: no event on exit -> node_failed then re-dispatch ─────────────────

#[test]
fn no_event_exit_emits_node_failed_then_redispatches() {
    let dir = TempDir::new().unwrap();
    let project_events = dir.path().join("events.jsonl");
    let global_events = dir.path().join("global-events.jsonl");

    let unit = make_unit("ab-002", "sess-002");
    let mut queue = FixedQueue::new(vec![unit]);
    // First session: writes nothing, exits 1.
    // Second session: writes DonePRGreen, exits 0.
    let dispatcher = MockDispatcher::new(
        project_events.clone(),
        vec![(None, 1), (Some("DonePRGreen".to_string()), 0)],
    );
    let budget = LoopBudget::new(10).unwrap();
    let journal = Journal::new_raw(project_events.clone(), global_events);

    let outcome = run_loop(&mut queue, &dispatcher, &budget, &journal, &|| None, None).unwrap();

    assert_eq!(outcome.reason, TerminationReason::NoWork);
    assert_eq!(outcome.iterations_used, 2);
    assert_eq!(
        outcome.units[0].evidence.reason,
        TerminationReason::DonePRGreen
    );

    // Exactly 2 dispatches.
    assert_eq!(dispatcher.count(), 2, "expected exactly 2 dispatches");

    // Exactly 1 node_failed (from first session).
    assert_eq!(
        count_events(&project_events, "node_failed"),
        1,
        "expected exactly one node_failed"
    );

    // 2 loop_unit_dispatched events.
    assert_eq!(
        count_events(&project_events, "loop_unit_dispatched"),
        2,
        "expected two loop_unit_dispatched events"
    );
}

// ── test 4: iteration ceiling terminates budget ────────────────────────────────

#[test]
fn iteration_ceiling_terminates_budget() {
    let dir = TempDir::new().unwrap();
    let project_events = dir.path().join("events.jsonl");
    let global_events = dir.path().join("global-events.jsonl");

    let unit = make_unit("ab-003", "sess-003");
    let mut queue = FixedQueue::new(vec![unit]);
    // Session always writes nothing, exits 1.
    let dispatcher = MockDispatcher::new(project_events.clone(), vec![(None, 1)]);
    let budget = LoopBudget::new(3).unwrap();
    let journal = Journal::new_raw(project_events.clone(), global_events);

    let outcome = run_loop(&mut queue, &dispatcher, &budget, &journal, &|| None, None).unwrap();

    assert_eq!(outcome.reason, TerminationReason::Budget);
    assert_eq!(
        outcome.iterations_used, 3,
        "should use exactly budget iterations"
    );
    assert_eq!(dispatcher.count(), 3, "expected exactly 3 dispatches");

    // 3 node_failed events.
    assert_eq!(
        count_events(&project_events, "node_failed"),
        3,
        "expected 3 node_failed"
    );

    // loop_terminated with axis=iterations.
    let events = read_jsonl(&project_events);
    let terminated: Vec<_> = events
        .iter()
        .filter(|v| v["type"].as_str() == Some("loop_terminated"))
        .collect();
    assert_eq!(terminated.len(), 1);
    assert_eq!(terminated[0]["data"]["reason"].as_str(), Some("Budget"));
    assert_eq!(
        terminated[0]["data"]["axis"].as_str(),
        Some("iterations"),
        "Budget termination must carry axis=iterations"
    );
}

// ── test 6: cancel returns Interrupted ────────────────────────────────────────

#[test]
fn cancel_returns_interrupted() {
    let dir = TempDir::new().unwrap();
    let project_events = dir.path().join("events.jsonl");
    let global_events = dir.path().join("global-events.jsonl");

    // cancel fires immediately (before any dispatch).
    let mut queue = FixedQueue::new(vec![make_unit("ab-005", "sess-005")]);
    let dispatcher = PanicDispatcher;
    let budget = LoopBudget::new(10).unwrap();
    let journal = Journal::new_raw(project_events.clone(), global_events);

    let cancel_flag = Arc::new(AtomicBool::new(true)); // pre-set to true
    let flag = cancel_flag.clone();
    let outcome = run_loop(
        &mut queue,
        &dispatcher,
        &budget,
        &journal,
        &move || {
            flag.load(Ordering::SeqCst).then_some(Cancelled {
                cause: "sigint",
                path: None,
                age_secs: None,
                clear_hint: String::new(),
                author: None,
                reason: None,
            })
        },
        None,
    )
    .unwrap();

    assert_eq!(outcome.reason, TerminationReason::Interrupted);

    let events = read_jsonl(&project_events);
    let terminated: Vec<_> = events
        .iter()
        .filter(|v| v["type"].as_str() == Some("loop_terminated"))
        .collect();
    assert_eq!(terminated.len(), 1);
    assert_eq!(
        terminated[0]["data"]["reason"].as_str(),
        Some("Interrupted")
    );
}

// ── test 8: journal write failure is fatal ────────────────────────────────────

#[test]
fn journal_write_failure_is_fatal() {
    let dir = TempDir::new().unwrap();
    // Create a REGULAR FILE at the path where the journal directory would need
    // to be created, so open(create=true) fails with EISDIR or ENOTDIR.
    // Specifically: place a file as the "parent" component of the events path
    // so that create_dir_all or open fails.
    let blocker = dir.path().join("blocker");
    fs::write(&blocker, b"not a dir").unwrap();
    // Make the project events path be inside the regular file (impossible path).
    let project_events = blocker.join("events.jsonl");
    let global_events = dir.path().join("global-events.jsonl");

    let mut queue = FixedQueue::new(vec![make_unit("ab-006", "sess-006")]);
    let dispatcher = PanicDispatcher; // should not be reached
    let budget = LoopBudget::new(5).unwrap();
    let journal = Journal::new_raw(project_events, global_events);

    let result = run_loop(&mut queue, &dispatcher, &budget, &journal, &|| None, None);
    assert!(
        result.is_err(),
        "journal write failure to project path must be fatal (Err)"
    );
}

#[test]
fn journal_project_lock_timeout_is_fatal() {
    let dir = TempDir::new().unwrap();
    let project_events = dir.path().join("events.jsonl");
    let global_events = dir.path().join("global-events.jsonl");
    let _store_lock = hold_store_write_lock(&project_events);
    let journal = Journal::new_raw(project_events, global_events);

    let result = journal.append("timeout_probe", serde_json::json!({}));
    assert!(
        matches!(result, Err(LoopError::Journal(ref message)) if message.contains("database is locked")),
        "project store lock timeout must stop the walk loudly: {result:?}"
    );
}

// ── bg-guard refusal (x-4504, AC1-ERR) ────────────────────────────────────────

/// A Session that returns a fixed exit code and a fixed `output_tail`, never
/// writing a termination event. Models a `claude --resume` that exited with the
/// bg-guard refusal message (or an ordinary crash, when the tail lacks it).
struct OutputSession {
    exit_code: i32,
    output_tail: Option<String>,
}

impl Session for OutputSession {
    fn wait(&mut self) -> Result<i32, LoopError> {
        Ok(self.exit_code)
    }
    fn output_tail(&self) -> Option<String> {
        self.output_tail.clone()
    }
}

/// A Dispatcher whose every session returns the same exit code + output tail.
struct OutputDispatcher {
    exit_code: i32,
    output_tail: Option<String>,
    dispatch_count: AtomicU64,
}

impl OutputDispatcher {
    fn new(exit_code: i32, output_tail: Option<&str>) -> Self {
        Self {
            exit_code,
            output_tail: output_tail.map(str::to_string),
            dispatch_count: AtomicU64::new(0),
        }
    }
    fn count(&self) -> u64 {
        self.dispatch_count.load(Ordering::SeqCst)
    }
}

impl Dispatcher for OutputDispatcher {
    fn run(&self, _unit: &Unit, _ctx: &DispatchCtx) -> Result<Box<dyn Session>, LoopError> {
        self.dispatch_count.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(OutputSession {
            exit_code: self.exit_code,
            output_tail: self.output_tail.clone(),
        }))
    }
}

/// AC1-ERR: a claude bg-guard refusal exit parks the unit after ONE dispatch —
/// it must not re-dispatch into a sustained respawn loop.
#[test]
fn bg_guard_refusal_parks_without_redispatch() {
    let dir = TempDir::new().unwrap();
    let project_events = dir.path().join("events.jsonl");
    let global_events = dir.path().join("global-events.jsonl");

    let unit = make_unit("ab-bg1", "sess-bg1");
    let mut queue = FixedQueue::new(vec![unit]);
    // exit 1 + the claude bg-guard message, never a termination event.
    let dispatcher = OutputDispatcher::new(
        1,
        Some("sess-bg1 is currently running as a background agent (bg). Use 'claude agents'."),
    );
    // Generous budget + NO per-unit cap: only the bg-guard branch can stop it,
    // so an unbounded budget would spin forever if the branch were absent.
    let budget = LoopBudget::new(100).unwrap();
    let journal = Journal::new_raw(project_events.clone(), global_events);

    let outcome = run_loop(&mut queue, &dispatcher, &budget, &journal, &|| None, None).unwrap();

    // The unit is parked (NoProgress) after exactly one dispatch; the walk then
    // drains the queue and finishes NoWork.
    assert_eq!(outcome.reason, TerminationReason::NoWork);
    assert_eq!(
        dispatcher.count(),
        1,
        "bg-guard refusal must NOT re-dispatch"
    );
    assert_eq!(
        outcome.units[0].evidence.reason,
        TerminationReason::NoProgress
    );

    // No node_failed (we did not treat it as a crash); exactly one dispatch.
    assert_eq!(
        count_events(&project_events, "node_failed"),
        0,
        "bg-guard refusal is terminal, not a crash-respawn"
    );
    assert_eq!(count_events(&project_events, "loop_unit_dispatched"), 1);

    // The bg-guard park is journaled as node_closed (like the per-unit-cap
    // park); the bg-guard identity lives in the in-memory evidence message.
    // It must NOT emit walk_paused (reserved for queue-level policy pauses,
    // schema.yaml enum consecutive_failures|p0_failed).
    assert_eq!(
        count_events(&project_events, "walk_paused"),
        0,
        "per-unit bg-guard park must not emit the queue-level walk_paused event"
    );
    assert_eq!(count_events(&project_events, "node_closed"), 1);
    assert!(
        outcome.units[0]
            .evidence
            .message
            .to_lowercase()
            .contains("bg-guard refusal"),
        "evidence must identify the bg-guard park"
    );
}

// ── store-committed terminations ──────────────────────────────────────────────

fn seed_store_termination(journal_path: &Path, session_key: &str, reason: &str, message: &str) {
    let line = format!(
        "{{\"ts\":\"2026-06-06T00:01:00Z\",\"type\":\"termination\",\"source\":\"hook\",\
         \"data\":{{\"session_id\":\"{session_key}\",\"reason\":\"{reason}\",\"message\":\"{message}\"}}}}"
    );
    fno_agent::event_store::append_envelope(journal_path, &line, None)
        .expect("commit termination to the store");
}

#[test]
fn a_store_committed_termination_supersedes_an_imported_raw_one() {
    // AC2-EDGE: raw NoProgress imported, later DonePRGreen store-only.
    let dir = TempDir::new().unwrap();
    let project_events = dir.path().join("events.jsonl");
    let global_events = dir.path().join("global-events.jsonl");
    seed_termination_event(&project_events, "sess-edge", "NoProgress");
    fno_agent::event_store::sync(&project_events).unwrap();
    seed_store_termination(&project_events, "sess-edge", "DonePRGreen", "newest");

    let journal = Journal::new_raw(project_events, global_events);
    let found = journal
        .find_termination("sess-edge")
        .expect("find_termination must not error");
    assert_eq!(
        found.unwrap().reason,
        TerminationReason::DonePRGreen,
        "the newest committed row wins, not the frozen live file"
    );
}
