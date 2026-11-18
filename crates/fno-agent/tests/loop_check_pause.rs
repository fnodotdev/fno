//! Pause-all integration coverage for both loop-check drivers.

use fno_agent::loopcheck::run_loop_check_capture;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

#[derive(Debug, serde::Deserialize)]
struct Decision {
    decision: String,
    termination_reason: Option<String>,
    message: String,
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Tests in this file run threaded and each points `HOME` at its own tree,
/// so the returned guard must live for the whole test.
#[must_use]
fn setup(cwd: &Path, home: &Path) -> std::sync::MutexGuard<'static, ()> {
    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    fs::create_dir_all(home.join(".fno")).unwrap();
    fs::create_dir_all(home.join(".fno/agents")).unwrap();
    fs::write(
        cwd.join(".fno/config.toml"),
        "[review]\nrequired_bots = [\"chatgpt-codex-connector\"]\n",
    )
    .unwrap();
    std::env::set_var("FNO_NUDGE_DISABLED", "1");
    std::env::set_var("FNO_LOOPCHECK_MIN_FIRE_GAP_SECS", "0");
    std::env::set_var("HOME", home);
    std::env::set_var("FNO_AGENT_HOME", home.join(".fno/agents"));
    guard
}

fn target_state(
    cwd: &Path,
    session_id: &str,
    node: Option<&str>,
    territory: Option<&str>,
) -> (PathBuf, PathBuf) {
    let state = cwd.join(format!("{session_id}-state.md"));
    let transcript = cwd.join(format!("{session_id}-transcript.jsonl"));
    let mut body = format!(
        "---\nfno_id: {session_id}\nsession_id: {session_id}\nharness_session_id: {session_id}\ncreated_at: 2026-06-05T00:00:00Z\n"
    );
    if let Some(node) = node {
        body.push_str(&format!("graph_node_id: {node}\n"));
    }
    if let Some(territory) = territory {
        body.push_str(&format!("territory: {territory}\n"));
    }
    body.push_str("---\n");
    fs::write(&state, body).unwrap();
    (state, transcript)
}

fn write_targeted_stop(home: &Path, target: &str, holds: &[&str], reason: &str) {
    let (kind, value) = target.split_once(':').unwrap();
    let filename = if kind == "territory" {
        format!("territory-{}.json", value.replace(',', "+"))
    } else {
        format!("session-{value}.json")
    };
    let dir = home.join(".fno/agents/fleet-stop.d");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(filename),
        serde_json::json!({
            "version": 1,
            "state": "stopped",
            "generation": 1,
            "changed_at": "2026-06-05T00:00:00Z",
            "changed_by": "operator",
            "reason": reason,
            "holds": holds,
            "target": target,
            "expires_at": "2099-12-31T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();
}

fn fire(args: &[&str]) -> Decision {
    let mut owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    owned.extend([
        "--global-settings".to_string(),
        "/nonexistent/global-settings.yaml".to_string(),
        "--global-events".to_string(),
        "/nonexistent/global-events.jsonl".to_string(),
        "--author-harness".to_string(),
        "none".to_string(),
    ]);
    let (_, output) = run_loop_check_capture(&owned);
    serde_json::from_str(&output).unwrap_or_else(|error| panic!("{error}: {output}"))
}

#[test]
fn paused_target_and_lead_allow_without_terminal_reason() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let _env = setup(tmp.path(), &home);
    fs::write(
        home.join(".fno/loops-paused.json"),
        r#"{"who":"operator","paused_at":10}"#,
    )
    .unwrap();

    for driver in ["target", "lead"] {
        let state = tmp.path().join(format!("{driver}-state.md"));
        let transcript = tmp.path().join(format!("{driver}-transcript.jsonl"));
        let decision = fire(&[
            "loop-check",
            "--state",
            state.to_str().unwrap(),
            "--transcript",
            transcript.to_str().unwrap(),
            "--cwd",
            tmp.path().to_str().unwrap(),
            "--driver",
            driver,
        ]);
        assert_eq!(decision.decision, "allow");
        assert!(decision.termination_reason.is_none());
        assert!(decision.message.contains("operator"));
    }
}

#[test]
fn targeted_breaker_holds_only_the_matching_session_loop() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let _env = setup(tmp.path(), &home);
    let session_id = "0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9";
    write_targeted_stop(
        &home,
        &format!("session:{session_id}"),
        &["loops"],
        "targeted session pause",
    );

    let (state, transcript) = target_state(tmp.path(), session_id, Some("x-child"), None);
    let decision = fire(&[
        "loop-check",
        "--state",
        state.to_str().unwrap(),
        "--transcript",
        transcript.to_str().unwrap(),
        "--cwd",
        tmp.path().to_str().unwrap(),
        "--driver",
        "target",
    ]);
    assert_eq!(decision.decision, "allow");
    assert!(decision.termination_reason.is_none());
    assert!(decision.message.contains("targeted session pause"));

    let (other_state, other_transcript) = target_state(
        tmp.path(),
        "11111111-2222-3333-4444-555555555555",
        None,
        None,
    );
    let other = fire(&[
        "loop-check",
        "--state",
        other_state.to_str().unwrap(),
        "--transcript",
        other_transcript.to_str().unwrap(),
        "--cwd",
        tmp.path().to_str().unwrap(),
        "--driver",
        "target",
    ]);
    assert!(!other.message.contains("targeted session pause"));
}
