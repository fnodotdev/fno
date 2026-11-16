//! The unplanned idea cap. The default bounds machine-filed idea growth in
//! each nearest-epic or project scope, while operator asks remain uncapped.
//! The cap warns, never refuses: a finding is never dropped for count
//! (guard-diet law). `idea_load` carries the `full` flag for readers that
//! want the over-cap state without scanning.

use crate::backlog::row_facts::RowFacts;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// One default for every machine. The 25 it replaced refused the 144th open
/// idea on a stock install while another machine ran 400 by config; a count
/// refusal dropped a real finding. A machine that wants a tighter cap sets
/// `backlog.max_open_ideas`.
pub const DEFAULT_MAX_OPEN_IDEAS: usize = 400;

pub fn configured_cap(graph: &Path) -> (Option<usize>, &'static str) {
    let dir = graph.parent().unwrap_or_else(|| Path::new(""));
    match crate::backlog_ready::backlog_config_int(dir, "max_open_ideas") {
        Some(0) => (None, "off"),
        Some(value) if value > 0 => (Some(value as usize), "config"),
        _ => (Some(DEFAULT_MAX_OPEN_IDEAS), "default"),
    }
}

fn capped_idea(row: &Value) -> bool {
    row.get("status").and_then(Value::as_str) == Some("idea")
        && row
            .get("plan_path")
            .map(|value| value.is_null() || value.as_str() == Some(""))
            .unwrap_or(true)
        && row.get("type").and_then(Value::as_str) != Some("epic")
        && row
            .get("group_slug")
            .map(|value| value.is_null() || value.as_str() == Some(""))
            .unwrap_or(true)
        && row.get("source_kind").and_then(Value::as_str) != Some("operator_request")
        && row.get("source_kind").and_then(Value::as_str) != Some("external_request")
}

fn scope_for(row: &Value, rows_by_id: &std::collections::HashMap<&str, &Value>) -> String {
    let project = row.get("project").and_then(Value::as_str).unwrap_or("");
    let mut current = row;
    for _ in 0..64 {
        let Some(parent) = current
            .get("parent")
            .and_then(Value::as_str)
            .filter(|parent| !parent.is_empty())
        else {
            break;
        };
        let Some(parent_row) = rows_by_id.get(parent).copied() else {
            break;
        };
        if parent_row.get("type").and_then(Value::as_str) == Some("epic") {
            return format!("epic:{parent}");
        }
        current = parent_row;
    }
    format!("project:{project}")
}

/// The over-cap warning for the first scope whose birth crossed the cap, or
/// `None` when the cap is off or no scope crossed. The node files either
/// way; the caller prints the warning.
pub fn enforce(pre: &[Value], post: &[Value], cap: Option<usize>) -> Option<String> {
    let cap = cap?;
    let pre_by_id = crate::graph_store::index_by_id(pre);
    let post_by_id = crate::graph_store::index_by_id(post);
    let mut births: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for row in post {
        if !capped_idea(row) {
            continue;
        }
        let scope = scope_for(row, &post_by_id);
        *counts.entry(scope.clone()).or_default() += 1;
        if let Some(id) = RowFacts::id_of(row) {
            if !pre_by_id.contains_key(id) {
                births.entry(scope).or_default().push(row);
            }
        }
    }
    for (scope, mut new_rows) in births {
        let Some(&open_ideas) = counts.get(&scope) else {
            continue;
        };
        if open_ideas <= cap {
            continue;
        }
        new_rows.sort_by_key(|row| RowFacts::id_of(row).unwrap_or(""));
        let row = new_rows[0];
        let id = RowFacts::id_of(row).unwrap_or("<missing-id>");
        let title: String = row
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(80)
            .collect();
        let mut oldest: Vec<&Value> = post
            .iter()
            .filter(|candidate| {
                capped_idea(candidate) && scope_for(candidate, &post_by_id) == scope
            })
            .collect();
        oldest.sort_by(|left, right| {
            left.get("created_at")
                .and_then(Value::as_str)
                .unwrap_or("\u{10ffff}")
                .cmp(
                    right
                        .get("created_at")
                        .and_then(Value::as_str)
                        .unwrap_or("\u{10ffff}"),
                )
                .then_with(|| {
                    RowFacts::id_of(left)
                        .unwrap_or("")
                        .cmp(RowFacts::id_of(right).unwrap_or(""))
                })
        });
        let oldest_ids = oldest
            .iter()
            .take(3)
            .filter_map(|candidate| RowFacts::id_of(candidate))
            .collect::<Vec<_>>()
            .join(", ");
        return Some(format!(
            "idea cap warning: filed '{id}' ('{title}') in {scope}, which now holds {open_ideas} unplanned ideas over the cap of {cap} (backlog.max_open_ideas). A finding is never refused for count. Free room by closing the oldest unplanned ideas in {scope}: {oldest_ids}. An operator ask files with --source-kind operator_request and is not capped."
        ));
    }
    None
}

pub fn idea_load(rows: &[Value], cap: Option<usize>) -> Vec<Value> {
    let by_id = crate::graph_store::index_by_id(rows);
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows {
        if capped_idea(row) {
            *counts.entry(scope_for(row, &by_id)).or_default() += 1;
        }
    }
    let mut load: Vec<(String, usize)> = counts.into_iter().collect();
    load.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    load.into_iter()
        .map(|(scope, open_ideas)| {
            serde_json::json!({
                "scope": scope,
                "open_ideas": open_ideas,
                "full": cap.is_some_and(|limit| open_ideas >= limit)
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn idea(id: &str, project: &str, created_at: &str) -> Value {
        json!({
            "id": id, "slug": id, "title": format!("title {id}"),
            "type": "feature", "status": "idea", "project": project,
            "created_at": created_at
        })
    }

    fn full_rows() -> Vec<Value> {
        (1..=25)
            .map(|i| {
                idea(
                    &format!("x-{i:02}"),
                    "p",
                    &format!("2026-01-{i:02}T00:00:00Z"),
                )
            })
            .collect()
    }

    fn operator_idea(id: &str, project: &str, created_at: &str) -> Value {
        let mut row = idea(id, project, created_at);
        row.as_object_mut()
            .expect("idea() builds an object")
            .insert("source_kind".to_string(), json!("operator_request"));
        row
    }

    #[test]
    fn idea_cap_warns_on_the_26th_birth_and_names_oldest_ids() {
        let pre = full_rows();
        let mut post = pre.clone();
        post.push(idea("x-26", "p", "2026-02-01T00:00:00Z"));
        let warning = enforce(&pre, &post, Some(25)).expect("over cap warns");
        assert!(warning.contains("idea cap warning:"), "{warning}");
        assert!(warning.contains("26 unplanned ideas"), "{warning}");
        assert!(warning.contains("never refused for count"), "{warning}");
        assert!(warning.contains("backlog.max_open_ideas"), "{warning}");
        assert!(warning.contains("x-01, x-02, x-03"), "{warning}");
    }

    #[test]
    fn idea_cap_stays_silent_under_the_cap_and_when_off() {
        let pre = full_rows();
        assert_eq!(enforce(&pre, &pre.clone(), Some(25)), None);
        let mut over = pre.clone();
        over.push(idea("x-26", "p", "2026-02-01T00:00:00Z"));
        assert_eq!(enforce(&pre, &over, None), None);
    }

    #[test]
    fn an_operator_ask_birth_never_warns_even_over_the_cap() {
        let pre = full_rows();
        let mut post = pre.clone();
        post.push(operator_idea("x-26", "p", "2026-02-01T00:00:00Z"));
        assert_eq!(enforce(&pre, &post, Some(25)), None);
    }

    #[test]
    fn the_default_cap_is_one_generous_number_for_every_machine() {
        assert_eq!(
            DEFAULT_MAX_OPEN_IDEAS, 400,
            "the stock install must file, not refuse: the 25 it replaced dropped findings"
        );
        assert_eq!(
            configured_cap(Path::new("/nonexistent-graph-parent")).0,
            Some(DEFAULT_MAX_OPEN_IDEAS)
        );
    }
}
