//! The unattended journey for opencode's serve lane. It covers launch,
//! steering, structured readback, and session liveness after the turn. Cleanup
//! deletes the session and stops the private serve.
//!
//! It runs against whichever `opencode` is on PATH and follows the contract
//! the boot classified: 1.x launches through the `run --attach` writer, 2.x
//! over the `/api/*` routes. Set `FNO_JOURNEY_OPENCODE_CONTRACT=2.x` (or
//! `1.x`) to refuse a run on the other contract, which is how a 2.x run
//! proves the 2.x lane and not a 1.x fallback.
//!
//! Server-kill survival, review-lane selection, and viewport materialisation
//! remain separate environment checks. They must not be inferred from a
//! successful launch or a non-empty reply.
//!
//! Opt-in: runs only when `FNO_JOURNEY_OPENCODE=1` AND an `opencode` binary is
//! on PATH (CI has neither).

use fno_agents::opencode_serve::{
    delete_session, dispatch_opencode_serve, ensure_serve, fetch_messages, fetch_session,
    steer_existing_session, ServeHandle, ServeMessage,
};
use fno_agents::paths::AgentsHome;
use fno_agents::state::load_registry;

use std::time::{Duration, Instant};

fn journey_enabled() -> bool {
    if std::env::var("FNO_JOURNEY_OPENCODE").ok().as_deref() != Some("1") {
        return false;
    }
    std::process::Command::new("opencode")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The last finished assistant text in the readback, if any.
fn last_assistant_text(messages: &[ServeMessage]) -> Option<String> {
    messages
        .iter()
        .filter(|m| m.role == "assistant" && m.complete)
        .filter_map(|m| m.text.clone())
        .last()
}

/// Poll the readback until a finished assistant reply contains `needle`.
fn wait_for_reply(serve: &ServeHandle, session_id: &str, needle: &str) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(180);
    while Instant::now() < deadline {
        if let Ok(messages) = fetch_messages(serve, session_id) {
            if let Some(text) = last_assistant_text(&messages).filter(|t| t.contains(needle)) {
                return Some(text);
            }
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    None
}

/// The session id the launch produced. 2.x names it in the receipt. 1.x
/// leaves it to the worker's session report, so the registry row is read.
fn launched_session_id(home: &AgentsHome, receipt: &serde_json::Value) -> String {
    if let Some(id) = receipt
        .pointer("/session_id")
        .and_then(|v| v.as_str())
        .filter(|id| !id.is_empty())
    {
        return id.to_string();
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if let Ok(reg) = load_registry(&home.registry_json()) {
            if let Some(id) = reg
                .find("wk-journey")
                .and_then(|row| row.harness_session_id.clone())
            {
                return id;
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    panic!("no session id: the receipt names none and no session report bound the row");
}

/// Kills the private serve pid on drop, so a FAILED assertion (which panics
/// past the inline cleanup) still leaves no orphan node process pointing at a
/// deleted tempdir.
struct ServeReaper(u32);
impl Drop for ServeReaper {
    fn drop(&mut self) {
        // Group TERM then group KILL: `opencode serve` spawns children, and a
        // pid TERM leaves them running.
        unsafe { libc::killpg(self.0 as libc::pid_t, libc::SIGTERM) };
        std::thread::sleep(std::time::Duration::from_millis(300));
        unsafe { libc::killpg(self.0 as libc::pid_t, libc::SIGKILL) };
    }
}

#[test]
fn opencode_bg_worker_completes_unattended_over_serve() {
    if !journey_enabled() {
        eprintln!(
            "skipping: set FNO_JOURNEY_OPENCODE=1 with opencode on PATH to run the live journey"
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let home = AgentsHome::at(dir.path().to_path_buf());
    let cwd = dir.path().join("worktree");
    std::fs::create_dir_all(&cwd).unwrap();

    let serve = ensure_serve(&home).expect("serve boots unattended");
    let _reaper = ServeReaper(serve.pid);
    eprintln!("journey contract: {}", serve.contract.label());
    if let Ok(wanted) = std::env::var("FNO_JOURNEY_OPENCODE_CONTRACT") {
        assert_eq!(
            serve.contract.label(),
            wanted,
            "the serve on PATH speaks the other contract"
        );
    }
    // Boot proof: the generated config carries the unattended permission posture.
    let config = std::fs::read_to_string(home.root().join("opencode-serve-config.json")).unwrap();
    assert!(
        config.contains("allow") && config.contains("*"),
        "config: {config}"
    );

    let outcome = dispatch_opencode_serve(
        &home,
        "wk-journey",
        "Reply with exactly: journey-ok",
        "journey",
        &cwd,
        None,
        None,
        None,
    );
    assert_eq!(outcome.exit_code, 0, "dispatch failed: {}", outcome.stderr);
    let receipt: serde_json::Value = serde_json::from_str(outcome.stdout.trim()).unwrap();
    assert_eq!(
        receipt.pointer("/contract").and_then(|v| v.as_str()),
        Some(serve.contract.label())
    );
    let session_id = launched_session_id(&home, &receipt);
    assert!(
        session_id.starts_with("ses_"),
        "session id shape: {session_id}"
    );

    // Fire-and-forget proof: the spawn returned; the TURN runs on the serve.
    let reply = wait_for_reply(&serve, &session_id, "journey-ok")
        .expect("no journey-ok assistant reply within 180s");
    eprintln!("first reply: {reply:?}");

    // Structured capture is the SERVE's message readback: model identity and
    // message ids a pane scrape could never give.
    let messages = fetch_messages(&serve, &session_id).expect("readback for capture");
    let assistant = messages
        .iter()
        .find(|m| m.role == "assistant")
        .expect("assistant message in readback");
    assert!(
        assistant.model.as_deref().is_some_and(|m| !m.is_empty()),
        "structured model identity on the readback: {assistant:?}"
    );
    assert!(
        assistant.id.starts_with("msg_"),
        "structured message id on the readback: {assistant:?}"
    );

    // Steering confirms a new serve message by id. The recalled token is the
    // positive session marker for the second turn; a merely non-empty reply
    // would only prove transport.
    let recalled = "serve-session-recall-efa4";
    let steer_id = steer_existing_session(
        &home,
        &session_id,
        &format!("Remember this token exactly and repeat it: {recalled}"),
        &cwd,
        None,
        Duration::from_secs(60),
    )
    .expect("steering readback");
    assert!(
        steer_id.starts_with("msg_"),
        "steering message id: {steer_id}"
    );
    wait_for_reply(&serve, &session_id, recalled)
        .unwrap_or_else(|| panic!("second turn did not recall {recalled}"));

    // The serve session remains addressable after the turn.
    let session = fetch_session(&serve, &session_id).expect("session liveness readback");
    assert_eq!(
        session.pointer("/id").and_then(|v| v.as_str()),
        Some(session_id.as_str())
    );

    // Cleanup: session deleted; the reaper drops the serve pid on exit.
    delete_session(&serve, &session_id).expect("session teardown");
}
