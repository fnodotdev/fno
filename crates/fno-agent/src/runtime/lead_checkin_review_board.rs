//! The review-board reading: the slug a lead registered with
//! `fno backlog note <epic> "Review board: <slug>"`, or the plain statement
//! that it has none. D3482 requires a board for every lead, and the beat is
//! the one place a lead without one is told, because it prints a line on
//! every beat instead of staying silent.

use serde_json::{json, Value};

const PREFIX: &str = "Review board:";

/// The newest `Review board:` slug on the scope node or any node the scope
/// fold holds. `Null` when no note names one.
pub(super) fn reading(
    rows: &[Value],
    scope: &str,
    members: &std::collections::HashSet<String>,
) -> Result<Value, String> {
    let text = |row: &Value, key: &str| row.get(key).and_then(Value::as_str).map(str::to_string);
    let mut newest: Option<(String, String)> = None;
    for row in rows {
        let on_scope = [text(row, "id"), text(row, "slug")]
            .iter()
            .flatten()
            .any(|v| v == scope || members.contains(v));
        if !on_scope {
            continue;
        }
        let Some(notes) = row.get("progress_notes").and_then(Value::as_array) else {
            continue;
        };
        for note in notes {
            let Some(body) = text(note, "text").filter(|t| t.starts_with(PREFIX)) else {
                continue;
            };
            let slug = body[PREFIX.len()..].trim().to_string();
            let ts = text(note, "ts").unwrap_or_default();
            if !slug.is_empty() && newest.as_ref().is_none_or(|(at, _)| ts > *at) {
                newest = Some((ts, slug));
            }
        }
    }
    Ok(json!({ "slug": newest.map(|(_, slug)| slug) }))
}

pub(super) fn lines(readings: &[crate::lead_checkin::Reading]) -> Vec<String> {
    let Some(reading) = readings.iter().find(|r| r.name == "review_board") else {
        return Vec::new();
    };
    if !reading.ok {
        return vec![format!("READER FAILED review_board: {}", reading.error)];
    }
    match reading.value.get("slug").and_then(Value::as_str) {
        Some(slug) => vec![format!("review_board: {slug}")],
        None => vec![
            "review_board: none: make one from the Gus or Lara Visual Log, then post it with `fno backlog note <epic> \"Review board: <slug>\"`"
                .to_string(),
        ],
    }
}
