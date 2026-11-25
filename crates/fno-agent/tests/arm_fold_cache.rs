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

/// Fixture stamps ride a base near now: the prune inside `sync` deletes
/// telemetry rows older than the 168h horizon, so fixed calendar dates
/// age out of the store about a week after they are written and the fold
/// answers empty. The offsets below keep the original shape: the acting
/// pr_idle beat at +23h sits before the +24h activity edge, and the two
/// newest beats land just past it.
fn stamp(base: chrono::DateTime<chrono::Utc>, hours: i64, minutes: i64) -> String {
    (base + chrono::Duration::hours(hours) + chrono::Duration::minutes(minutes))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn ms(base: chrono::DateTime<chrono::Utc>, hours: i64, minutes: i64) -> i64 {
    (base + chrono::Duration::hours(hours) + chrono::Duration::minutes(minutes)).timestamp_millis()
}

#[test]
fn arm_maxima_cache_matches_the_full_fold_and_advances() {
    let base = chrono::Utc::now() - chrono::Duration::hours(26);
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    append(
        &live,
        &[
            // pr_idle's acting tick sits before the activity edge: an
            // acted=1 beat marks the arm active only inside the window,
            // and a later acted=0 beat never retires it.
            tick_row(&stamp(base, 23, 0), "pr_idle", 1),
            tick_row(&stamp(base, 25, 5), "pr_idle", 0),
            tick_row(&stamp(base, 25, 2), "otlp", 1),
        ],
    );
    let store = sync(&live).unwrap().store;
    let since = base.timestamp_millis();
    let active_since = ms(base, 24, 0);
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
            ("otlp".to_string(), ms(base, 25, 2)),
            ("pr_idle".to_string(), ms(base, 25, 5)),
        ]
    );
    assert_eq!(cold.active_arms, vec!["otlp".to_string()]);
    // The second fold rides the cache and answers identically.
    let cached = reduce();
    assert_eq!(cached.max_ts, cold.max_ts);
    assert_eq!(cached.active_arms, cold.active_arms);
    // A row past the watermark merges without a full pass.
    append(&live, &[tick_row(&stamp(base, 25, 10), "otlp", 1)]);
    sync(&live).unwrap();
    let incremental = reduce();
    assert_eq!(
        incremental.max_ts,
        vec![
            ("otlp".to_string(), ms(base, 25, 10)),
            ("pr_idle".to_string(), ms(base, 25, 5)),
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
