//! The review-board reading: the slug a lead registered with
//! `fno backlog note <epic> "Review board: <slug>"`, or the plain statement
//! that it has none. D3482 requires a board for every lead, and the beat is
//! the one place a lead without one is told, because it prints a line on
//! every beat instead of staying silent.

use serde::Deserialize;
use serde_json::{json, Value};

const PREFIX: &str = "Review board:";

#[derive(Deserialize, Default)]
struct Row {
    id: Option<String>,
    slug: Option<String>,
    #[serde(default)]
    progress_notes: Vec<Note>,
}

#[derive(Deserialize, Default)]
struct Note {
    ts: Option<String>,
    text: Option<String>,
}

#[derive(Deserialize, Default)]
struct Board {
    slug: Option<String>,
}

/// The newest `Review board:` slug on the scope node or any node the scope
/// fold holds. `Null` when no note names one.
pub(super) fn reading(
    rows: &[Value],
    scope: &str,
    members: &std::collections::HashSet<String>,
) -> Value {
    let mut newest: Option<(String, String)> = None;
    for raw in rows {
        let row = Row::deserialize(raw).unwrap_or_default();
        let on_scope = [row.id.as_deref(), row.slug.as_deref()]
            .into_iter()
            .flatten()
            .any(|v| v == scope || members.contains(v));
        if !on_scope {
            continue;
        }
        for note in row.progress_notes {
            let Some(body) = note.text.filter(|t| t.starts_with(PREFIX)) else {
                continue;
            };
            let slug = body[PREFIX.len()..].trim().to_string();
            let ts = note.ts.unwrap_or_default();
            if !slug.is_empty() && newest.as_ref().is_none_or(|(at, _)| ts > *at) {
                newest = Some((ts, slug));
            }
        }
    }
    json!({ "slug": newest.map(|(_, slug)| slug) })
}

pub(super) fn lines(readings: &[crate::lead_checkin::Reading]) -> Vec<String> {
    let Some(reading) = readings.iter().find(|r| r.name == "review_board") else {
        return Vec::new();
    };
    if !reading.ok {
        return vec![format!("READER FAILED review_board: {}", reading.error)];
    }
    match Board::deserialize(&reading.value).unwrap_or_default().slug {
        Some(slug) => vec![format!("review_board: {slug}")],
        None => vec![
            "review_board: none: make one from the Gus or Lara Visual Log, then post it with `fno backlog note <epic> \"Review board: <slug>\"`"
                .to_string(),
        ],
    }
}
