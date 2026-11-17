use super::*;

#[test]
fn shadow_transition_rejection_changes_no_legacy_decision() {
    let dir = tempfile::tempdir().unwrap();
    let run_log = dir.path().join("run-log.jsonl");
    let events = dir.path().join("events.jsonl");
    let run_id = "20260823T060900Z-cx73523-e04109";
    crate::run_state::append_transition(
        &run_log,
        run_id,
        crate::run_state::RunEvent::DispatchClassified,
    )
    .unwrap();
    crate::run_state::append_transition(
        &run_log,
        run_id,
        crate::run_state::RunEvent::PrepareHandoff,
    )
    .unwrap();
    crate::run_state::append_transition(
        &run_log,
        run_id,
        crate::run_state::RunEvent::SuccessorProven,
    )
    .unwrap();
    let legacy = allow_output("block", None, "keep working", 2, None);

    observe_shadow_transition(
        &run_log,
        run_id,
        crate::run_state::RunEvent::DispatchClassified,
        &events,
        &events,
    );

    assert_eq!(legacy, allow_output("block", None, "keep working", 2, None));
    let telemetry = crate::events::committed_journal_text(&events);
    assert!(telemetry.contains("\"type\":\"transition_rejected\""));
    assert!(telemetry.contains("invalid transition Closed + DispatchClassified"));
}

// ── streak debounce ─────────────────────────────────────────────
//
// These drive `read_prior_fires` with an explicit `now` and gap, so they need
// no env var and are parallel-safe -- unlike the integration suite, which
// pins FNO_LOOPCHECK_MIN_FIRE_GAP_SECS=0 process-wide.

#[test]
fn wake_grace_restarts_the_streak_once_per_wake() {
    // The wake door journals type:"wake" rows whose data.session_id is the
    // manifest's fno session id - the same key the gate's per-session reads
    // answer. The grace applies once per wake: the fire that follows the
    // newest wake restarts the streak; the next fire sees the wake older
    // than the fire and the streak behaves exactly as before.
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("events.jsonl");
    let sid = "f-run-1";

    // The event judge requires these nine data fields on loop_check, so a
    // row the checked emitter accepts must carry all of them.
    fn judged_loop_check_row(sid: &str) -> serde_json::Value {
        serde_json::json!({
            "session_id": sid,
            "fingerprint": "a|none|none|none",
            "fires": 1,
            "consecutive_unchanged": 1,
            "decision": "allow",
            "intent": "none",
            "pr_state": "none",
            "ci": "none",
            "reviewed": false,
        })
    }

    // The CHECKED emitter: a lost write must fail the test here, naming the
    // cause, never read downstream as a silent no-grace.

    // No wake row: no grace, streak untouched.
    emit_to_both_checked(&journal, &journal, "loop_check", judged_loop_check_row(sid)).unwrap();
    assert!(!woke_since_last_fire(&journal, sid));

    // A wake row after the fire: grace. emit_to_both stamps the envelope
    // ts, so ordering is real write order.
    std::thread::sleep(std::time::Duration::from_millis(5));
    emit_to_both_checked(
        &journal,
        &journal,
        "wake",
        serde_json::json!({ "session_id": sid }),
    )
    .unwrap();
    assert!(woke_since_last_fire(&journal, sid));

    // The fire after the wake spends the grace: the newest loop_check row
    // is newer than the newest wake, so the streak behaves as before.
    std::thread::sleep(std::time::Duration::from_millis(5));
    emit_to_both_checked(&journal, &journal, "loop_check", judged_loop_check_row(sid)).unwrap();
    assert!(
        !woke_since_last_fire(&journal, sid),
        "the grace is one fire per wake, not standing immunity"
    );

    // Another session's wake row grants nothing: the rows are keyed by the
    // data.session_id the gate reads with.
    let other = tempfile::tempdir().unwrap();
    let other_journal = other.path().join("events.jsonl");
    emit_to_both_checked(
        &other_journal,
        &other_journal,
        "loop_check",
        judged_loop_check_row(sid),
    )
    .unwrap();
    emit_to_both_checked(
        &other_journal,
        &other_journal,
        "wake",
        serde_json::json!({ "session_id": "f-run-2" }),
    )
    .unwrap();
    assert!(!woke_since_last_fire(&other_journal, sid));
}
