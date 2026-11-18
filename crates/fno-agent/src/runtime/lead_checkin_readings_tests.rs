#[test]
fn cause_rows() {
    let stderr = "fno config: a is not modeled\nfno config: b is not modeled\ngh: API rate limit exceeded for user ID 4994564. (HTTP 403)";
    assert_eq!(
        stderr_cause(stderr),
        "gh: API rate limit exceeded for user ID 4994564. (HTTP 403)"
    );

    let error = "gh api repos/{owner}/{repo}/commits/<sha>/check-runs failed: fno config: x is not modeled\ngh: API rate limit exceeded (HTTP 403)";
    assert_eq!(
        gh_error_cause(error),
        "gh: API rate limit exceeded (HTTP 403)"
    );

    assert_eq!(
        stderr_cause("fno config: first\nfno config: last"),
        "fno config: last"
    );
    assert_eq!(stderr_cause(" \n\t"), "no stderr");

    let cause = "é".repeat(300);
    let result = stderr_cause(&cause);
    assert_eq!(result.chars().count(), 120);
    assert_eq!(result, "é".repeat(120));
}

#[test]
fn capacity_lanes_render_quota_resets_and_blocked_workers() {
    let gate = serde_json::json!({
        "verdict": "accepted",
        "lanes": {
            "zai": {
                "live": 1,
                "cap": 1,
                "quota": "closed (resets 2026-11-18T19:42:00Z)",
                "quota_blocked": ["t-glm-a", "t-glm-b"],
            },
            "codex": {"live": 0, "cap": null, "quota": "unmeasured", "quota_blocked": []},
        }
    });
    let out = r_capacity_pair(&serde_json::json!({}), &gate).unwrap();
    let lanes = out["lanes"].as_str().unwrap();
    assert!(
        lanes.contains("zai 1/1 closed (resets 2026-11-18T19:42:00Z)"),
        "{lanes}"
    );
    assert!(lanes.contains("blocked: t-glm-a, t-glm-b"), "{lanes}");
    assert!(lanes.contains("codex 0/- unmeasured"), "{lanes}");
}
