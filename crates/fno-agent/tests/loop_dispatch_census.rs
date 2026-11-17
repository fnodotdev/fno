//! The telemetry trap the dispatch census exists to close.
//!
//! A green board read is not evidence the loop works: the 70-second board run
//! also returned a valid 48KB payload and was still killed. And iteration 1
//! has already happened once under a broken board. The positive marker is a
//! `loop_unit_dispatched` row naming a real team scope at iteration 2 or
//! higher, and a health read must separate fixture titles from real ones -
//! a journal of only fixture rows reports ZERO real dispatches, never 756.

use fno_agent::loop_runtime::census_loop_unit_dispatches;

fn row(title: &str, iteration: u64) -> String {
    format!(
        r#"{{"ts":"2026-09-02T22:00:00Z","type":"loop_unit_dispatched","source":"loop","data":{{"unit_id":"u1","session_id":"s1","iteration":{iteration},"title":"{title}"}}}}"#
    )
}

#[test]
fn the_positive_marker_is_a_real_team_scope_at_iteration_two_or_later() {
    // AC5-HP's shape: `lead lead over fno` at iteration 2 clears the bar;
    // the same title at iteration 1 does not (AC5-EDGE - iteration 1 already
    // happened once under a board that could not be read twice). Malformed
    // lines and other event types are skipped, not counted.
    let journal = format!(
        "not json at all\n{}\n{}\n{}\n",
        row("lead lead over fno", 1),
        row("lead lead over fno", 2),
        r#"{"ts":"2026-09-02T22:00:01Z","type":"loop_terminated","source":"loop","data":{"reason":"DonePRGreen"}}"#
    );
    let census = census_loop_unit_dispatches(&journal);
    assert_eq!(census.real, 2);
    assert_eq!(census.fixture, 0);
    assert_eq!(census.real_at_iteration_two_or_later, 1);
}
