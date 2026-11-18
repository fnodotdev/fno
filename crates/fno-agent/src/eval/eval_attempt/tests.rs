use super::{classify, validate_cohorts};
use serde_json::json;

fn decl() -> serde_json::Value {
    json!({
        "train": ["t1", "t2"],
        "validation": ["v1"],
        "qualification": ["q1", "q2"],
    })
}

#[test]
fn cohorts_overlap_is_refused() {
    let mut bad = decl();
    bad["qualification"] = json!(["q1", "t1"]);
    let (ok, errors, _) = validate_cohorts(&bad, None, None);
    assert!(!ok);
    assert!(errors
        .iter()
        .any(|e| e.contains("'t1'") && e.contains("both")));
}

// --- the cohort door: aggregate fold, train export, yaml load --------------

use super::qualify_aggregate;

fn rows_text() -> String {
    let graded = json!({
        "task_id": "q1", "tier": "regression", "pass": true,
        "bank_rev": "rev123",
        "obs": {"fixture_prepared": true, "worker_required": false,
                 "grader_ran": true, "grader_passed": true},
    });
    let infra = json!({
        "task_id": "q1", "tier": "regression", "pass": false,
        "obs": {"fixture_prepared": false},
    });
    let legacy = json!({"task_id": "q1", "tier": "regression", "pass": false});
    let wrong_rev = json!({
        "task_id": "q1", "tier": "regression", "pass": true,
        "bank_rev": "other",
        "obs": {"fixture_prepared": true, "worker_required": false,
                 "grader_ran": true, "grader_passed": true},
    });
    format!("{graded}\n{infra}\n{legacy}\n{wrong_rev}\n")
}

fn qual_decl() -> serde_json::Value {
    json!({
        "bank_rev": "rev123",
        "train": [],
        "validation": [],
        "qualification": ["q1"],
    })
}

#[test]
fn aggregate_folds_valid_grades_and_excludes_wrong_rev_and_legacy() {
    let out = qualify_aggregate(&rows_text(), &qual_decl(), None, None, None);
    assert_eq!(out["qualified"], true);
    assert_eq!(out["valid_grades"], 1);
    assert_eq!(out["passes"], 1);
    assert_eq!(out["attempts"]["graded"], 1);
    assert_eq!(out["attempts"]["infrastructure"], 1);
    assert_eq!(out["excluded"]["wrong_rev"], 1);
    assert_eq!(out["excluded"]["legacy"], 1);
    assert_eq!(out["missing_tasks"], 0);
}

#[test]
fn aggregate_refuses_when_the_bank_changed_since_the_pin() {
    // A repo dir with no git metadata: the rev check cannot certify, so it
    // refuses (the same fail-closed shape as an unreadable pin).
    let tmp = tempfile::TempDir::new().unwrap();
    let out = qualify_aggregate(
        &rows_text(),
        &qual_decl(),
        None,
        Some(tmp.path().to_str().unwrap()),
        None,
    );
    assert_eq!(out["qualified"], false);
    assert!(out["reason"].as_str().unwrap().contains("bank changed"));
}

fn obs(extra: serde_json::Value) -> serde_json::Value {
    json!({ "obs": extra })
}

#[test]
fn graded_task_failure_is_distinct_from_infrastructure() {
    // AC1-EDGE: the worker did the task wrong - a valid grade of false, never
    // an infrastructure/unavailable retryable row.
    let v = classify(
        &obs(json!({
            "fixture_prepared": true, "worker_required": true,
            "worker_started": true, "grader_ran": true, "grader_passed": false,
        })),
        None,
    );
    assert_eq!(v.status, "graded");
    assert_eq!(v.graded, Some(false));
    assert!(!v.retryable);
}
