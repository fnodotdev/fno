// The check-in `rate_limits` reading - provider_rate_limited rows across
// the beat's journals, via #[path] mod, so it shares lead_checkin's module
// scope. Quiet when nothing hit: a lead who was never rate-limited reads no
// line, and one who was reads it on the next beat.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;

/// One 24h lookback: wide enough to span a quiet weekend, narrow enough
/// that a burst from yesterday's stuck worker does not shadow today's.
const LOOKBACK_MS: i64 = 24 * 3600 * 1000;
/// Sessions named in the summary line; the hit count stays whole.
const MAX_SESSIONS: usize = 5;

pub(crate) fn reading(events_paths: &[PathBuf]) -> Result<Value, String> {
    let since = chrono::Utc::now().timestamp_millis() - LOOKBACK_MS;
    let query = crate::event_store::EventQuery {
        types: vec!["provider_rate_limited".to_string()],
        since_ms: Some(since),
        ..Default::default()
    };
    let mut rows = Vec::new();
    for path in events_paths {
        // Fail soft per journal, the way the usage export reads: a journal
        // that does not exist yet (fresh install) is an empty read, never a
        // READER FAILED line on every beat.
        let found = crate::event_store::query_events(path, &query).unwrap_or_default();
        rows.extend(found);
    }
    // One emitter writes one journal, but the beat can be handed overlapping
    // paths; the row id, not the path, decides.
    let mut seen = HashSet::new();
    rows.retain(|r| seen.insert(r.event_id.clone()));
    let hits = rows.len();
    if hits == 0 {
        return Ok(json!({ "hits": 0 }));
    }
    let mut sessions: Vec<String> = Vec::new();
    let mut nodes: Vec<String> = Vec::new();
    let mut statuses: std::collections::BTreeMap<i64, usize> = Default::default();
    let mut custom = 0usize;
    for row in &rows {
        let Ok(line) = serde_json::from_str::<Value>(&row.line) else {
            continue;
        };
        let data = line.get("data").cloned().unwrap_or(Value::Null);
        if let Some(s) = data.get("status").and_then(Value::as_i64) {
            *statuses.entry(s).or_default() += 1;
        }
        if data
            .get("custom_base_url")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            custom += 1;
        }
        if let Some(sid) = data.get("session_id").and_then(Value::as_str) {
            if !sessions.iter().any(|s| s == sid) {
                sessions.push(sid.to_string());
            }
        }
        if let Some(node) = data.get("node_id").and_then(Value::as_str) {
            if !nodes.iter().any(|n| n == node) {
                nodes.push(node.to_string());
            }
        }
    }
    sessions.truncate(MAX_SESSIONS);
    let status_word = statuses
        .iter()
        .map(|(s, n)| format!("{s} x{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut summary = format!(
        "{hits} in 24h ({status_word}; sessions: {})",
        if sessions.is_empty() {
            "-".to_string()
        } else {
            sessions.join(", ")
        }
    );
    if !nodes.is_empty() {
        summary.push_str(&format!("; nodes: {}", nodes.join(", ")));
    }
    if custom > 0 {
        summary.push_str(&format!(
            "; {custom} on a custom base URL route (that provider, not the Anthropic account)"
        ));
    }
    Ok(json!({ "hits": hits, "summary": summary }))
}
