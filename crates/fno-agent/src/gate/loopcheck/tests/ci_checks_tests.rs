use super::*;

/// A `fail` bucket is a verdict, beside any number of cancels.
#[test]
fn ci_conclusion_fail_beside_cancels_is_failure() {
    let checks = serde_json::json!([
        {"name": "deploy", "state": "CANCELLED", "bucket": "cancel"},
        {"name": "smoke", "state": "FAILED", "bucket": "fail"}
    ]);
    assert_eq!(
        compute_ci_conclusion(&checks).unwrap(),
        CiConclusion::Failure(Some("smoke".to_string()))
    );
}

/// An unknown or missing bucket fails closed as Pending, never green.
#[test]
fn ci_conclusion_unknown_bucket_fails_closed() {
    let unknown = serde_json::json!([
        {"name": "ci", "state": "SUCCESS", "bucket": "mystery"}
    ]);
    assert_eq!(
        compute_ci_conclusion(&unknown).unwrap(),
        CiConclusion::Pending
    );

    let missing = serde_json::json!([{"name": "ci", "state": "SUCCESS"}]);
    assert_eq!(
        compute_ci_conclusion(&missing).unwrap(),
        CiConclusion::Pending
    );
}
