use super::*;

#[test]
fn a_zero_file_retraction_for_an_old_head_spares_the_newer_pass() {
    // The command emits the retraction after any newer pass, so the events
    // order is pass@head2 then retract@head1: the named (old) pass is
    // already superseded, and revoking it must not also revoke the live
    // head-2 pass nobody asked about.
    let mut text = String::new();
    text.push_str(
        &serde_json::json!({
            "ts": "2026-01-01T00:00:00Z",
            "source": "test",
            "type": "review_attestation",
            "data": {"reviewer": "code-review", "head_sha": "head2", "verdict": "pass",
                     "attester_session_id": "sess-a", "branch": "b",
                     "reviewed_line_count": 5, "reviewed_file_count": 1}
        })
        .to_string(),
    );
    text.push('\n');
    text.push_str(
        &serde_json::json!({
            "ts": "2026-01-01T00:00:00Z",
            "source": "test",
            "type": "review_attestation",
            "data": {"reviewer": "code-review", "head_sha": "head1", "verdict": "fail",
                     "attester_session_id": "sess-a", "branch": "b",
                     "retracts_attester": "sess-a",
                     "reviewed_line_count": 3, "reviewed_file_count": 1}
        })
        .to_string(),
    );
    // The pair map keeps every latest verdict, so "the head-2 pass
    // survives" is "a PASS at head2 exists" - an answered fail alone
    // must not satisfy this assertion.
    let (passes, _raised) = local_latest_attestations(&text, "b", "head2");
    assert!(
        passes.iter().any(|p| p.head == "head2" && p.is_pass),
        "the newer head-2 pass must survive a retraction naming head-1: {passes:?}"
    );
}
