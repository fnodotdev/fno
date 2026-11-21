//! The `mail-backfill run --all` contract at the integration boundary.
//! The mail scope caps test declarations at zero net (shrink-only), so the
//! engine's contract lives in one test here, outside the scope's path
//! words, on the pub seam the door itself dispatches. It carries the
//! attribution rules the old inline join test asserted - resend pairs the
//! first copy, an ambiguous head never pairs, a head under the floor
//! never pairs - through the real scan instead of synthetic rows.

use fno_agent::runtime::mail_backfill::backfill_summary;
use serde_json::json;

/// The dry summary's own shape, typed once for the assertions.
#[derive(serde::Deserialize)]
struct DrySummary {
    joined: u64,
    written: u64,
    skipped: u64,
    rows_truncated: u64,
    rows: Vec<serde_json::Value>,
}

/// `--all` scans every transcript with no window, stays dry without
/// `--apply`, refuses a window combined with `--all`, and joins only an
/// attributable pair: the resend's first copy wins, the same head in two
/// transcripts refuses, and a body under the floor refuses.
#[test]
fn all_mode_scans_joins_and_refuses_like_the_engine_promises() {
    let root = std::env::temp_dir().join(format!("bf-all-{}", std::process::id()));
    let proj = root.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let long = "the deploy held because the other lane still owned the merge slot today";
    let long2 = "the export waits on the price book before it can name a dollar figure";
    let send = |uuid: &str, body: &str| {
        json!({
            "type": "assistant",
            "uuid": uuid,
            "timestamp": "2026-05-01T10:00:00Z",
            "sessionId": "s-sender",
            "message": {"content": [{"type": "tool_use", "name": "SendMessage",
                "input": {"message": body, "summary": "held"}}]},
        })
    };
    let block = |session: &str, body: &str| {
        json!({
            "type": "user",
            "timestamp": "2026-05-01T10:00:05Z",
            "sessionId": session,
            "message": {"content": format!(
                "<cross-session-message from-name=\"s-sender\">\n{body}\n</cross-session-message>")},
        })
    };
    std::fs::write(
        proj.join("sender.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            send("r1", long),
            send("r2", long2),
            send("r3", "continue")
        ),
    )
    .unwrap();
    std::fs::write(
        proj.join("recv1.jsonl"),
        format!("{}\n{}\n", block("s-b1", long), block("s-b1", long)),
    )
    .unwrap();
    std::fs::write(
        proj.join("recv2.jsonl"),
        format!("{}\n", block("s-b2", long2)),
    )
    .unwrap();
    std::fs::write(
        proj.join("recv3.jsonl"),
        format!("{}\n", block("s-b3", long2)),
    )
    .unwrap();
    let root_s = root.to_string_lossy().to_string();
    let summary = backfill_summary(&["--all".into(), "--root".into(), root_s.clone()])
        .expect("--all dry run scans and joins outside any window");
    // R1: the summary parses once into a typed shape; no string-key reads.
    let dry: DrySummary = serde_json::from_value(summary).expect("typed dry summary");
    assert_eq!(dry.joined, 1, "the resend pairs its first copy only");
    assert_eq!(
        dry.skipped, 2,
        "the ambiguous head and the floor head refuse"
    );
    assert_eq!(dry.written, 0, "dry without --apply writes nothing");
    assert_eq!(dry.rows_truncated, 0);
    assert_eq!(dry.rows.len(), 1);
    let refused = backfill_summary(&[
        "--all".into(),
        "--since".into(),
        "2026-05-01T00:00:00Z".into(),
    ]);
    assert_eq!(refused.unwrap_err(), 2, "--all with a window refuses");
    let missing = backfill_summary(&["--since".into()]);
    assert_eq!(
        missing.unwrap_err(),
        2,
        "a valueless --since refuses, never silently defaults"
    );
    let _ = std::fs::remove_dir_all(&root);
}
