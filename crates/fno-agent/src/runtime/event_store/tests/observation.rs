use super::*;
use serde_json::json;

/// `mm:ss` past an hour anchored once, one to two hours ago: inside every
/// prune horizon, and late enough that each coalescing window has expired.
fn at(mm_ss: &str) -> String {
    static ANCHOR: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let hour = ANCHOR.get_or_init(|| {
        (chrono::Utc::now() - chrono::Duration::hours(1))
            .format("%Y-%m-%dT%H")
            .to_string()
    });
    format!("{hour}:{mm_ss}Z")
}

fn allow(ts: &str, tool: &str) -> serde_json::Value {
    json!({"ts": ts, "type": "guard_decision", "source": "hook",
           "data": {"guard": "git-protection", "decision": "allow", "tool": tool}})
}

#[test]
fn blocks_and_malformed_never_coalesce() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    for ts in [&at("00:00"), &at("01:00"), &at("02:00")] {
        let block = json!({"ts": ts, "type": "guard_decision", "source": "hook",
            "data": {"guard": "git-protection", "decision": "block", "tool": "Bash"}});
        let receipt = append_envelope(&live, &block.to_string(), None).unwrap();
        assert!(receipt.inserted);
        assert!(
            !receipt.suppressed,
            "each block is a separate attempted action"
        );
    }
    let malformed = json!({"ts": &at("03:00"), "type": "guard_decision",
        "source": "hook", "data": {"guard": "git-protection", "tool": "Bash"}});
    // The judge owns the schema now: a guard_decision with no decision is
    // refused at the commit, so the always-audit tolerance is gone and the
    // row never lands to coalesce with anything.
    let err = append_envelope(&live, &malformed.to_string(), None)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("missing required data field: decision"),
        "err: {err}"
    );
    assert_eq!(count_events(&store_path(&live)), 3);
}

#[test]
fn unavailable_observation_state_is_an_explicit_failure() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    append_envelope(&live, &allow(&at("00:00"), "Bash").to_string(), None).unwrap();
    let store = store_path(&live);
    {
        let conn = Connection::open(&store).unwrap();
        conn.execute_batch(
            "DROP TABLE event_observation_state;
             CREATE TABLE event_observation_state (x INTEGER);",
        )
        .unwrap();
    }
    let broken = append_envelope(&live, &allow(&at("01:00"), "Bash").to_string(), None);
    let err = broken.unwrap_err().to_string();
    assert!(err.contains("event_observation_state"), "err: {err}");
    let conn = Connection::open(&store).unwrap();
    let pending: i64 = conn
        .query_row(
            "SELECT count(*) FROM event_observation_pending
             WHERE obs_type = 'guard_decision'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        pending, 0,
        "nothing counted pending when the state table is broken"
    );
    let guards: i64 = conn
        .query_row(
            "SELECT count(*) FROM events WHERE type = 'guard_decision'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(guards, 1, "no row was added beyond the first");
}
