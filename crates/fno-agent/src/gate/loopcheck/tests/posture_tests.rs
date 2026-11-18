use super::*;

#[test]
fn identity_free_same_model_peer_is_an_unsatisfiable_local_gate() {
    let s = Settings {
        peers: vec![PeerEntry {
            provider: "codex".into(),
            model: None,
            identity: None,
        }],
        ..Default::default()
    };
    assert_eq!(
        resolved_local_peer_reviewers_for_author(&s, Some("codex")),
        vec![SAME_MODEL_LOCAL_PEER_SENTINEL.to_string()]
    );
}

/// A same-model peer whose identity COLLIDES with a required App login is
/// fail-closed, not exempt (codex peer review on PR #375): the App login is
/// kept (its requirement is not loosened) AND the sentinel is added, so a
/// same-model review under the shared login cannot clear the gate.
#[test]
fn base_app_login_collision_is_fail_closed() {
    let s = Settings {
        github_apps: Some(vec!["fno-peer-bot".into()]),
        peers: vec![PeerEntry {
            provider: "codex".into(),
            model: None,
            identity: None,
        }],
        peer_identity: Some("fno-peer-bot".into()),
        ..Default::default()
    };
    let logins = resolved_required_bots_for_author(&s, Some("codex"));
    assert!(logins.iter().any(|l| l == "fno-peer-bot")); // App requirement kept
    assert!(logins.iter().any(|l| l == SAME_MODEL_PEER_SENTINEL)); // gate held
}
