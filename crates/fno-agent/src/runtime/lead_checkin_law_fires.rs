// The check-in `law_fires` reading - the laws that fired at an action (a
// pre-tool call or a door) in the beat's journals, via #[path] mod. A law
// fire at an action is a session about to do what a law speaks to, so the
// lead sees which laws its sessions keep walking into. Quiet when none
// fired: the reading answers null and the beat prints no line.

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

const LOOKBACK_MS: i64 = 24 * 3600 * 1000;
const MAX_LAWS: usize = 5;
const MAX_SESSIONS: usize = 3;

#[derive(Deserialize)]
struct SpanLine {
    #[serde(default)]
    data: SpanData,
}

#[derive(Deserialize, Default)]
struct SpanData {
    #[serde(default)]
    span_kind: String,
    #[serde(default)]
    event: String,
    law: Option<String>,
    #[serde(default)]
    trace: SpanTrace,
}

#[derive(Deserialize, Default)]
struct SpanTrace {
    actor_session: Option<String>,
}

pub(crate) fn reading(events_paths: &[PathBuf]) -> Value {
    reading_with(events_paths, &crate::numbers::labels())
}

/// The summary over the 24h window, laws by fire count, each named by its
/// short number when it has one.
fn reading_with(events_paths: &[PathBuf], labels: &HashMap<String, String>) -> Value {
    let query = crate::event_store::EventQuery {
        types: vec!["decision_span".to_string()],
        since_ms: Some(chrono::Utc::now().timestamp_millis() - LOOKBACK_MS),
        ..Default::default()
    };
    let mut seen = HashSet::new();
    let mut by_law: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
    for path in events_paths {
        for row in crate::event_store::query_events(path, &query).unwrap_or_default() {
            if !seen.insert(row.event_id.clone()) {
                continue;
            }
            let Ok(span) = serde_json::from_str::<SpanLine>(&row.line) else {
                continue;
            };
            let d = span.data;
            let at_action = d.event == "pre-tool" || d.event.starts_with("door:");
            let (true, true, Some(law)) = (d.span_kind == "rule", at_action, d.law) else {
                continue;
            };
            let entry = by_law.entry(law).or_default();
            entry.0 += 1;
            if let Some(sid) = d.trace.actor_session {
                let short: String = sid.chars().take(8).collect();
                if !entry.1.contains(&short) {
                    entry.1.push(short);
                }
            }
        }
    }
    if by_law.is_empty() {
        return Value::Null;
    }
    let mut laws: Vec<_> = by_law.into_iter().collect();
    laws.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    let parts: Vec<String> = laws
        .into_iter()
        .take(MAX_LAWS)
        .map(|(law, (n, mut sessions))| {
            let label = labels.get(&law).cloned().unwrap_or(law);
            sessions.truncate(MAX_SESSIONS);
            if sessions.is_empty() {
                format!("{label} x{n}")
            } else {
                format!("{label} x{n} (sessions {})", sessions.join(", "))
            }
        })
        .collect();
    json!(parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn law_fires_at_actions_count_per_law_and_a_quiet_window_is_null() {
        let td = tempfile::TempDir::new().unwrap();
        let journal = td.path().join("events.jsonl");
        assert_eq!(
            reading_with(&[journal.clone()], &HashMap::new()),
            Value::Null
        );
        let span = |law: Option<&str>, event: &str, sid: &str| {
            json!({
                "ts": chrono::Utc::now().to_rfc3339(), "type": "decision_span", "source": "target",
                "data": {"span_kind": "rule", "rule": "r", "law": law, "event": event,
                         "verdict": "warn", "trace": {"actor_session": sid}},
            })
            .to_string()
        };
        for line in [
            span(Some("d-e952ed19"), "door:send", "a1b2c3d4-0000"),
            span(Some("d-e952ed19"), "door:send", "a1b2c3d4-0000"),
            span(Some("d-e952ed19"), "prompt", "ffff0000-0000"),
            span(None, "pre-tool", "ffff0000-0000"),
        ] {
            crate::event_store::append_envelope(&journal, &line, None).unwrap();
        }
        let labels = HashMap::from([("d-e952ed19".to_string(), "D1051".to_string())]);
        assert_eq!(
            reading_with(&[journal], &labels),
            json!("D1051 x2 (sessions a1b2c3d4)"),
            "a prompt fire and a law-less rule are not law fires at an action"
        );
    }
}
