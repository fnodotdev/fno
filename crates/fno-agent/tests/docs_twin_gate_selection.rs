//! The stop gate's PR target never lands on a docs twin: end-to-end through
//! run_loop_check, in the shape the incident hit. A node whose only numbered
//! PR row is the twin (note "docs", url in the docs repo) yields no target,
//! so every gh read answers against the ambient origin - the session's own
//! repo - and a green read completes instead of demanding a push. This file
//! sits outside the G3 scope on purpose: the selection fn is crate-internal,
//! so coverage rides the public binary surface (test-audit README, the
//! drives-G3-from-outside row).

use fno_agent::loopcheck::run_loop_check_capture;
#[path = "space_paths.rs"]
mod space_paths;
use serde_json::{json, Value};
use space_paths::project_events;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

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

/// Green gh that RECORDS every argv into $RECORD_CALLS. The reviews answer
/// satisfies the required connector bot at the mocked head; `pr view` answers
/// the branch-resolved shape (number 1, head equal to the git stub's HEAD).
fn recording_gh(dir: &Path) -> PathBuf {
    make_script(
        dir,
        "gh",
        r#"printf '%s\n' "$*" >> "$RECORD_CALLS"
if echo "$*" | grep -q -- "--version"; then
  echo 'gh version 2.x'
  exit 0
fi
if echo "$*" | grep -q "headRefName"; then
  echo '{"state":"OPEN","number":1,"headRefName":"feature","headRefOid":"deadbeefdeadbeefdeadbeefdeadbeef00000001","mergeable":"MERGEABLE","mergeStateStatus":"clean","baseRefName":"main","author":{"login":"jj"},"url":"https://github.com/a/r/pull/1"}'
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
  echo '{"reviews":[{"author":{"login":"chatgpt-codex-connector"},"state":"COMMENTED","submittedAt":"2026-06-05T01:00:00Z","commit":{"oid":"deadbeefdeadbeefdeadbeefdeadbeef00000001"}}],"comments":[]}'
  exit 0
fi
exit 1
"#,
    )
}

fn git_stub(dir: &Path) -> PathBuf {
    make_script(
        dir,
        "git",
        r#"case "$*" in
  *--raw*) exit 1 ;;
  *) echo "deadbeefdeadbeefdeadbeefdeadbeef00000001" ;;
esac"#,
    )
}

/// The node row the store holds: only a docs twin, or a code row beside it.
fn node_row(id: &str, prs: Value) -> Value {
    json!({
        "id": id, "slug": id, "title": id, "type": "feature",
        "status": "in_progress", "priority": "p2", "domain": "code",
        "created_at": "2026-11-21T00:00:00+00:00",
        "additional_prs": prs,
    })
}

fn twin_row() -> Value {
    json!([{
        "number": 265,
        "url": "https://github.com/footnote-sh/fno-web/pull/265",
        "note": "docs"
    }])
}

fn code_row() -> Value {
    json!([{
        "number": 100,
        "url": "https://github.com/fnodotdev/fno/pull/100"
    }])
}

/// The state root the gate resolves for this cwd: the run door's
/// FNO_STATE_DIR pin when it is set, else the config key the fixture wrote.
fn state_root(cwd: &Path) -> PathBuf {
    if let Some(v) = std::env::var_os("FNO_STATE_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(v);
    }
    let raw = fs::read_to_string(cwd.join(".fno/config.toml")).unwrap();
    let dir = raw
        .lines()
        .find_map(|l| {
            l.strip_prefix("state_dir = \"")
                .map(|r| r.trim_end_matches('"').to_string())
        })
        .expect("fixture config carries state_dir");
    PathBuf::from(dir)
}

#[derive(Debug, serde::Deserialize)]
struct Decision {
    termination_reason: Option<String>,
    message: String,
}

fn fire(cwd: &Path, manifest: &Path, transcript: &Path, gh: &Path, git: &Path) -> (i32, Decision) {
    let args = vec![
        "loop-check".to_string(),
        "--state".to_string(),
        manifest.to_string_lossy().to_string(),
        "--transcript".to_string(),
        transcript.to_string_lossy().to_string(),
        "--cwd".to_string(),
        cwd.to_string_lossy().to_string(),
        "--now".to_string(),
        "2026-06-05T00:30:00Z".to_string(),
        format!("--gh-bin={}", gh.display()),
        format!("--git-bin={}", git.display()),
        "--global-settings".to_string(),
        "/nonexistent/global-settings.yaml".to_string(),
        "--gh-budget-ledger".to_string(),
        format!("{}/.fno/gh-budget.json", cwd.display()),
        "--global-events".to_string(),
        "/nonexistent/global-events.jsonl".to_string(),
        "--author-harness".to_string(),
        "none".to_string(),
    ];
    let (code, json_str) = run_loop_check_capture(&args);
    let d: Decision = serde_json::from_str(&json_str)
        .unwrap_or_else(|e| panic!("non-JSON decision: {e}: {json_str}"));
    (code, d)
}

/// One green loop-check over a node whose store rows vary. Whatever rows the
/// node carries, the docs twin never becomes the gate's PR target.
#[test]
fn the_gate_never_targets_a_docs_twin_row() {
    std::env::set_var("FNO_NUDGE_DISABLED", "1");
    std::env::set_var("FNO_LOOPCHECK_MIN_FIRE_GAP_SECS", "0");
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    let state = state_root(cwd);
    fs::create_dir_all(&state).unwrap();
    fs::write(
        cwd.join(".fno/config.toml"),
        "[review]\nrequired_bots = [\"chatgpt-codex-connector\"]\n",
    )
    .unwrap();
    let graph = fno_agent::runtime::state_layout::graph_anchor(&state);
    let transcript = cwd.join("transcript.jsonl");
    fs::write(
        &transcript,
        "{\"message\":{\"role\":\"assistant\",\"content\":\"Done! <promise>MISSION COMPLETE</promise>\"}}\n",
    )
    .unwrap();

    let mock_dir = TempDir::new().unwrap();
    let gh = recording_gh(mock_dir.path());
    let git = git_stub(mock_dir.path());

    // The incident shape: the twin is the node's only numbered row (the code
    // PR exists but is unrecorded). The gate reads the ambient origin's
    // branch-resolved PR and the green world completes - no push demand.
    let manifest = cwd.join("target-state.md");
    fs::write(
        &manifest,
        "---\nsession_id: sess-twin\ncreated_at: 2026-06-05T00:00:00Z\nattended: true\n---\ngraph_node_id: x-twin\n",
    )
    .unwrap();
    fno_agent::graph_store::seed_rows(&graph, &[node_row("x-twin", twin_row())]).unwrap();
    let record = mock_dir.path().join("calls-docs-only");
    std::env::set_var("RECORD_CALLS", &record);
    let (code, d) = fire(cwd, &manifest, &transcript, &gh, &git);
    assert_eq!(code, 0, "allow exit: {}", d.message);
    assert_eq!(
        d.termination_reason.as_deref(),
        Some("DonePRGreen"),
        "{}",
        d.message
    );
    assert!(
        !d.message.contains("push the latest commits"),
        "the twin must not yield a push demand: {}",
        d.message
    );
    let argv = fs::read_to_string(&record).unwrap();
    assert!(argv.contains("pr view"), "a PR read happened: {argv}");
    assert!(
        !argv.contains("footnote-sh/fno-web"),
        "no read names the docs repo: {argv}"
    );
    assert!(
        !argv.contains("--repo"),
        "the docs-only node pins no repo: {argv}"
    );

    // A twin beside a live code row: the code row pins, on its own number
    // and its own repo slug.
    fno_agent::graph_store::seed_rows(
        &graph,
        &[node_row(
            "x-twin",
            json!([{"number": 265, "url": "https://github.com/footnote-sh/fno-web/pull/265", "note": "docs"},
                   {"number": 100, "url": "https://github.com/fnodotdev/fno/pull/100"}]),
        )],
    )
    .unwrap();
    let record = mock_dir.path().join("calls-both");
    std::env::set_var("RECORD_CALLS", &record);
    let (_code, d) = fire(cwd, &manifest, &transcript, &gh, &git);
    assert!(
        !d.message.contains("push the latest commits"),
        "{}",
        d.message
    );
    let argv = fs::read_to_string(&record).unwrap();
    assert!(
        argv.contains("--repo fnodotdev/fno"),
        "the code row pins its repo: {argv}"
    );
    assert!(
        !argv.contains("footnote-sh/fno-web"),
        "the docs repo is never read: {argv}"
    );

    // A plain row keeps the pin exactly as before the docs filter.
    fno_agent::graph_store::seed_rows(&graph, &[node_row("x-twin", code_row())]).unwrap();
    let record = mock_dir.path().join("calls-plain");
    std::env::set_var("RECORD_CALLS", &record);
    let (_code, d) = fire(cwd, &manifest, &transcript, &gh, &git);
    assert!(
        !d.message.contains("push the latest commits"),
        "{}",
        d.message
    );
    let argv = fs::read_to_string(&record).unwrap();
    assert!(
        argv.contains("--repo fnodotdev/fno"),
        "the plain row keeps the pin: {argv}"
    );

    // The journal landed for the completed fire (the sandboxed project log).
    assert!(project_events(cwd).exists());
}
