//! `evals-trend` tests: the fold semantics the Python fold carried (tier
//! segments, half-open windows, modal-rev compare, graduation), the views,
//! and the exit contract. Pinned clocks via `--now`; fixture histories are
//! tempdir JSONL, never the real one.
use super::*;

use std::fs;
use tempfile::TempDir;

fn ts_rfc(days_ago: f64, now: DateTime<Utc>) -> String {
    (now - chrono::Duration::milliseconds((days_ago * 86400.0 * 1000.0) as i64)).to_rfc3339()
}

fn row_json(task_id: &str, tier: &str, pass: bool, ts: &str) -> String {
    json!({
        "ts": ts, "task_id": task_id, "tier": tier, "pass": pass,
        "reason": "", "duration_s": 1.0, "repeat_index": 0,
        "bank_rev": null, "worker_provider": null,
        "obs": {"fixture_prepared": true, "worker_required": false,
                 "grader_ran": true, "grader_passed": pass},
    })
    .to_string()
}

// The specimen: one run per window flipped a task to REGRESSED and fired exit 4.
#[test]
fn one_grade_per_window_is_insufficient_and_fires_nothing() {
    let now = now_pinned();
    let tmp = TempDir::new().unwrap();
    let h = write_history(
        &tmp,
        &[
            row_json("r", "regression", true, &ts_rfc(10.0, now)),
            row_json("r", "regression", false, &ts_rfc(1.0, now)),
        ],
    );
    let rows = read_rows(&h, Some("baseline"), None);
    let (view, regressed) = trend_fold(&rows, 7, now);
    assert!(regressed.is_empty());
    assert_eq!(view["tasks"]["r"]["verdict"], "insufficient");
    assert_eq!(view["regression_alarm"], json!([]));
    let report = report_fold(&rows, 7, now, None, None);
    assert_eq!(report["regression_alarm"], json!([]));
    assert_eq!(report["too_few_grades"], json!(["r"]));
}

fn write_history(tmp: &TempDir, lines: &[String]) -> String {
    let p = tmp.path().join("history.jsonl");
    fs::write(&p, lines.join("\n") + "\n").unwrap();
    p.to_string_lossy().into_owned()
}

fn now_pinned() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-15T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

#[test]
fn trend_regressed_ac1_err() {
    let now = now_pinned();
    let tmp = TempDir::new().unwrap();
    let mut lines = Vec::new();
    for _ in 0..3 {
        lines.push(row_json("r", "regression", true, &ts_rfc(10.0, now)));
    }
    lines.push(row_json("r", "regression", true, &ts_rfc(1.0, now)));
    lines.push(row_json("r", "regression", false, &ts_rfc(1.0, now)));
    lines.push(row_json("r", "regression", false, &ts_rfc(1.0, now)));
    let h = write_history(&tmp, &lines);
    let out = run_evals_trend(&[
        "--history".into(),
        h.clone(),
        "--stale-days".into(),
        "7".into(),
        "--now".into(),
        now.to_rfc3339(),
        "--mode".into(),
        "trend".into(),
    ]);
    assert_eq!(out, 4, "3/3 prior then 1/3 recent is regressed");
    // Summary mode carries both keys; the fold reads the same rows.
    let rows = read_rows(&h, Some("baseline"), None);
    let (view, regressed) = trend_fold(&rows, 7, now);
    assert_eq!(regressed, vec!["r"]);
    assert_eq!(view["regression_alarm"], json!(["r"]));
}

// --- attempt-aware denominators + the planned view -------------------------

fn modern_row_json(task_id: &str, tier: &str, passed: bool, ts: &str, attempt: usize) -> String {
    json!({
        "ts": ts, "task_id": task_id, "tier": tier, "pass": passed,
        "attempt_index": attempt,
        "obs": {"fixture_prepared": true, "worker_required": false,
                 "grader_ran": true, "grader_passed": passed},
    })
    .to_string()
}

fn infra_row_json(task_id: &str, tier: &str, ts: &str) -> String {
    json!({
        "ts": ts, "task_id": task_id, "tier": tier, "pass": false,
        "obs": {"fixture_prepared": false, "worker_required": true},
    })
    .to_string()
}

#[test]
fn report_infra_attempt_never_dilutes_the_pass_rate() {
    let now = now_pinned();
    let tmp = TempDir::new().unwrap();
    let h = write_history(
        &tmp,
        &[
            modern_row_json("t", "regression", true, &ts_rfc(1.0, now), 0),
            infra_row_json("t", "regression", &ts_rfc(1.1, now)),
        ],
    );
    let rows = read_rows(&h, Some("baseline"), None);
    let report = report_fold(&rows, 7, now, None, None);
    let t = &report["tasks"][0];
    assert_eq!(t["runs"], 2);
    assert_eq!(t["grades"], 1);
    assert_eq!(t["passes"], 1);
    assert_eq!(t["pass_at_1"], serde_json::json!(1.0)); // not 0.5
    assert_eq!(t["attempts"]["infrastructure"], 1);
    assert!(report["regression_alarm"].as_array().unwrap().is_empty());
}
