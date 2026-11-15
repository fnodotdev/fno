//! `fno backlog reconcile --audit-contained`: the contained children that
//! closed before the cascade closed on a name only.
//!
//! Until the cascade read the owner PR's names alone, a surface match or a
//! shared PR number also closed a contained child. Those closes carry the
//! cascade's note and no proof the owner PR built the child. This pass lists
//! each one whose owner PR names it nowhere (body closure line or commit
//! message), and `--apply` reopens it with a note naming the owner PR.

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// One contained close with no naming evidence: (child, owner, owner PR).
pub(crate) type UnnamedClose = (String, String, i64);

fn text<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// The cascade's own closes whose owner PR names the child nowhere.
/// `names_by_owner` maps an owner id to every id its PR body and commits
/// name; an owner missing from the map was unreadable and lists nothing,
/// since an unread PR is no proof either way.
pub(crate) fn unnamed_contained_closes(
    entries: &[Value],
    names_by_owner: &BTreeMap<String, Vec<String>>,
) -> Vec<UnnamedClose> {
    let mut out = Vec::new();
    for row in entries {
        let (Some(id), Some(owner)) = (text(row, "id"), text(row, "contained_in")) else {
            continue;
        };
        if row.get("completed_at").is_none_or(Value::is_null) {
            continue;
        }
        let cascade_note = format!("auto-closed: shipped inside {owner} ");
        if !text(row, "completion_note").is_some_and(|n| n.starts_with(&cascade_note)) {
            continue;
        }
        let Some(names) = names_by_owner.get(owner) else {
            continue;
        };
        if names.iter().any(|n| n == id) {
            continue;
        }
        let pr = entries
            .iter()
            .find(|r| text(r, "id") == Some(owner))
            .and_then(|r| r.get("pr_number"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        out.push((id.to_string(), owner.to_string(), pr));
    }
    out
}

/// Every id the owner PR names: its body's closure line and its commit
/// messages, the same two reads the merge-time cascade folds. `None` when
/// either read fails.
fn names_in_pr(cwd: &Path, slug: &str, pr: i64) -> Option<Vec<String>> {
    let body = crate::pr_push::gh_api("gh", cwd, &format!("repos/{slug}/pulls/{pr}"), &[]).ok()?;
    let body: Value = serde_json::from_str(&body).ok()?;
    let mut names =
        crate::org_board::pr_closure::parse(body.get("body").and_then(Value::as_str).unwrap_or(""));
    let commits = crate::pr_push::gh_api(
        "gh",
        cwd,
        &format!("repos/{slug}/pulls/{pr}/commits?per_page=100"),
        &[],
    )
    .ok()?;
    let commits: Value = serde_json::from_str(&commits).ok()?;
    let messages: Vec<String> = commits
        .as_array()?
        .iter()
        .filter_map(|c| c.pointer("/commit/message").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    names.extend(super::drift_scan::ids_named_in_text(&messages));
    Some(names)
}

/// The door: read the graph, read each owner PR's names, print the unnamed
/// closes, and with `apply` reopen each one.
pub(crate) fn run(graph: &Path, apply: bool, json_out: bool) -> i32 {
    let entries = match crate::graph_store::read_rows(graph) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("reconcile --audit-contained: graph read failed: {e}");
            return 1;
        }
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut names_by_owner: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut unread: Vec<String> = Vec::new();
    let owners: std::collections::BTreeSet<&str> = entries
        .iter()
        .filter(|r| r.get("completed_at").is_some_and(|v| !v.is_null()))
        .filter_map(|r| text(r, "contained_in"))
        .collect();
    for owner in owners {
        let row = entries.iter().find(|r| text(r, "id") == Some(owner));
        let pr = row.and_then(|r| r.get("pr_number")).and_then(Value::as_i64);
        let slug = super::pr_link::repo_slug_from_url(row.and_then(|r| text(r, "pr_url")));
        match (pr, slug) {
            (Some(pr), Some(slug)) => match names_in_pr(&cwd, &slug, pr) {
                Some(names) => {
                    names_by_owner.insert(owner.to_string(), names);
                }
                None => unread.push(format!("{owner} (PR {slug}#{pr} unreadable)")),
            },
            _ => unread.push(format!("{owner} (no PR url)")),
        }
    }
    let found = unnamed_contained_closes(&entries, &names_by_owner);
    if json_out {
        let rows: Vec<Value> = found
            .iter()
            .map(|(c, o, p)| serde_json::json!({"node": c, "owner": o, "pr": p}))
            .collect();
        println!(
            "{}",
            serde_json::json!({"unnamed": rows, "unread": unread, "applied": apply})
        );
    } else {
        for (child, owner, pr) in &found {
            println!(
                "unnamed close: {child} closed inside {owner} (PR #{pr}), which never names it"
            );
        }
        for owner in &unread {
            println!("unread owner: {owner}; its children were not judged");
        }
        println!(
            "{} unnamed contained close(s){}",
            found.len(),
            if apply {
                "; reopening"
            } else {
                "; rerun with --apply to reopen them"
            }
        );
    }
    if !apply {
        return 0;
    }
    let mut failed = 0;
    for (child, owner, pr) in &found {
        let reason = format!(
            "audit: closed inside {owner} (PR #{pr}) on a surface match or shared PR ref; the PR never names it"
        );
        let tail = [child.clone(), "--reason".into(), reason, "--force".into()];
        if super::workflows::run_reopen(&tail) != 0 {
            failed += 1;
        }
    }
    i32::from(failed > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_cascade_closes_the_owner_pr_never_names_are_listed() {
        let note =
            "auto-closed: shipped inside x-owner (PR #9); cost and session are recorded on x-owner";
        let entries = vec![
            json!({"id": "x-owner", "pr_number": 9}),
            json!({"id": "x-surface", "contained_in": "x-owner",
                   "completed_at": "2026-10-01T00:00:00Z", "completion_note": note}),
            json!({"id": "x-named", "contained_in": "x-owner",
                   "completed_at": "2026-10-01T00:00:00Z", "completion_note": note}),
            json!({"id": "x-hand", "contained_in": "x-owner",
                   "completed_at": "2026-10-01T00:00:00Z", "completion_note": "done by hand"}),
            json!({"id": "x-open", "contained_in": "x-owner"}),
        ];
        let names: BTreeMap<String, Vec<String>> =
            [("x-owner".to_string(), vec!["x-named".to_string()])].into();
        assert_eq!(
            unnamed_contained_closes(&entries, &names),
            vec![("x-surface".to_string(), "x-owner".to_string(), 9)]
        );
        assert!(unnamed_contained_closes(&entries, &BTreeMap::new()).is_empty());
    }
}
