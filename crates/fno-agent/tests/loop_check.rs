#![allow(unused_imports)]

/// Integration tests for `fno-agent loop-check` command.
///
/// Each test drives the public `run_loop_check` function directly (no process
/// spawn), using temporary directories for all file I/O.  gh/git are mocked
/// via the `FNO_LOOPCHECK_GH_BIN` / `FNO_LOOPCHECK_GIT_BIN` env overrides so
/// tests never hit the network.
///
/// All tests assert exactly which JSON fields the output carries and that
/// `target-state.md` bytes are unmodified after any fire (read-only invariant).
use fno_agent::loopcheck::run_loop_check;
#[path = "space_paths.rs"]
mod space_paths;
use space_paths::project_events;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// ── helpers ──────────────────────────────────────────────────────────────────

/// Write an executable shell script to `dir/<name>` that prints `body` to
/// stdout and exits 0.  Returns the path.
///
/// The temp sibling plus rename does NOT make ETXTBSY unreachable: a sibling
/// thread that forks while the temp fd is open copies it, and the copied fd
/// follows the inode the rename publishes. CI runs these integration
/// binaries with `--test-threads=1` (cli-ci.yml), so the window has no
/// sibling fork; that single-threaded condition is why no retry is needed.
fn make_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    fs::write(&tmp, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut perms = fs::metadata(&tmp).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&tmp, perms).unwrap();
    fs::rename(&tmp, &path).unwrap();
    path
}

fn event_text(path: &Path) -> String {
    fno_agent::event_store::journal_text(path, &[])
}

/// Build the two mock bin scripts (gh + git) in a temp dir and return the dir
/// so it is not dropped early.  The caller sets `FNO_LOOPCHECK_GH_BIN` and
/// `FNO_LOOPCHECK_GIT_BIN` to the returned paths.
struct MockBins {
    _dir: TempDir,
    pub gh: PathBuf,
    pub git: PathBuf,
}

impl MockBins {
    /// `gh pr view` returns `{"state":"OPEN","number":1,"headRefName":"main"}`;
    /// `gh pr checks` returns a passing check JSON; `gh pr view --json
    /// reviews,comments` returns one completed review by the DEFAULT required
    /// bot (chatgpt-codex-connector) with state COMMENTED - the shape both
    /// bots actually emit (verified on PR #447); proves COMMENTED counts as a
    /// completed pass (AC1-HP, "not approval-state").
    fn green() -> Self {
        let dir = TempDir::new().unwrap();
        let gh = make_script(
            dir.path(),
            "gh",
            r#"
# version probe (availability check)
if echo "$*" | grep -q -- "--version"; then
  echo 'gh version 2.x'
  exit 0
fi
# gh pr view --json state,number,headRefName
if echo "$*" | grep -q "headRefName"; then
  echo '{"state":"OPEN","number":1,"headRefName":"main","headRefOid":"deadbeefdeadbeefdeadbeefdeadbeef00000001"}'
  exit 0
fi
# gh pr checks --json name,state,bucket (real schema: bucket is the rollup;
# `conclusion` is NOT an available field on this subcommand)
if echo "$*" | grep -q "checks"; then
  echo '[{"name":"ci","state":"SUCCESS","bucket":"pass"}]'
  exit 0
fi
# gh pr view --json reviews,comments
if echo "$*" | grep -q "pulls/"; then
  echo '[]'
  exit 0
fi
if echo "$*" | grep -q "reviews"; then
  echo '{"reviews":[{"author":{"login":"chatgpt-codex-connector"},"state":"COMMENTED","submittedAt":"2026-06-05T01:00:00Z","commit":{"oid":"deadbeefdeadbeefdeadbeefdeadbeef00000001"}}],"comments":[]}'
  exit 0
fi
exit 1
"#,
        );
        let git = make_script(
            dir.path(),
            "git",
            r#"case "$*" in
  # A test env has no real repo, so the freshness identity must be
  # UNCOMPUTABLE. Without this the stub answers `git diff --raw` with the
  # same one line at every sha, which compares equal to itself and
  # fabricates a carry out of nothing - the absence-matched-against-
  # absence shape the predicate exists to refuse. Scoped to --raw so
  # `git diff --name-only` (classify_payload) behaves exactly as before.
  *--raw*) exit 1 ;;
  *) echo "deadbeefdeadbeefdeadbeefdeadbeef00000001" ;;
esac"#,
        );
        MockBins { _dir: dir, gh, git }
    }

    /// gh always exits 1 (simulates outage / transient failure) EXCEPT for
    /// --version (availability probe must succeed so the code treats it as
    /// "gh present but commands failing").
    fn failing_gh() -> Self {
        let dir = TempDir::new().unwrap();
        let gh = make_script(
            dir.path(),
            "gh",
            r#"if echo "$*" | grep -q -- "--version"; then echo 'gh version 2.x'; exit 0; fi
exit 1"#,
        );
        let git = make_script(
            dir.path(),
            "git",
            r#"case "$*" in
  # A test env has no real repo, so the freshness identity must be
  # UNCOMPUTABLE. Without this the stub answers `git diff --raw` with the
  # same one line at every sha, which compares equal to itself and
  # fabricates a carry out of nothing - the absence-matched-against-
  # absence shape the predicate exists to refuse. Scoped to --raw so
  # `git diff --name-only` (classify_payload) behaves exactly as before.
  *--raw*) exit 1 ;;
  *) echo "deadbeefdeadbeefdeadbeefdeadbeef00000001" ;;
esac"#,
        );
        MockBins { _dir: dir, gh, git }
    }

    /// gh not present (path is empty; the env var points to /dev/null for git).
    fn no_gh() -> (PathBuf, PathBuf) {
        // Return non-existent paths; callers unset FNO_LOOPCHECK_GH_BIN so the
        // code falls through to PATH where no gh exists in the test env.
        (
            PathBuf::from("/nonexistent/gh"),
            PathBuf::from("/nonexistent/git"),
        )
    }

    /// CI red: `gh pr checks` returns a fail bucket.
    fn ci_red() -> Self {
        let dir = TempDir::new().unwrap();
        let gh = make_script(
            dir.path(),
            "gh",
            r#"
if echo "$*" | grep -q -- "--version"; then
  echo 'gh version 2.x'; exit 0
fi
if echo "$*" | grep -q "headRefName"; then
  echo '{"state":"OPEN","number":7,"headRefName":"feat","headRefOid":"deadbeefdeadbeefdeadbeefdeadbeef00000007"}'
  exit 0
fi
if echo "$*" | grep -q "checks"; then
  echo '[{"name":"unit-tests","state":"FAILURE","bucket":"fail"}]'
  exit 0
fi
if echo "$*" | grep -q "pulls/"; then
  echo '[]'
  exit 0
fi
if echo "$*" | grep -q "reviews"; then
  echo '{"reviews":[{"author":{"login":"codex[bot]"},"state":"APPROVED","submittedAt":"2026-06-05T01:00:00Z","commit":{"oid":"deadbeefdeadbeefdeadbeefdeadbeef00000001"}}],"comments":[]}'
  exit 0
fi
exit 1
"#,
        );
        let git = make_script(
            dir.path(),
            "git",
            r#"echo "deadbeefdeadbeefdeadbeefdeadbeef00000007""#,
        );
        MockBins { _dir: dir, gh, git }
    }

    /// CI is still pending on an otherwise shipped, reviewed PR.
    fn ci_pending() -> Self {
        let dir = TempDir::new().unwrap();
        let gh = make_script(
            dir.path(),
            "gh",
            r#"
if echo "$*" | grep -q -- "--version"; then echo 'gh version 2.x'; exit 0; fi
if echo "$*" | grep -q "headRefName"; then
  echo '{"state":"OPEN","number":17,"headRefName":"feat","headRefOid":"deadbeefdeadbeefdeadbeefdeadbeef00000017"}'
  exit 0
fi
if echo "$*" | grep -q "checks"; then
  echo '[{"name":"unit-tests","state":"IN_PROGRESS","bucket":"pending"}]'
  exit 0
fi
if echo "$*" | grep -q "pulls/"; then
  echo '[]'
  exit 0
fi
if echo "$*" | grep -q "reviews"; then
  echo '{"reviews":[{"author":{"login":"chatgpt-codex-connector"},"state":"COMMENTED","submittedAt":"2026-06-05T01:00:00Z","commit":{"oid":"deadbeefdeadbeefdeadbeefdeadbeef00000017"}}],"comments":[]}'
  exit 0
fi
exit 1
"#,
        );
        let git = make_script(
            dir.path(),
            "git",
            r#"echo "deadbeefdeadbeefdeadbeefdeadbeef00000017""#,
        );
        MockBins { _dir: dir, gh, git }
    }

    /// No PR: `gh pr view` exits 1 with gh's real no-PR stderr (distinct
    /// from an outage, which exits 1 with other stderr - see failing_gh).
    /// --version exits 0 so gh is detected as available.
    fn no_pr() -> Self {
        let dir = TempDir::new().unwrap();
        let gh = make_script(
            dir.path(),
            "gh",
            r#"if echo "$*" | grep -q -- "--version"; then echo 'gh version 2.x'; exit 0; fi
echo 'no pull requests found for branch "feat"' >&2
exit 1"#,
        );
        let git = make_script(
            dir.path(),
            "git",
            r#"echo "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1""#,
        );
        MockBins { _dir: dir, gh, git }
    }

    /// x-8b64 (E): PR merged out-of-band. `gh pr view` reports state=MERGED.
    /// CI is FAILURE and there are NO reviews - proving the merge short-circuits
    /// both the CI and review reads (a merged PR is terminal). git HEAD matches
    /// headRefOid so done()'s head_shipped guard passes.
    fn merged() -> Self {
        let dir = TempDir::new().unwrap();
        let gh = make_script(
            dir.path(),
            "gh",
            r#"
if echo "$*" | grep -q -- "--version"; then
  echo 'gh version 2.x'; exit 0
fi
if echo "$*" | grep -q "headRefName"; then
  echo '{"state":"MERGED","number":42,"headRefName":"feat","headRefOid":"deadbeefdeadbeefdeadbeefdeadbeef00000042"}'
  exit 0
fi
# These must NOT be reached for a merged PR; return red/empty so the test
# fails loudly if the short-circuit ever regresses.
if echo "$*" | grep -q "checks"; then
  echo '[{"name":"unit-tests","state":"FAILURE","bucket":"fail"}]'
  exit 0
fi
if echo "$*" | grep -q "pulls/"; then
  echo '[]'
  exit 0
fi
if echo "$*" | grep -q "reviews"; then
  echo '{"reviews":[],"comments":[]}'
  exit 0
fi
exit 1
"#,
        );
        let git = make_script(
            dir.path(),
            "git",
            r#"echo "deadbeefdeadbeefdeadbeefdeadbeef00000042""#,
        );
        MockBins { _dir: dir, gh, git }
    }
}

/// Write a config.toml to `<cwd>/.fno/config.toml` so tests are
/// isolated from the real `$HOME/.fno/config.toml`. Pins the standard
/// review gate (`required_bots: [chatgpt-codex-connector]`) because the PRODUCT
/// default is now an EMPTY required_bots list (fresh installs complete without a
/// configured review bot). Every test that calls this helper historically ran
/// under the codex gate (a blank settings used to resolve to the codex default),
/// so pinning it here makes that assumption explicit and keeps gate-mechanics
/// tests gated. Tests of the no-gate path set their own settings instead. Must
/// be called after `fs::create_dir_all(cwd.join(".fno"))`.
fn isolate_settings(cwd: &Path) {
    // ab-098967b4: disable the P2 inbox-nudge shell-out so in-process decide()
    // calls never touch the announcement bus (latency + real-bus side
    // effects). Idempotent set; never unset, so it is parallel-safe.
    std::env::set_var("FNO_NUDGE_DISABLED", "1");
    // Pin the streak debounce OFF so this suite keeps counting FIRES, which is
    // what every backstop assertion here was written against: these tests drive
    // decide() in-process and fire within the same second, so the production
    // 300s gap would collapse each burst to a single observation and no
    // backstop could ever trip. The gap logic itself is covered by the
    // read_prior_fires unit tests in src/gate/loopcheck.rs, which pass `now` and the
    // gap explicitly and so need no env at all. Same idempotent-set,
    // never-unset discipline as FNO_NUDGE_DISABLED above: every test in this
    // binary sets the identical value, so parallel execution is deterministic.
    std::env::set_var("FNO_LOOPCHECK_MIN_FIRE_GAP_SECS", "0");
    fs::write(
        cwd.join(".fno/config.toml"),
        "[review]\nrequired_bots = [\"chatgpt-codex-connector\"]\n",
    )
    .unwrap();
}

// ── fixture builders ──────────────────────────────────────────────────────────

/// A minimal valid target-state.md for a NEW (non-legacy) session.
fn new_manifest(session_id: &str, created_at: &str, attended: bool) -> String {
    format!(
        "---\nsession_id: {session_id}\ncreated_at: {created_at}\nattended: {}\n---\n",
        if attended { "true" } else { "false" }
    )
}

/// A minimal transcript JSONL where the last assistant message contains text.
fn transcript_with_promise() -> String {
    let msg = serde_json::json!({
        "message": {
            "role": "assistant",
            "content": "Done! <promise>MISSION COMPLETE</promise>"
        }
    });
    serde_json::to_string(&msg).unwrap() + "\n"
}

fn transcript_with_watching() -> String {
    let msg = serde_json::json!({
        "message": {
            "role": "assistant",
            "content": "<watching reason=\"ci\" timeout=\"30m\">"
        }
    });
    serde_json::to_string(&msg).unwrap() + "\n"
}

fn transcript_with_aborted() -> String {
    let msg = serde_json::json!({
        "message": {
            "role": "assistant",
            "content": "<aborted reason=\"user cancel\">session aborted</aborted>"
        }
    });
    serde_json::to_string(&msg).unwrap() + "\n"
}

fn transcript_empty() -> String {
    // A user message only - no assistant message.
    let msg = serde_json::json!({
        "message": { "role": "user", "content": "go" }
    });
    serde_json::to_string(&msg).unwrap() + "\n"
}

/// Parse the stdout JSON decision from run_loop_check return value.
#[derive(Debug, serde::Deserialize)]
struct Decision {
    decision: String,
    termination_reason: Option<String>,
    message: String,
    fires: u64,
    #[allow(dead_code)]
    fingerprint: Option<String>,
}

/// Run the command, capture stdout, parse the JSON decision.
fn fire(args: &[&str]) -> (i32, Decision) {
    // run_loop_check writes JSON to stdout via println!; we capture it via
    // an in-process pipe simulation.  For simplicity, call via the public
    // function which returns the JSON string.
    let mut args_owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    // Hermeticity: never let the developer's real ~/.fno/config.toml
    // merge under test-local settings (the global+local merge is exercised
    // by the bash e2e harness, which controls HOME per case).
    args_owned.push("--global-settings".to_string());
    args_owned.push("/nonexistent/global-settings.yaml".to_string());
    // Hermeticity, third door: the fleet GitHub request budget ledger
    // defaults to the machine locks folder's github-request-budget.json.
    // This suite runs under a live footnote session whose gh calls stamp that
    // real ledger all session long, so an unisolated test inherits whatever
    // backoff or window pressure the machine is actually under. Default to
    // the case's own (absent) ledger beside its cwd; a case that wants a
    // seeded budget passes `--gh-budget-ledger`, and this skips.
    if !args.iter().any(|a| a.starts_with("--gh-budget-ledger")) {
        let cwd = args
            .iter()
            .position(|a| *a == "--cwd")
            .and_then(|p| args.get(p + 1))
            .expect("every fire passes --cwd");
        args_owned.push("--gh-budget-ledger".to_string());
        args_owned.push(format!("{cwd}/.fno/gh-budget.json"));
    }
    // Hermeticity, fourth door: global_events defaults to the developer's
    // real ~/.fno/events.jsonl. This very suite runs under a live footnote
    // session whose own stop-hook fires write rows to that real file all
    // session long, so an unisolated test silently inherits unrelated
    // journal state. A case that wants real journal rows passes its own
    // `--global-events`, and this skips.
    if !args.iter().any(|a| a.starts_with("--global-events")) {
        args_owned.push("--global-events".to_string());
        args_owned.push("/nonexistent/global-events.jsonl".to_string());
    }
    // Hermeticity, second door: the author harness came from ambient env
    // markers, so these cases passed in CI (no marker) and failed under
    // `cargo test` run from inside Claude Code, where the marker floors a
    // self-review reviewer the review-gate cases do not expect. A case that
    // wants a real harness passes its own `--author-harness`, and this skips.
    if !args.iter().any(|a| a.starts_with("--author-harness")) {
        args_owned.push("--author-harness".to_string());
        args_owned.push("none".to_string());
    }
    let (code, json_str) = fno_agent::loopcheck::run_loop_check_capture(&args_owned);
    let d: Decision = serde_json::from_str(&json_str).expect(&format!(
        "run_loop_check returned non-JSON (code={code}): {json_str}"
    ));
    (code, d)
}
// ── tests ─────────────────────────────────────────────────────────────────────

mod awaiting_merge_ruling;
mod cancel_sentinel;
mod gh_budget_gate;
mod merged_stamp;
mod watch_lease_gate;

/// AC1-HP: promise with green PR -> DonePRGreen, exit 0, termination event.
#[test]
fn ac1_hp_promise_green_pr_done() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");

    fs::write(
        &manifest_path,
        new_manifest("sess-hp1", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_with_promise()).unwrap();

    let mock = MockBins::green();
    let manifest_before = fs::read(&manifest_path).unwrap();

    let (code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ]);

    assert_eq!(code, 0, "exit code must be 0 for allow");
    assert_eq!(d.decision, "allow");
    assert_eq!(
        d.termination_reason.as_deref(),
        Some("DonePRGreen"),
        "expected DonePRGreen but got {:?}",
        d.termination_reason
    );
    assert_eq!(d.fires, 1);

    let manifest_after = fs::read(&manifest_path).unwrap();
    assert_eq!(
        manifest_before, manifest_after,
        "target-state.md must not be mutated"
    );

    let events_path = project_events(&cwd);
    assert!(fno_agent::event_store::store_path(&events_path).exists());
    let events_content = event_text(&events_path);
    assert!(
        events_content.contains("\"termination\""),
        "termination event expected in events.jsonl"
    );
    assert!(
        events_content.contains("DonePRGreen"),
        "DonePRGreen in termination event"
    );
}

/// Promise with an out-of-band MERGED PR ends the loop despite red CI and no
/// required bot review; a failing check reads DoneMergedUnsettled.
#[test]
fn out_of_band_merged_pr_done() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();

    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");

    fs::write(
        &manifest_path,
        new_manifest("sess-merged1", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_with_promise()).unwrap();

    let mock = MockBins::merged();

    let (code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ]);

    assert_eq!(code, 0, "exit code must be 0 for allow");
    assert_eq!(d.decision, "allow");
    assert_eq!(
        d.termination_reason.as_deref(),
        Some("DoneMergedUnsettled"),
        "a merged PR with a failing check must terminate DoneMergedUnsettled; got {:?} ({})",
        d.termination_reason,
        d.message
    );
}

/// AC1-ERR: gh outage never passes a promise -> block + loop_check_gh_error.
/// On a harness the daemon can wake, the same failed read parks instead, and
/// a second failed fire in a row doubles the park.
#[test]
fn ac1_err_gh_outage_blocks_promise() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");

    fs::write(
        &manifest_path,
        new_manifest("sess-err1", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_with_promise()).unwrap();

    let mock = MockBins::failing_gh();
    let fire_as = |harness: &str| {
        fire(&[
            "loop-check",
            "--state",
            manifest_path.to_str().unwrap(),
            "--transcript",
            transcript_path.to_str().unwrap(),
            "--cwd",
            cwd.to_str().unwrap(),
            "--now",
            "2026-06-05T00:30:00Z",
            "--author-harness",
            harness,
            &format!("--gh-bin={}", mock.gh.display()),
            &format!("--git-bin={}", mock.git.display()),
        ])
    };

    let (code, d) = fire_as("none");
    assert_eq!(code, 0, "exit code must be 0 even when blocking");
    assert_eq!(d.decision, "block");
    assert!(d.termination_reason.is_none(), "no termination on block");

    // loop_check_gh_error event must exist
    let events_path = project_events(&cwd);
    let events = event_text(&events_path);
    assert!(
        events.contains("loop_check_gh_error"),
        "loop_check_gh_error event expected; events: {events}"
    );
    // No termination event
    assert!(
        !events.contains("\"termination\""),
        "no termination event on gh failure"
    );

    // A fresh claude session on the same failed read parks.
    fs::write(
        &manifest_path,
        new_manifest("sess-park1", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    let rows = |kind: &str| -> Vec<serde_json::Value> {
        event_text(&events_path)
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v.pointer("/type").and_then(|t| t.as_str()) == Some(kind))
            .filter_map(|v| v.pointer("/data").cloned())
            .filter(|d| d.pointer("/session_id").and_then(|s| s.as_str()) == Some("sess-park1"))
            .collect()
    };
    let expiry = |row: &serde_json::Value| {
        let blocker = row.pointer("/blocker").and_then(|b| b.as_str());
        (
            blocker.map(str::to_string),
            row.pointer("/expires_at_ms").and_then(|e| e.as_i64()),
        )
    };
    let now_ms = chrono::DateTime::parse_from_rfc3339("2026-06-05T00:30:00Z")
        .unwrap()
        .timestamp_millis();
    let slack_ms = 12 * 60_000;
    let park = |ms: i64| (Some("gh_read".to_string()), Some(now_ms + ms + slack_ms));

    let (_, d) = fire_as("claude");
    assert_eq!(d.decision, "allow", "a failed read parks: {}", d.message);
    assert!(
        d.message.contains("PARKED") && !d.message.contains("<help"),
        "{}",
        d.message
    );
    let idle: Vec<_> = rows("loop_check_watch_idle").iter().map(expiry).collect();
    assert_eq!(idle, vec![park(180_000)]);
    let fired = rows("loop_check");
    assert_eq!(fired.len(), 1, "{fired:?}");
    let field = |key: &str| fired.first().and_then(|r| r.pointer(key)).cloned();
    assert_eq!(field("/decision"), Some("allow".into()));
    assert_eq!(field("/fp_read_failed"), Some(true.into()));

    let (_, d) = fire_as("claude");
    assert_eq!(d.decision, "allow");
    let idle: Vec<_> = rows("loop_check_watch_idle").iter().map(expiry).collect();
    assert_eq!(
        idle.last(),
        Some(&park(360_000)),
        "the second failed fire doubles the park"
    );
}

/// AC1-UI: CI red -> block, message names the failing check.
#[test]
fn ac1_ui_ci_red_block_names_check() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");

    fs::write(
        &manifest_path,
        new_manifest("sess-ui1", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_with_promise()).unwrap();

    let mock = MockBins::ci_red();

    let (_code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ]);

    assert_eq!(d.decision, "block");
    assert!(
        d.message.contains("unit-tests") || d.message.contains("CI") || d.message.contains("ci"),
        "block message should name the failing check; got: {}",
        d.message
    );
}

/// AC3-HP: budget trip via FLAT budget_cap key (ab-41b13d9d fold-in proof).
#[test]
fn ac3_hp_budget_flat_key_trips_cost() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");

    // Manifest with NO nested budget block; the settings.yaml has flat budget_cap
    fs::write(
        &manifest_path,
        new_manifest("sess-budget", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_empty()).unwrap();

    // Settings file with flat budget_cap: 0.01 (very low)
    let settings_path = cwd.join(".fno/config.toml");
    fs::write(&settings_path, "budget_cap = 0.01\n").unwrap();

    let ledger_path = cwd.join(".fno/ledger.json");
    std::fs::create_dir_all(ledger_path.parent().unwrap()).unwrap();
    let ledger = serde_json::json!({"entries": [
        {"session_id": "sess-budget", "cost_usd": 0.05, "tokens": 1000}
    ]});
    fs::write(&ledger_path, serde_json::to_string(&ledger).unwrap()).unwrap();

    let mock = MockBins::green();

    let (code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        "--settings",
        settings_path.to_str().unwrap(),
        "--ledger",
        ledger_path.to_str().unwrap(),
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ]);

    assert_eq!(code, 0);
    assert_eq!(d.decision, "allow");
    assert_eq!(d.termination_reason.as_deref(), Some("Budget"));

    // Verify axis=cost in the termination event
    let events = event_text(&project_events(&cwd));
    assert!(
        events.contains("cost"),
        "axis=cost expected in Budget termination event; got: {events}"
    );
}

/// <aborted> tag -> Aborted termination.
#[test]
fn aborted_tag_terminates() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");
    fs::write(
        &manifest_path,
        new_manifest("sess-aborted", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_with_aborted()).unwrap();

    let mock = MockBins::green();

    let (_code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ]);

    assert_eq!(d.decision, "allow");
    assert_eq!(d.termination_reason.as_deref(), Some("Aborted"));
}

/// no_ship manifest + promise -> DoneAdvisory (no done() PR reads needed).
#[test]
fn no_ship_advisory_terminates() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");

    let manifest = "---\nsession_id: sess-nship\ncreated_at: 2026-06-05T00:00:00Z\nattended: true\nno_ship: true\n---\n";
    fs::write(&manifest_path, manifest).unwrap();
    fs::write(&transcript_path, transcript_with_promise()).unwrap();

    let mock = MockBins::green();

    let (_code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ]);

    assert_eq!(d.decision, "allow");
    assert_eq!(d.termination_reason.as_deref(), Some("DoneAdvisory"));
}

/// AC2-HP: N=3 consecutive identical fingerprints (unattended) -> NoProgress.
#[test]
fn ac2_hp_fingerprint_backstop_no_progress() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");
    let events_path = project_events(&cwd);

    // Unattended session (N=3)
    fs::write(
        &manifest_path,
        new_manifest("sess-backstop", "2026-06-05T00:00:00Z", false),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_empty()).unwrap();

    // gh says no PR -> pr_state=none for all fires
    let mock = MockBins::no_pr();
    let args_base = [
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
        "--events",
        events_path.to_str().unwrap(),
    ];

    // Fire 1 and 2: block, no termination
    let (_, d1) = fire(&args_base);
    assert_eq!(d1.decision, "block");
    let (_, d2) = fire(&args_base);
    assert_eq!(d2.decision, "block");

    // Fire 3: should trip backstop -> NoProgress
    let (_, d3) = fire(&args_base);
    assert_eq!(d3.decision, "allow");
    assert_eq!(d3.termination_reason.as_deref(), Some("NoProgress"));
    assert_eq!(d3.fires, 3);
}

/// AC5-HP: declared no-CI (ci.declared_none: true) -> CI read skipped, DonePRGreen reachable.
#[test]
fn ac5_hp_declared_no_ci_skipped() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");
    let settings_path = cwd.join(".fno/config.toml");

    fs::write(
        &manifest_path,
        new_manifest("sess-noci", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_with_promise()).unwrap();
    fs::write(
        &settings_path,
        "[ci]\ndeclared_none = true\n[review]\nself_review_required = false\n",
    )
    .unwrap();

    // gh: returns no checks (empty array) but we declared no-ci so it should skip
    let dir = TempDir::new().unwrap();
    let gh = make_script(
        dir.path(),
        "gh",
        r#"
if echo "$*" | grep -q -- "--version"; then echo 'gh version 2.x'; exit 0; fi
if echo "$*" | grep -q "headRefName"; then
  echo '{"state":"OPEN","number":5,"headRefName":"main","headRefOid":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"}'
  exit 0
fi
if echo "$*" | grep -q "checks"; then
  echo '[]'
  exit 0
fi
if echo "$*" | grep -q "pulls/"; then
  echo '[]'
  exit 0
fi
if echo "$*" | grep -q "reviews"; then
  echo '{"reviews":[{"author":{"login":"chatgpt-codex-connector"},"state":"COMMENTED","submittedAt":"2026-06-05T01:00:00Z","commit":{"oid":"deadbeefdeadbeefdeadbeefdeadbeef00000001"}}],"comments":[]}'
  exit 0
fi
exit 1
"#,
    );
    let git = make_script(
        dir.path(),
        "git",
        r#"echo "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee""#,
    );

    let (_, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        "--settings",
        settings_path.to_str().unwrap(),
        &format!("--gh-bin={}", gh.display()),
        &format!("--git-bin={}", git.display()),
    ]);

    assert_eq!(d.decision, "allow");
    // x-0eaf: the fixture explicitly opts out of the local self-review floor,
    // so the no-review configuration reaches DonePRGreen.
    assert_eq!(d.termination_reason.as_deref(), Some("DonePRGreen"));
}

/// x-2e20: a hosted workflow revokes declared no-CI before its first check exists.
#[test]
fn detected_workflow_revokes_declared_no_ci() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    fs::create_dir_all(cwd.join(".github/workflows")).unwrap();
    isolate_settings(cwd);

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");
    let settings_path = cwd.join(".fno/config.toml");
    fs::write(
        &manifest_path,
        new_manifest("sess-new-ci", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_with_promise()).unwrap();
    fs::write(&settings_path, "[ci]\ndeclared_none = true\n").unwrap();
    fs::write(cwd.join(".github/workflows/ci.yml"), "name: ci\n").unwrap();

    let dir = TempDir::new().unwrap();
    let gh = make_script(
        dir.path(),
        "gh",
        r#"
if echo "$*" | grep -q -- "--version"; then echo 'gh version 2.x'; exit 0; fi
if echo "$*" | grep -q "headRefName"; then
  echo '{"state":"OPEN","number":5,"headRefName":"main","headRefOid":"dddddddddddddddddddddddddddddddddddddddd"}'
  exit 0
fi
if echo "$*" | grep -q "checks"; then echo '[]'; exit 0; fi
if echo "$*" | grep -q "pulls/"; then echo '[]'; exit 0; fi
if echo "$*" | grep -q "reviews"; then
  echo '{"reviews":[],"comments":[]}'
  exit 0
fi
exit 1
"#,
    );
    let git = make_script(
        dir.path(),
        "git",
        r#"echo "dddddddddddddddddddddddddddddddddddddddd""#,
    );

    let (_, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        "--settings",
        settings_path.to_str().unwrap(),
        &format!("--gh-bin={}", gh.display()),
        &format!("--git-bin={}", git.display()),
    ]);

    assert_eq!(d.decision, "block");
    assert!(d.message.contains("no CI checks found"));
}

/// Corrupt manifest -> allow + note on stderr (never panics, never traps).
#[test]
fn corrupt_manifest_allows_exit() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");

    fs::write(&manifest_path, "this is not yaml frontmatter at all").unwrap();
    fs::write(&transcript_path, transcript_empty()).unwrap();

    let mock = MockBins::green();

    let (code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ]);

    assert_eq!(code, 0, "corrupt manifest must not exit non-zero");
    assert_eq!(
        d.decision, "allow",
        "corrupt manifest -> allow (never trap)"
    );
}

/// Events are appended to BOTH project and global paths. The same no-intent
/// stop blocks for the loop, but a goal owner allows it; a goal-owned promise
/// still meets every done read.
#[test]
fn events_appended_to_both_project_and_global() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let global_dir = tmp.path().join("global_fno");
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);
    fs::create_dir_all(&global_dir).unwrap();

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");
    let global_events = global_dir.join("events.jsonl");

    fs::write(
        &manifest_path,
        new_manifest("sess-dual", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_empty()).unwrap();

    let mock = MockBins::no_pr();

    let (_, nagged) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        "--global-events",
        global_events.to_str().unwrap(),
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
        &format!(
            "--gh-budget-ledger={}",
            cwd.join(".fno/gh-budget.json").display()
        ),
    ]);

    assert!(fno_agent::event_store::store_path(&project_events(&cwd)).exists());
    assert!(fno_agent::event_store::store_path(&global_events).exists());

    let proj_events = event_text(&project_events(&cwd));
    let glob_events = event_text(&global_events);
    assert!(
        !proj_events.is_empty() && !glob_events.is_empty(),
        "both event files must have content"
    );
    assert_eq!(nagged.decision, "block");
    assert!(
        nagged.message.contains("continue working"),
        "{}",
        nagged.message
    );

    // One session per goal fire: a second fire on one session reaches the
    // mute probe's done read, not Step 5.
    let goal_fire = |session: &str, transcript: String| {
        let manifest = new_manifest(session, "2026-06-05T00:00:00Z", true);
        fs::write(&manifest_path, manifest).unwrap();
        fs::write(&transcript_path, transcript).unwrap();
        fire(&[
            "loop-check",
            "--state",
            manifest_path.to_str().unwrap(),
            "--transcript",
            transcript_path.to_str().unwrap(),
            "--cwd",
            cwd.to_str().unwrap(),
            "--now",
            "2026-06-05T00:30:00Z",
            "--continuation-owner",
            "goal",
            &format!("--gh-bin={}", mock.gh.display()),
            &format!("--git-bin={}", mock.git.display()),
        ])
        .1
    };
    let owned = goal_fire("sess-goal-a", transcript_empty());
    assert_eq!(owned.decision, "allow", "{}", owned.message);
    assert!(
        owned.fingerprint.is_some(),
        "the fingerprint is still recorded"
    );
    let false_done = goal_fire("sess-goal-b", transcript_with_promise());
    assert_eq!(false_done.decision, "block", "{}", false_done.message);
}

/// AC5-ERR: CLI misuse (no --state flag) -> exit code 2 from parse_args
/// validation, never a panic downstream.
#[test]
fn cli_misuse_exits_2() {
    let args: Vec<String> = vec!["loop-check".to_string()];
    let (code, json) = fno_agent::loopcheck::run_loop_check_capture(&args);
    assert_eq!(code, 2, "missing --state must exit 2 (CLI misuse): {json}");
    assert!(
        json.contains("--state is required"),
        "error JSON must name the missing flag; got: {json}"
    );
}

/// AC5-UI: golden fingerprint - the exact fingerprint string for a known
/// world state must survive the enum refactor byte-identically.
/// head_sha|pr_state|ci_conclusion|latest_review_ts
#[test]
fn ac5_ui_golden_fingerprint_byte_identical() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");

    fs::write(
        &manifest_path,
        new_manifest("sess-golden", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_with_promise()).unwrap();

    let mock = MockBins::green();

    let (_code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ]);

    assert_eq!(
        d.fingerprint.as_deref(),
        Some("deadbeefdeadbeefdeadbeefdeadbeef00000001|OPEN|SUCCESS|2026-06-05T01:00:00Z"),
        "fingerprint format must be byte-identical to the pre-enum string"
    );
}

/// A gh that exists but cannot spawn (644 file, EACCES) is spawn trouble,
/// NOT absence: the fire must not drop to advisory mode, and the probe's
/// conclusion must be observable as a gh_probe event row. This is the
/// integration-level positive marker for the x-c425 class fix.
#[test]
fn gh_unspawnable_stays_out_of_advisory_mode() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");
    fs::write(
        &manifest_path,
        new_manifest("sess-trouble", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_with_promise()).unwrap();

    // gh exists but is not executable: every spawn fails EACCES, a spawn
    // error that is NOT absence.
    let gh_644 = cwd.join("gh-644");
    fs::write(&gh_644, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = fs::metadata(&gh_644).unwrap().permissions();
    perms.set_mode(0o644);
    fs::set_permissions(&gh_644, perms).unwrap();

    let (.., git) = MockBins::no_gh();
    let (code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:10:00Z",
        &format!("--gh-bin={}", gh_644.display()),
        &format!("--git-bin={}", git.display()),
    ]);

    assert_eq!(code, 0);
    let events = event_text(&project_events(&cwd));
    assert!(
        events.contains("\"gh_probe\""),
        "probe outcome row expected: {events}"
    );
    assert!(
        events.contains("\"spawn_trouble\""),
        "EACCES must read as spawn trouble, not absence: {events}"
    );
    assert!(
        !events.contains("\"loop_advisory_mode\""),
        "a spawn failure must not degrade the session to advisory mode: {events}"
    );
    assert_ne!(
        d.termination_reason.as_deref(),
        Some("DoneAdvisory"),
        "spawn trouble is not absence; no advisory termination"
    );
}

/// codex P1 on #447: a green PR whose head != local HEAD (unpushed local
/// commit) must NOT terminate DonePRGreen; the block message names the push.
#[test]
fn promise_green_pr_with_unpushed_head_blocks() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");
    fs::write(
        &manifest_path,
        new_manifest("sess-unpushed", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_with_promise()).unwrap();

    // green() PR head is deadbeef...0001; local git stub reports a DIFFERENT sha.
    let mock = MockBins::green();
    let dir = TempDir::new().unwrap();
    let git_ahead = make_script(
        dir.path(),
        "git",
        r#"echo "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaunpushed1""#,
    );

    let (code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", git_ahead.display()),
    ]);

    assert_eq!(code, 0);
    assert_eq!(
        d.decision, "block",
        "unpushed local HEAD must block: {}",
        d.message
    );
    assert!(d.termination_reason.is_none());
    assert!(
        d.message.contains("push"),
        "block message should tell the agent to push; got: {}",
        d.message
    );
}

// ── step 2: required_bots review gate (US1) ──────────────────────────────────

/// gh mock: green CI + head-shipped, but only a NON-required bot (gemini)
/// reviewed. Under the codex-only default this must block.
fn green_gemini_only_reviewed() -> MockBins {
    let dir = TempDir::new().unwrap();
    let gh = make_script(
        dir.path(),
        "gh",
        r#"
if echo "$*" | grep -q -- "--version"; then echo 'gh version 2.x'; exit 0; fi
if echo "$*" | grep -q "headRefName"; then
  echo '{"state":"OPEN","number":9,"headRefName":"feat","headRefOid":"deadbeefdeadbeefdeadbeefdeadbeef00000009"}'
  exit 0
fi
if echo "$*" | grep -q "checks"; then
  echo '[{"name":"ci","state":"SUCCESS","bucket":"pass"}]'
  exit 0
fi
if echo "$*" | grep -q "pulls/"; then
  echo '[]'
  exit 0
fi
if echo "$*" | grep -q "reviews"; then
  echo '{"reviews":[{"author":{"login":"gemini-code-assist[bot]"},"state":"COMMENTED","submittedAt":"2026-06-05T01:00:00Z","commit":{"oid":"deadbeefdeadbeefdeadbeefdeadbeef00000001"}}],"comments":[]}'
  exit 0
fi
exit 1
"#,
    );
    let git = make_script(
        dir.path(),
        "git",
        r#"echo "deadbeefdeadbeefdeadbeefdeadbeef00000009""#,
    );
    MockBins { _dir: dir, gh, git }
}

/// AC1-ERR + AC1-UI: PR green and head-shipped but the required bot (codex
/// default) has not reviewed -> block, message names the missing bot, no
/// termination. A lone gemini COMMENTED review must no longer flip reviewed
/// (the PR #390 miss).
///
/// x-b167: chatgpt-codex-connector is now nudgeable by default, so a missing-codex
/// block renders the NeedsNudge message (name the bot + the mention it needs)
/// rather than the old passive "has not reviewed" line - the one install whose
/// behavior changes on upgrade is exactly one pinning a login footnote nudges.
#[test]
fn ac1_err_missing_required_bot_blocks_naming_bot() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");
    fs::write(
        &manifest_path,
        new_manifest("sess-reqbot1", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_with_promise()).unwrap();

    let mock = green_gemini_only_reviewed();

    let (code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ]);

    assert_eq!(code, 0);
    assert_eq!(
        d.decision, "block",
        "gemini-only review must not satisfy the codex-required default: {}",
        d.message
    );
    assert!(d.termination_reason.is_none());
    assert!(
        d.message.contains("chatgpt-codex-connector"),
        "block message must name the missing required bot; got: {}",
        d.message
    );
    // x-b167: the nudgeable-bot block names the mention it needs, not "has not
    // reviewed". (The mock gh has no `pr comment` handler, so the runtime post
    // fails and the bot stays NeedsNudge with the post-by-hand command shown.)
    assert!(
        d.message.contains("has not been asked") && d.message.contains("gh pr comment"),
        "block message must render the NeedsNudge nudge instruction; got: {}",
        d.message
    );
}

// ── step 2: inline findings gate (US2) ────────────────────────────────────────

/// Green CI + codex reviewed, with a parameterized /pulls/N/comments payload
/// (Read 4) and commits payload. The mock writes the JSON to files so shell
/// quoting stays trivial.
fn findings_mock(comments_json: &str, commits_json: &str) -> MockBins {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("comments.json"), comments_json).unwrap();
    fs::write(dir.path().join("commits.json"), commits_json).unwrap();
    let comments_path = dir.path().join("comments.json");
    let commits_path = dir.path().join("commits.json");
    let gh = make_script(
        dir.path(),
        "gh",
        &format!(
            r#"
if echo "$*" | grep -q -- "--version"; then echo 'gh version 2.x'; exit 0; fi
if echo "$*" | grep -q "headRefName"; then
  echo '{{"state":"OPEN","number":4,"headRefName":"feat","headRefOid":"deadbeefdeadbeefdeadbeefdeadbeef00000004"}}'
  exit 0
fi
if echo "$*" | grep -q "checks"; then
  echo '[{{"name":"ci","state":"SUCCESS","bucket":"pass"}}]'
  exit 0
fi
if echo "$*" | grep -q "/comments"; then
  cat "{comments}"
  exit 0
fi
if echo "$*" | grep -q "/commits"; then
  cat "{commits}"
  exit 0
fi
if echo "$*" | grep -q "reviews"; then
  echo '{{"reviews":[{{"author":{{"login":"chatgpt-codex-connector"}},"state":"COMMENTED","submittedAt":"2026-06-05T01:00:00Z","commit":{{"oid":"deadbeefdeadbeefdeadbeefdeadbeef00000001"}}}}],"comments":[]}}'
  exit 0
fi
exit 1
"#,
            comments = comments_path.display(),
            commits = commits_path.display()
        ),
    );
    let git = make_script(
        dir.path(),
        "git",
        r#"echo "deadbeefdeadbeefdeadbeefdeadbeef00000004""#,
    );
    MockBins { _dir: dir, gh, git }
}

fn fire_findings(cwd: &Path, mock: &MockBins) -> (i32, Decision) {
    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");
    fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T02:00:00Z",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ])
}

fn findings_cwd(session: &str) -> TempDir {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);
    fs::write(
        cwd.join("target-state.md"),
        new_manifest(session, "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(cwd.join("transcript.jsonl"), transcript_with_promise()).unwrap();
    tmp
}

/// x-f8d4 AC2-HP: an OPEN operator `review_finding` holds the success terminal
/// even with a green + reviewed PR and a promise; an explicit `resolve` clears
/// it and the next fire terminates DonePRGreen. The manifest is never mutated.
#[test]
fn operator_review_finding_blocks_until_resolved() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    // graph_node_id lets the gate scope findings to this node.
    let manifest = format!(
        "{}graph_node_id: x-gate\n",
        new_manifest("sess-finding", "2026-06-05T00:00:00Z", true)
    );
    let manifest_path = cwd.join("target-state.md");
    fs::write(&manifest_path, &manifest).unwrap();
    fs::write(cwd.join("transcript.jsonl"), transcript_with_promise()).unwrap();

    // Seed through the store the gate reads; the tail restores FNO_HOME (CI is single-threaded).
    let restore_home = std::env::var_os("FNO_HOME").unwrap_or_default();
    let home = TempDir::new().unwrap();
    std::env::set_var("FNO_HOME", home.path());
    fno_agent::graph_store::seed_rows(&home.path().join("graph.json"), &[serde_json::json!({"id": "x-gate", "slug": "x-gate", "title": "n", "type": "feature", "status": "ready", "priority": "p1"})]).unwrap();
    let store = fno_agent::backlog::api::Store::new(&home.path().join("graph.json"));
    let input = fno_agent::backlog::api::FindingInput {
        body: "operator says fix the retry".into(),
        ..Default::default()
    };
    let receipt = fno_agent::backlog::api::finding_create(&store, "x-gate", input).unwrap();

    let mock = MockBins::green();
    let manifest_before = fs::read(&manifest_path).unwrap();

    let (code, d) = fire_findings(cwd, &mock);
    assert_eq!(code, 0);
    assert_eq!(
        d.decision, "block",
        "an open operator finding must hold the gate even on a green PR: {}",
        d.message
    );
    assert!(d.termination_reason.is_none());
    let remedy = format!("fno backlog note --resolve {}", receipt.finding_id);
    assert!(
        d.message.contains(&receipt.finding_id) && d.message.contains(&remedy),
        "reason must quote the finding id + resolve remedy; got: {}",
        d.message
    );
    assert_eq!(
        manifest_before,
        fs::read(&manifest_path).unwrap(),
        "target-state.md must not be mutated"
    );

    // Resolve it -> the gate clears and the promise terminates DonePRGreen.
    fno_agent::backlog::api::finding_resolve(&store, &receipt.finding_id, Some("sess-finding"))
        .unwrap();

    let (_, d2) = fire_findings(cwd, &mock);
    std::env::set_var("FNO_HOME", restore_home);
    assert_eq!(
        d2.decision, "allow",
        "a resolved finding must no longer block: {}",
        d2.message
    );
    assert_eq!(d2.termination_reason.as_deref(), Some("DonePRGreen"));
}

const CODEX_P1_NO_REPLY: &str = r#"[
  {"id": 100, "in_reply_to_id": null,
   "user": {"login": "chatgpt-codex-connector[bot]"},
   "body": "![P1 Badge](https://img.shields.io/badge/P1-orange?style=flat) Off-by-one",
   "path": "src/x.rs", "line": 42, "created_at": "2026-06-05T01:10:00Z"}
]"#;

/// AC2-ERR + AC2-UI: unaddressed P1 blocks; message carries path:line and the
/// remedy. Also proves the inline ts feeds the fingerprint's 4th component
/// (01:10 > the 01:00 review ts).
#[test]
fn ac2_err_unaddressed_p1_blocks_with_path_line() {
    let tmp = findings_cwd("sess-p1");
    let mock = findings_mock(CODEX_P1_NO_REPLY, r#"[]"#);

    let (code, d) = fire_findings(tmp.path(), &mock);

    assert_eq!(code, 0);
    assert_eq!(
        d.decision, "block",
        "unaddressed P1 must block: {}",
        d.message
    );
    assert!(d.termination_reason.is_none());
    assert!(
        d.message.contains("src/x.rs:42"),
        "message must carry the finding's path:line; got: {}",
        d.message
    );
    assert!(
        d.message.contains("wontfix:") && d.message.contains("Reply in-thread"),
        "message must name the remedy; got: {}",
        d.message
    );
    // A no-reply finding is the top-level-comment blind spot this message
    // exists for: it names the missing mechanism, not just the remedy.
    assert!(
        d.message.contains("no in-thread reply"),
        "no-reply message must name the missing mechanism; got: {}",
        d.message
    );
    assert!(
        d.fingerprint
            .as_deref()
            .unwrap_or("")
            .ends_with("|2026-06-05T01:10:00Z"),
        "inline finding ts must feed the fingerprint 4th component; got: {:?}",
        d.fingerprint
    );
}

/// AC2-HP (commit arm): P1 + non-bot in-thread reply + commit after the
/// finding -> addressed -> DonePRGreen.
#[test]
fn ac2_hp_addressed_via_commit_terminates() {
    let comments = r#"[
  {"id": 100, "in_reply_to_id": null,
   "user": {"login": "chatgpt-codex-connector[bot]"},
   "body": "![P1 Badge](https://img.shields.io/badge/P1-orange?style=flat) Off-by-one",
   "path": "src/x.rs", "line": 42, "created_at": "2026-06-05T01:10:00Z"},
  {"id": 101, "in_reply_to_id": 100,
   "user": {"login": "bllshttng"},
   "body": "Fixed in deadbeef.",
   "created_at": "2026-06-05T01:20:00Z"}
]"#;
    let commits = r#"[{"commit":{"committer":{"date":"2026-06-05T01:30:00Z"}}}]"#;
    let tmp = findings_cwd("sess-p1fix");
    let mock = findings_mock(comments, commits);

    let (_, d) = fire_findings(tmp.path(), &mock);
    assert_eq!(d.decision, "allow", "addressed P1 must pass: {}", d.message);
    assert_eq!(d.termination_reason.as_deref(), Some("DonePRGreen"));
}

// ── step 2: declared no-review repo (US3) ─────────────────────────────────────

/// Green PR + CI mock whose review endpoints (reviews / pulls comments /
/// commits) all FAIL: proves Reads 3+4 are genuinely skipped, not just
/// tolerated, when the repo declares `required_bots: []`.
fn green_reviews_unreachable() -> MockBins {
    let dir = TempDir::new().unwrap();
    let gh = make_script(
        dir.path(),
        "gh",
        r#"
if echo "$*" | grep -q -- "--version"; then echo 'gh version 2.x'; exit 0; fi
if echo "$*" | grep -q "headRefName"; then
  echo '{"state":"OPEN","number":11,"headRefName":"feat","headRefOid":"deadbeefdeadbeefdeadbeefdeadbeef00000011"}'
  exit 0
fi
if echo "$*" | grep -q "checks"; then
  echo '[{"name":"ci","state":"SUCCESS","bucket":"pass"}]'
  exit 0
fi
exit 1
"#,
    );
    let git = make_script(
        dir.path(),
        "git",
        r#"echo "deadbeefdeadbeefdeadbeefdeadbeef00000011""#,
    );
    MockBins { _dir: dir, gh, git }
}

/// gh/git pair for the PR-payload floor tests: the branch's PR carries
/// `files_json`, CI is green, reviews read empty, and the LOCAL cwd diff is
/// EMPTY (the any-directory case: git resolves origin/main but the diff
/// answers nothing). `pr_files` is served for `pulls/N/files`.
fn pr_payload_mock(pr_files: &str) -> MockBins {
    let dir = TempDir::new().unwrap();
    let gh = make_script(
        dir.path(),
        "gh",
        &format!(
            r#"
if echo "$*" | grep -q -- "--version"; then echo 'gh version 2.x'; exit 0; fi
if echo "$*" | grep -q "headRefName"; then
  echo '{{"state":"OPEN","number":11,"headRefName":"feat","headRefOid":"deadbeefdeadbeefdeadbeefdeadbeef00000011"}}'
  exit 0
fi
if echo "$*" | grep -q "checks"; then
  echo '[{{"name":"ci","state":"SUCCESS","bucket":"pass"}}]'
  exit 0
fi
if echo "$*" | grep -q "files"; then
  printf '{pr_files}'
  exit 0
fi
if echo "$*" | grep -q "comments"; then
  echo '[]'
  exit 0
fi
if echo "$*" | grep -q "reviews"; then
  echo '{{"reviews":[],"comments":[]}}'
  exit 0
fi
exit 1
"#,
        ),
    );
    let git = make_script(
        dir.path(),
        "git",
        r#"case "$*" in
  # Freshness identity stays uncomputable (see green()).
  *--raw*) exit 1 ;;
  # classify_payload: origin/main resolves, and the branch diff answers
  # EMPTY - the fire sits where the local diff says nothing.
  rev-parse*origin/main*) echo "sha" ;;
  *origin/main*) exit 0 ;;
  *) echo "deadbeefdeadbeefdeadbeefdeadbeef00000011" ;;
esac"#,
    );
    MockBins { _dir: dir, gh, git }
}

fn fire_pr_payload(cwd: &Path, settings: &str, mock: &MockBins) -> (i32, Decision) {
    let settings_path = cwd.join(".fno/config.toml");
    fs::write(&settings_path, settings).unwrap();
    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");
    fs::write(
        &manifest_path,
        new_manifest("sess-prpayload", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_with_promise()).unwrap();
    fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        "--settings",
        settings_path.to_str().unwrap(),
        // The fire() hermeticity default pins `none`, the hermetic opt-out
        // that disables the floor; the payload tests exercise the floor, so
        // they pin a commandful harness the way a live claude fire resolves.
        "--author-harness",
        "claude",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ])
}

/// x-8cb6 payload: a code PR must floor the self-review reviewer no matter
/// what the local diff says. review_skipped is the field that told the truth
/// while the terminal message lied, so it carries the assertion.
#[test]
fn code_pr_floors_even_where_the_cwd_diff_is_empty() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();

    // self_review_required defaults true; no lane configured.
    let mock = pr_payload_mock(r#"[{"filename":"crates/fno-agent/src/lib.rs"}]"#);
    let (code, d) = fire_pr_payload(cwd, "[review]\nrequired_bots = []\n", &mock);

    assert_eq!(code, 0);
    let events = event_text(&project_events(&cwd));
    assert!(
        events.contains("\"review_skipped\":false"),
        "the floor must engage from the PR payload; review_skipped is the proof: {events}"
    );
    assert!(
        !events.contains("green and reviewed"),
        "an uncovered head must never reach the green-and-reviewed terminal: {events}"
    );
    assert_ne!(
        d.termination_reason.as_deref(),
        Some("DonePRGreen"),
        "an uncovered code PR must not terminate DonePRGreen: {}",
        d.message
    );
}

/// A no_external session on a repo with an ACTIVE login gate suppressed reads
/// the config demanded, so the coverage axis must serialize as "unknown"
/// (retry remedy intact) - never a definitive "uncovered" for bots that were
/// never queried. The inactive-gate skip (nothing configured) stays a known
/// zero; see ac3_hp_empty_required_bots_skips_review_reads.
#[test]
fn no_external_on_active_gate_serializes_unknown_coverage_not_uncovered() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();

    let settings_path = cwd.join(".fno/config.toml");
    fs::write(
        &settings_path,
        "[review]\nrequired_bots = [\"chatgpt-codex-connector\"]\n",
    )
    .unwrap();

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");
    let manifest = "---\nsession_id: sess-noext2\ncreated_at: 2026-06-05T00:00:00Z\nattended: true\nno_external: true\n---\n";
    fs::write(&manifest_path, manifest).unwrap();
    fs::write(&transcript_path, transcript_with_promise()).unwrap();

    let mock = green_reviews_unreachable();

    let (_code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        "--settings",
        settings_path.to_str().unwrap(),
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ]);

    assert_eq!(
        d.decision, "allow",
        "no_external must not block: {}",
        d.message
    );
    let events = event_text(&project_events(&cwd));
    assert!(
        events.contains("\"coverage\":\"unknown\""),
        "no_external on an active gate must read unknown, not a fabricated zero: {events}"
    );
    assert!(
        !events.contains("\"coverage\":\"uncovered\""),
        "an unqueried GitHub axis must never serialize as a definitive uncovered: {events}"
    );
}

// ── x-2219: the round budget on the no-external lane ─────────────────────────
// A stock install (required_bots empty, no optional lane) never reached the
// external arm's reviews refresh, so rounds that exist only as GitHub review
// objects read 0/2 forever and the cap could not fire on exactly the lane
// that spun (PR #1225: five real rounds, counter 0/2, six invocations).

// ── x-e703: config.review.reviewers local-attestation gate ──────────────────

/// The green() git+gh mock's HEAD (== headRefOid, so head_shipped passes). An
/// attestation must carry this exact sha to satisfy the head-pin.
const GREEN_HEAD: &str = "deadbeefdeadbeefdeadbeefdeadbeef00000001";

/// AC3-HP: a `reviewers: [sigma]` gate with NO matching attestation holds the
/// session closed even when the PR is green, CI passes, and HEAD is shipped -
/// the local attestation is required, absence fails closed.
#[test]
fn reviewers_gate_blocks_without_attestation() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();

    let settings_path = cwd.join(".fno/config.toml");
    fs::write(&settings_path, "[review]\nreviewers = [\"sigma\"]\n").unwrap();

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");
    fs::write(
        &manifest_path,
        new_manifest("sess-rvw-block", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_with_promise()).unwrap();

    let mock = MockBins::green();
    let (_code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        "--settings",
        settings_path.to_str().unwrap(),
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ]);

    assert_eq!(
        d.decision, "block",
        "reviewers gate with no attestation must block: {}",
        d.message
    );
    assert!(d.termination_reason.is_none());

    // The message, not just the verdict. Every unit test builds a PrInfo
    // literal, so `read_pr_info` could stop threading the unattested list
    // through (`unattested_reviewers: Vec::new()`) and every one of them would
    // still pass while the reason silently reverted to the generic fallback -
    // the exact failure this node exists to delete. This is the only assertion
    // in the suite that runs the real wiring end to end.
    assert!(
        d.message.contains("reviewers gate unmet"),
        "block reason must name the reviewers gate: {}",
        d.message
    );
    assert!(
        d.message.contains("sigma") && d.message.contains("/fno:review"),
        "block reason must name the reviewer and its invocation: {}",
        d.message
    );
    assert!(
        !d.message.contains("/fno:review sigma"),
        "sigma is retired; the invocation names the lane, never a panel run: {}",
        d.message
    );
    assert!(
        !d.message.contains("bot reviewer"),
        "block reason must not blame a bot: {}",
        d.message
    );
    assert!(
        !d.message.contains("<watching"),
        "an unmet local gate is work to do, never an idle: {}",
        d.message
    );
}

/// AC3-HP / AC8-HP: the gate clears once a head-pinned `review_attestation`
/// (reviewer sigma, verdict pass, head_sha == current HEAD) exists.
#[test]
fn reviewers_gate_clears_with_head_pinned_attestation() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();

    let settings_path = cwd.join(".fno/config.toml");
    fs::write(&settings_path, "[review]\nreviewers = [\"sigma\"]\n").unwrap();
    // The attestation lands in the project events log loop-check reads.
    fs::write(
        project_events(&cwd),
        format!(
            "{{\"ts\":\"2026-06-05T00:10:00Z\",\"type\":\"review_attestation\",\"source\":\"target\",\"data\":{{\"reviewer\":\"sigma\",\"head_sha\":\"{GREEN_HEAD}\",\"verdict\":\"pass\"}}}}\n"
        ),
    )
    .unwrap();

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");
    fs::write(
        &manifest_path,
        new_manifest("sess-rvw-pass", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_with_promise()).unwrap();

    let mock = MockBins::green();
    let (_code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        "--settings",
        settings_path.to_str().unwrap(),
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ]);

    assert_eq!(
        d.decision, "allow",
        "a head-pinned sigma attestation must clear the gate: {}",
        d.message
    );
    assert_eq!(d.termination_reason.as_deref(), Some("DonePRGreen"));
}

// ── step 2: gh-outage streak freeze (US4) ─────────────────────────────────────

/// Locked decision 6 (REVERSES the wedge): backstop tripped + done() gh
/// error -> block-and-retry with the read named, NEVER NoProgress.
#[test]
fn ac4_err_done_read_failure_never_no_progress() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");
    let events_path = project_events(&cwd);

    fs::write(
        &manifest_path,
        new_manifest("sess-rev1659", "2026-06-05T00:00:00Z", false),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_empty()).unwrap();

    // Pre-read endpoints (state/checks/reviews) healthy; Read 4 (pulls/)
    // fails -> only the done() path errors.
    let dir = TempDir::new().unwrap();
    let gh = make_script(
        dir.path(),
        "gh",
        r#"
if echo "$*" | grep -q -- "--version"; then echo 'gh version 2.x'; exit 0; fi
if echo "$*" | grep -q "headRefName"; then
  echo '{"state":"OPEN","number":12,"headRefName":"feat","headRefOid":"deadbeefdeadbeefdeadbeefdeadbeef00000012"}'
  exit 0
fi
if echo "$*" | grep -q "checks"; then
  echo '[{"name":"ci","state":"SUCCESS","bucket":"pass"}]'
  exit 0
fi
if echo "$*" | grep -q "pulls/"; then
  echo 'API rate limit exceeded' >&2
  exit 1
fi
if echo "$*" | grep -q "reviews"; then
  echo '{"reviews":[{"author":{"login":"chatgpt-codex-connector"},"state":"COMMENTED","submittedAt":"2026-06-05T01:00:00Z","commit":{"oid":"deadbeefdeadbeefdeadbeefdeadbeef00000001"}}],"comments":[]}'
  exit 0
fi
exit 1
"#,
    );
    let git = make_script(
        dir.path(),
        "git",
        r#"echo "deadbeefdeadbeefdeadbeefdeadbeef00000012""#,
    );

    let args = [
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", gh.display()),
        &format!("--git-bin={}", git.display()),
        "--events",
        events_path.to_str().unwrap(),
    ];

    // Fires 1-2: quiet healthy blocks (streak 1, 2).
    let (_, d1) = fire(&args);
    assert_eq!(d1.decision, "block");
    let (_, d2) = fire(&args);
    assert_eq!(d2.decision, "block");

    // Fire 3: streak hits N=3 -> backstop trips -> done() runs -> Read 4
    // fails. The wedge terminated NoProgress here; step 2 blocks-and-retries.
    let (_, d3) = fire(&args);
    assert_eq!(
        d3.decision, "block",
        "gh-errored done() must block, not terminate: {}",
        d3.message
    );
    assert!(
        d3.termination_reason.is_none(),
        "REVERSED: no NoProgress on a gh-errored done() read; got {:?}",
        d3.termination_reason
    );
    assert!(
        d3.message.contains("pulls_comments"),
        "block must name the failing read; got: {}",
        d3.message
    );

    let events = event_text(&events_path);
    assert!(events.contains("loop_check_gh_error"));
    assert!(
        !events.contains("\"termination\""),
        "no termination event during the gh error; events: {events}"
    );

    // AC4-UI: the gh-error event records the failed read AND the stderr tail.
    let err_event = events
        .lines()
        .find(|l| l.contains("loop_check_gh_error"))
        .expect("loop_check_gh_error event");
    let v: serde_json::Value = serde_json::from_str(err_event).unwrap();
    assert_eq!(
        v.pointer("/data/read").and_then(|x| x.as_str()),
        Some("pulls_comments")
    );
    assert!(
        v.pointer("/data/stderr_tail")
            .and_then(|x| x.as_str())
            .map(|s| s.contains("rate limit"))
            .unwrap_or(false),
        "stderr_tail must carry the gh error text; event: {err_event}"
    );
}

// ── AC2: budget cap comment-tail tolerance (ab-610d2ee3) ─────────────────────

/// Build a manifest with raw (hand-crafted) budget lines, bypassing the
/// fixture builder that always writes clean numeric values.
fn manifest_with_raw_budget(session_id: &str, created_at: &str, extra_lines: &str) -> String {
    format!(
        "---\nsession_id: {session_id}\ncreated_at: {created_at}\nattended: true\n{extra_lines}---\n"
    )
}

/// AC2-ERR: `budget_cost_cap_usd: abc` still classifies as Budget (fail closed).
#[test]
fn ac2_err_budget_cost_cap_malformed_fails_closed() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");

    fs::write(
        &manifest_path,
        manifest_with_raw_budget(
            "sess-ac2err",
            "2026-06-05T00:00:00Z",
            "budget_cost_cap_usd: abc\n",
        ),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_empty()).unwrap();

    let mock = MockBins::no_pr();

    let (_code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ]);

    // Non-numeric value (even after stripping) must still fail closed as Budget
    assert_eq!(
        d.termination_reason.as_deref(),
        Some("Budget"),
        "malformed `budget_cost_cap_usd: abc` must fail closed as Budget; \
         got termination_reason={:?}",
        d.termination_reason
    );
}

// ── done_probes: operational evidence at the ship gate (x-e54c) ───────────────

/// Build a plan doc with an optional `done_probes` frontmatter block.
fn plan_doc(probes: &[&str]) -> String {
    let mut s = String::from("---\ntitle: p\nstatus: ready\n");
    if !probes.is_empty() {
        s.push_str("done_probes:\n");
        for p in probes {
            s.push_str(&format!("  - \"{p}\"\n"));
        }
    }
    s.push_str("---\n\n# plan\n");
    s
}

fn manifest_with_plan(session_id: &str, created_at: &str, plan_path: &Path) -> String {
    format!(
        "---\nsession_id: {session_id}\ncreated_at: {created_at}\nattended: true\nplan_path: {}\n---\n",
        plan_path.display()
    )
}

/// Stage a probe-gated session: returns (cwd tempdir, manifest, transcript, plan).
fn probe_fixture(session_id: &str, probes: &[&str]) -> (TempDir, PathBuf, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    let plan = cwd.join("plan.md");
    fs::write(&plan, plan_doc(probes)).unwrap();

    let manifest = cwd.join("target-state.md");
    fs::write(
        &manifest,
        manifest_with_plan(session_id, "2026-06-05T00:00:00Z", &plan),
    )
    .unwrap();

    let transcript = cwd.join("transcript.jsonl");
    fs::write(&transcript, transcript_with_promise()).unwrap();

    (tmp, manifest, transcript)
}

fn fire_probe_gate(cwd: &Path, manifest: &Path, transcript: &Path, mock: &MockBins) -> Decision {
    let (_code, d) = fire(&[
        "loop-check",
        "--state",
        manifest.to_str().unwrap(),
        "--transcript",
        transcript.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ]);
    d
}

/// AC1-HP: every other conjunct holds and the declared probe exits 0 ->
/// DonePRGreen, with this fire's probe result recorded in the loop_check event.
#[test]
fn done_probes_ac1_hp_passing_probe_grants_done() {
    let (tmp, manifest, transcript) = probe_fixture("sess-probe-hp", &["echo probe-passed"]);
    let cwd = tmp.path();
    let mock = MockBins::green();

    let d = fire_probe_gate(cwd, &manifest, &transcript, &mock);

    assert_eq!(d.decision, "allow");
    assert_eq!(
        d.termination_reason.as_deref(),
        Some("DonePRGreen"),
        "a passing probe must not change the verdict: {}",
        d.message
    );

    let events = event_text(&project_events(&cwd));
    assert!(
        events.contains("\"done_probes\""),
        "probe evidence must be recorded in the loop_check event"
    );
    assert!(
        events.contains("\"echo probe-passed\":\"pass\""),
        "the probe result for THIS fire must be recorded: {events}"
    );
}

/// AC1-ERR: a failing probe refuses done, and the reason carries BOTH the
/// verbatim command and the literal exit code.
#[test]
fn done_probes_ac1_err_failing_probe_refuses_done() {
    let (tmp, manifest, transcript) =
        probe_fixture("sess-probe-fail", &["test -f /nonexistent-groom-report"]);
    let cwd = tmp.path();
    let mock = MockBins::green();

    let d = fire_probe_gate(cwd, &manifest, &transcript, &mock);

    assert_eq!(
        d.decision, "block",
        "a green PR with a failing probe must NOT be done: {}",
        d.message
    );
    assert_eq!(d.termination_reason, None);
    assert!(
        d.message.contains("test -f /nonexistent-groom-report"),
        "reason must name the verbatim probe: {}",
        d.message
    );
    assert!(
        d.message.contains("exited 1"),
        "reason must carry the literal exit code: {}",
        d.message
    );
}

/// AC1-HP (the clause the substring assertion above cannot pin): the gate must
/// run probes on THIS fire. A prior fire's recorded pass must never satisfy it.
#[test]
fn done_probes_a_prior_fires_pass_never_satisfies_this_fire() {
    let (tmp, manifest, transcript) =
        probe_fixture("sess-probe-cache", &["test -f /nonexistent-groom-report"]);
    let cwd = tmp.path();

    // Seed a prior fire in which the very same probe passed.
    fs::write(
        project_events(&cwd),
        format!(
            "{}\n",
            serde_json::json!({
                "type": "loop_check",
                "data": {
                    "session_id": "sess-probe-cache",
                    "done_probes": {"test -f /nonexistent-groom-report": "pass"}
                }
            })
        ),
    )
    .unwrap();

    let mock = MockBins::green();
    let d = fire_probe_gate(cwd, &manifest, &transcript, &mock);

    assert_eq!(
        d.decision, "block",
        "a cached pass from an earlier fire must not grant done: {}",
        d.message
    );
    assert!(d.message.contains("test -f /nonexistent-groom-report"));
}

// ── x-b167 nudge give-up policy, driven end-to-end through decide() ───────────
//
// The helper-level tests in loopcheck.rs pin classification/idle/message; these
// drive the real decision function so the bot_nudges -> post-loop -> backstop
// give-up wiring is exercised, not just its pieces (sigma integration-test gap).
// The mock gh controls the `pr comment` outcome, so no env var is needed to keep
// the suite from posting to a real PR.

/// A gh mock for a PR where the required codex bot has NOT reviewed. `comments`
/// is the JSON array body for `reviews,comments`; `comment_exit` is the exit
/// code of `gh pr comment` (0 = post lands, non-0 = post fails); when `marker`
/// is set, each `pr comment` call appends a line to it so a test can count posts.
fn nudge_mock(comments_json: &str, comment_exit: i32, marker: Option<&Path>) -> MockBins {
    let dir = TempDir::new().unwrap();
    let record = match marker {
        Some(p) => format!("echo x >> '{}'; ", p.display()),
        None => String::new(),
    };
    let body = format!(
        r#"
if echo "$*" | grep -q -- "--version"; then echo 'gh version 2.x'; exit 0; fi
if echo "$*" | grep -q "pr comment"; then {record}exit {comment_exit}; fi
if echo "$*" | grep -q "headRefName"; then
  echo '{{"state":"OPEN","number":1,"headRefName":"main","headRefOid":"deadbeefdeadbeefdeadbeefdeadbeef00000001"}}'
  exit 0
fi
if echo "$*" | grep -q "checks"; then echo '[{{"name":"ci","state":"SUCCESS","bucket":"pass"}}]'; exit 0; fi
if echo "$*" | grep -q "pulls/"; then echo '[]'; exit 0; fi
if echo "$*" | grep -q "reviews"; then echo '{{"reviews":[],"comments":{comments_json}}}'; exit 0; fi
exit 1
"#
    );
    let gh = make_script(dir.path(), "gh", &body);
    let git = make_script(
        dir.path(),
        "git",
        r#"case "$*" in
  # A test env has no real repo, so the freshness identity must be
  # UNCOMPUTABLE. Without this the stub answers `git diff --raw` with the
  # same one line at every sha, which compares equal to itself and
  # fabricates a carry out of nothing - the absence-matched-against-
  # absence shape the predicate exists to refuse. Scoped to --raw so
  # `git diff --name-only` (classify_payload) behaves exactly as before.
  *--raw*) exit 1 ;;
  *) echo "deadbeefdeadbeefdeadbeefdeadbeef00000001" ;;
esac"#,
    );
    MockBins { _dir: dir, gh, git }
}

/// AC10-HP: a NeedsNudge bot with a successful runtime post lands exactly one
/// `gh pr comment` and the block message reports the bot as nudged and awaiting.
#[test]
fn nudge_needs_nudge_posts_once_and_reports_awaiting() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);

    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");
    let marker = cwd.join("posts.txt");
    fs::write(
        &manifest_path,
        new_manifest("sess-nudge-post", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_with_promise()).unwrap();

    // No review and no mention -> NeedsNudge -> post succeeds -> Awaiting.
    let mock = nudge_mock("[]", 0, Some(&marker));

    let (code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ]);

    assert_eq!(code, 0);
    assert_eq!(d.decision, "block", "got: {}", d.message);
    assert!(
        d.message.contains("nudged") && d.message.contains("awaiting"),
        "post landed -> message must report nudged + awaiting; got: {}",
        d.message
    );
    let posts = fs::read_to_string(&marker).unwrap_or_default();
    assert_eq!(
        posts.lines().count(),
        1,
        "exactly one gh pr comment must be issued; got: {posts:?}"
    );
}

// ── coverage classifier (x-0eaf task 1.1) ────────────────────────────────────
use fno_agent::loopcheck::{
    classify_coverage, coverage_receipt_line, AttestationOrigin, Coverage, CoverageProducer,
    CoverageVerdict, Freshness,
};

const COV_HEAD: &str = "abc1234567890abcdef1234567890abcdef1234";

/// A review object as `gh pr view --json reviews` returns one, including the
/// `commit.oid` the review was submitted against. That field is not decoration:
/// coverage reads it to decide whether the verdict still describes HEAD, and a
/// fixture without it exercises the absent-commit path rather than the one the
/// test means to (x-5b99). Defaults to COV_HEAD, so a bot review is fresh
/// unless a test says otherwise.
fn gh_review(author: &str, state: &str) -> serde_json::Value {
    gh_review_at(author, state, COV_HEAD)
}

fn gh_review_at(author: &str, state: &str, oid: &str) -> serde_json::Value {
    serde_json::json!({"author": {"login": author}, "state": state,
                       "submittedAt": "2026-08-05T10:00:00Z", "commit": {"oid": oid}})
}

/// The bare sha equality the freshness predicate replaced, as a resolver. The
/// pre-x-5b99 coverage tests below run against it unchanged: with no carry ever
/// granted, the new code must reproduce the old verdicts exactly.
fn at_head(sha: &str) -> Freshness {
    if !sha.is_empty() && sha == COV_HEAD {
        Freshness::Fresh
    } else {
        Freshness::Stale
    }
}

fn gh_comment(author: &str, body: &str) -> serde_json::Value {
    serde_json::json!({"author": {"login": author}, "body": body, "createdAt": "2026-08-05T10:00:00Z"})
}

/// An attestation line naming the branch the attestation is about -
/// the scoping field every post-x-e601 producer stamps. A legacy line (no
/// branch) is admitted only on exact head equality, so a test exercising a
/// MOVED head must scope by branch or its fixture silently vanishes.
fn attestation_line_on_branch(reviewer: &str, head: &str, verdict: &str, branch: &str) -> String {
    serde_json::json!({
        "type": "review_attestation",
        "data": {"reviewer": reviewer, "head_sha": head, "verdict": verdict, "branch": branch}
    })
    .to_string()
}

// ── attestation scope (x-e601: which PR an attestation is about) ─────────────
//
// The events journal is shared across every worktree by design, so an
// unscoped scan reads every branch's attestations into every PR's verdict
// list. These tests pin the scoping predicate through BOTH consumers.

/// The spawned-reviewer lane (review-lanes.md): the reviewer's worktree
/// necessarily carries a branch of its own - git refuses two worktrees on one
/// branch - so its exact-HEAD attestation must count for the author's PR
/// despite the branch mismatch. A foreign branch cannot share this head sha
/// without being this commit.
#[test]
fn attestation_scope_reviewer_worktree_branch_at_exact_head_counts() {
    let events = attestation_line_on_branch("code-review", COV_HEAD, "pass", "wt/reviewer-1");
    let rep = classify_coverage(
        &[],
        &[],
        &events,
        &[],
        true,
        None,
        &at_head,
        "feature/a",
        COV_HEAD,
    );
    assert_eq!(rep.coverage, Coverage::Covered(1));
    let local = rep
        .verdicts
        .iter()
        .find(|v| v.producer == CoverageProducer::LocalAttestation)
        .unwrap();
    assert_eq!(local.reviewed_sha, COV_HEAD);
}

/// A PR read that returned no branch fails closed: an attestation at a
/// DIFFERENT head cannot be admitted by branch (the PR's branch is unknown),
/// so it does not count. An exact-head line still counts with an unknown PR
/// branch - a foreign branch cannot share this head sha without being this
/// commit - which is the reviewer-lane case above.
#[test]
fn attestation_scope_unknown_pr_branch_fails_closed() {
    let foreign_head = "bbbb0000000000000000000000000000000000000";
    let events = attestation_line_on_branch("code-review", foreign_head, "pass", "feature/a");
    let rep = classify_coverage(&[], &[], &events, &[], true, None, &at_head, "", COV_HEAD);
    assert_eq!(rep.coverage, Coverage::Covered(0));
}

/// AC1-HP: only bot output is the usage-limit refusal -> coverage 0, refused.
#[test]
fn coverage_classify_quota_refusal_is_zero_coverage_refused() {
    let comments = vec![gh_comment(
        "chatgpt-codex-connector[bot]",
        "You have reached your Codex usage limits for code reviews.",
    )];
    let rep = classify_coverage(
        &[],
        &comments,
        "",
        &["chatgpt-codex-connector".to_string()],
        true,
        None,
        &at_head,
        "",
        COV_HEAD,
    );
    assert_eq!(rep.coverage, Coverage::Covered(0));
    let bot = rep
        .verdicts
        .iter()
        .find(|v| v.name == "chatgpt-codex-connector")
        .unwrap();
    assert_eq!(bot.producer, CoverageProducer::GithubApp);
    assert_eq!(bot.verdict, CoverageVerdict::Refused);
}

// ── clean-pass comments (x-e601: the PR #947 specimen) ───────────────────────
//
// chatgpt-codex-connector submits a formal review ONLY when it has findings;
// on a clean pass it posts a plain issue comment pinning the commit it read.
// Nothing read that comment, so a clean pass could never clear the gate and
// the gate was strictly easier to satisfy with a flawed PR than a clean one.

/// AC4-HP: the specimen comment verbatim (modulo the head being the test's
/// COV_HEAD) earns a Reviewed verdict pinned to that sha.
#[test]
fn clean_pass_comment_counts_as_a_head_pinned_review() {
    let comments = vec![gh_comment(
        "chatgpt-codex-connector[bot]",
        &format!(
            "Codex Review: Didn't find any major issues. Bravo. Reviewed commit: {}",
            COV_HEAD
        ),
    )];
    let rep = classify_coverage(
        &[],
        &comments,
        "",
        &["chatgpt-codex-connector".to_string()],
        true,
        None,
        &at_head,
        "",
        COV_HEAD,
    );
    assert_eq!(rep.coverage, Coverage::Covered(1));
    let v = rep
        .verdicts
        .iter()
        .find(|v| v.name == "chatgpt-codex-connector")
        .unwrap();
    assert_eq!(v.verdict, CoverageVerdict::Reviewed);
    assert_eq!(v.reviewed_sha, COV_HEAD);
}

/// AC4-EDGE: a clean pass naming an older sha whose code no longer matches is
/// stale - recorded, excluded from the count, and nudgeable for a re-read.
#[test]
fn clean_pass_comment_at_an_older_sha_is_stale() {
    let old = "0000000000000000000000000000000000000000";
    let comments = vec![gh_comment(
        "chatgpt-codex-connector[bot]",
        &format!("Codex Review: Didn't find any major issues. Reviewed commit: {old}"),
    )];
    let rep = classify_coverage(
        &[],
        &comments,
        "",
        &["chatgpt-codex-connector".to_string()],
        true,
        None,
        &at_head,
        "",
        COV_HEAD,
    );
    assert_eq!(rep.coverage, Coverage::Covered(0));
    let v = rep
        .verdicts
        .iter()
        .find(|v| v.name == "chatgpt-codex-connector")
        .unwrap();
    assert_eq!(v.verdict, CoverageVerdict::Stale);
    assert_eq!(v.reviewed_sha, old);
}

// ── coverage receipt line (x-0eaf task 3.1) ──────────────────────────────────

#[test]
fn coverage_receipt_covered_names_reviewers() {
    // gh_review, not a hand-rolled object: a review with no `commit.oid`
    // exercises the absent-commit path (fail closed) rather than the reviewed
    // one this test is about.
    let rep = classify_coverage(
        &[gh_review("chatgpt-codex-connector", "COMMENTED")],
        &[],
        "",
        &["chatgpt-codex-connector".to_string()],
        true,
        None,
        &at_head,
        "",
        COV_HEAD,
    );
    let line = coverage_receipt_line(&rep, None, None);
    assert!(line.starts_with("review coverage: 1 reviewed ("), "{line}");
    assert!(line.contains("chatgpt-codex-connector"), "{line}");
}

// ── attestation origin (producer records the emitting process) ───────────────

/// Like `attestation_line` but stamps the harness session that emitted the
/// attestation - the field the authorship join keys on.
fn attestation_line_attested(reviewer: &str, head: &str, verdict: &str, attester: &str) -> String {
    serde_json::json!({
        "type": "review_attestation",
        "data": {"reviewer": reviewer, "head_sha": head, "verdict": verdict, "attester_session_id": attester}
    })
    .to_string()
}

const AUTHOR: &str = "author-session-aaaa";
const OTHER: &str = "reviewer-session-bbbb";

// ── pair-key: (reviewer, attester_session_id) ────────────────────────────────

/// AC1-HP: two passes at the same head under the same reviewer label but from
/// DIFFERENT attester sessions join instead of replacing. Before the pair key
/// the second `insert` overwrote the first and a peer-reviewed PR read
/// `1 reviewed` while deleting its own control. Now it reads two verdicts - one
/// `SelfAttested` (the author) and one `OtherSession` (the spawned peer) - and
/// coverage_count is 2. This is the load-bearing test for the spawned-reviewer
/// lane: it is what proves a non-author attestation is both producible AND
/// countable, not silently collapsed.
#[test]
fn coverage_two_sessions_same_reviewer_join_not_replace() {
    let events = format!(
        "{}\n{}\n",
        attestation_line_attested("code-review", COV_HEAD, "pass", AUTHOR),
        attestation_line_attested("code-review", COV_HEAD, "pass", OTHER),
    );
    let rep = classify_coverage(
        &[],
        &[],
        &events,
        &[],
        true,
        Some(AUTHOR),
        &at_head,
        "",
        COV_HEAD,
    );
    let locals: Vec<_> = rep
        .verdicts
        .iter()
        .filter(|v| v.producer == CoverageProducer::LocalAttestation)
        .collect();
    assert_eq!(
        locals.len(),
        2,
        "two sessions must produce two verdicts, not one"
    );
    assert_eq!(
        locals
            .iter()
            .filter(|v| v.attestation_origin == AttestationOrigin::SelfAttested)
            .count(),
        1
    );
    assert_eq!(
        locals
            .iter()
            .filter(|v| v.attestation_origin == AttestationOrigin::OtherSession)
            .count(),
        1
    );
    assert_eq!(rep.coverage, Coverage::Covered(2));
    assert_eq!(rep.coverage_count(), Some(2));
}

// ── GraphQL quota floor + exhaustion naming ───────────────────────────────

/// A gh mock that answers ONLY `api rate_limit` (GraphQL bucket per `remaining`
/// in the script body) and `--version`, fails everything else, and logs every
/// invocation to `calls.log` so a test can prove which reads a fire spent.
fn quota_gh(dir: &Path, remaining: i64, green_pr_view: bool) -> PathBuf {
    let reset = chrono::Utc::now().timestamp() + 40 * 60;
    let pr_view_body = if green_pr_view {
        r#"if echo "$*" | grep -q "headRefName"; then
  echo '{"state":"OPEN","number":1,"headRefName":"main","headRefOid":"deadbeefdeadbeefdeadbeefdeadbeef00000001"}'
  exit 0
fi"#
    } else {
        "true"
    };
    make_script(
        dir,
        "gh",
        &format!(
            r#"echo "$*" >> "{calls}"
if echo "$*" | grep -q -- "--version"; then echo 'gh version 2.x'; exit 0; fi
if [ "$1" = api ] && [ "$2" = rate_limit ]; then
  echo '{{"resources":{{"graphql":{{"remaining":{remaining},"reset":{reset}}}}}}}'
  exit 0
fi
{pr_view_body}
exit 1"#,
            calls = dir.join("calls.log").display(),
            remaining = remaining,
            reset = reset,
            pr_view_body = pr_view_body,
        ),
    )
}

/// Item 2: an exhausted quota turns the bare read failure into the exhaustion
/// name with a reset horizon, and drops the "retrying next fire" advice.
#[test]
fn exhaustion_named_on_read_failure() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);
    let manifest_path = cwd.join("target-state.md");
    let transcript_path = cwd.join("transcript.jsonl");
    fs::write(
        &manifest_path,
        new_manifest("sess-exh", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript_path, transcript_with_promise()).unwrap();

    // remaining 0 with a FAILING pr view: the done() read errors and the
    // reason must name the bucket, not just "retrying next fire".
    let gh = quota_gh(cwd, 0, false);
    let git = MockBins::green().git;
    let (code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", gh.display()),
        &format!("--git-bin={}", git.display()),
    ]);
    assert_eq!(code, 0);
    assert_eq!(d.decision, "block");
    assert!(
        d.message.contains("GraphQL quota exhausted"),
        "got: {}",
        d.message
    );
    assert!(d.message.contains("resets in ~"), "got: {}", d.message);
    assert!(d.message.contains("fno do pr status"), "got: {}", d.message);
    assert!(
        !d.message.contains("retrying next fire"),
        "exhaustion must not be advised to retry: {}",
        d.message
    );
}

// ── coverage commit-status publish (x-6352) ─────────────────────────────────
//
// AC8: both emitters of the review_coverage row (the stop-hook decide path and
// the standalone command) also publish the commit status, and the load-bearing
// assert is the POSTED TARGET SHA + CONTEXT - never merely that a helper ran.
// A status on the wrong sha or under a mistyped context is a green marker on
// nothing, which is worse than no marker: the ruleset reads one exact name on
// one exact commit.

/// The PR head the green mock reports; the git mock echoes the same sha as
/// local HEAD, so the emitted row is pinned to the head it publishes on.
const PUB_HEAD: &str = "deadbeefdeadbeefdeadbeefdeadbeef00000001";

/// green()-shaped gh mock that additionally tees every `gh api` invocation to
/// `record`, so a test can assert on what was POSTED, not just that gh ran.
fn recording_green_gh(dir: &Path, record: &Path, reviewed: bool) -> PathBuf {
    let reviews = if reviewed {
        format!(
            r#"{{"reviews":[{{"author":{{"login":"chatgpt-codex-connector"}},"state":"COMMENTED","submittedAt":"2026-08-14T01:00:00Z","commit":{{"oid":"{PUB_HEAD}"}}}}],"comments":[]}}"#
        )
    } else {
        r#"{"reviews":[],"comments":[]}"#.to_string()
    };
    let record_s = record.display().to_string();
    make_script(
        dir,
        "gh",
        &format!(
            r#"
if echo "$*" | grep -q -- "--version"; then echo 'gh version 2.x'; exit 0; fi
if echo "$*" | grep -q "headRefName"; then
  echo '{{"state":"OPEN","number":1,"headRefName":"main","headRefOid":"{PUB_HEAD}","mergeable":"MERGEABLE","baseRefName":"main"}}'
  exit 0
fi
if echo "$*" | grep -q "checks"; then
  echo '[{{"name":"ci","state":"SUCCESS","bucket":"pass"}}]'
  exit 0
fi
if echo "$*" | grep -q "pulls/"; then echo '[]'; exit 0; fi
if echo "$*" | grep -q "reviews"; then echo '{reviews}'; exit 0; fi
if echo "$*" | grep -q -- "--json labels"; then echo 'false'; exit 0; fi
if echo "$*" | grep -q "^api"; then echo "$*" >> "{record_s}"; echo '{{}}'; exit 0; fi
exit 1
"#,
        ),
    )
}

fn pub_git(dir: &Path) -> PathBuf {
    make_script(
        dir,
        "git",
        r#"case "$*" in
  *--raw*) exit 1 ;;
  *) echo "deadbeefdeadbeefdeadbeefdeadbeef00000001" ;;
esac"#,
    )
}

fn pub_fixture(cwd: &Path, review_config: &str) {
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    fs::write(cwd.join(".fno/config.toml"), review_config).unwrap();
}

/// Run the standalone command against `cwd`, returning its (exit code, stdout).
fn fire_command(cwd: &Path, gh: &Path, git: &Path, project: &Path, global: &Path) -> (i32, String) {
    fno_agent::loopcheck::run_review_coverage_capture(&[
        "review-coverage".to_string(),
        "--cwd".to_string(),
        cwd.display().to_string(),
        "--events".to_string(),
        project.display().to_string(),
        "--global-events".to_string(),
        global.display().to_string(),
        format!("--gh-bin={}", gh.display()),
        format!("--git-bin={}", git.display()),
        "--global-settings".to_string(),
        "/nonexistent/global-settings.yaml".to_string(),
        "--author-harness".to_string(),
        "none".to_string(),
    ])
}

fn status_posts(record: &Path) -> Vec<String> {
    fs::read_to_string(record)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("/statuses/"))
        .map(|l| l.to_string())
        .collect()
}

const BOT_LANE: &str = "[review]\nrequired_bots = [\"chatgpt-codex-connector\"]\n";

/// Covered fixture: BOTH emitters post the required verdict and the diagnostic
/// instrument-health status to the same PR head sha.
#[test]
fn coverage_status_publish_fires_from_both_emitters_to_the_pr_head() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("proj");
    pub_fixture(&cwd, BOT_LANE);
    let project = project_events(&cwd);
    let global = tmp.path().join("global.jsonl");
    let manifest = cwd.join("target-state.md");
    fs::write(
        &manifest,
        new_manifest("sess-pub", "2026-08-14T00:00:00Z", true),
    )
    .unwrap();
    fs::write(cwd.join("transcript.jsonl"), transcript_with_promise()).unwrap();
    let record = tmp.path().join("gh-api-record");
    let bins = TempDir::new().unwrap();
    let gh = recording_green_gh(bins.path(), &record, true);
    let git = pub_git(bins.path());
    std::env::set_var("FNO_NUDGE_DISABLED", "1");
    std::env::set_var("FNO_LOOPCHECK_MIN_FIRE_GAP_SECS", "0");

    // Emitter 1: the stop-hook path (decide -> run_done).
    let (code, d) = fire(&[
        "loop-check",
        "--state",
        manifest.to_str().unwrap(),
        "--transcript",
        cwd.join("transcript.jsonl").to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-08-14T00:30:00Z",
        &format!("--gh-bin={}", gh.display()),
        &format!("--git-bin={}", git.display()),
        "--events",
        project.to_str().unwrap(),
        "--global-events",
        global.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "loop-check must allow: {:?}", d);

    // Emitter 2: the standalone command.
    let (vcode, vjson) = fire_command(&cwd, &gh, &git, &project, &global);
    assert_eq!(vcode, 0, "command must emit a row: {vjson}");

    let posts = status_posts(&record);
    assert_eq!(posts.len(), 4, "two POSTs per emitter, got: {posts:?}");
    assert_eq!(
        posts
            .iter()
            .filter(|post| post.contains("context=fno/review-coverage "))
            .count(),
        2,
        "required context must be posted by both emitters: {posts:?}"
    );
    assert_eq!(
        posts
            .iter()
            .filter(|post| post.contains("context=fno/review-coverage-unavailable "))
            .count(),
        2,
        "diagnostic context must be posted by both emitters: {posts:?}"
    );
    for post in &posts {
        assert!(
            post.contains(&format!("statuses/{PUB_HEAD}")),
            "posted to the wrong sha: {post}"
        );
        if post.contains("context=fno/review-coverage ") {
            assert!(
                post.contains("state=success")
                    && post.contains(&format!("reviewed at {}", &PUB_HEAD[..8])),
                "covered fixture must post a success naming count+sha: {post}"
            );
        } else {
            assert!(
                post.contains("context=fno/review-coverage-unavailable ")
                    && post.contains("state=success")
                    && post.contains("coverage read healthy"),
                "covered fixture must clear instrument unavailability: {post}"
            );
        }
    }
}

// ── bounded external reads: every stop-gate read is bounded and a killed
// child is named as the exact read that timed out ──────────────────────────
//
// Each case wedges ONE logical read behind a 30s sleep and injects a 1s
// bound, then asserts three things: the fire RETURNS (bounded), the decision
// message names exactly the wedged read as a killed timeout, and it never
// names an unrelated read (the pr_info_rest misattribution this family of
// tests exists to close).

/// A wedged LOCAL git read (the external-diff-driver shape) must not hang
/// the fire either: the stop-gate git calls route through the same bounded
/// transport, and a killed read degrades to each caller's conservative
/// no-answer instead of wedging the stop gate.
#[test]
fn wedged_git_read_degrades_bounded_instead_of_hanging() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);
    let manifest = cwd.join("target-state.md");
    let transcript = cwd.join("transcript.jsonl");
    fs::write(
        &manifest,
        new_manifest("sess-gitwedge", "2026-06-05T00:00:00Z", true),
    )
    .unwrap();
    fs::write(&transcript, transcript_with_promise()).unwrap();
    let bins = TempDir::new().unwrap();
    // Everything answers green except the payload-classify diff (the
    // three-dot range), which sleeps past any test bound.
    let git = make_script(
        bins.path(),
        "git",
        r#"if echo "$*" | grep -q -- "--version"; then exit 0; fi
if [ "$1" = "diff" ] && echo "$*" | grep -q "\.\.\."; then sleep 30; fi
if [ "$1" = "rev-parse" ]; then
  if echo "$*" | grep -q "HEAD"; then echo "deadbeefdeadbeefdeadbeefdeadbeef00000001"; exit 0; fi
  echo "cafecafecafecafecafecafecafecafe00000001"; exit 0
fi
[ "$1" = "merge-base" ] && exit 1
[ "$1" = "status" ] && exit 0
[ "$1" = "branch" ] && { echo "feature/x"; exit 0; }
exit 0"#,
    );
    let gh = MockBins::green().gh;
    let started = std::time::Instant::now();
    let (code, d) = fire(&[
        "loop-check",
        "--state",
        manifest.to_str().unwrap(),
        "--transcript",
        transcript.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", gh.display()),
        &format!("--git-bin={}", git.display()),
        "--read-timeout-ms=1000",
    ]);
    let elapsed = started.elapsed();
    assert_eq!(code, 0, "a degraded git read still decides: {d:?}");
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "a wedged git read must not hang the fire, took {elapsed:?}"
    );
    assert!(
        !d.message.is_empty(),
        "the fire must emit a decision message, not die silently: {d:?}"
    );
}

// ── operator-law waiver (review coverage) ───────────────────────────────────
use fno_agent::loopcheck::operator_waiver;

const HEAD40: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

/// A stub `fno` whose `backlog decisions` answers `single` for one subject
/// pattern (shell case pattern against $3, the subject) and `none` for every
/// other. `--json` stdout is the exact shape `fno backlog decisions --json`
/// prints when the law verdict resolves; the single row carries the
/// affirmative decision value the coverage-waive command mints. Pass a
/// different `row_decision` to model a note or denial recorded at the same
/// subject.
fn law_single_stub(dir: &Path, name: &str, single_pattern: &str) -> PathBuf {
    law_stub_with(
        dir,
        name,
        single_pattern,
        "review coverage waived for this head",
    )
}

fn law_stub_with(dir: &Path, name: &str, single_pattern: &str, row_decision: &str) -> PathBuf {
    make_script(
        dir,
        name,
        &format!(
            r#"case "$3" in
{single_pattern}) echo '{{"canonical_subject":"s","current_law":{{"status":"single","decision_ids":["d-1"],"decision_id":"d-1"}},"decisions":[{{"decision_id":"d-1","decision":"{row_decision}","authority_source":"operator"}}]}}'; exit 0 ;;
*) echo '{{"canonical_subject":"s","current_law":{{"status":"none"}},"decisions":[]}}'; exit 0 ;;
esac"#
        ),
    )
}

#[test]
fn standing_waiver_never_clears_a_hard_finding() {
    let dir = TempDir::new().unwrap();
    let fno = law_single_stub(dir.path(), "fno", "review-coverage-waiver");
    // A hard blocker stands: the standing ruling waives an unreviewed head,
    // never an unresolved CONFIRMED correctness or security finding.
    let (waiver_hard, unknown_hard) = operator_waiver(
        fno.to_str().unwrap(),
        dir.path(),
        "acme/widgets",
        42,
        HEAD40,
        true,
    );
    assert_eq!(waiver_hard, None);
    assert!(!unknown_hard, "a narrow no is a clean no, not an unknown");
    // Without the hard blocker the standing law waives.
    let (waiver_soft, _) = operator_waiver(
        fno.to_str().unwrap(),
        dir.path(),
        "acme/widgets",
        42,
        HEAD40,
        false,
    );
    assert_eq!(waiver_soft.as_deref(), Some("standing operator decision"));
}

// ── review.posture: the coverage row carries the resolved rung ────
//
// The Rust producer resolves review.posture (explicit, legacy-inferred, or the
// shipped default), evaluates it against the same verdicts coverage used, and
// emits `review_posture` on the row. Python reads the fields verbatim; these
// tests pin the wire contract the receipt consumes.

use serde_json::Value;

fn last_review_coverage_row(project: &Path) -> Value {
    let text = event_text(project);
    let mut row: Option<Value> = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) == Some("review_coverage") {
            row = Some(v);
        }
    }
    row.expect("a review_coverage row was emitted")
}

/// Bare install (no review config at all): the posture resolves to the shipped
/// DEFAULT floor self_review (rung 3), and with no review evidence the row
/// reports unsatisfied with the exact self gap.
#[test]
fn review_posture_defaults_to_self_review_and_reports_gaps() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("proj");
    pub_fixture(&cwd, "");
    let project = project_events(&cwd);
    let global = tmp.path().join("global.jsonl");
    let bins = TempDir::new().unwrap();
    let gh = recording_green_gh(bins.path(), &tmp.path().join("rec"), false);
    let git = pub_git(bins.path());

    let (code, json) = fire_command(&cwd, &gh, &git, &project, &global);
    assert_eq!(code, 0, "{json}");
    let p = last_review_coverage_row(&project)["data"]["review_posture"].clone();
    assert_eq!(p["posture"], "self_review");
    assert_eq!(p["rank"], 3);
    assert_eq!(p["source"], "default");
    assert_eq!(p["cost"], "one review");
    assert_eq!(p["posture_satisfied"], false);
    let gaps = p["posture_gaps"].as_array().unwrap();
    assert_eq!(gaps.len(), 1, "{p}");
    assert!(gaps[0].as_str().unwrap().starts_with("self:"), "{p}");
}

/// independent_review: other evidence without the fresh-context marker never
/// satisfies rung 4 (AC4-ERR); adding a pass attestation that carries
/// reviewer_context=fresh at the same head does.
#[test]
fn review_posture_independent_rung_requires_fresh_context_marker() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("proj");
    pub_fixture(
        &cwd,
        "[review]\nposture = \"independent_review\"\nreviewers = [\"code-review\"]\n",
    );
    let project = project_events(&cwd);
    let global = tmp.path().join("global.jsonl");
    let bins = TempDir::new().unwrap();
    let gh = recording_green_gh(bins.path(), &tmp.path().join("rec"), false);
    let git = pub_git(bins.path());

    // A head-pinned pass with NO context marker (the whole pre-landing
    // cohort): the rung stays unsatisfied.
    fs::write(
        &project,
        format!(
            "{{\"type\":\"review_attestation\",\"data\":{{\"reviewer\":\"code-review\",\"head_sha\":\"{PUB_HEAD}\",\"verdict\":\"pass\",\"session_id\":\"run-a\",\"attester_session_id\":\"sess-other\",\"branch\":\"main\"}}}}\n"
        ),
    )
    .unwrap();
    let (code, json) = fire_command(&cwd, &gh, &git, &project, &global);
    assert_eq!(code, 0, "{json}");
    let p = last_review_coverage_row(&project)["data"]["review_posture"].clone();
    assert_eq!(p["posture"], "independent_review");
    assert_eq!(p["posture_satisfied"], false, "{p}");

    // The same reviewer re-attests at the same head WITH the positive fresh
    // marker: the marker, not the session identity, is what rung 4 reads.
    let mut seeded = fs::read_to_string(&project).unwrap();
    seeded.push_str(&format!(
        "{{\"type\":\"review_attestation\",\"data\":{{\"reviewer\":\"code-review\",\"head_sha\":\"{PUB_HEAD}\",\"verdict\":\"pass\",\"session_id\":\"run-a\",\"attester_session_id\":\"sess-fresh\",\"branch\":\"main\",\"reviewer_context\":\"fresh\"}}}}\n"
    ));
    fs::write(&project, seeded).unwrap();
    let (code, json) = fire_command(&cwd, &gh, &git, &project, &global);
    assert_eq!(code, 0, "{json}");
    let p = last_review_coverage_row(&project)["data"]["review_posture"].clone();
    assert_eq!(p["posture_satisfied"], true, "{p}");
    assert_eq!(
        p["posture_gaps"].as_array().unwrap().len(),
        0,
        "gaps must be empty when satisfied: {p}"
    );
}
