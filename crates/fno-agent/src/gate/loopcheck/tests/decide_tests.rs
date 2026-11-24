use super::*;

#[test]
fn fire_budget_clamp_stays_under_remaining_and_above_the_floor() {
    let s = std::time::Duration::from_secs(30);
    // Budget-rich: the configured ceiling stands.
    assert_eq!(
        clamp_to_fire_budget(s, s + std::time::Duration::from_secs(1)),
        s
    );
    // Budget-poor: what remains stands.
    assert_eq!(
        clamp_to_fire_budget(s, std::time::Duration::from_millis(500)),
        std::time::Duration::from_millis(500)
    );
    // Budget spent: every read still gets a positive, killable bound.
    assert_eq!(
        clamp_to_fire_budget(s, std::time::Duration::ZERO),
        STOPGATE_BOUND_FLOOR
    );
}

#[test]
fn freshness_shrunk_diff_carries_as_subset() {
    // The specimen shape: one doc sentence reworded, one test-file hunk
    // gone because main absorbed it. Every raw line still shipping was
    // read, so the shrunk diff carries instead of costing a re-review.
    let f = facts_lines(
        &[
            ":100644 100644 aaa bbb M\tcli/src/a.py",
            ":100644 100644 ccc ddd M\tcli/tests/test_a.py",
        ],
        &[":100644 100644 aaa bbb M\tcli/src/a.py"],
        Some(&["docs/x.md"]),
    );
    let verdict = review_freshness("r1", "h1", &f);
    assert_eq!(verdict, Freshness::CarriedSubset);
    assert!(verdict.counts());
}

#[test]
fn freshness_subset_does_not_swallow_equal_or_added_lines() {
    // Equal sets are not a strict subset (they hash equal and take the
    // tree-paths branch instead), and a HEAD line the reviewer never saw
    // is new unreviewed code: Stale.
    assert_eq!(
        review_freshness(
            "r",
            "h",
            &facts_lines(&["l1", "l2"], &["l1", "l2"], Some(&[]))
        ),
        Freshness::CarriedBaseSync
    );
    assert_eq!(
        review_freshness("r", "h", &facts_lines(&["l1"], &["l1", "l2"], None)),
        Freshness::Stale
    );
}

#[test]
fn reviewers_gate_stays_fail_closed() {
    // AC7: promoting the predicate's return value to a list must not move
    // the gate. Missing file, missing event, stale head, and a `fail`
    // verdict all still leave the reviewer unsatisfied.
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("absent.jsonl");
    let sigma = vec!["sigma".to_string()];
    assert!(!unattested_reviewers(&missing, &sigma, "h", "").is_empty());

    let stale = tmp.path().join("stale.jsonl");
    std::fs::write(&stale, format!("{}\n", r#"{"ts":"2026-01-01T00:00:00Z","source":"test","type":"review_attestation","data":{"reviewer":"sigma","head_sha":"OLD","verdict":"pass","branch":"feature/x"}}"#)).unwrap();
    let out = unattested_reviewers(&stale, &sigma, "NEW", "feature/x");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].superseded_head.as_deref(), Some("OLD"));
    assert!(!out[0].failed_at_head);

    let failed = tmp.path().join("fail.jsonl");
    std::fs::write(&failed, format!("{}\n", r#"{"ts":"2026-01-01T00:00:00Z","source":"test","type":"review_attestation","data":{"reviewer":"sigma","head_sha":"h","verdict":"fail"}}"#)).unwrap();
    let out = unattested_reviewers(&failed, &sigma, "h", "");
    assert_eq!(out.len(), 1);
    // A head-pinned fail is not a superseded pass; do not offer a stale head.
    assert_eq!(out[0].superseded_head, None);
    // ...but it IS an attestation at this head, and the message says so
    // rather than claiming none exists. Pinned at the PARSER: the message
    // test hand-builds the struct and never exercises this derivation, so
    // `failed_at_head: false` survived the whole suite before this line.
    assert!(
        out[0].failed_at_head,
        "a fail at HEAD must be reported as such"
    );
}

#[test]
fn unpinned_attestation_never_counts_as_evidence() {
    // codex P1 on this PR: defaulting a missing head_sha to "" made an
    // unpinned event MATCH a caller whose own head_sha is "", turning
    // no-evidence into a pass.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("e.jsonl");
    std::fs::write(
            &p,
            format!(
                "{}\n",
                r#"{"ts":"2026-01-01T00:00:00Z","source":"test","type":"review_attestation","data":{"reviewer":"sigma","verdict":"pass"}}"#
            ),
        )
        .unwrap();
    let out = unattested_reviewers(&p, &["sigma".to_string()], "", "");
    assert_eq!(out.len(), 1, "unpinned evidence must not satisfy the gate");
    assert_eq!(out[0].superseded_head, None);
}

#[test]
fn block_reason_stops_demanding_a_push_for_a_shipped_head() {
    // The wedge, end to end. Positive marker AND absence: a panicking
    // renderer would also fail to print the push message.
    let pr = shipped_pr("23480a0e", PrState::Merged);
    let reason = build_block_reason(&pr, "fe407c3b", true, true);
    assert!(
        !reason.contains("push the latest commits"),
        "shipped head must not be told to push: {reason}"
    );
    assert!(!reason.is_empty(), "a reason is still rendered: {reason}");
    // The person gate rides this declaration (G3 is shrink-only): an owed
    // person with no other named gap must read the person sentence, not the
    // generic config lecture that teaches a wait nothing will satisfy.
    let mut pr = PrInfo::default();
    pr.state = PrState::Open;
    pr.number = 7;
    pr.ci_conclusion = CiConclusion::Success;
    pr.human_approval_owed = true;
    let reason = build_block_reason(&pr, "23480a0e", true, true);
    assert!(
        reason.contains("review.human"),
        "the block must name the person gate: {reason}"
    );
    assert!(
        !reason.contains("arm a watcher"),
        "the person gate teaches no arm-and-tag ritual: {reason}"
    );
}

#[test]
fn watch_idle_window_defaults_clamps_and_slacks() {
    // Default (no tag timeout): 15m + 12m slack.
    assert_eq!(
        watch_window_ms(None),
        15 * 60_000 + watch_lease::WATCH_SLACK_MS
    );
    // Honored within range.
    assert_eq!(
        watch_window_ms(Some("10m")),
        10 * 60_000 + watch_lease::WATCH_SLACK_MS
    );
    // Above the 15m ceiling clamps down: the ci-push-cadence law wakes a
    // parked session every 15 minutes, so a 30m tag reads as 15m.
    assert_eq!(
        watch_window_ms(Some("30m")),
        15 * 60_000 + watch_lease::WATCH_SLACK_MS
    );
    // Below the 5m floor clamps up.
    assert_eq!(
        watch_window_ms(Some("1m")),
        5 * 60_000 + watch_lease::WATCH_SLACK_MS
    );
    // The same ceiling holds for hours-long tags.
    assert_eq!(
        watch_window_ms(Some("5h")),
        15 * 60_000 + watch_lease::WATCH_SLACK_MS
    );
    // Garbage falls back to the default.
    assert_eq!(
        watch_window_ms(Some("soon")),
        15 * 60_000 + watch_lease::WATCH_SLACK_MS
    );
    // The event deadline uses the normalized watch duration, not claim slack.
    assert_eq!(watch_lease::watch_expiry_ms(Some("30m"), 1_000), 901_000);
    assert_eq!(watch_lease::watch_expiry_ms(Some("1m"), 1_000), 301_000);
}
