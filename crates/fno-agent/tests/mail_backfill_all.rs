//! The `mail-backfill run --all` contract at the integration boundary.
//! The mail scope caps new inline tests at zero (shrink-only), so the
//! engine's all-history test lives here, outside the scope's path words,
//! on the pub seam the door itself dispatches.

use fno_agent::runtime::mail_backfill::backfill_summary;
use serde_json::json;

/// The dry summary's own shape, typed once for the assertions.
#[derive(serde::Deserialize)]
struct DrySummary {
    joined: u64,
    written: u64,
    rows_truncated: u64,
    rows: Vec<serde_json::Value>,
}

/// `--all` joins a pair stamped far outside the outage window (no window
/// means every row scans), stays dry without `--apply`, and refuses a
/// window combined with `--all` instead of silently intersecting.
#[test]
fn all_mode_joins_whole_history_and_refuses_a_window() {
    let root = std::env::temp_dir().join(format!("bf-all-{}", std::process::id()));
    let proj = root.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let body = "the deploy held because the other lane still owned the merge slot today";
    let send_line = json!({
        "type": "assistant",
        "uuid": "row-u1",
        "timestamp": "2026-05-01T10:00:00Z",
        "sessionId": "s-sender",
        "message": {"content": [{"type": "tool_use", "name": "SendMessage",
            "input": {"message": body, "summary": "held"}}]},
    });
    let block_line = json!({
        "type": "user",
        "timestamp": "2026-05-01T10:00:05Z",
        "sessionId": "s-receiver",
        "message": {"content": format!(
            "<cross-session-message from-name=\"s-sender\">\n{body}\n</cross-session-message>")},
    });
    std::fs::write(proj.join("sender.jsonl"), format!("{send_line}\n")).unwrap();
    std::fs::write(proj.join("receiver.jsonl"), format!("{block_line}\n")).unwrap();
    let root_s = root.to_string_lossy().to_string();
    let summary = backfill_summary(&["--all".into(), "--root".into(), root_s.clone()])
        .expect("--all dry run joins the pair outside any window");
    // R1: the summary parses once into a typed shape; no string-key reads.
    let dry: DrySummary = serde_json::from_value(summary).expect("typed dry summary");
    assert_eq!(dry.joined, 1);
    assert_eq!(dry.written, 0);
    assert_eq!(dry.rows_truncated, 0);
    assert_eq!(dry.rows.len(), 1);
    let refused = backfill_summary(&[
        "--all".into(),
        "--since".into(),
        "2026-05-01T00:00:00Z".into(),
    ]);
    assert_eq!(refused.unwrap_err(), 2, "--all with a window refuses");
    let _ = std::fs::remove_dir_all(&root);
}
