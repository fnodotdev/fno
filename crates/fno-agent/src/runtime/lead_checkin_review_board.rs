//! The review-board reading: the slug a lead registered with
//! `fno backlog note <epic> "Review board: <slug>"`, or the plain statement
//! that it has none. D3482 requires a board for every lead, and the beat is
//! the one place a lead without one is told, because it prints a line on
//! every beat instead of staying silent.

use serde_json::{json, Value};

const PREFIX: &str = "Review board:";

/// The newest `Review board:` slug on the scope node or a direct child of it.
/// `Null` when no note names one.
pub(super) fn reading(rows: &[Value], scope: &str) -> Result<Value, String> {
    let text = |row: &Value, key: &str| row.get(key).and_then(Value::as_str).map(str::to_string);
    let mut newest: Option<(String, String)> = None;
    for row in rows {
        let on_scope = [text(row, "id"), text(row, "slug"), text(row, "parent")]
            .iter()
            .flatten()
            .any(|v| v == scope);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, parent: &str, notes: &[(&str, &str)]) -> Value {
        json!({
            "id": id,
            "parent": parent,
            "progress_notes": notes.iter().map(|(ts, text)| json!({"ts": ts, "text": text})).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn newest_slug_on_scope_or_child_wins_and_other_scopes_are_ignored() {
        let rows = vec![
            node("x-epic", "", &[("2026-11-01", "Review board: old")]),
            node("x-kid", "x-epic", &[("2026-11-02", "Review board: new")]),
            node("x-other", "", &[("2026-11-03", "Review board: elsewhere")]),
        ];
        let got = reading(&rows, "x-epic").unwrap();
        assert_eq!(got["slug"], "new");
        assert_eq!(reading(&rows, "x-none").unwrap()["slug"], Value::Null);
    }

    #[test]
    fn a_missing_board_prints_the_remedy_and_a_found_one_prints_the_slug() {
        let reading_of = |value| crate::lead_checkin::Reading::took("review_board", value);
        let none = lines(&[reading_of(json!({"slug": null}))]);
        assert!(none[0].starts_with("review_board: none: make one from the Gus or Lara Visual Log"));
        let some = lines(&[reading_of(json!({"slug": "abc"}))]);
        assert_eq!(some, vec!["review_board: abc".to_string()]);
    }
}
