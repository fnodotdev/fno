//! `--by-cohort` tests: cohorts stay apart, exclusions count by reason, and
//! cost reads measured only when every scored row has usage and a price line.
//! Tempdir histories, pinned clock via the fold's arguments.

use super::cohort_fold;
use serde_json::{json, Value};
use std::fs;
use tempfile::TempDir;

const NOW: &str = "2026-09-15T12:00:00Z";

fn lane_row(
    task: &str,
    pass: bool,
    cohort: &str,
    status: &str,
    lane_status: &str,
    usage: Value,
    model: &str,
) -> String {
    let mut v = json!({
        "ts": NOW, "task_id": task, "tier": "capability",
        "pass": pass, "duration_s": 2.0, "status": status, "lane_status": lane_status,
        "experiment_id": cohort, "observed_model": model,
    });
    if !usage.is_null() {
        v["usage"] = usage;
    }
    v.to_string()
}

fn write_history(tmp: &TempDir, lines: &[String]) -> String {
    let p = tmp.path().join("history.jsonl");
    fs::write(&p, lines.join("\n") + "\n").unwrap();
    p.to_string_lossy().into_owned()
}

fn fold_of(tmp: &TempDir, lines: &[String]) -> Value {
    let h = write_history(tmp, lines);
    let rows = super::read_rows(&h, None, None);
    cohort_fold(&rows, None)
}

fn two_cohorts_kept_apart() {
    let tmp = TempDir::new().unwrap();
    let laned = |row: String| {
        let mut v: Value = serde_json::from_str(&row).unwrap();
        v["requested_lane"] = json!("zai-flash");
        v.to_string()
    };
    let fold = fold_of(
        &tmp,
        &[
            laned(lane_row(
                "t1",
                true,
                "cohort-a",
                "graded",
                "ok",
                json!({"input": 1, "output": 1}),
                "glm-5.3-flash",
            )),
            laned(lane_row(
                "t2",
                true,
                "cohort-a",
                "graded",
                "ok",
                json!({"input": 1, "output": 1}),
                "glm-5.3-flash",
            )),
            laned(lane_row(
                "t3",
                false,
                "cohort-a",
                "graded",
                "ok",
                json!(null),
                "glm-5.3-flash",
            )),
            lane_row(
                "t4",
                true,
                "cohort-b",
                "graded",
                "ok",
                json!(null),
                "glm-5.3-flash",
            ),
        ],
    );
    let a = &fold["cohorts"]["cohort-a"];
    let b = &fold["cohorts"]["cohort-b"];
    assert_eq!(a["attempts"], 3);
    assert_eq!(a["accepted"], 2);
    assert_eq!(b["attempts"], 1);
    assert_eq!(b["accepted"], 1);
    // Each cohort reads as a seat; a row with no lane is no lane.
    assert_eq!(a["requested_lanes"], json!({"zai-flash": 3}));
    assert_eq!(a["observed_models"], json!({"glm-5.3-flash": 3}));
    assert_eq!(b["requested_lanes"], json!({"": 1}));
}

#[test]
fn wilson_interval_pins_20_of_40_and_the_old_math_fails_it() {
    // 20 of 40 must read about [0.35, 0.65]; the bootstrap percentile this
    // replaces read about [0.01, 0.99] at the same rows and passed a mere
    // bracketing assertion.
    let tmp = TempDir::new().unwrap();
    let rows: Vec<String> = (0..40)
        .map(|i| {
            lane_row(
                "t",
                i % 2 == 0,
                "n40-half",
                "graded",
                "ok",
                json!(null),
                "glm-5.3-flash",
            )
        })
        .collect();
    let fold = fold_of(&tmp, &rows);
    let cohort = &fold["cohorts"]["n40-half"];
    assert_eq!(cohort["graded"], 40);
    let lo = cohort["pass_rate_ci95"][0].as_f64().unwrap();
    let hi = cohort["pass_rate_ci95"][1].as_f64().unwrap();
    assert!((lo - 0.35).abs() < 0.005, "lo {lo} not near 0.35");
    assert!((hi - 0.65).abs() < 0.005, "hi {hi} not near 0.65");
}

fn substituted_is_excluded_and_counted_never_scored() {
    let tmp = TempDir::new().unwrap();
    let fold = fold_of(
        &tmp,
        &[
            lane_row(
                "t1",
                true,
                "c",
                "graded",
                "ok",
                json!({"input": 1, "output": 1}),
                "m",
            ),
            lane_row(
                "t2",
                true,
                "c",
                "graded",
                "substituted",
                json!(null),
                "other",
            ),
            lane_row(
                "t3",
                false,
                "c",
                "unavailable",
                "unavailable",
                json!(null),
                "m",
            ),
        ],
    );
    let c = &fold["cohorts"]["c"];
    assert_eq!(c["accepted"], 1);
    assert_eq!(c["excluded"]["total"], 2);
    assert_eq!(c["lane_status_counts"]["substituted"], 1);
    // A row with no excluded_reason still lands in by_reason, under its lane
    // status or row status.
    assert_eq!(
        c["excluded"]["by_reason"],
        json!({"substituted": 1, "unavailable": 1})
    );
}

fn unmeasured_when_any_scored_row_lacks_usage() {
    let tmp = TempDir::new().unwrap();
    let fold = fold_of(
        &tmp,
        &[
            lane_row(
                "t1",
                true,
                "c",
                "graded",
                "ok",
                json!({"input": 1000000, "output": 1000000}),
                "m",
            ),
            lane_row("t2", true, "c", "graded", "ok", json!(null), "m"),
        ],
    );
    let c = &fold["cohorts"]["c"];
    assert_eq!(c["accepted"], 2);
    assert_eq!(c["cost"]["measured"], false);
    assert_eq!(c["excluded"]["usage_missing"], 1);
    assert_eq!(c["tokens"]["input"], 0);
}

#[test]
fn all_contracts_in_one_declaration() {
    two_cohorts_kept_apart();
    substituted_is_excluded_and_counted_never_scored();
    unmeasured_when_any_scored_row_lacks_usage();
}

#[test]
fn gate_main_summary_folds_the_newest_day_across_both_seats() {
    let tmp = TempDir::new().unwrap();
    let row = |cohort: &str, pass: bool, lane: &str, ts: &str| {
        let mut v: Value = serde_json::from_str(&lane_row(
            "t",
            pass,
            cohort,
            "graded",
            "ok",
            Value::Null,
            "m",
        ))
        .unwrap();
        v["requested_lane"] = json!(lane);
        v["ts"] = json!(ts);
        v.to_string()
    };
    let h = write_history(
        &tmp,
        &[
            row(
                "gate-main-2026-09-14-ab",
                true,
                "old",
                "2026-09-14T01:00:00Z",
            ),
            row(
                "gate-main-2026-09-15-ab",
                true,
                "zai-flash",
                "2026-09-15T01:00:00Z",
            ),
            row(
                "gate-main-2026-09-15-ab",
                false,
                "zai-flash",
                "2026-09-15T01:05:00Z",
            ),
            row(
                "gate-main-2026-09-15-ab-lead",
                true,
                "opus",
                "2026-09-15T02:00:00Z",
            ),
            row("gate-head-xyz", true, "head", "2026-09-15T03:00:00Z"),
        ],
    );
    let now = chrono::DateTime::parse_from_rfc3339(NOW).unwrap().into();
    let s = super::gate_main_summary(&h, now);
    assert_eq!(s["day"], "2026-09-15");
    assert_eq!(s["rows"], 3);
    assert_eq!(s["lanes"], 2);
    assert!((s["pass_rate"].as_f64().unwrap() - 2.0 / 3.0).abs() < 1e-9);
    assert_eq!(super::gate_main_summary("/nonexistent", now), Value::Null);
}
