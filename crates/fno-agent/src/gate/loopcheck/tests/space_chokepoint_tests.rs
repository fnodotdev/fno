//! Decision-chokepoint tests that pin the spaces-move behavior: the run log
//! lands in the worktree slice of the repo's space, not in a checkout. A
//! child of `mod tests` so the private `allow_output` helper stays shared.

use super::{allow_output, observe_decision};

/// Pins the state root inside `dir` and returns the resolved run log. The
/// returned guard holds the env lock for the test body, serializing against
/// the other FNO_AGENT_HOME setters in this suite.
fn pin_space(dir: &std::path::Path) -> (std::sync::MutexGuard<'static, ()>, std::path::PathBuf) {
    let guard = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let agents_home = dir.join(".fnohome/agents");
    std::env::set_var("FNO_AGENT_HOME", &agents_home);
    let run_log = crate::paths::worktree_space_dir(dir).join("run-log.jsonl");
    (guard, run_log)
}

#[test]
fn decision_chokepoint_prefers_canonical_fno_id() {
    let dir = tempfile::tempdir().unwrap();
    let (_env, run_log) = pin_space(dir.path());
    let state = dir.path().join("target-state.md");
    let events = dir.path().join("events.jsonl");
    let canonical = "20260823T060900Z-cx73523-e04109";
    std::fs::write(
        &state,
        format!("---\nfno_id: {canonical}\nsession_id: legacy-run\n---\n"),
    )
    .unwrap();
    let args = vec![
        "loop-check".to_string(),
        "--state".to_string(),
        state.display().to_string(),
        "--transcript".to_string(),
        dir.path().join("transcript.jsonl").display().to_string(),
        "--cwd".to_string(),
        dir.path().display().to_string(),
        "--events".to_string(),
        events.display().to_string(),
        "--global-events".to_string(),
        events.display().to_string(),
    ];

    observe_decision(&args, &allow_output("block", None, "keep working", 1, None));

    assert_eq!(
        crate::run_state::fold_run_state(&run_log, canonical).unwrap(),
        crate::run_state::RunState::Working
    );
    assert_eq!(
        crate::run_state::fold_run_state(&run_log, "legacy-run").unwrap(),
        crate::run_state::RunState::Open
    );
}
