use super::*;

#[test]
fn blocking_severity_codex_p1_both_forms() {
    // The exact markup codex emits (pinned from PR #447).
    assert_eq!(
        blocking_severity("![P1 Badge](https://img.shields.io/badge/P1-orange?style=flat) Bug"),
        Some("P1")
    );
    // Alt-text only and URL only each match.
    assert_eq!(blocking_severity("![P1 Badge] something"), Some("P1"));
    assert_eq!(
        blocking_severity("see https://img.shields.io/badge/P1-orange"),
        Some("P1")
    );
}

/// sigma-review: a blocking finding row with a missing id is SKIPPED
/// (under-block per locked decision 4), never pooled on a default id
/// where one stray reply could clear multiple findings.
#[test]
fn finding_missing_id_skipped_not_pooled() {
    let no_id = serde_json::json!({
        "in_reply_to_id": null,
        "user": {"login": "chatgpt-codex-connector[bot]"},
        "body": "![P1 Badge](https://img.shields.io/badge/P1-orange) idless",
        "path": "src/z.rs", "line": 3,
        "created_at": "2026-06-05T01:05:00Z"
    });
    let real = finding_comment(
        100,
        "![P1 Badge](https://img.shields.io/badge/P1-orange) real",
        "2026-06-05T01:10:00Z",
    );
    // A stray reply keyed to id 0 must not ack anything.
    let stray = reply_comment(
        101,
        0,
        "bllshttng",
        "wontfix: stray",
        "2026-06-05T01:20:00Z",
    );
    let comments = vec![no_id, real, stray];
    let (_, unaddressed) = compute_unaddressed_findings(&comments, &[], &req_vec(), &[]);
    assert_eq!(unaddressed.len(), 1, "only the real finding remains");
    assert_eq!(unaddressed[0].id, 100);
}
