use super::*;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pr_status")
}

fn load_fixture(name: &str) -> Value {
    let text = std::fs::read_to_string(fixtures_dir().join(format!("{name}.json")))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"));
    serde_json::from_str(&text).unwrap()
}

/// Serves the fixture's raw gh responses off the same URL shapes the fake
/// runner matched on the Python side.
struct FakeGh {
    raw: Value,
    log_reads: AtomicUsize,
    log_args: std::sync::Mutex<Vec<String>>,
    /// Force the pulls read to fail with this stderr (refusal scenarios).
    pulls_fail: Option<String>,
    /// The GraphQL reply when the route chain asks `api graphql`; None is a
    /// fixture-miss Err.
    graphql_answer: Option<Value>,
    /// Force the graphql read to fail with this stderr.
    graphql_fail: Option<String>,
    /// Every probe call's args, oldest first, for asked-path assertions.
    calls: std::sync::Mutex<Vec<Vec<String>>>,
    /// Set by `refusing`: `rate_limit` serves this payload and every other
    /// call refuses with a 403 rate-limit stderr.
    rate_limit: Option<&'static str>,
}

impl FakeGh {
    /// A keyless-gh double: the rate_limit probe answers `payload`, and any
    /// other call is a 403 rate-limit refusal.
    fn refusing(payload: &'static str) -> Self {
        FakeGh {
            raw: Value::Null,
            log_reads: AtomicUsize::new(0),
            log_args: std::sync::Mutex::new(Vec::new()),
            pulls_fail: None,
            graphql_answer: None,
            graphql_fail: None,
            calls: std::sync::Mutex::new(Vec::new()),
            rate_limit: Some(payload),
        }
    }

    fn from_fixture(name: &str) -> Self {
        let fixture = load_fixture(name);
        FakeGh {
            raw: fixture["inputs"]["raw"].clone(),
            log_reads: AtomicUsize::new(0),
            log_args: std::sync::Mutex::new(Vec::new()),
            pulls_fail: None,
            graphql_answer: None,
            graphql_fail: None,
            calls: std::sync::Mutex::new(Vec::new()),
            rate_limit: None,
        }
    }
}

impl GhProbe for FakeGh {
    fn run_gh(&self, _cwd: &Path, args: &[String]) -> Result<(bool, String, String), String> {
        let cmd = args.join(" ");
        self.calls.lock().unwrap().push(args.to_vec());
        if let Some(payload) = self.rate_limit {
            if cmd.contains("rate_limit") {
                return Ok((true, payload.to_string(), String::new()));
            }
            return Ok((
                false,
                String::new(),
                "gh: API rate limit exceeded (HTTP 403)".into(),
            ));
        }
        let serve = |v: &Value| (true, v.to_string(), String::new());
        // A forced failure wins over serving: the reviews path also contains
        // "/pulls/", so this arm is checked first.
        if let Some(stderr) = &self.pulls_fail {
            if cmd.contains("/pulls/") {
                return Ok((false, String::new(), stderr.clone()));
            }
        }
        if cmd.contains("graphql") {
            if let Some(stderr) = &self.graphql_fail {
                return Ok((false, String::new(), stderr.clone()));
            }
            if let Some(answer) = &self.graphql_answer {
                return Ok(serve(answer));
            }
            return Err(format!("fixture fake has no graphql answer: {cmd}"));
        }
        if cmd.contains("/reviews?per_page=") {
            let page: usize = cmd
                .rsplit("page=")
                .next()
                .unwrap_or("1")
                .trim()
                .parse()
                .unwrap_or(1);
            let pages = self.raw["review_pages"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let body = pages.get(page - 1).cloned().unwrap_or_else(|| json!([]));
            return Ok(serve(&body));
        }
        if cmd.contains("/pulls/") {
            return Ok(serve(&self.raw["pulls"]));
        }
        if cmd.contains("/check-runs") && cmd.contains("page=") {
            let page: usize = cmd
                .rsplit("page=")
                .next()
                .unwrap_or("1")
                .trim()
                .parse()
                .unwrap_or(1);
            let pages = self.raw["check_runs_pages"].as_array().unwrap();
            let body = pages
                .get(page - 1)
                .cloned()
                .unwrap_or(serde_json::json!({"total_count": 0, "check_runs": []}));
            return Ok(serve(&body));
        }
        if cmd.contains("/actions/runs?head_sha=") {
            return Ok(serve(&self.raw["runs_listing"]));
        }
        if cmd.contains("/actions/runs/") && cmd.contains("/jobs?per_page=") {
            // A run is zero-job in the fixture iff a zero_rows entry names
            // its run id in the details URL; every other run has jobs.
            let run_id = cmd
                .split("/actions/runs/")
                .nth(1)
                .unwrap_or("")
                .split('/')
                .next()
                .unwrap_or("");
            let zero = self.raw["zero_rows"]
                .as_array()
                .map(|rows| {
                    rows.iter().any(|r| {
                        r.get("detailsUrl")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .contains(&format!("/runs/{run_id}"))
                    })
                })
                .unwrap_or(false);
            let total = if zero { 0 } else { 2 };
            return Ok(serve(
                &serde_json::json!({"total_count": total, "jobs": []}),
            ));
        }
        if cmd.ends_with("/status") {
            return Ok(serve(&self.raw["statuses"]));
        }
        if cmd.contains("/actions/jobs/") && cmd.ends_with("/logs") {
            self.log_reads.fetch_add(1, Ordering::SeqCst);
            *self.log_args.lock().unwrap() = args.to_vec();
            let job = cmd
                .rsplit("/jobs/")
                .next()
                .unwrap()
                .split('/')
                .next()
                .unwrap()
                .to_string();
            let failed = self.raw["failed_job_logs"]
                .get(&job)
                .and_then(Value::as_str);
            if let Some(stderr) = failed {
                return Ok((false, String::new(), stderr.to_string()));
            }
            let log = self.raw["job_logs"]
                .get(&job)
                .and_then(Value::as_str)
                .unwrap_or("");
            return Ok((true, log.to_string(), String::new()));
        }
        if cmd.contains("/actions/jobs/") {
            let job = cmd
                .rsplit("/jobs/")
                .next()
                .unwrap()
                .split('?')
                .next()
                .unwrap()
                .to_string();
            let steps = self.raw["job_steps"]
                .get(&job)
                .cloned()
                .unwrap_or(serde_json::json!([]));
            return Ok(serve(&serde_json::json!({ "steps": steps })));
        }
        if cmd.contains("rate_limit") {
            return Ok(serve(
                &serde_json::json!({ "resources": { "core": { "remaining": 10 } } }),
            ));
        }
        Err(format!("fixture fake has no answer for: {cmd}"))
    }
}

/// The assembled pr_json the fixture's raw responses produce: the same
/// construction the Python leg's fake runner drove at capture time.
fn pr_json_from(name: &str) -> Value {
    let fake = FakeGh::from_fixture(name);
    read_pr(&fake, Path::new("/tmp"), "Owner/Repo", 42, None).unwrap()
}

/// The composer replays the live-read fixtures: the payload it assembles and
/// the stderr lines it prints equal the golden, field for field and line for
/// line. Failure detail arrives resolved (its own collection is pinned by the
/// red fixture's collection test).
#[test]
fn composer_replays_the_goldens() {
    for name in [
        "green_settled",
        "draft_green_settled",
        "red_detailed_capped",
        "pending_mixed",
        "all_status_contexts",
        "zero_job_runs",
        "terminal_merged",
        "evals_gate_red_ci_green",
    ] {
        let fixture = load_fixture(name);
        let expected: Value =
            serde_json::from_str(fixture["expected"]["stdout"].as_str().unwrap()).unwrap();
        let inputs = crate::pr_status::compose::ComposeInputs {
            pr: "42".to_string(),
            pr_json: pr_json_from(name),
            rerun_recovery: fixture["inputs"]["rerun_recovery"].clone(),
            branch_history: fixture["inputs"]["branch_history"].clone(),
            optional_reviews: fixture["inputs"]["optional_reviews"].clone(),
            coverage_row: fixture["inputs"]["coverage_row"].clone(),
            hold_reason: fixture["inputs"]["hold_reason"].clone(),
            review_activity: fixture["inputs"]["review_activity"].clone(),
            receipt: fixture["inputs"]["receipt"].clone(),
            github_merge_blockers: fixture["inputs"]["github_merge_blockers"].clone(),
            merge_authority: fixture["inputs"]["merge_authority"].clone(),
            merge_execution: fixture["inputs"]["merge_execution"].clone(),
            failures: expected.get("failures").cloned().unwrap_or(Value::Null),
            review_lane: fixture["inputs"]["review_lane"].as_bool().unwrap_or(false),
            platform_incident: Value::Null,
            ci_tier_keeper: Value::Null,
            ci_unmeasured: Value::Null,
        };
        let (code, payload, stderr) = crate::pr_status::compose::compose_payload(&inputs);
        assert_eq!(
            code,
            fixture["expected"]["exit"].as_i64().unwrap() as i32,
            "{name}: exit"
        );
        assert_eq!(payload, expected, "{name}: payload");
        let expected_lines: Vec<String> = fixture["expected"]["stderr_lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(stderr, expected_lines, "{name}: stderr");
    }
}

/// A cancelled run whose jobs started after the passing run's jobs must not
/// flip the read: the passing row keeps the slot (check_supersession keeps a
/// settled row over a newer taken-away), the verdict is green, and the
/// "produced no result" note stays silent because the note reads the deduped
/// rows, not the raw rollup.
#[test]
fn a_cancelled_run_never_kicks_a_passing_run_off_the_slot() {
    let pr_json = json!({
        "number": 4649,
        "state": "OPEN",
        "headRefOid": "9d52b776efb6ff12bed70be16ed4e4617a394be2",
        "statusCheckRollup": [
            {"name": "guards", "workflow": "guards", "status": "COMPLETED", "conclusion": "SUCCESS", "startedAt": "2026-11-07T18:56:37Z"},
            {"name": "guards", "workflow": "guards", "status": "COMPLETED", "conclusion": "CANCELLED", "startedAt": "2026-11-07T18:57:30Z"}
        ]
    });
    let inputs = crate::pr_status::compose::ComposeInputs {
        pr: "4649".to_string(),
        pr_json,
        rerun_recovery: Value::Null,
        branch_history: Value::Null,
        optional_reviews: json!({
            "optional_reviews": [],
            "optional_reviews_unresolved": 0,
            "optional_reviews_resolved_unchanged": 0,
        }),
        coverage_row: Value::Null,
        hold_reason: Value::Null,
        review_activity: Value::Null,
        receipt: Value::Null,
        github_merge_blockers: Value::Null,
        merge_authority: Value::Null,
        merge_execution: Value::Null,
        failures: Value::Null,
        review_lane: false,
        platform_incident: Value::Null,
        ci_tier_keeper: Value::Null,
        ci_unmeasured: Value::Null,
    };
    let (code, payload, stderr) = crate::pr_status::compose::compose_payload(&inputs);
    assert_eq!(code, 0);
    assert_eq!(
        payload.get("verdict").and_then(Value::as_str),
        Some("green")
    );
    assert_eq!(payload.get("settled").and_then(Value::as_bool), Some(true));
    assert!(
        !stderr.iter().any(|l| l.contains("produced no result")),
        "the cancelled history row must not read as an absent check: {stderr:?}"
    );
}

/// The compose gates `ready` on the head's own CI verdict, not only on the
/// receipt: an attempt in flight reads ready:false naming the hold even
/// while GitHub has not computed mergeStateStatus (the merge-blocker op
/// never runs in that window, so no supplied blocker names it). This is the
/// 2026-11-10 incident's shape, closed at the last layer that could still
/// read ready:true.
#[test]
fn an_in_flight_rerun_reads_ready_false_even_before_the_merge_state_arrives() {
    let pr_json = json!({
        "number": 9,
        "state": "OPEN",
        "headRefOid": "abc123def456",
        "statusCheckRollup": [
            {"name": "cli-ci", "workflow": "cli-ci", "status": "IN_PROGRESS", "conclusion": Value::Null},
        ],
    });
    let inputs = crate::pr_status::compose::ComposeInputs {
        pr: "9".to_string(),
        pr_json,
        rerun_recovery: Value::Null,
        branch_history: Value::Null,
        optional_reviews: json!({
            "optional_reviews": [],
            "optional_reviews_unresolved": 0,
            "optional_reviews_resolved_unchanged": 0,
        }),
        coverage_row: Value::Null,
        hold_reason: Value::Null,
        review_activity: Value::Null,
        receipt: json!({"outcome": "authorized", "blockers": []}),
        github_merge_blockers: Value::Null,
        merge_authority: Value::Null,
        merge_execution: Value::Null,
        failures: Value::Null,
        review_lane: false,
        platform_incident: Value::Null,
        ci_tier_keeper: Value::Null,
        ci_unmeasured: Value::Null,
    };
    let (code, payload, _stderr) = crate::pr_status::compose::compose_payload(&inputs);
    assert_eq!(code, 2, "an in-flight rerun reads pending");
    assert_eq!(payload["ready"].as_bool(), Some(false));
    assert_eq!(payload["ready_blockers"], json!(["ci_pending"]));
    assert_eq!(payload["verdict"].as_str(), Some("pending"));
}

/// The refused read composes the error payload: exit 4, the reason field,
/// the rate-limit class, and the one stderr line.
#[test]
fn composer_replays_the_refusal() {
    let fixture = load_fixture("rest_refusal_rate_limit");
    let reason = RestReason {
        text: fixture["inputs"]["fetch_stderr"]
            .as_str()
            .unwrap()
            .to_string(),
        rate_limit_class: fixture["inputs"]["rate_limit_class"]
            .as_str()
            .unwrap()
            .to_string(),
    };
    let (code, payload, stderr) = crate::pr_status::compose::error_payload("42", &reason);
    assert_eq!(code, 4);
    let expected: Value =
        serde_json::from_str(fixture["expected"]["stdout"].as_str().unwrap()).unwrap();
    assert_eq!(payload, expected, "error payload");
    let expected_lines: Vec<String> = fixture["expected"]["stderr_lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(stderr, expected_lines, "error stderr");
}

/// The 2026-11-09 iMac refusal: a keyless `gh` reads GitHub's anonymous
/// 60-request bucket. It must classify as a local auth fault with no
/// rate-limit class, so no caller writes the shared fleet hold. The same
/// classifier words a 404 by the read that answered it.
#[test]
fn a_refusal_is_classed_by_its_cause_and_its_read() {
    let _env = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let state = tempfile::tempdir().unwrap();
    std::env::set_var("FNO_STATE_DIR", state.path());
    let probe = FakeGh::refusing(
        r#"{"resources":{"core":{"limit":60,"remaining":59,"reset":1794300000}}}"#,
    );
    let stderr = "gh: API rate limit exceeded for 203.0.113.7. (HTTP 403)";
    let reason = crate::pr_status::rest_reason(&probe, Path::new("/tmp"), stderr);
    std::env::remove_var("FNO_STATE_DIR");
    assert_eq!(reason.rate_limit_class, "", "no class, so no hold");
    assert!(reason.text.contains("no token here"), "{}", reason.text);

    // A 404 on a read other than the PR itself names that read and stops
    // telling the caller to check the PR number: the PR read already answered.
    let not_found = "gh: Not Found (HTTP 404)";
    for (path, label) in [
        (
            "repos/o/r/commits/abc/check-runs?per_page=100&page=1",
            "check-runs",
        ),
        ("repos/o/r/commits/abc/status", "status"),
    ] {
        let named = rest_reason_for(&probe, Path::new("/tmp"), not_found, read_label(path));
        assert!(
            named.text.contains(&format!("on the {label} read")),
            "{}",
            named.text
        );
        assert!(
            !named.text.contains("Check the PR number"),
            "{}",
            named.text
        );
    }
    let pr = rest_reason_for(
        &probe,
        Path::new("/tmp"),
        not_found,
        read_label("repos/o/r/pulls/7"),
    );
    assert!(pr.text.contains("Check the PR number"), "{}", pr.text);
}

/// The pinned read: `--expect-head` turns a settled answer about another
/// head into a refusal that names the mismatch. A merged PR kept answering
/// settled for a watcher armed on a later push; the pin is the refusal.
#[test]
fn the_pinned_read_refuses_any_answer_about_another_head() {
    let mut merged = json!({
        "pr": "1068",
        "head": "37deafc2abcdef",
        "pr_state": "MERGED",
        "settled": true,
        "verdict": "green",
        "ready": true,
        "ready_blockers": [],
    });
    let note = crate::pr_status::compose::apply_expect_head(
        merged.as_object_mut().unwrap(),
        "aaaaaaaaaaaa1234",
    )
    .expect("the heads differ, so the pin applies");
    assert_eq!(
        merged["settled"],
        json!(false),
        "the named sha is not tested"
    );
    assert_eq!(
        merged["head_mismatch"]["expected"],
        json!("aaaaaaaaaaaa1234")
    );
    assert_eq!(merged["head_mismatch"]["actual"], json!("37deafc2abcdef"));
    assert_eq!(merged["ready"], json!(false));
    assert_eq!(merged["ready_blockers"], json!(["head_mismatch"]));
    assert!(note.contains("PR 1068 is MERGED at 37deafc2abcd"), "{note}");
    assert!(note.contains("will never test aaaaaaaaaaaa"), "{note}");

    // A matching pin mutates nothing; the compare is case-insensitive.
    let mut open = json!({
        "pr": "9",
        "head": "37deafc2abcdef",
        "pr_state": "OPEN",
        "settled": true,
        "ready": true,
        "ready_blockers": [],
    });
    let matched = crate::pr_status::compose::apply_expect_head(
        open.as_object_mut().unwrap(),
        "37DEAFC2ABCDEF",
    );
    assert!(matched.is_none(), "a match is a pass");
    assert_eq!(open["settled"], json!(true), "a match mutates nothing");

    // A pin spelled as a 7+ hex prefix of the head matches; shorter or
    // diverging prefixes still mismatch.
    for pin in ["37deafc", "37DEAFC2A"] {
        let mut o = open.clone();
        assert!(
            crate::pr_status::compose::apply_expect_head(o.as_object_mut().unwrap(), pin).is_none(),
            "{pin} names the head"
        );
    }
    for pin in ["37deaf", "37deafd", ""] {
        let mut o = open.clone();
        crate::pr_status::compose::apply_expect_head(o.as_object_mut().unwrap(), pin);
        assert_eq!(o["settled"], json!(false), "{pin:?} does not name the head");
    }

    // A short hex pin names the floor it missed.
    let mut short_pin = open.clone();
    let note =
        crate::pr_status::compose::apply_expect_head(short_pin.as_object_mut().unwrap(), "37deaf")
            .expect("a 6-char pin does not name the head");
    assert!(
        note.contains("at least 7 hex characters"),
        "the refusal names the floor: {note}"
    );

    // An unreadable head (a degraded serve) cannot verify the pin: refused.
    let mut degraded = json!({"pr": "9", "head": Value::Null, "settled": false});
    let note = crate::pr_status::compose::apply_expect_head(
        degraded.as_object_mut().unwrap(),
        "aaaaaaaaaaaa1234",
    )
    .expect("no head provable, so the pin cannot pass");
    assert!(note.contains("names no head"), "{note}");
}

/// A rate-limited REST read answers through the GraphQL route inside the
/// same call: the rollup query maps to the pr_json shape and the verdict
/// reads off it. Tonight's specimen: `pr status` printed a backoff stale
/// row while one GraphQL query showed the PR fully green (and merged).
#[test]
fn a_rate_limited_read_answers_through_the_graphql_route() {
    let mut fake = FakeGh::from_fixture("green_settled");
    fake.pulls_fail = Some("gh: API rate limit exceeded for user ID 1 (HTTP 403)".to_string());
    fake.graphql_answer = Some(json!({
        "data": {"repository": {"pullRequest": {
            "number": 42,
            "state": "OPEN",
            "merged": false,
            "mergedAt": null,
            "url": "https://github.com/Owner/Repo/pull/42",
            "body": "the body",
            "headRefOid": "b41ac4bfeedface0123456789abcdef012345678",
            "headRefName": "feature/route-chain",
            "baseRefName": "main",
            "mergeable": "MERGEABLE",
            "mergeStateStatus": "CLEAN",
            "statusCheckRollup": {"totalCount": 1, "nodes": [
                {"__typename": "CheckRun", "name": "ci", "status": "COMPLETED",
                 "conclusion": "SUCCESS", "startedAt": "2026-09-25T00:00:00Z",
                 "detailsUrl": "https://github.com/Owner/Repo/actions/runs/1/job/1001"}
            ]},
        }}}
    }));
    let pr_json = read_pr_routes(&fake, Path::new("/tmp"), "Owner/Repo", 42, None).unwrap();
    assert_eq!(pr_json.get("state"), Some(&json!("OPEN")));
    assert_eq!(
        pr_json.get("headRefOid"),
        Some(&json!("b41ac4bfeedface0123456789abcdef012345678"))
    );
    assert_eq!(
        pr_json.get("transport"),
        Some(&json!("graphql")),
        "the route is named"
    );
    let rollup = pr_json
        .get("statusCheckRollup")
        .and_then(Value::as_array)
        .unwrap();
    let (verdict, code, _) = verdict_for(&generic_ci_rollup(rollup));
    assert_eq!(
        (verdict.as_str(), code),
        ("green", 0),
        "the verdict answers"
    );
    let calls = fake.calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .any(|a| a.contains(&"api".to_string()) && a.contains(&"graphql".to_string())),
        "the graphql route ran: {calls:?}"
    );
}
