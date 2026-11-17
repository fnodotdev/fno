use super::*;

#[test]
fn github_app_review_without_a_commit_oid_fails_closed() {
    // An older review object, or a payload shape change, leaves no commit
    // to pin. That is an absence, and an absence is never freshness.
    let reviews = vec![serde_json::json!({
        "author": {"login": "chatgpt-codex-connector"},
        "state": "APPROVED"
    })];
    let rep = classify_coverage(
        &reviews,
        &[],
        "",
        &["chatgpt-codex-connector".to_string()],
        true,
        None,
        // The real predicate, not a stub: an empty sha must reach Stale on
        // its own rather than because a fake said so.
        &|sha| review_freshness(sha, "89bc0b91", &FreshnessFacts::default()),
        "",
        "",
    );
    assert_eq!(rep.verdicts[0].verdict, CoverageVerdict::Stale);
    assert_eq!(rep.coverage, Coverage::Covered(0));
}

#[test]
fn a_retraction_revokes_the_named_pair_not_the_retractor() {
    // The retraction command addresses the EVENT, not the identity: an
    // operator session emits a fail carrying retracts_attester naming the
    // pair that passed. The pair-keyed scan must revoke THAT pair while
    // the retracting session's own entry stays untouched - undoing an
    // impersonation must not require performing it a second time.
    let pass = serde_json::json!({
        "ts": "2026-01-01T00:00:00Z", "source": "test",
        "type": "review_attestation",
        "data": {"reviewer": "code-review", "head_sha": "h", "verdict": "pass",
                 "attester_session_id": "sess-A", "branch": "feature/x"}
    })
    .to_string();
    let control = classify_coverage(
        &[],
        &[],
        &pass,
        &[],
        false,
        None,
        &|_| Freshness::Fresh,
        "feature/x",
        "h",
    );
    assert_eq!(control.coverage, Coverage::Covered(1));

    let retraction = serde_json::json!({
        "ts": "2026-01-01T00:00:00Z", "source": "test",
        "type": "review_attestation",
        "data": {"reviewer": "code-review", "head_sha": "h", "verdict": "fail",
                 "attester_session_id": "sess-operator",
                 "retracts_attester": "sess-A", "branch": "feature/x"}
    })
    .to_string();
    let revoked = classify_coverage(
        &[],
        &[],
        &format!("{}\n{}", pass, retraction),
        &[],
        false,
        None,
        &|_| Freshness::Fresh,
        "feature/x",
        "h",
    );
    assert!(
        revoked.verdicts.is_empty(),
        "the named pair must yield no verdict"
    );
    assert_eq!(revoked.coverage, Coverage::Unknown);
}
