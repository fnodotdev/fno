//! The submit arm's row rule ([`crate::mux::server::human_input`]): which
//! witness rows fire the conversation arm at Enter. Declared in the test
//! hub (`server_tests.rs`), a descendant of server, so the private
//! `human_input` module is reachable by descent without widening it past
//! the server. Pure over the row JSON, so no server fixture is needed; the
//! detached spawn itself is the std-command shape shared with the release
//! timer and carries no logic.

use crate::mux::server::human_input::arm_session_from_row;

fn submit_row(resolution: &str, harness_session: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "ts": "2026-11-20T00:00:00Z",
        "type": "operator_submit",
        "source": "daemon",
        "data": {
            "mux_session": "main",
            "pane": 7,
            "via": "pane",
            "submit_ms": 1_000,
            "resolution": resolution,
            "harness_session": harness_session,
        }
    })
}

#[test]
fn a_resolved_submit_arms_its_harness_session() {
    let row = submit_row("ok", Some("cccccccc-1111-2222-3333-444455556666"));
    assert_eq!(
        arm_session_from_row(&row).as_deref(),
        Some("cccccccc-1111-2222-3333-444455556666"),
        "a bound Enter arms the session it resolved to"
    );
}

#[test]
fn an_unresolved_or_addressless_submit_arms_nothing() {
    let shell = submit_row("unresolved", None);
    assert!(
        arm_session_from_row(&shell).is_none(),
        "a hand-started harness in a shell pane has no address to arm"
    );
    let empty = submit_row("ok", Some(""));
    assert!(
        arm_session_from_row(&empty).is_none(),
        "a resolved bind with an empty session id arms nothing"
    );
}
