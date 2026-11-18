//! `evals-arm` tests: one per surviving deleted phase test plus the two
//! claim paths. No test runs the real bank: `--fno-bin` is a fixture
//! shell script, and tick mode runs run mode through an injected spawner that
//! re-parses the child argv in-process.
use super::*;

use serde_json::Value;
use tempfile::TempDir;

/// The checkout the run grades: `<tmp>/repo` carrying an `evals/bank` dir.
fn bank_repo_dir(tmp: &TempDir) -> PathBuf {
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("evals").join("bank")).unwrap();
    repo
}

fn base_args(tmp: &TempDir, extra: &[&str]) -> Vec<String> {
    let repo = bank_repo_dir(tmp);
    let mut v: Vec<String> = [
        "--history",
        &tmp.path().join("history.jsonl").to_string_lossy(),
        "--events",
        &tmp.path().join("events.jsonl").to_string_lossy(),
        "--fno-bin",
        &tmp.path().join("fno-bin.sh").to_string_lossy(),
        "--schedule-days",
        "7",
        "--stale-days",
        "7",
        "--claims-root",
        &tmp.path().join("claims-root").to_string_lossy(),
        "--repo",
        &repo.to_string_lossy(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    v.extend(extra.iter().map(|s| s.to_string()));
    v
}

fn reg_row(task_id: &str, ts_rfc3339: &str) -> String {
    serde_json::json!({
        "ts": ts_rfc3339, "task_id": task_id, "tier": "regression",
        "pass": true, "reason": "", "duration_s": 1.0, "repeat_index": 0,
        "bank_rev": null, "worker_provider": null,
    })
    .to_string()
}

/// Fixture `fno-bin`: `doctor evals run` appends ROWS (stamped one second
/// after the run window opens, so `ts >= started` holds at second
/// granularity) and records its cwd in `cwd.log`; `inbox notify` records
/// the call.
fn write_fixture(tmp: &TempDir, rows: &[String], history: &Path, notify_log: &Path) {
    let mut script = format!(
        "#!/bin/sh\ncase \"$1/$2\" in\n  doctor/evals)\n    pwd -P > '{}'\n    sleep 1\n",
        tmp.path().join("cwd.log").to_string_lossy()
    );
    // Rows baked as literal JSON; the ts is stamped at run time (second
    // granularity), which `sleep 1` places strictly after `--started`. The
    // JSON rides a double-quoted echo so `$(date)` substitutes; its own
    // double quotes are escaped for the shell.
    for row in rows {
        let stamped = row
            .replace("\"ts\":\"\"", "\"ts\":\"__TS__\"")
            .replace('"', "\\\"")
            .replace("__TS__", "$(date -u +%Y-%m-%dT%H:%M:%SZ)");
        script += &format!(
            "    echo \"{}\" >> '{}'\n",
            stamped,
            history.to_string_lossy()
        );
    }
    script += "    ;;\n  inbox/notify)\n    echo \"$*\" >> '";
    script += &notify_log.to_string_lossy();
    script += "'\n    ;;\nesac\n";
    crate::write_exec_stub(tmp.path(), "fno-bin.sh", &script);
}

/// The injected spawner: re-parses the child argv (the same contract the real
/// binary exec satisfies) and runs run mode on a detached thread.
fn thread_spawner() -> impl Fn(&[String]) -> Result<u32, String> {
    |argv: &[String]| {
        assert_eq!(argv[1], "evals-arm");
        assert_eq!(argv[2], "--run");
        let child = parse_args(&argv[2..])?;
        std::thread::spawn(move || {
            run_run_mode(&child);
        });
        Ok(std::process::id())
    }
}

fn wait_for_row(events: &Path, kind: &str, timeout: Duration) -> Option<Value> {
    // Committed rows, not journal bytes: the store cutover commits ticks in
    // the store beside the journal.
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let rows =
            crate::event_store::query_events(events, &crate::event_store::EventQuery::default())
                .unwrap_or_default();
        for r in rows.iter().rev() {
            if let Ok(v) = serde_json::from_str::<Value>(&r.line) {
                if v.get("type").and_then(Value::as_str) == Some(kind) {
                    return Some(v);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

fn claim_state(root: &Path) -> claims::ClaimState {
    claims::status(CLAIM_KEY, Some(root)).0
}

fn ok_gate() -> GateReading {
    GateReading {
        slots: 0,
        max_live: 5,
        ram_gb: Some(16.0),
        min_free_gb: 1.0,
    }
}

/// The clear verdict as a closure run_tick_mode reads; the incident test
/// passes `armed` instead.
fn clear() -> crate::loops_pause::DispatchPause {
    crate::loops_pause::DispatchPause::Clear
}

// AC2-HP: summary 9 days old, schedule 7, gate headroom, fixture appends 3
// rows -> one evals_scheduled_run with task_count 3, claim free at the end.
#[test]
fn fires_past_window_journals_run_and_frees_claim() {
    let tmp = TempDir::new().unwrap();
    let history = tmp.path().join("history.jsonl");
    let notify_log = tmp.path().join("notify.log");
    let events = tmp.path().join("events.jsonl");
    let rows: Vec<String> = (1..=3)
        .map(|i| reg_row(&format!("reg-task-{i}"), ""))
        .collect();
    write_fixture(&tmp, &rows, &history, &notify_log);
    let summary = r#"{"age_days": 9.0, "never_ran": false}"#;
    let args = base_args(&tmp, &["--summary-json", summary]);
    let o = parse_args(&args).unwrap();

    run_tick_mode(&o, &ok_gate, &clear, &thread_spawner());

    let row = wait_for_row(&events, "evals_scheduled_run", Duration::from_secs(20))
        .expect("scheduled-run row within 20s");
    let data = row.get("data").unwrap();
    assert_eq!(data.get("task_count"), Some(&Value::from(3)));
    assert_eq!(data.get("passes"), Some(&Value::from(3)));
    assert_eq!(data.get("window_days"), Some(&Value::from(7)));
    let claims_root = tmp.path().join("claims-root");
    assert_eq!(claim_state(&claims_root), claims::ClaimState::Free);
    // The bank resolves from the child's cwd, so the run must sit in the
    // repo, never in the launchd tick's $HOME.
    let ran_in = std::fs::read_to_string(tmp.path().join("cwd.log")).unwrap();
    assert_eq!(
        Path::new(ran_in.trim()),
        bank_repo_dir(&tmp).canonicalize().unwrap()
    );
}

// A graded failure files one node; plumbing files none; an open node holds
// the next run's filing. The fake `fno` logs each filing.
#[test]
fn graded_failure_files_one_node_and_an_open_node_holds_the_next() {
    let tmp = TempDir::new().unwrap();
    let log = tmp.path().join("idea.log");
    let script = format!(
        "#!/bin/sh\ncase \"$1/$2\" in\n  backlog/idea)\n    echo \"$*\" >> '{}'\n    ;;\nesac\n",
        log.to_string_lossy()
    );
    crate::write_exec_stub(tmp.path(), "fno-bin.sh", &script);
    let graph = tmp.path().join("graph.json");
    crate::graph_store::seed_rows(&graph, &[]).unwrap();
    let args = base_args(
        &tmp,
        &[
            "--run",
            "--started",
            &Utc::now().to_rfc3339(),
            "--graph",
            &graph.to_string_lossy(),
        ],
    );
    let o = parse_args(&args).unwrap();
    let grade = |task: &str, ran: bool| {
        serde_json::json!({
            "task_id": task, "pass": false, "reason": "exit 1",
            "obs": {
                "fixture_prepared": ran, "worker_required": false,
                "grader_ran": ran, "grader_passed": false,
            },
        })
    };
    let rows = vec![grade("real-fail", true), grade("plumbing-fail", false)];

    let (filed, refused) = file_regression_nodes(&o, &rows);
    assert_eq!(filed, vec!["real-fail".to_string()], "refused: {refused:?}");
    let logged = std::fs::read_to_string(&log).unwrap();
    assert_eq!(logged.lines().count(), 1);
    assert!(logged.contains("--origin-evidence evals-regression:real-fail"));

    let open = serde_json::json!({
        "id": "x-open", "title": "t", "type": "bug", "status": "idea", "priority": "p2",
        "origin_evidence": "evals-regression:real-fail",
    });
    crate::graph_store::seed_rows(&graph, &[open]).unwrap();
    let (filed, _) = file_regression_nodes(&o, &rows);
    assert!(filed.is_empty(), "an open node holds the filing");
}

// The process-group kill: a fixture that outlives the run budget is killed
// and journaled as a timeout, and the claim is released.
#[test]
fn run_timeout_kills_group_journals_timeout() {
    let tmp = TempDir::new().unwrap();
    let events = tmp.path().join("events.jsonl");
    let notify_log = tmp.path().join("notify.log");
    // A fixture whose evals run sleeps far past the budget.
    let mut script = String::from("#!/bin/sh\ncase \"$1/$2\" in\n  doctor/evals)\n    sleep 30\n    ;;\n  inbox/notify)\n    echo \"$*\" >> '");
    script += &notify_log.to_string_lossy();
    script += "'\n    ;;\nesac\n";
    crate::write_exec_stub(tmp.path(), "fno-bin.sh", &script);
    let args = base_args(
        &tmp,
        &[
            "--run",
            "--started",
            &Utc::now().to_rfc3339(),
            "--run-timeout-s",
            "1",
        ],
    );
    let o = parse_args(&args).unwrap();

    run_run_mode(&o);

    let row = wait_for_row(&events, "evals_stale", Duration::from_secs(5)).unwrap();
    assert_eq!(row["data"]["reason"], "timeout");
    assert_eq!(
        claim_state(&tmp.path().join("claims-root")),
        claims::ClaimState::Free
    );
}
