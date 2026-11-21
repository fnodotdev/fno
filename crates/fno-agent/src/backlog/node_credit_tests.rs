//! The holding-lead stamp's tests, in their own file: the sibling-tests
//! layout `maintenance_tests.rs` uses. The file, not the module, is what
//! the test-audit scope table reads.

use super::*;
use serde_json::json;

fn row(ts: &str, name: Option<&str>, holder: Option<&str>) -> Value {
    let mut event = json!({"scope": "epic-alpha", "ts": ts, "change": "beat", "data": {}});
    let obj = event.as_object_mut().unwrap();
    if let Some(n) = name {
        obj.insert("name".into(), json!(n));
    }
    if let Some(h) = holder {
        if let Some(data) = obj.get_mut("data").and_then(Value::as_object_mut) {
            data.insert("holder_session".into(), json!(h));
        }
    }
    event
}

#[test]
fn holder_at_picks_newest_row_at_or_before_completed() {
    let events = vec![
        row("2026-11-20T12:00:00Z", Some("Avery"), Some("s-b")),
        row("2026-11-18T09:00:00Z", Some("Barnaby"), Some("s-a")),
    ];
    let (name, holder) =
        holder_from_events(&events, "2026-11-19T00:00:00Z").expect("a row inside the bound");
    assert_eq!(name.as_deref(), Some("Barnaby"));
    assert_eq!(holder.as_deref(), Some("s-a"));
    // A bound between the two rows keeps the older tenure's credit, and a
    // bound before every row credits nobody.
    assert!(holder_from_events(&events, "2026-11-10T00:00:00Z").is_none());
}

#[test]
fn holder_at_reads_none_without_history_in_the_window() {
    let events = vec![row("2026-11-20T12:00:00Z", Some("Avery"), Some("s-b"))];
    assert!(holder_from_events(&events, "2026-11-01T00:00:00Z").is_none());
    assert!(holder_from_events(&events, "not-a-ts").is_none());
    assert!(holder_from_events(&[], "2026-11-20T12:00:00Z").is_none());
}

#[test]
fn holders_over_window_dedupes_and_orders_oldest_first() {
    // The older tenure carries the alphabetically larger id, so a
    // session-id ordering cannot fake tenure order: s-z (Barnaby, from
    // 11-18) must come before s-a (Avery, from 11-19).
    let events = vec![
        row("2026-11-20T12:00:00Z", Some("Avery"), Some("s-a")),
        row("2026-11-19T12:00:00Z", Some("Avery"), Some("s-a")),
        row("2026-11-18T12:00:00Z", Some("Barnaby"), Some("s-z")),
        row("2026-11-01T12:00:00Z", Some("Early"), Some("s-0")),
    ];
    let holders = holders_over_window(
        &events,
        Some("2026-11-15T00:00:00Z"),
        "2026-11-21T00:00:00Z",
    );
    assert_eq!(
        holders.len(),
        2,
        "the pre-window row and the re-check-in dedupe"
    );
    let first = holders.first().expect("two holders inside the window");
    assert_eq!(s_at(first, "holder_session"), Some("s-z"));
    assert_eq!(s_at(first, "name"), Some("Barnaby"));
    let second = holders.get(1).expect("two holders inside the window");
    assert_eq!(s_at(second, "holder_session"), Some("s-a"));
    assert_eq!(
        s_at(second, "name"),
        Some("Avery"),
        "the first label in the window wins"
    );
    assert!(holders_over_window(&events, None, "bad-ts").is_empty());
}

#[test]
fn stamp_reads_unknown_when_nothing_resolves() {
    let node = json!({"id": "x-aaaa", "cwd": "/nonexistent/no-such-dir", "type": "feature"});
    let stamp = stamp_at_close(&node, &[]);
    assert_eq!(s_at(&stamp, "name"), Some(UNKNOWN));
    assert_eq!(s_at(&stamp, "holder_session"), None);
    assert!(!is_stamped(&json!({})));
    assert!(is_stamped(&json!({"holding_lead": {"name": "x"}})));
    assert!(!is_stamped(&json!({"holding_lead": Value::Null})));
}
