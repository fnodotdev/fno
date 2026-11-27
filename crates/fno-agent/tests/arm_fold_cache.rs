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

/// `control_plane_tick` is telemetry and expires after a week, so a fixed
/// fixture date goes stale and the fold reads nothing. Every stamp sits a
/// few hours before now instead.
fn stamp(base: chrono::DateTime<chrono::Utc>, minutes: i64) -> (String, i64) {
    let at = base + chrono::Duration::minutes(minutes);
    (
        at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        at.timestamp_millis(),
    )
}

#[test]
fn arm_maxima_cache_matches_the_full_fold_and_advances() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    // The activity edge sits two hours ago, on a whole second.
    let edge = chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() - 7200, 0).unwrap();
    let (_, since) = stamp(edge, -24 * 60);
    let (_, active_since) = stamp(edge, 0);
    let (pr_idle_acted, _) = stamp(edge, -60);
    let (pr_idle_last, pr_idle_ms) = stamp(edge, 65);
    let (otlp_first, otlp_first_ms) = stamp(edge, 62);
    let (otlp_next, otlp_next_ms) = stamp(edge, 70);
    append(
        &live,
        &[
            // pr_idle's acting tick sits before the activity edge: an
            // acted=1 beat marks the arm active only inside the window,
            // and a later acted=0 beat never retires it.
            tick_row(&pr_idle_acted, "pr_idle", 1),
            tick_row(&pr_idle_last, "pr_idle", 0),
            tick_row(&otlp_first, "otlp", 1),
        ],
    );
    let store = sync(&live).unwrap().store;
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
            ("otlp".to_string(), otlp_first_ms),
            ("pr_idle".to_string(), pr_idle_ms),
        ]
    );
    assert_eq!(cold.active_arms, vec!["otlp".to_string()]);
    // The second fold rides the cache and answers identically.
    let cached = reduce();
    assert_eq!(cached.max_ts, cold.max_ts);
    assert_eq!(cached.active_arms, cold.active_arms);
    // A row past the watermark merges without a full pass.
    append(&live, &[tick_row(&otlp_next, "otlp", 1)]);
    sync(&live).unwrap();
    let incremental = reduce();
    assert_eq!(
        incremental.max_ts,
        vec![
            ("otlp".to_string(), otlp_next_ms),
            ("pr_idle".to_string(), pr_idle_ms),
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
