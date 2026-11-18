use fno_agent::loop_runtime::{
    run_loop, Cancelled, CloseOutcome, DispatchCtx, Dispatcher, Journal, LoopBudget, LoopError,
    Queue, Session, Unit,
};
use fno_agent::loopcheck::TerminationReason;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct OneUnitQueue {
    unit: Option<Unit>,
}

impl OneUnitQueue {
    fn new() -> Self {
        Self {
            unit: Some(Unit {
                id: "lead-k".to_string(),
                title: "lead fixture".to_string(),
                session_key: "walk-k".to_string(),
                plan_path: None,
            }),
        }
    }
}

impl Queue for OneUnitQueue {
    fn next(&mut self) -> Result<Option<Unit>, LoopError> {
        Ok(self.unit.take())
    }

    fn has_pending(&mut self) -> Result<bool, LoopError> {
        Ok(self.unit.is_some())
    }

    fn close(
        &mut self,
        _unit: &Unit,
        _evidence: &fno_agent::loop_runtime::Evidence,
    ) -> Result<CloseOutcome, LoopError> {
        Ok(CloseOutcome::Closed)
    }
}

struct NoDispatch;

impl Dispatcher for NoDispatch {
    fn run(&self, _unit: &Unit, _ctx: &DispatchCtx) -> Result<Box<dyn Session>, LoopError> {
        panic!("cancelled walk must not dispatch")
    }
}

/// Serializes the tests that mutate process env. The lib's
/// `claims::test_env_lock` is #[cfg(test)]-gated, so an integration binary
/// carries its own.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn read_events(path: &Path) -> Vec<serde_json::Value> {
    // The child run pins FNO_AGENT_HOME, so its store resolves under the
    // fixture root; the reader derives the same pin or it falls back to the
    // retired jsonl and reads nothing.
    let _guard = ENV_LOCK.lock().unwrap();
    let prior = std::env::var_os("FNO_AGENT_HOME");
    let pin = path.parent().unwrap_or(path).join("agents");
    std::env::set_var("FNO_AGENT_HOME", &pin);
    let text = fno_agent::event_store::journal_text(path, &[]);
    match prior {
        Some(v) => std::env::set_var("FNO_AGENT_HOME", v),
        None => std::env::remove_var("FNO_AGENT_HOME"),
    }
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

const LOOP_BINARY: &str = env!("CARGO_BIN_EXE_fno-agent");

fn write_executable(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn lead_fixture() -> (TempDir, PathBuf, PathBuf, PathBuf) {
    let dir = TempDir::new().unwrap();
    let fno_dir = dir.path().join(".fno");
    fs::create_dir_all(&fno_dir).unwrap();
    let leads_dir = dir
        .path()
        .join(".fno")
        .join("spaces")
        .join(fno_agent::paths::space_slug(
            &fs::canonicalize(dir.path()).unwrap(),
        ))
        .join("leads");
    let lib_dir = dir.path().join("lib");
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&leads_dir).unwrap();
    fs::create_dir_all(&lib_dir).unwrap();
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &dir.path().join("fake-fno"),
        "if [ \"$1\" = \"agents\" ] && [ \"$2\" = \"hq\" ] && [ \"$4\" = \"drain\" ]; then\n\
         \x20 printf '%s\\n' '{\"scope\":\"k\",\"undelivered\":1}'\n\
         \x20 exit 0\n\
         fi\n\
         printf '%s\\n' '{\"actionable\":1,\"queues\":[{\"name\":\"ready\",\"actionable\":true,\"rows\":[{\"id\":\"row-1\"}]}]}'",
    );
    write_executable(&bin_dir.join("claude"), "exit 0");
    write_executable(
        &lib_dir.join("driver-claude-code.sh"),
        "driver_default_max() { echo 1; }\ndriver_invoke() { exit 0; }",
    );
    fs::write(
        leads_dir.join("k.md"),
        "---\nfno_id: fixture-lead\nscope: k\nrespawn_ceiling: 0\n---\n",
    )
    .unwrap();
    (dir, lib_dir, bin_dir, fno_dir.join("events.jsonl"))
}

fn run_lead(dir: &TempDir, lib_dir: &Path, bin_dir: &Path) -> Output {
    Command::new(LOOP_BINARY)
        .envs(fno_agent::test_run::self_owner_env())
        .args([
            "loop",
            "run",
            "--driver",
            "lead",
            "--dispatcher",
            "claude-code",
            "--driver-lib-dir",
            lib_dir.to_str().unwrap(),
            "--cwd",
            dir.path().to_str().unwrap(),
            "--scope",
            "k",
            "--max-iterations",
            "1",
        ])
        .env("PATH", format!("{}:/bin:/usr/bin", bin_dir.display()))
        .env("HOME", dir.path())
        .env("FNO_AGENT_HOME", dir.path().join(".fno/agents"))
        .env("FNO_LOOPCHECK_FNO_BIN", dir.path().join("fake-fno"))
        .output()
        .unwrap()
}

#[test]
fn sentinel_refusal_records_cause_path_and_age() {
    let dir = TempDir::new().unwrap();
    let events = dir.path().join(".fno/events.jsonl");
    let sentinel = dir.path().join(".fno/leads/k.cancelled");
    let cancelled = Cancelled {
        cause: "sentinel",
        path: Some(sentinel.clone()),
        age_secs: Some(2 * 60 * 60),
        clear_hint: "fno agent hq cancel --scope k --clear".to_string(),
        author: None,
        reason: None,
    };
    let mut queue = OneUnitQueue::new();
    let budget = LoopBudget::new(2).unwrap();
    let journal = Journal::new_raw(events.clone(), dir.path().join("global.jsonl"));
    let outcome = run_loop(
        &mut queue,
        &NoDispatch,
        &budget,
        &journal,
        &move || Some(cancelled.clone()),
        None,
    )
    .unwrap();

    assert_eq!(outcome.reason, TerminationReason::Interrupted);
    assert_eq!(outcome.iterations_used, 0);
    let event = read_events(&events)
        .into_iter()
        .find(|event| event["type"] == "loop_terminated")
        .expect("cancel refusal must emit loop_terminated");
    assert_eq!(event["data"]["cancel_cause"], "sentinel");
    assert_eq!(
        event["data"]["cancel_path"],
        sentinel.to_string_lossy().to_string()
    );
    assert_eq!(event["data"]["cancel_age_seconds"], 2 * 60 * 60);
}

#[test]
fn lead_cancel_refusal_names_its_file_age_and_clear_command() {
    let (dir, lib_dir, bin_dir, events) = lead_fixture();
    let sentinel = dir
        .path()
        .join(".fno")
        .join("spaces")
        .join(fno_agent::paths::space_slug(
            &fs::canonicalize(dir.path()).unwrap(),
        ))
        .join("leads/k.cancelled");
    fs::write(&sentinel, "").unwrap();
    let sentinel = sentinel.canonicalize().unwrap();

    let output = run_lead(&dir, &lib_dir, &bin_dir);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(130));
    assert!(
        stderr.contains(sentinel.to_string_lossy().as_ref()),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("age:"), "stderr: {stderr}");
    assert!(
        stderr.contains("fno agent hq cancel --scope k --clear"),
        "stderr: {stderr}"
    );
    let refusal = read_events(&events)
        .into_iter()
        .find(|event| event["type"] == "loop_terminated")
        .expect("lead refusal must emit loop_terminated");
    assert_eq!(refusal["data"]["cancel_cause"], "sentinel");
    assert_eq!(
        refusal["data"]["cancel_path"],
        sentinel.to_string_lossy().to_string()
    );
    assert!(refusal["data"]["cancel_age_seconds"].is_number());

    Command::new("touch")
        .args(["-t", "202608310000", sentinel.to_str().unwrap()])
        .status()
        .unwrap();
    let stale = run_lead(&dir, &lib_dir, &bin_dir);
    assert!(
        String::from_utf8_lossy(&stale.stderr).contains("stale:"),
        "stale cancel must be called out: {}",
        String::from_utf8_lossy(&stale.stderr)
    );
}
