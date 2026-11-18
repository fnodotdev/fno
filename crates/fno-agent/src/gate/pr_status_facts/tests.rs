//! AC1-AC9 for the status ops (the 2026-09-18 false readings).

use super::*;
use serde_json::json;
use std::cell::RefCell;

struct FakeGh {
    ok: bool,
    output: String,
    /// The answer for a `gh pr view --json reviewDecision` probe; the rules
    /// read and every other call get `output`.
    review_decision: String,
    calls: RefCell<Vec<Vec<String>>>,
}

impl GhProbe for FakeGh {
    fn run_gh(&self, _cwd: &Path, args: &[String]) -> Result<(bool, String, String), String> {
        self.calls.borrow_mut().push(args.to_vec());
        if args.contains(&"reviewDecision".to_string()) {
            return Ok((self.ok, self.review_decision.clone(), String::new()));
        }
        if args.iter().any(|a| a.contains("jobs?per_page=1")) {
            return Ok((self.ok, r#"{"total_count":0}"#.to_string(), String::new()));
        }
        Ok((self.ok, self.output.clone(), String::new()))
    }
}

// --- AC1-HP .. AC4-EDGE: merge_blocker --------------------------------------

/// The receipt's `ready` must read the head's own CI verdict, not GitHub's
/// merge state alone: `unstable` means pending or failing NON-required
/// checks, and a PR read ready:true on it while its cli-ci rerun was in
/// flight (merged 2026-11-10, main went red).
#[test]
fn ci_verdict_fact_blocks_ready_on_red_and_pending() {
    let probes = FakeGh {
        ok: true,
        output: String::new(),
        review_decision: String::new(),
        calls: RefCell::new(Vec::new()),
    };
    // An attempt in flight: pending holds ready until it settles.
    let out = merge_blocker(
        &probes,
        &json!({"merge_state": "unstable", "cwd": "/repo", "ci_verdict": "pending"}),
    );
    assert_eq!(out["blockers"], json!(["ci_pending"]));

    // A settled failure reads red even under GitHub's unstable state.
    let out = merge_blocker(
        &probes,
        &json!({"merge_state": "unstable", "cwd": "/repo", "ci_verdict": "red"}),
    );
    assert_eq!(out["blockers"], json!(["ci_red"]));

    // Green adds nothing, and a payload without the fact (legacy callers,
    // no checks configured) keeps today's answer.
    let out = merge_blocker(
        &probes,
        &json!({"merge_state": "unstable", "cwd": "/repo", "ci_verdict": "green"}),
    );
    assert_eq!(out["blockers"], json!([]));
    let out = merge_blocker(&probes, &json!({"merge_state": "clean", "cwd": "/repo"}));
    assert_eq!(out["blockers"], json!([]));

    // The CI fact rides beside the GitHub ones, sorted and deduped with them.
    let out = merge_blocker(
        &probes,
        &json!({
            "merge_state": "unstable",
            "mergeable": "CONFLICTING",
            "cwd": "/repo",
            "ci_verdict": "pending",
        }),
    );
    assert_eq!(
        out["blockers"],
        json!(["ci_pending", "not_mergeable_conflicting"])
    );
}

/// The unmeasured read: PR 82 merged ready with all 7 Actions runs
/// on its head still queued and none started - the check-run rollup carried
/// only fno's own passing statuses, so the verdict read unknown and unknown
/// added no blocker. The runs listing the reader already holds is the
/// evidence: zero completed runs on the head is unmeasured, never green.
#[test]
fn ci_unmeasured_reads_the_runs_listing() {
    let head = "11a2fc423508de47db4b5669f06cf3a2c229f7ac";
    // The specimen: 7 runs queued, none started.
    let queued: Vec<Value> = [
        "cli-ci",
        "rust-ci",
        "guards",
        "crates-publish",
        "review-coverage-gate",
        "multiplayer-attestation",
        "stacked-base-guard",
    ]
    .iter()
    .map(|name| json!({"name": name, "status": "queued", "conclusion": null}))
    .collect();
    let out = ci_unmeasured_reason(&json!(queued), head);
    assert_eq!(
        out.as_deref(),
        Some(format!("ci unmeasured: 7 runs queued, none completed at {head}").as_str())
    );

    // A head with completed runs plus some still running is measured:
    // today's D3173 behavior keeps it.
    let mixed = json!([
        {"name": "cli-ci", "status": "completed", "conclusion": "success"},
        {"name": "rust-ci", "status": "queued", "conclusion": null},
    ]);
    assert_eq!(ci_unmeasured_reason(&mixed, head), None);

    // A listing that read and names no run blocks too: absent runs are
    // unmeasured (the node's own words), not ready.
    let out = ci_unmeasured_reason(&json!([]), head);
    assert_eq!(
        out.as_deref(),
        Some(format!("ci unmeasured: no runs at {head}").as_str())
    );

    // A listing that was never read (the GraphQL route, a legacy payload)
    // has no evidence either way and adds nothing.
    assert_eq!(ci_unmeasured_reason(&Value::Null, head), None);
}

#[test]
fn op_rows() {
    let out = run_op("status-nonsense", &json!({}));
    assert!(out.contains("unknown op status-nonsense"));

    let zero = 0u64;
    let found = zero_job_failures(&specimen_runs(), &specimen_check_runs(), &|_| Ok(zero)).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, ".github/workflows/cli-ci.yml");
    assert_eq!(
        found[0].url,
        "https://github.com/o/r/actions/runs/35366958901"
    );
    assert_eq!(found[0].conclusion, "failure");

    assert!(zero_job_failures(
        &[json!({"id": 7u64, "path": "w.yml", "status": "completed", "conclusion": "failure"})],
        &[],
        &|_| Err("jobs read failed".to_string()),
    )
    .is_err());
    // The op answers an error for a malformed payload, never empty rows.
    let out = run_op(
        "status-zero-job-runs",
        &json!({"slug": "o/r", "check_runs": []}),
    );
    assert!(out.contains("\"error\""), "got {out}");
    assert!(!out.contains("\"rows\""));

    let runs = json!([
        {"id": 9u64, "path": "w.yml", "status": "completed", "conclusion": "failure",
         "html_url": "https://github.com/o/r/actions/runs/9"},
    ])
    .as_array()
    .unwrap()
    .clone();
    let checks = json!([
        {"name": "w", "details_url": "https://github.com/o/r/actions/runs/9/job/3"},
    ])
    .as_array()
    .unwrap()
    .clone();
    let found = zero_job_failures(&runs, &checks, &|_| panic!("jobs_total must not run")).unwrap();
    assert!(found.is_empty());

    let runs = json!([
        {"id": 5u64, "path": "w.yml", "status": "completed", "conclusion": "failure",
         "html_url": "https://github.com/o/r/actions/runs/5"},
        {"id": 6u64, "path": "w.yml", "status": "completed", "conclusion": "success",
         "html_url": "https://github.com/o/r/actions/runs/6"},
    ])
    .as_array()
    .unwrap()
    .clone();
    let found = zero_job_failures(&runs, &[], &|_| panic!("jobs_total must not run")).unwrap();
    assert!(found.is_empty());

    let runs = json!([
        {"id": 7u64, "path": "w.yml", "status": "completed", "conclusion": "failure",
         "html_url": "https://github.com/o/r/actions/runs/7"},
    ])
    .as_array()
    .unwrap()
    .clone();
    let found = zero_job_failures(&runs, &[], &|_| Ok(3)).unwrap();
    assert!(found.is_empty());

    let probes = FakeGh {
        ok: true,
        // `--paginate --slurp` output: a JSON array of pages. The zero-job
        // failure lives on page 2.
        output: r#"[{"workflow_runs":[{"id":1,"path":"w.yml","status":"completed","conclusion":"success","html_url":"https://github.com/o/r/actions/runs/1"}]},{"workflow_runs":[{"id":2,"path":"w.yml","status":"completed","conclusion":"failure","created_at":"2026-09-19T07:00:00Z","html_url":"https://github.com/o/r/actions/runs/2"}]}]"#
            .to_string(),
        review_decision: String::new(),
        calls: RefCell::new(Vec::new()),
    };
    let payload = json!({"slug": "o/r", "cwd": "/repo", "sha": "abc123", "check_runs": []});
    let out: Value = serde_json::from_str(&zero_job_runs_op(&probes, &payload).to_string())
        .unwrap_or(Value::Null);
    let rows = out["rows"].as_array().expect("rows array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "w.yml");
    // The listing rides the answer for the caller's workflow-name mapping.
    assert_eq!(out["listing"].as_array().map(|a| a.len()), Some(2));
    let calls = probes.calls.borrow();
    assert!(calls[0]
        .iter()
        .any(|a| a.contains("actions/runs?head_sha=abc123")));
    assert!(calls[0].iter().any(|a| a == "--paginate"));

    let probes = FakeGh {
        ok: true,
        // `--paginate --slurp` output: one page holding the specimen runs.
        output: format!(
            r#"[{{"workflow_runs":{}}}] "#,
            serde_json::to_string(&specimen_runs()).unwrap()
        ),
        review_decision: String::new(),
        calls: RefCell::new(Vec::new()),
    };
    let payload = json!({
        "slug": "o/r",
        "cwd": "/repo",
        "sha": "abc123",
        "check_runs": specimen_check_runs(),
    });
    let out: Value = serde_json::from_str(&zero_job_runs_op(&probes, &payload).to_string())
        .unwrap_or(Value::Null);
    let rows = out["rows"].as_array().expect("rows array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], ".github/workflows/cli-ci.yml");
    assert_eq!(rows[0]["status"], "completed");
    assert_eq!(rows[0]["conclusion"], "failure");
    assert_eq!(rows[0]["workflow"], ".github/workflows/cli-ci.yml");
    assert_eq!(
        rows[0]["detailsUrl"],
        "https://github.com/o/r/actions/runs/35366958901"
    );
    let calls = probes.calls.borrow();
    assert!(calls.iter().any(|argv| argv
        .iter()
        .any(|a| a.contains("repos/o/r/actions/runs/35366958901/jobs"))));
}

// --- zero-job runs: a run that failed before minting a job ---------------

/// The 504b255 specimen: two failed cli-ci runs with 0 jobs (one newest),
/// one successful rust-ci run a check run links to.
fn specimen_runs() -> Vec<Value> {
    json!([
        {"id": 35344487208u64, "path": ".github/workflows/cli-ci.yml", "status": "completed",
         "conclusion": "failure", "created_at": "2026-09-19T06:00:00Z",
         "html_url": "https://github.com/o/r/actions/runs/35344487208"},
        {"id": 35366958901u64, "path": ".github/workflows/cli-ci.yml", "status": "completed",
         "conclusion": "failure", "created_at": "2026-09-19T07:00:00Z",
         "html_url": "https://github.com/o/r/actions/runs/35366958901"},
        {"id": 35344488345u64, "path": ".github/workflows/rust-ci.yml", "status": "completed",
         "conclusion": "success", "created_at": "2026-09-19T06:00:00Z",
         "html_url": "https://github.com/o/r/actions/runs/35344488345"},
    ])
    .as_array()
    .unwrap()
    .clone()
}

fn specimen_check_runs() -> Vec<Value> {
    json!([
        {"name": "rust-ci", "status": "completed", "conclusion": "success",
         "details_url": "https://github.com/o/r/actions/runs/35344488345/job/99"},
    ])
    .as_array()
    .unwrap()
    .clone()
}
