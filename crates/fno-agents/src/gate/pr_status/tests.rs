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
}

impl FakeGh {
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
        }
    }
}

impl GhProbe for FakeGh {
    fn run_gh(&self, _cwd: &Path, args: &[String]) -> Result<(bool, String, String), String> {
        let cmd = args.join(" ");
        self.calls.lock().unwrap().push(args.to_vec());
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

fn known_from_prior(_prior: &Value) -> BTreeMap<String, Value> {
    BTreeMap::new()
}

/// The assembled pr_json the fixture's raw responses produce: the same
/// construction the Python leg's fake runner drove at capture time.
fn pr_json_from(name: &str) -> Value {
    let fake = FakeGh::from_fixture(name);
    read_pr(&fake, Path::new("/tmp"), "Owner/Repo", 42, None).unwrap()
}

/// The red fixture's failure detail replays from the canned job logs: five
/// detailed entries plus the truncation entry, equal to the golden.
#[test]
fn red_fixture_collects_the_golden_failures() {
    let fake = FakeGh::from_fixture("red_detailed_capped");
    let pr_json = read_pr(&fake, Path::new("/tmp"), "Owner/Repo", 42, None).unwrap();
    let rollup = generic_ci_rollup(pr_json["statusCheckRollup"].as_array().unwrap());
    let failing: Vec<Value> = crate::check_supersession::latest_per_name(&Value::Array(rollup))
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| classify_check(c) == "fail" && has_settled_marker(c))
        .cloned()
        .collect();
    let failures = collect_failures(
        &fake,
        Path::new("/tmp"),
        "Owner--Repo",
        &failing,
        &known_from_prior(&Value::Null),
    );
    let expected: Value = serde_json::from_str(
        load_fixture("red_detailed_capped")["expected"]["stdout"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        Value::Array(failures),
        expected["failures"],
        "failure detail"
    );
}

/// A cancelled run of a GitHub-native (`dynamic/`) workflow cannot be rerun,
/// so its row is advisory: the verdict stays green and cancels nothing.
#[test]
fn cancelled_dynamic_workflow_row_is_advisory() {
    assert!(is_dynamic_workflow_path(
        "dynamic/github-code-scanning/codeql"
    ));
    assert!(!is_dynamic_workflow_path(".github/workflows/ci.yml"));
    let rollup = vec![
        json!({"name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"}),
        json!({"name": "Analyze (rust)", "status": "COMPLETED", "conclusion": "CANCELLED", "advisory": true}),
    ];
    let generic = generic_ci_rollup(&rollup);
    assert_eq!(generic.len(), 1);
    let (verdict, code, _) = verdict_for(&generic);
    assert_eq!((verdict.as_str(), code), ("green", 0));
}

/// A red evals/gate status is an eval fact, not a CI failure: the generic
/// rollup drops it, so the verdict stays green over green check runs, and
/// the fact extractor reads the newest row for the context.
#[test]
fn evals_gate_red_never_reds_the_ci_verdict() {
    let rollup = vec![
        json!({"name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"}),
        json!({
            "context": "evals/gate",
            "state": "FAILURE",
            "createdAt": "2026-09-25T00:05:00Z",
            "description": "no result. Clear: fno-agents evals gate-retry abc. Cause: limit",
        }),
    ];
    let (verdict, code, _) = verdict_for(&generic_ci_rollup(&rollup));
    assert_eq!(verdict, "green", "the gate red stays out of the CI verdict");
    assert_eq!(code, 0);
    let fact = evals_gate_fact(&rollup).expect("the gate row is readable");
    assert_eq!(fact["state"], "FAILURE");
    assert_eq!(
        fact["description"],
        "no result. Clear: fno-agents evals gate-retry abc. Cause: limit"
    );
    // Supersession: the newest row for the context wins.
    let two_rows = vec![
        json!({"context": "evals/gate", "state": "FAILURE", "createdAt": "2026-09-25T00:05:00Z", "description": "old"}),
        json!({"context": "evals/gate", "state": "SUCCESS", "createdAt": "2026-09-25T00:06:00Z", "description": "new"}),
    ];
    let fact = evals_gate_fact(&two_rows).expect("newest row");
    assert_eq!(fact["state"], "SUCCESS");
    assert!(evals_gate_fact(&[
        json!({"name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"})
    ])
    .is_none());
}

/// A log read lands in the job cache and the second read never spends a gh
/// call; a failed log read is never cached. An escape-laden log comes back
/// stripped, so the scan reads the name the color codes hid, and the refusal
/// that fires on a gh without the flag names the file-redirect remedy. One
/// test, one tempdir: the env override is process-global, and cargo runs
/// tests in parallel threads.
#[test]
fn job_log_caches_one_attempt_and_never_a_failure() {
    let _guard = super::cache_env_lock();
    let green = FakeGh::from_fixture("green_settled");
    let red = FakeGh::from_fixture("red_detailed_capped");
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("FNO_PR_STATUS_CACHE_DIR", dir.path());
    let cwd = Path::new("/tmp");
    let first = job_log(&green, cwd, "Owner--Repo", "Owner", "Repo", "1001").unwrap();
    assert!(
        green
            .log_args
            .lock()
            .unwrap()
            .contains(&"--allow-escape-sequences".to_string()),
        "job logs permit the escape sequences emitted by Actions"
    );
    let second = job_log(&green, cwd, "Owner--Repo", "Owner", "Repo", "1001").unwrap();
    assert_eq!(first, second, "the same log both times");
    assert_eq!(
        green.log_reads.load(Ordering::SeqCst),
        1,
        "the second read serves from the cache"
    );
    let cached = std::fs::read_to_string(dir.path().join("job-Owner--Repo-1001.json")).unwrap();
    let row: Value = serde_json::from_str(&cached).unwrap();
    assert!(row["ts"].as_f64().is_some(), "the row carries ts");
    assert_eq!(row["log"].as_str().unwrap(), first);

    let err = job_log(&red, cwd, "Owner--Repo", "Owner", "Repo", "2003")
        .err()
        .expect("a failed read is an Err");
    assert!(err.contains("secondary rate limit"), "{err}");
    assert!(
        !dir.path().join("job-Owner--Repo-2003.json").exists(),
        "a failed read writes no cache row"
    );

    let colored = format!(
        "2026-10-03T18:00:00.0000000Z {}step failed, stopping (fail-fast):{} cargo-test-shard{}\n",
        "\u{1b}[31;1m", "\u{1b}[0m", "\u{1b}[K"
    );
    let colored_fake = FakeGh {
        raw: serde_json::json!({ "job_logs": { "3001": colored } }),
        log_reads: AtomicUsize::new(0),
        log_args: std::sync::Mutex::new(Vec::new()),
        pulls_fail: None,
        graphql_answer: None,
        graphql_fail: None,
        calls: std::sync::Mutex::new(Vec::new()),
    };
    let stripped = job_log(&colored_fake, cwd, "Owner--Repo", "Owner", "Repo", "3001").unwrap();
    assert!(!stripped.contains('\u{1b}'), "escape-free: {stripped:?}");
    assert_eq!(failing_step(&stripped).as_deref(), Some("cargo-test-shard"));
    let colored_row =
        std::fs::read_to_string(dir.path().join("job-Owner--Repo-3001.json")).unwrap();
    assert!(
        !colored_row.contains('\u{1b}'),
        "the cache row is escape-free"
    );

    let remedy = escape_refusal_remedy(
        "the response contains terminal escape sequences; pass \
         --allow-escape-sequences to output it anyway",
        "repos/Owner/Repo/actions/jobs/3001/logs",
    )
    .expect("the escape refusal names its remedy");
    assert!(remedy.contains("> ci-log.txt"), "{remedy}");
    assert!(
        escape_refusal_remedy("secondary rate limit", "repos/x/y").is_none(),
        "another failure carries no remedy"
    );

    // A row written before the strip carries raw ANSI and no gh call fixes
    // it: the hit path normalizes it too.
    let legacy_row = json!({"ts": 1.0, "log": "\u{1b}[31mstep failed, stopping (fail-fast): legacy-shard\u{1b}[0m\n"});
    std::fs::write(
        dir.path().join("job-Owner--Repo-4001.json"),
        legacy_row.to_string(),
    )
    .unwrap();
    let empty_fake = FakeGh {
        raw: serde_json::json!({}),
        log_reads: AtomicUsize::new(0),
        log_args: std::sync::Mutex::new(Vec::new()),
        pulls_fail: None,
        graphql_answer: None,
        graphql_fail: None,
        calls: std::sync::Mutex::new(Vec::new()),
    };
    let from_legacy = job_log(&empty_fake, cwd, "Owner--Repo", "Owner", "Repo", "4001").unwrap();
    assert!(
        !from_legacy.contains('\u{1b}'),
        "a legacy row serves escape-free: {from_legacy:?}"
    );
    assert_eq!(failing_step(&from_legacy).as_deref(), Some("legacy-shard"));
    assert_eq!(
        empty_fake.log_reads.load(Ordering::SeqCst),
        0,
        "the hit spent no gh call"
    );
}

/// The composer replays the live-read fixtures: the payload it assembles and
/// the stderr lines it prints equal the golden, field for field and line for
/// line. Failure detail arrives resolved (its own collection is pinned by the
/// red fixture's collection test).
#[test]
fn composer_replays_the_goldens() {
    for name in [
        "green_settled",
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

/// A broken proxy helper dies in Python. The reason names the traceback's
/// cause line, never its header and never the admit banner ahead of it.
#[test]
fn a_traceback_refusal_names_its_cause_line() {
    let stderr = "gh proxy: draft guard unavailable; admitting\n\
                  Traceback (most recent call last):\n  \
                  File \"/x/fno-gh-proxy\", line 4, in <module>\n    \
                  from fno.pr.gh_proxy import main\n\
                  ModuleNotFoundError: No module named 'fno.pr'\n";
    let probe = FakeGh::from_fixture("rest_refusal_rate_limit");
    let reason = crate::pr_status::rest_reason(&probe, Path::new("/tmp"), stderr);
    assert_eq!(reason.text, "ModuleNotFoundError: No module named 'fno.pr'");
}

/// Serves one fixed `gh api rate_limit` payload; every other call refuses.
struct RateLimitProbe(&'static str);

impl GhProbe for RateLimitProbe {
    fn run_gh(&self, _cwd: &Path, args: &[String]) -> Result<(bool, String, String), String> {
        if args.join(" ").contains("rate_limit") {
            return Ok((true, self.0.to_string(), String::new()));
        }
        Ok((
            false,
            String::new(),
            "gh: API rate limit exceeded (HTTP 403)".into(),
        ))
    }
}

/// The 2026-11-09 iMac refusal: a keyless `gh` reads GitHub's anonymous
/// 60-request bucket. It must classify as a local auth fault with no
/// rate-limit class, so no caller writes the shared fleet hold.
#[test]
fn a_keyless_refusal_is_an_auth_fault_not_a_fleet_hold() {
    let _env = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let state = tempfile::tempdir().unwrap();
    std::env::set_var("FNO_STATE_DIR", state.path());
    let probe =
        RateLimitProbe(r#"{"resources":{"core":{"limit":60,"remaining":59,"reset":1794300000}}}"#);
    let stderr = "gh: API rate limit exceeded for 203.0.113.7. (HTTP 403)";
    let reason = crate::pr_status::rest_reason(&probe, Path::new("/tmp"), stderr);
    std::env::remove_var("FNO_STATE_DIR");
    assert_eq!(reason.rate_limit_class, "", "no class, so no hold");
    assert!(reason.text.contains("no token here"), "{}", reason.text);
}

/// The stranded tier fact rides a cancelled-only payload: the field carries
/// the rerun remedy and one stderr note names it. A null fact adds neither,
/// so the golden replay stays byte-identical.
#[test]
fn a_cancelled_only_head_warns_and_proceeds() {
    let cancelled_row = json!({"name": "ci", "status": "completed", "conclusion": "cancelled"});
    let pr_json = json!({"state": "OPEN", "statusCheckRollup": [cancelled_row]});
    let inputs = |keeper: Value| crate::pr_status::compose::ComposeInputs {
        pr: "42".to_string(),
        pr_json: pr_json.clone(),
        rerun_recovery: Value::Null,
        branch_history: Value::Null,
        optional_reviews: Value::Null,
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
        ci_tier_keeper: keeper,
    };
    let stranded = crate::ci_tier::stranded_fact_pure(false, false, &[70, 71], &[], &[]);
    let (code, payload, stderr) = crate::pr_status::compose::compose_payload(&inputs(stranded));
    assert_eq!(code, 2, "a cancelled-only head reads pending, never red");
    assert_eq!(
        payload.pointer("/checks/fail"),
        Some(&json!(0)),
        "{payload}"
    );
    let fact = payload
        .get("ci_tier_keeper")
        .expect("the tier fact rides the payload");
    assert_eq!(fact["remedy"], "rerun");
    assert_eq!(fact["cancelled_runs"], json!([70, 71]));
    assert!(
        stderr.iter().any(|l| l.contains("gh run rerun")),
        "the stranded note names the remedy: {stderr:?}"
    );
    let (_, payload, stderr) = crate::pr_status::compose::compose_payload(&inputs(Value::Null));
    assert!(payload.get("ci_tier_keeper").is_none());
    assert!(
        !stderr.iter().any(|l| l.contains("gh run rerun")),
        "a null fact stays silent: {stderr:?}"
    );
}

/// The optional-reviews read walks REST pages through the probe and composes
/// from the REST author key; a failed page is the unknown sentinel, never an
/// empty list.
#[test]
fn optional_reviews_list_reads_rest_pages_and_keys_on_user_login() {
    let cwd = Path::new("/tmp");
    let page_row = |n: usize| {
        json!({
            "user": {"login": format!("reviewer-{n}")},
            "state": "COMMENTED",
        })
    };
    let page1: Vec<Value> = (1..=100).map(page_row).collect();
    let page2: Vec<Value> = (101..=102).map(page_row).collect();
    let fake = FakeGh {
        raw: json!({ "review_pages": [page1, page2] }),
        log_reads: AtomicUsize::new(0),
        log_args: std::sync::Mutex::new(Vec::new()),
        pulls_fail: None,
        graphql_answer: None,
        graphql_fail: None,
        calls: std::sync::Mutex::new(Vec::new()),
    };
    let rows =
        reviews::fetch_reviews(&fake, cwd, "octocat/wordplay", 7).expect("both REST pages read");
    assert_eq!(rows.len(), 102, "100-row page 1 + 2-row page 2");
    let calls = fake.calls.lock().unwrap();
    assert_eq!(calls.len(), 2, "page 2 is short: the walk stops");
    assert!(
        calls.iter().any(|a| a
            .join(" ")
            .contains("repos/octocat/wordplay/pulls/7/reviews?per_page=100&page=1")),
        "page 1 rides the door: {calls:?}"
    );
    assert!(
        calls.iter().any(|a| a
            .join(" ")
            .contains("repos/octocat/wordplay/pulls/7/reviews?per_page=100&page=2")),
        "a full page 1 asks for page 2: {calls:?}"
    );
    drop(calls);

    // The compose keys the author at the REST /user/login and strips [bot].
    let rows = vec![json!({
        "user": {"login": "chatgpt-codex-connector[bot]"},
        "state": "COMMENTED",
    })];
    let composed = reviews::compose_reviews(&rows, &["codex".to_string()], &[]);
    let list = composed["optional_reviews"].as_array().unwrap();
    assert_eq!(list.len(), 1, "one author row: {composed}");
    assert_eq!(list[0]["author"], json!("chatgpt-codex-connector"));
    assert_eq!(list[0]["state"], json!("COMMENTED"));

    // A failed read is the unknown sentinel, distinct from an empty list.
    let failed = FakeGh {
        raw: json!({}),
        log_reads: AtomicUsize::new(0),
        log_args: std::sync::Mutex::new(Vec::new()),
        pulls_fail: Some("boom".to_string()),
        graphql_answer: None,
        graphql_fail: None,
        calls: std::sync::Mutex::new(Vec::new()),
    };
    assert!(reviews::fetch_reviews(&failed, cwd, "octocat/wordplay", 7).is_none());
    let out = reviews::optional_reviews(&failed, cwd, 7, "octocat/wordplay");
    assert_eq!(out["optional_reviews"], json!("unknown"));
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

/// Every route drained, the read fails and NAMES each route it tried, with
/// no wait-out guidance anywhere in the text.
#[test]
fn a_drained_rate_limit_names_the_routes_without_wait_guidance() {
    let mut fake = FakeGh::from_fixture("green_settled");
    fake.pulls_fail = Some("gh: API rate limit exceeded for user ID 1 (HTTP 403)".to_string());
    fake.graphql_fail = Some("gh: API rate limit exceeded (HTTP 403)".to_string());
    let err = read_pr_routes(&fake, Path::new("/tmp"), "Owner/Repo", 42, None).unwrap_err();
    assert_eq!(err.rate_limit_class, "secondary", "the class rides on");
    let low = err.text.to_lowercase();
    assert!(
        low.contains("routes tried"),
        "names the routes: {}",
        err.text
    );
    assert!(
        low.contains("graphql"),
        "names the graphql route: {}",
        err.text
    );
    for banned in [
        "back off",
        "backoff",
        "wait for its reset",
        "retry later",
        "wait out",
    ] {
        assert!(!low.contains(banned), "no {banned} guidance: {}", err.text);
    }
}

/// The rate-limit classification carries no instruction to wait: the class
/// and the live measurement answer, never a remedy that stalls the agent.
#[test]
fn the_rate_limit_reason_carries_no_wait_guidance() {
    let probe = FakeGh::from_fixture("rest_refusal_rate_limit");
    let reason = crate::pr_status::rest_reason(
        &probe,
        Path::new("/tmp"),
        "gh: API rate limit exceeded for user ID 1 (HTTP 403)",
    );
    assert!(
        reason.text.to_lowercase().contains("secondary"),
        "{}",
        reason.text
    );
    let low = reason.text.to_lowercase();
    for banned in ["back off", "wait for its reset", "retry later", "wait out"] {
        assert!(!low.contains(banned), "no {banned}: {}", reason.text);
    }
}
