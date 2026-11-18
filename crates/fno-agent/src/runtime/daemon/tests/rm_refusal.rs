//! The rm live-row refusal per roster verdict: unknown read, partial list.

use super::*;

#[tokio::test]
async fn rm_refuses_the_registry_row_for_its_own_session_before_teardown() {
    let _env_lock = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _clear_identity: Vec<_> = crate::claims::AMBIENT_IDENTITY_NAMES
        .iter()
        .map(|name| crate::claims::EnvVarGuard::unset(name))
        .collect();
    let _harness = crate::claims::EnvVarGuard::set("FNO_HARNESS_NAME", "fno");
    let _session = crate::claims::EnvVarGuard::set(
        "FNO_HARNESS_SESSION_ID",
        "worker-session-123456789012345678901234",
    );

    let home = short_home("rmself");
    let mut row = claude_rm_row(
        "self-worker",
        "self1234",
        "worker-session-123456789012345678901234",
    );
    row.status = AgentStatus::Live;
    state::update_registry(&home.registry_json(), |registry| registry.entries.push(row)).unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agent-worker"));
    let request = Request::new(1, "agent.rm", json!({"name": "self-worker"}));
    let response = handle_rm_with(
        &ctx,
        &request,
        &|| panic!("self-removal refusal returns before reading the Claude roster"),
        &|_| panic!("self-removal refusal returns before stopping the harness session"),
        &|_, _| panic!("self-removal refusal returns before the stop leg"),
        &|_, _| panic!("self-removal refusal returns before killing the mux pane"),
        &|_, _| panic!("self-removal refusal returns before probing the mux pane"),
    )
    .await;

    let error = response.error().expect("self-removal is refused");
    assert!(error.message.contains("cannot remove its own registry row"));
    let registry = state::load_registry(&home.registry_json()).unwrap();
    assert_eq!(registry.entries.len(), 1);
    assert_eq!(registry.entries[0].name, "self-worker");
    std::fs::remove_dir_all(home.root()).ok();
}

/// A partial list cannot prove absence: a row hidden among the skipped
/// rows would read as gone. The refusal names the warnings instead.
#[tokio::test]
async fn rm_absence_is_not_proof_on_a_warning_carrying_list() {
    let home = short_home("rmpartialabsence");
    let mut row = claude_rm_row(
        "done-worker",
        "aaabbb14",
        "aaabbb14-1111-2222-3333-444444444444",
    );
    row.status = AgentStatus::Live;
    state::update_registry(&home.registry_json(), |registry| registry.entries.push(row)).unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agent-worker"));
    let request = Request::new(1, "agent.rm", json!({"name": "done-worker"}));
    let response = handle_rm_with(
        &ctx,
        &request,
        &|| crate::claude_roster::ClaudeAgentsSnapshot::Known {
            rows: Vec::new(),
            warnings: vec!["one malformed row".into()],
        },
        &|_| Ok(()),
        &|_, _| crate::daemon::rm_teardown::StopLeg::Asked,
        &|_, _| Ok(true),
        &|_, _| PaneProbe::Unknown,
    )
    .await;

    let message = &response.error().unwrap().message;
    assert!(message.contains("absence is not proof"));
    assert_eq!(
        state::load_registry(&home.registry_json())
            .unwrap()
            .entries
            .len(),
        1
    );
    std::fs::remove_dir_all(home.root()).ok();
}

/// The user's pin outranks every machine rm, `--force` included: a named
/// reason meets the keep mark and holds before any stop or teardown arm runs.
#[tokio::test]
async fn a_machine_rm_holds_on_a_pinned_row_even_forced() {
    let home = short_home("rmpinned");
    let row = claude_rm_row(
        "pinned-worker",
        "eeee5555",
        "eeee5555-1111-2222-3333-444444444444",
    );
    state::update_registry(&home.registry_json(), |registry| registry.entries.push(row)).unwrap();
    crate::keep_mark::set(
        &home,
        "eeee5555-1111-2222-3333-444444444444",
        "pinned-worker",
    )
    .unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agent-worker"));
    let request = Request::new(
        1,
        "agent.rm",
        json!({"name": "pinned-worker", "force": true, "audit_reason": "codex-sandbox-blocked"}),
    );
    let response = handle_rm_with(
        &ctx,
        &request,
        &|| panic!("the hold returns before the roster read"),
        &|_| panic!("the hold returns before the harness cascade"),
        &|_, _| panic!("the hold returns before the stop leg"),
        &|_, _| panic!("the hold returns before the pane arms"),
        &|_, _| PaneProbe::Unknown,
    )
    .await;

    let message = &response.error().unwrap().message;
    assert!(message.contains("held: keep mark: set "), "{message}");
    assert!(message.contains("--unkeep pinned-worker"), "{message}");
    assert_eq!(
        state::load_registry(&home.registry_json())
            .unwrap()
            .entries
            .len(),
        1
    );
    std::fs::remove_dir_all(home.root()).ok();
}
