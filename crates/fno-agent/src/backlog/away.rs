//! The away report: what finished while the user was gone and what waits on them.
//!
//! A pure fold over rows `day` already reads: graph rows, the decision
//! journal and the open operator questions. It adds no store of its own.

use chrono::{DateTime, FixedOffset};
use serde_json::{json, Value};

use super::outstanding::Question;

const ITEM_CAP: usize = 25;

fn in_window(raw: Option<&str>, from: DateTime<FixedOffset>, to: DateTime<FixedOffset>) -> bool {
    raw.and_then(|r| DateTime::parse_from_rfc3339(r).ok())
        .is_some_and(|ts| ts >= from && ts < to)
}

fn text<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

pub fn fold_away(
    entries: &[Value],
    decisions_raw: &str,
    asks: &[Question],
    from: DateTime<FixedOffset>,
    to: DateTime<FixedOffset>,
) -> Value {
    let mut closed = Vec::new();
    let mut merged = Vec::new();
    let mut started = Vec::new();
    for entry in entries {
        let Some(id) = text(entry, "id") else {
            continue;
        };
        let title = text(entry, "title").unwrap_or("");
        if in_window(text(entry, "completed_at"), from, to) {
            let pr = entry.pointer("/pr_number").and_then(Value::as_i64);
            closed.push(json!({"node": id, "title": title, "project": text(entry, "project"), "pr_number": pr}));
            if let Some(pr) = pr.filter(|_| text(entry, "merge_status") == Some("merged")) {
                merged.push(json!({"pr_number": pr, "pr_url": text(entry, "pr_url"), "node": id, "title": title}));
            }
        }
        let began = entry
            .pointer("/sessions")
            .and_then(Value::as_array)
            .is_some_and(|sessions| {
                sessions
                    .iter()
                    .any(|s| in_window(text(s, "started_at"), from, to))
            });
        if began {
            started.push(json!({"node": id, "title": title, "status": text(entry, "status")}));
        }
    }
    let mut rulings = Vec::new();
    for line in decisions_raw.lines() {
        let Ok(row) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if text(&row, "type") != Some("operator_decision") || !in_window(text(&row, "ts"), from, to)
        {
            continue;
        }
        let Some(data) = row.pointer("/data") else {
            continue;
        };
        rulings.push(json!({
            "id": text(data, "decision_id"),
            "subject": text(data, "subject"),
            "decision": text(data, "decision"),
            "by": text(data, "authority_source"),
        }));
    }
    let waiting: Vec<Value> = asks
        .iter()
        .map(|q| {
            json!({
                "id": q.id,
                "node": q.node,
                "asked_at": q.ts,
                "question": q.question,
                "action": q.ask,
                "options": q.options,
            })
        })
        .collect();
    json!({
        "closed": closed,
        "merged": merged,
        "started": started,
        "rulings": rulings,
        "waiting": waiting,
    })
}

fn section(out: &mut Vec<String>, title: &str, rows: &[Value], line: impl Fn(&Value) -> String) {
    out.push(format!("## {title} ({})", rows.len()));
    if rows.is_empty() {
        out.push("- none".to_string());
    }
    for row in rows.iter().take(ITEM_CAP) {
        out.push(format!("- {}", line(row)));
    }
    if rows.len() > ITEM_CAP {
        out.push(format!(
            "- and {} more (use --json for all)",
            rows.len() - ITEM_CAP
        ));
    }
}

fn s<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

pub fn render(payload: &Value) -> String {
    let away = &payload["away"];
    let list = |key: &str| away[key].as_array().cloned().unwrap_or_default();
    let mut out = vec![format!(
        "# Away report: {} to {}",
        payload
            .pointer("/window/from")
            .and_then(Value::as_str)
            .unwrap_or(""),
        payload
            .pointer("/window/to")
            .and_then(Value::as_str)
            .unwrap_or("")
    )];
    section(&mut out, "Waiting on you", &list("waiting"), |q| {
        let action = if s(q, "action").is_empty() {
            s(q, "question")
        } else {
            s(q, "action")
        };
        let options = q["options"].as_array().map(|o| {
            o.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" | ")
        });
        match options.filter(|o| !o.is_empty()) {
            Some(o) => format!("{} {}: {action} [{o}]", s(q, "id"), s(q, "node")),
            None => format!("{} {}: {action}", s(q, "id"), s(q, "node")),
        }
    });
    section(&mut out, "PRs merged", &list("merged"), |r| {
        format!("#{} {} ({})", r["pr_number"], s(r, "title"), s(r, "node"))
    });
    section(&mut out, "Nodes closed", &list("closed"), |r| {
        format!("{} {}", s(r, "node"), s(r, "title"))
    });
    section(&mut out, "Rulings", &list("rulings"), |r| {
        format!("{} {}: {}", s(r, "id"), s(r, "subject"), s(r, "decision"))
    });
    section(&mut out, "Work started", &list("started"), |r| {
        format!("{} {} [{}]", s(r, "node"), s(r, "title"), s(r, "status"))
    });
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_keeps_window_rows_and_the_exact_ask() {
        let at = |raw: &str| DateTime::parse_from_rfc3339(raw).unwrap();
        let entries = vec![
            json!({"id":"x-1","title":"in","completed_at":"2026-11-26T03:00:00Z","pr_number":7,"pr_url":"u","merge_status":"merged"}),
            json!({"id":"x-2","title":"old","completed_at":"2026-11-20T03:00:00Z","pr_number":6}),
            json!({"id":"x-3","title":"begun","status":"in_progress","sessions":[{"started_at":"2026-11-26T04:00:00Z"}]}),
        ];
        let ask = Question {
            id: "q-1".into(),
            ask: Some("run fno do pr merge 9".into()),
            ..Question::default()
        };
        let decisions = r#"{"type":"operator_decision","ts":"2026-11-26T05:00:00Z","data":{"decision_id":"D1","subject":"x-1","decision":"ship"}}"#;
        let out = fold_away(
            &entries,
            decisions,
            &[ask],
            at("2026-11-26T00:00:00Z"),
            at("2026-11-26T09:00:00Z"),
        );
        assert_eq!(
            out.pointer("/merged")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(out.pointer("/closed/0/node"), Some(&json!("x-1")));
        assert_eq!(out.pointer("/started/0/node"), Some(&json!("x-3")));
        assert_eq!(out.pointer("/rulings/0/id"), Some(&json!("D1")));
        assert_eq!(
            out.pointer("/waiting/0/action"),
            Some(&json!("run fno do pr merge 9"))
        );
    }
}
