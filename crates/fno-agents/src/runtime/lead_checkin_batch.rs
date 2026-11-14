//! The check-in's batch groups: open, planless rows in one lead's scope that
//! read as one subject and could ship as one PR. The beat names each group
//! with the `fno backlog contain` command that folds it; it never folds.
//! Scoring is `relatedness::score_pair`, the scorer the filing-time dedup
//! already uses, so there is no second notion of "similar".
use serde_json::{json, Value};

use crate::backlog::relatedness::{score_pair, BATCH_MIN_SCORE};

/// The most groups one beat prints.
const MAX_GROUPS: usize = 3;

/// The most rows scored pairwise: the pair count grows with its square.
const POOL_CAP: usize = 30;

fn s<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
}

/// A row that may join a group: it has an id, no plan and no owner. A row
/// with a plan needs a re-plan to fold, and a contained row already rides an
/// owner.
fn joinable(row: &Value) -> bool {
    s(row, "id").is_some() && s(row, "plan_path").is_none() && s(row, "contained_in").is_none()
}

/// Groups around an anchor: the highest-priority unassigned row anchors, and
/// a row joins only when its own pair score with the anchor clears
/// [`BATCH_MIN_SCORE`]. A chain A~B, B~C never pulls C beside A. The epic
/// bonus stays off because every sibling shares the epic. Largest group
/// first, at most [`MAX_GROUPS`]. Each group is
/// `{owner, members, reason, command}`.
pub(crate) fn groups(rows: &[Value]) -> Vec<Value> {
    let mut pool: Vec<&Value> = rows.iter().filter(|r| joinable(r)).collect();
    pool.sort_by_key(|r| {
        (
            s(r, "priority").unwrap_or("p9").to_string(),
            s(r, "id").unwrap_or("").to_string(),
        )
    });
    pool.dedup_by_key(|r| s(r, "id").unwrap_or("").to_string());
    pool.truncate(POOL_CAP);
    let mut taken = vec![false; pool.len()];
    let mut out: Vec<(usize, Value)> = Vec::new();
    for a in 0..pool.len() {
        if taken[a] {
            continue;
        }
        let mut members: Vec<&str> = Vec::new();
        let mut reason = String::new();
        let mut best = 0.0;
        for b in (a + 1)..pool.len() {
            if taken[b] {
                continue;
            }
            let (score, why) = score_pair(pool[a], pool[b], false);
            if score >= BATCH_MIN_SCORE {
                taken[b] = true;
                members.push(s(pool[b], "id").unwrap_or(""));
                if score > best {
                    best = score;
                    reason = why;
                }
            }
        }
        if members.is_empty() {
            continue;
        }
        taken[a] = true;
        let owner = s(pool[a], "id").unwrap_or("");
        let command = format!("fno backlog contain {owner} {}", members.join(" "));
        out.push((
            members.len(),
            json!({ "owner": owner, "members": members, "reason": reason, "command": command }),
        ));
    }
    out.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    out.into_iter().take(MAX_GROUPS).map(|(_, g)| g).collect()
}

/// The `batch:` lines for a reading's groups.
pub(crate) fn render_lines(groups: &[Value]) -> Vec<String> {
    groups
        .iter()
        .map(|g| {
            let members: Vec<&str> = g["members"]
                .as_array()
                .map(|m| m.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            format!(
                "  batch: {} + {} ({}) -> {}",
                g["owner"].as_str().unwrap_or("-"),
                members.join(", "),
                g["reason"].as_str().unwrap_or("-"),
                g["command"].as_str().unwrap_or("-"),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, priority: &str, title: &str) -> Value {
        json!({"id": id, "priority": priority, "title": title, "parent": "x-epic"})
    }

    #[test]
    fn one_subject_forms_a_group_and_the_parent_epic_alone_forms_none() {
        let g = groups(&[
            row("x-aaaa", "p1", "lead check-in names idea children by id"),
            row("x-bbbb", "p2", "lead check-in idea children offer line"),
            row("x-cccc", "p1", "retry the websocket handshake"),
        ]);
        assert_eq!(g.len(), 1, "{g:?}");
        assert_eq!(g[0]["command"], "fno backlog contain x-aaaa x-bbbb");
        assert!(render_lines(&g)[0].starts_with("  batch: x-aaaa + x-bbbb"));
    }

    #[test]
    fn planned_and_contained_rows_never_join() {
        let mut planned = row("x-bbbb", "p2", "lead check-in idea children offer line");
        planned["plan_path"] = json!("/plans/x.md");
        let mut contained = row("x-cccc", "p2", "lead check-in idea children offer line");
        contained["contained_in"] = json!("x-owner");
        let rows = [
            row("x-aaaa", "p1", "lead check-in names idea children by id"),
            planned,
            contained,
        ];
        assert!(groups(&rows).is_empty());
    }
}
