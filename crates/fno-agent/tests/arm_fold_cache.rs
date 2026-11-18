//! The arm-maxima watermark cache answers the same maxima as a full fold
//! and advances incrementally (integration: exercises the public store
//! surface end to end).

use fno_agent::runtime::event_store::{reduce_arm_ticks, sync};
use fno_agent::store_conn::open_write;
use serde_json::json;
use std::io::Write;
use std::path::Path;

fn append(path: &Path, rows: &[serde_json::Value]) {
    let mut fh = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    for row in rows {
        writeln!(fh, "{row}").unwrap();
    }
}

fn tick_row(ts: &str, arm: &str, acted: u64) -> serde_json::Value {
    json!({"ts": ts, "type": "control_plane_tick", "source": "daemon",
           "data": {"arm": arm, "acted": acted}})
}

fn ms_at(stamp: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(stamp)
        .unwrap()
        .timestamp_millis()
}

#[test]
fn arm_maxima_cache_matches_the_full_fold_and_advances() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    append(
        &live,
        &[
            tick_row("2026-11-18T01:00:00Z", "pr_idle", 1),
            tick_row("2026-11-18T01:05:00Z", "pr_idle", 0),
            tick_row("2026-11-18T01:02:00Z", "otlp", 1),
        ],
    );
    let store = sync(&live).unwrap().store;
    let since = ms_at("2026-11-17T00:00:00Z");
    let active_since = ms_at("2026-11-18T00:00:00Z");
    let reduce = || {
        reduce_arm_ticks(
            &live,
            "control_plane_tick",
            "pr_heal_tick",
            since,
            active_since,
            &["starved"],
            "*no-such-row*",
        )
        .unwrap()
    };
    let cold = reduce();
    assert_eq!(
        cold.max_ts,
        vec![
            ("otlp".to_string(), ms_at("2026-11-18T01:02:00Z")),
            ("pr_idle".to_string(), ms_at("2026-11-18T01:05:00Z")),
        ]
    );
    assert_eq!(cold.active_arms, vec!["otlp".to_string()]);
    // The second fold rides the cache and answers identically.
    let cached = reduce();
    assert_eq!(cached.max_ts, cold.max_ts);
    assert_eq!(cached.active_arms, cold.active_arms);
    // A row past the watermark merges without a full pass.
    append(&live, &[tick_row("2026-11-18T01:10:00Z", "otlp", 1)]);
    sync(&live).unwrap();
    let incremental = reduce();
    assert_eq!(
        incremental.max_ts,
        vec![
            ("otlp".to_string(), ms_at("2026-11-18T01:10:00Z")),
            ("pr_idle".to_string(), ms_at("2026-11-18T01:05:00Z")),
        ]
    );
    // A fresh cold fold over the same rows agrees with the incremental one.
    open_write(&store)
        .unwrap()
        .execute(
            "DELETE FROM events_meta WHERE key = 'arm_maxima_cache_v1'",
            [],
        )
        .unwrap();
    let full_again = reduce();
    assert_eq!(full_again.max_ts, incremental.max_ts);
    assert_eq!(full_again.active_arms, incremental.active_arms);
}
