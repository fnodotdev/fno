//! Integration tests for the `fno-agent review-coverage` command (x-3a3f).
//!
//! The load-bearing assertion is PAYLOAD PARITY: the `review_coverage` data
//! object the command emits is equal, field for field, to the one the stop hook's
//! `run_done` emits for the same fixture. The comparison is on the whole `data`
//! object, never the event tag - a parity assertion on the tag pins the name
//! and not the destination.
//!
//! The non-weakening cases pin that exposing the producer relaxes nothing: an
//! unreviewed PR still recomputes to `uncovered` / 0, a failed gh read still
//! lands as `unknown`, and a stale attestation still lands as a `stale`
//! verdict rather than `reviewed`.

mod common;

use common::make_script;
use fno_agent::loopcheck::{run_loop_check_capture, run_review_coverage_capture};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const HEAD: &str = "deadbeefdeadbeefdeadbeefdeadbeef00000001";
const LOCAL_HEAD: &str = "cafebabecafebabecafebabecafebabe00000001";

/// gh: OPEN PR #1 at HEAD, green CI, one COMMENTED review by the configured
/// bot at HEAD (counts as reviewed), no inline comments. git: HEAD echo, with
/// `diff --raw` failing so freshness never fabricates a carry (same discipline
/// as the loop_check suite's green() mock).
#[path = "space_paths.rs"]
mod space_paths;

use space_paths::project_events;

fn green_bins(dir: &Path) -> (PathBuf, PathBuf) {
    let gh = make_script(
        dir,
        "gh",
        &format!(
            r#"
if echo "$*" | grep -q -- "--version"; then echo 'gh version 2.x'; exit 0; fi
if echo "$*" | grep -q "headRefName"; then
  echo '{{"state":"OPEN","number":1,"headRefName":"main","headRefOid":"{HEAD}","mergeable":"MERGEABLE","baseRefName":"main"}}'
  exit 0
fi
if echo "$*" | grep -q "checks"; then
  echo '[{{"name":"ci","state":"SUCCESS","bucket":"pass"}}]'
  exit 0
fi
if echo "$*" | grep -q "pulls/"; then echo '[]'; exit 0; fi
if echo "$*" | grep -q "reviews"; then
  echo '{{"reviews":[{{"author":{{"login":"chatgpt-codex-connector"}},"state":"COMMENTED","submittedAt":"2026-08-14T01:00:00Z","commit":{{"oid":"{HEAD}"}}}}],"comments":[]}}'
  exit 0
fi
exit 1
"#,
        ),
    );
    let git = make_script(
        dir,
        "git",
        r#"case "$*" in
  *--raw*) exit 1 ;;
  *) echo "deadbeefdeadbeefdeadbeefdeadbeef00000001" ;;
esac"#,
    );
    (gh, git)
}

/// A local head-pinned `pass` attestation in the project events log.
fn attestation_line(reviewer: &str, head: &str, attester: &str) -> String {
    serde_json::json!({
        "type": "review_attestation",
        "data": {"reviewer": reviewer, "head_sha": head, "verdict": "pass",
                 "attester_session_id": attester}
    })
    .to_string()
}

/// The last `review_coverage` data object in one events log, or None.
fn last_coverage(path: &Path) -> Option<serde_json::Value> {
    let text = fno_agent::event_store::journal_text(path, &[]);
    let mut last = None;
    for line in text.lines() {
        if !line.contains("review_coverage") {
            continue;
        }
        if let Ok(ev) = serde_json::from_str::<serde_json::Value>(line) {
            if ev.get("type").and_then(|t| t.as_str()) == Some("review_coverage") {
                last = ev.get("data").cloned();
            }
        }
    }
    last
}

/// cwd with `.fno/config.toml` naming the bot (so the login gate reads), a
/// project events log, and a hermetic global events path.
fn fixture(parent: &Path, name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let cwd = parent.join(name);
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    fs::write(
        cwd.join(".fno/config.toml"),
        "[review]\nrequired_bots = [\"chatgpt-codex-connector\"]\n",
    )
    .unwrap();
    let project = project_events(&cwd);
    let global = parent.join(format!("{name}-global-events.jsonl"));
    (cwd, project, global)
}

/// Shared hermetic flags: nonexistent global settings + no ambient author
/// harness (the same doors tests/loop_check.rs `fire()` closes).
fn hermetic_tail() -> Vec<String> {
    vec![
        "--global-settings".to_string(),
        "/nonexistent/global-settings.yaml".to_string(),
        "--author-harness".to_string(),
        "none".to_string(),
    ]
}

// ── 1. payload parity ────────────────────────────────────────────────────────

/// The whole `data` object the command emits equals the one `run_done` emits for
/// the same fixture, including `verdicts`, `reviewed_count`,
/// `self_attested_count`, `head_sha` and (when resolvable) `repo`. This is the
/// `parity_test_impossible` kill criterion's probe: if the extraction is not
/// truly manifest-independent, this fails rather than papering over the
/// difference.
#[test]
fn command_payload_equals_run_done_payload() {
    let parent = TempDir::new().unwrap();
    let (cwd, project, global) = fixture(parent.path(), "parity");

    // One fresh local attestation by the authoring session: it exercises the
    // attestation_origin axis, so parity covers self_attested_count too.
    fs::write(
        &project,
        attestation_line("code-review", HEAD, "sess-par") + "\n",
    )
    .unwrap();

    // run_done side: a manifest-bearing session promises on a green PR.
    let manifest = cwd.join("target-state.md");
    fs::write(
        &manifest,
        "---\nsession_id: sess-par\nharness_session_id: sess-par\ncreated_at: 2026-08-14T00:00:00Z\nattended: true\n---\n",
    )
    .unwrap();
    let transcript = cwd.join("transcript.jsonl");
    fs::write(
        &transcript,
        serde_json::json!({"message": {"role": "assistant",
            "content": "Done! <promise>MISSION COMPLETE</promise>"}})
        .to_string()
            + "\n",
    )
    .unwrap();
    let bins = TempDir::new().unwrap();
    let (gh, git) = green_bins(bins.path());
    let mut args: Vec<String> = vec![
        "loop-check".to_string(),
        "--state".to_string(),
        manifest.display().to_string(),
        "--transcript".to_string(),
        transcript.display().to_string(),
        "--cwd".to_string(),
        cwd.display().to_string(),
        "--now".to_string(),
        "2026-08-14T00:30:00Z".to_string(),
        format!("--gh-bin={}", gh.display()),
        format!("--git-bin={}", git.display()),
        "--events".to_string(),
        project.display().to_string(),
        "--global-events".to_string(),
        global.display().to_string(),
    ];
    args.extend(hermetic_tail());
    // Same discipline as the loop_check suite: no ambient nudge shell-outs,
    // no streak debounce.
    std::env::set_var("FNO_NUDGE_DISABLED", "1");
    std::env::set_var("FNO_LOOPCHECK_MIN_FIRE_GAP_SECS", "0");
    let (code, json_str) = run_loop_check_capture(&args);
    assert_eq!(code, 0, "loop-check must allow: {json_str}");
    let stop_hook_data = last_coverage(&project).expect("run_done emitted coverage");

    // Command side: NO manifest read at all - the session id arrives by flag, the
    // exact way the merge recompute will pass what it knows.
    let (vcode, vjson) = run_review_coverage_capture(&vec![
        "review-coverage".to_string(),
        "--cwd".to_string(),
        cwd.display().to_string(),
        "--session-id".to_string(),
        "sess-par".to_string(),
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
    ]);
    assert_eq!(vcode, 0, "command must emit a row: {vjson}");
    let command_stdout: serde_json::Value = serde_json::from_str(&vjson).unwrap();
    assert_eq!(
        command_stdout, stop_hook_data,
        "the command's payload must equal run_done's field for field"
    );
    // And the emitted row is the same object, in BOTH logs.
    assert_eq!(last_coverage(&project).as_ref(), Some(&command_stdout));
    assert_eq!(last_coverage(&global).as_ref(), Some(&command_stdout));
}

// ── 2-4. non-weakening ───────────────────────────────────────────────────────

/// A failed gh read (non no-PR stderr) with a known PR number -> exit 4 and an
/// emitted `unknown` row, never a pass.
#[test]
fn failed_gh_read_emits_unknown_exit_4() {
    let parent = TempDir::new().unwrap();
    let (cwd, project, global) = fixture(parent.path(), "ghfail");
    let bins = TempDir::new().unwrap();
    let gh = make_script(
        bins.path(),
        "gh",
        r#"if echo "$*" | grep -q -- "--version"; then echo 'gh version 2.x'; exit 0; fi
echo 'gh: network unreachable' >&2
exit 1"#,
    );
    let git = make_script(bins.path(), "git", &format!("echo {HEAD}"));
    let (code, json) = run_review_coverage_capture(&vec![
        "review-coverage".to_string(),
        "--cwd".to_string(),
        cwd.display().to_string(),
        "--pr".to_string(),
        "842".to_string(),
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
    ]);
    assert_eq!(code, 4, "a failed read is exit 4: {json}");
    let data: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(data["coverage"], serde_json::json!("unknown"));
    assert_eq!(data["pr"], serde_json::json!(842));
    assert_eq!(
        data["head_sha"], "",
        "failed PR read must not use local HEAD"
    );
    assert_ne!(data["head_sha"], HEAD);
    // Exit-4 stdout additionally carries the stdout-only quota diagnostic
    // (null here: the stub cannot answer `api rate_limit` either); the
    // persisted row keeps the bare schema, so compare with the keys stripped.
    let mut row = data.clone();
    if let Some(obj) = row.as_object_mut() {
        obj.remove("graphql_remaining");
        obj.remove("graphql_exhausted");
        obj.remove("reason");
    }
    // The unknown row reached both logs, so the merge re-read sees the failed
    // read rather than nothing.
    assert_eq!(last_coverage(&project).as_ref(), Some(&row));
    assert_eq!(last_coverage(&global).as_ref(), Some(&row));
}

/// An explicit PR selector pins the emitted row to that PR's head even when
/// the command runs from a checkout whose local HEAD is different.
#[test]
fn pr_selector_resolves_the_pr_head_not_the_checkout_head() {
    let parent = TempDir::new().unwrap();
    let (cwd, project, global) = fixture(parent.path(), "selected-head");
    let bins = TempDir::new().unwrap();
    let gh = make_script(
        bins.path(),
        "gh",
        &format!(
            r#"
if echo "$*" | grep -q -- "--version"; then echo 'gh version 2.x'; exit 0; fi
if echo "$*" | grep -q "headRefOid"; then
  echo '{{"state":"OPEN","number":842,"headRefName":"feature/selected","headRefOid":"{HEAD}","mergeable":"MERGEABLE","baseRefName":"main"}}'
  exit 0
fi
if echo "$*" | grep -q "checks"; then echo '[{{"name":"ci","state":"SUCCESS","bucket":"pass"}}]'; exit 0; fi
if echo "$*" | grep -q "pulls/"; then echo '[]'; exit 0; fi
if echo "$*" | grep -q "reviews"; then echo '{{"reviews":[],"comments":[]}}'; exit 0; fi
exit 1"#,
        ),
    );
    let git = make_script(bins.path(), "git", &format!("echo {LOCAL_HEAD}"));
    let (code, json) = run_review_coverage_capture(&vec![
        "review-coverage".to_string(),
        "--cwd".to_string(),
        cwd.display().to_string(),
        "--pr".to_string(),
        "842".to_string(),
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
    ]);
    assert_eq!(code, 0, "{json}");
    let data: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(data["head_sha"], serde_json::json!(HEAD));
    assert_ne!(data["head_sha"], serde_json::json!(LOCAL_HEAD));
    assert_eq!(last_coverage(&project).unwrap()["head_sha"], HEAD);
}
