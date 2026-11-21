// The check-in `rate_limits` reading - provider_rate_limited rows across
// the beat's journals, via #[path] mod, so it shares lead_checkin's module
// scope. Quiet when nothing hit: the reading answers null and the beat
// prints no line; one hit and the summary rides the next beat.

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;

/// One 24h lookback: wide enough to span a quiet weekend, narrow enough
/// that a burst from yesterday's stuck worker does not shadow today's.
const LOOKBACK_MS: i64 = 24 * 3600 * 1000;
/// Sessions named in the summary line; the hit count stays whole.
const MAX_SESSIONS: usize = 5;

/// One committed event row's envelope, parsed once (rust-style R1).
#[derive(Deserialize)]
struct RowLine {
    #[serde(default)]
    data: RowData,
}

#[derive(Deserialize, Default)]
struct RowData {
    status: Option<i64>,
    custom_base_url: Option<bool>,
    session_id: Option<String>,
    node_id: Option<String>,
}

/// The summary line for the 24h window, or null when it holds no row. The
/// caller renders the string and drops the null, so a clean fleet changes
/// the beat's shape by nothing.
pub(crate) fn reading(events_paths: &[PathBuf]) -> Value {
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
        rows.extend(crate::event_store::query_events(path, &query).unwrap_or_default());
    }
    // One emitter writes one journal, but the beat can be handed overlapping
    // paths; the row id, not the path, decides.
    let mut seen = HashSet::new();
    rows.retain(|r| seen.insert(r.event_id.clone()));
    if rows.is_empty() {
        return Value::Null;
    }
    let mut sessions: Vec<String> = Vec::new();
    let mut nodes: Vec<String> = Vec::new();
    let mut statuses: std::collections::BTreeMap<i64, usize> = Default::default();
    let mut custom = 0usize;
    for row in &rows {
        let Ok(line) = serde_json::from_str::<RowLine>(&row.line) else {
            continue;
        };
        if let Some(s) = line.data.status {
            *statuses.entry(s).or_default() += 1;
        }
        if line.data.custom_base_url == Some(true) {
            custom += 1;
        }
        if let Some(sid) = line.data.session_id {
            if !sessions.contains(&sid) {
                sessions.push(sid);
            }
        }
        if let Some(node) = line.data.node_id {
            if !nodes.contains(&node) {
                nodes.push(node);
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
        "{} in 24h ({}; sessions: {})",
        rows.len(),
        status_word,
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
    json!(summary)
}
