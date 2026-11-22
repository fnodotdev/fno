//! PR claim binding: validate and bind a PR's closure claims as one
//! graph-owned mutation, ported from graph/_reconcile.py. Validation
//! completes before any row changes, so unknown or cross-repository claims
//! cannot leave a partial binding behind.

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

use super::merge_evidence::{blocking_pr_refs, node_pr_refs, primary_docs_twin_number};
use super::node_ref::find_node_index;
use super::pr_link::repo_slug_from_url;
use crate::backlog_ready::node_is_open;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PrRowBinding {
    pub node_id: String,
    /// filled_primary | appended_additional | already_bound | already_done
    /// | released
    pub action: &'static str,
}

#[derive(Debug, Clone)]
pub(crate) struct PrRowBindResult {
    pub outcome: &'static str,
    pub bindings: Vec<PrRowBinding>,
    pub refusal: Option<String>,
}

impl PrRowBindResult {
    fn refused(refusal: impl Into<String>) -> Self {
        Self {
            outcome: "refused",
            bindings: Vec::new(),
            refusal: Some(refusal.into()),
        }
    }

    /// The ids this bind actually bound.
    pub fn bound_ids(&self) -> Vec<String> {
        self.bindings
            .iter()
            .filter(|b| matches!(b.action, "filled_primary" | "appended_additional"))
            .map(|b| b.node_id.clone())
            .collect()
    }
}

/// Validate and bind every PR claim as one mutation. Callers may run this
/// against a copy for a dry-run or under the graph lock for persistence.
///
/// `open_pr` marks a claim from an open-binding heal (the PR is open on
/// GitHub). A finished node then records it in `additional_prs`, the same
/// state `--add-pr` writes by hand, instead of answering `already_done`.
/// A closing PR's claim still skips: closure never re-binds a done row.
pub(crate) fn bind_pr_rows(
    entries: &mut [Value],
    claimed_ids: &[String],
    pr_number: i64,
    pr_url: Option<&str>,
    repo: Option<&str>,
    rebind: bool,
    open_pr: bool,
    aliases: &BTreeMap<String, String>,
) -> PrRowBindResult {
    if claimed_ids.is_empty() {
        return PrRowBindResult::refused("no closure claims to bind");
    }
    let our_repo = repo
        .map(str::to_string)
        .or_else(|| repo_slug_from_url(pr_url));
    // Validation pass first: the same rows pass two would find.
    let mut resolved: Vec<usize> = Vec::with_capacity(claimed_ids.len());
    for nid in claimed_ids {
        let Some(index) = find_node_index(entries, nid) else {
            return PrRowBindResult::refused(format!("unknown node: {nid}"));
        };
        if let Some(our) = &our_repo {
            for (number, url) in blocking_pr_refs(&entries[index]) {
                let Some(existing_repo) = repo_slug_from_url(url.as_deref()) else {
                    return PrRowBindResult::refused(format!(
                        "{nid} already carries a PR #{number} ref with no \
                         resolvable repo; this PR is {our} - refusing an \
                         unverifiable cross-repo claim"
                    ));
                };
                if !crate::backlog::pr_link::same_repo_slug(&existing_repo, our, aliases) {
                    if primary_docs_twin_number(&entries[index]) == Some(number) {
                        // The twin won the primary slot (the state the 404
                        // hint once produced): the shape is broken, and the
                        // sanctioned repair demotes it. The refusal names
                        // that door instead of a bare cross-repo claim.
                        return PrRowBindResult::refused(format!(
                            "{nid} carries docs PR #{number} in its primary \
                             slot; run `fno backlog reconcile --node {nid} \
                             --repair-docs-twin` to demote it - refusing a \
                             docs-twin primary"
                        ));
                    }
                    return PrRowBindResult::refused(format!(
                        "{nid} already carries a {existing_repo} PR ref; \
                         this PR is {our} - refusing a cross-repo claim"
                    ));
                }
            }
        } else if !blocking_pr_refs(&entries[index]).is_empty() {
            return PrRowBindResult::refused(format!(
                "{nid} already carries a PR ref and this PR's repo is \
                 unresolvable - refusing an unscoped claim"
            ));
        }
        resolved.push(index);
    }

    // Mutation pass, claim order kept.
    let claimed: BTreeSet<&str> = claimed_ids.iter().map(String::as_str).collect();
    let mut bindings: Vec<PrRowBinding> = Vec::with_capacity(resolved.len());
    for (nid, index) in claimed_ids.iter().zip(resolved) {
        let node = &mut entries[index];
        // The claim line predates the release: skip it, bind the rest.
        let released_from = node
            .get("released_from")
            .and_then(Value::as_str)
            .map(|r| claimed.contains(r))
            .unwrap_or(false);
        let contained_in = node
            .get("contained_in")
            .map(|v| !v.is_null())
            .unwrap_or(false);
        if released_from && !contained_in {
            bindings.push(PrRowBinding {
                node_id: nid.clone(),
                action: "released",
            });
            continue;
        }
        let refs = node_pr_refs(node);
        let primary_absent = !node.get("pr_number").map(Value::is_i64).unwrap_or(false);
        let bound = refs.iter().any(|(number, _)| *number == pr_number);
        let action = if bound && rebind && primary_absent {
            // A closed PR reopened after the closed-PR leg moved it aside.
            let obj = node.as_object_mut().expect("graph rows are objects");
            if let Some(rows) = obj.get_mut("additional_prs").and_then(Value::as_array_mut) {
                rows.retain(|r| r.get("number").and_then(Value::as_i64) != Some(pr_number));
            }
            obj.insert("pr_number".into(), json!(pr_number));
            obj.insert("pr_url".into(), json!(pr_url));
            "filled_primary"
        } else if bound {
            "already_bound"
        } else if !node_is_open(node) {
            // A finished row keeps its primary untouched: the follow-up
            // lands in additional_prs, where the openness machinery judges
            // whether it still holds a worker.
            if open_pr {
                let obj = node.as_object_mut().expect("graph rows are objects");
                let mut additional: Vec<Value> = obj
                    .get("additional_prs")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                additional.push(json!({"number": pr_number, "url": pr_url}));
                obj.insert("additional_prs".into(), Value::Array(additional));
                "appended_additional"
            } else {
                "already_done"
            }
        } else if rebind && node.get("pr_number").map(Value::is_i64).unwrap_or(false) {
            let obj = node.as_object_mut().expect("graph rows are objects");
            let old = json!({
                "number": obj.get("pr_number").cloned().unwrap_or(Value::Null),
                "url": obj.get("pr_url").cloned().unwrap_or(Value::Null),
            });
            let mut additional = vec![old];
            additional.extend(
                obj.get("additional_prs")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
            obj.insert("additional_prs".into(), Value::Array(additional));
            obj.insert("pr_number".into(), json!(pr_number));
            obj.insert("pr_url".into(), json!(pr_url));
            obj.insert("merge_status".into(), Value::Null);
            "appended_additional"
        } else if !node.get("pr_number").map(Value::is_i64).unwrap_or(false) {
            let obj = node.as_object_mut().expect("graph rows are objects");
            obj.insert("pr_number".into(), json!(pr_number));
            obj.insert("pr_url".into(), json!(pr_url));
            "filled_primary"
        } else {
            let obj = node.as_object_mut().expect("graph rows are objects");
            let mut additional: Vec<Value> = obj
                .get("additional_prs")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            additional.push(json!({"number": pr_number, "url": pr_url}));
            obj.insert("additional_prs".into(), Value::Array(additional));
            "appended_additional"
        };
        bindings.push(PrRowBinding {
            node_id: nid.clone(),
            action,
        });
    }

    PrRowBindResult {
        outcome: "bound",
        bindings,
        refusal: None,
    }
}

/// Does `entry`'s own `cwd` sit inside `our_root` (or is it missing)?
///
/// Full contract: docs/architecture/backlog-graph-command-contracts.md
pub(crate) fn node_cwd_in_repo(entry: &Value, our_root: &str) -> bool {
    let Some(raw_cwd) = entry.get("cwd").and_then(Value::as_str) else {
        return true;
    };
    if raw_cwd.is_empty() {
        return true;
    }
    let expanded = expanduser(raw_cwd);
    let norm = normalize_path(&expanded);
    let root_norm = normalize_path(our_root);
    let prefix = format!("{}/", root_norm.trim_end_matches('/'));
    norm == root_norm || norm.starts_with(&prefix)
}

/// `os.path.expanduser` for the shapes a node cwd carries: a leading `~`
/// or `~/`. An unmatched `~user` stays verbatim, as the Python twin's.
fn expanduser(path: &str) -> String {
    if path == "~" {
        return std::env::var("HOME").unwrap_or_else(|_| path.to_string());
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    path.to_string()
}

/// Lexical normalization (`os.path.normpath` for the prefix shapes this
/// comparison sees): `.` and `..` resolved, duplicate slashes collapsed,
/// no filesystem access.
fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    if path.starts_with('/') {
        format!("/{joined}")
    } else {
        joined
    }
}

/// The docs-twin repair outcome: `repaired` carries what moved, `refused`
/// the reason. The refusal-in-struct shape `PrRowBindResult` uses.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DocsTwinRepair {
    pub action: &'static str, // repaired | refused
    pub detail: Option<String>,
}

/// Demote a docs twin from a node's primary slot: clear `pr_number`,
/// `pr_url` and `merge_status`, leaving the twin in `additional_prs` with
/// note "docs". The primary number sitting in the docs-noted set is the
/// proof the primary IS the twin; any other primary refuses, and so does a
/// primary twin with no additional row to prove it (nothing there says the
/// primary was docs). Validation first, one mutation, like `bind_pr_rows`.
pub(crate) fn repair_docs_twin_primary(entries: &mut [Value], node_id: &str) -> DocsTwinRepair {
    let refused = |why: String| DocsTwinRepair {
        action: "refused",
        detail: Some(why),
    };
    let Some(index) = find_node_index(entries, node_id) else {
        return refused(format!("unknown node: {node_id}"));
    };
    let node = &mut entries[index];
    let Some(primary) = primary_docs_twin_number(node) else {
        return refused(format!(
            "{node_id}'s primary is not a proven docs twin: no primary PR, \
             or no additional_prs row names it with note docs at a repo the \
             aliases join"
        ));
    };
    let obj = node.as_object_mut().expect("graph rows are objects");
    obj.remove("pr_number");
    obj.remove("pr_url");
    obj.remove("merge_status");
    DocsTwinRepair {
        action: "repaired",
        detail: Some(format!(
            "{node_id}: primary docs PR #{primary} demoted; the twin stays \
             in additional_prs with note docs"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(extra: Value) -> Value {
        let mut base = json!({"id": "x-aaaa"});
        if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                b.insert(k.clone(), v.clone());
            }
        }
        base
    }

    #[test]
    fn an_unknown_claim_refuses_before_any_row_changes() {
        let mut entries = vec![node(json!({}))];
        let claimed = vec!["x-aaaa".to_string(), "x-missing".to_string()];
        let result = bind_pr_rows(
            &mut entries,
            &claimed,
            7,
            None,
            None,
            false,
            false,
            &BTreeMap::new(),
        );
        assert_eq!(result.outcome, "refused");
        assert_eq!(result.refusal.as_deref(), Some("unknown node: x-missing"));
        assert!(entries[0]["pr_number"].is_null(), "no partial binding");
    }

    #[test]
    fn a_primary_fill_and_an_additional_append_bind_together() {
        let mut entries = vec![
            node(json!({"id": "x-aaaa"})),
            node(
                json!({"id": "x-bbbb", "pr_number": 2, "pr_url": "https://github.com/o/r/pull/2"}),
            ),
        ];
        let claimed = vec!["x-aaaa".to_string(), "x-bbbb".to_string()];
        let result = bind_pr_rows(
            &mut entries,
            &claimed,
            7,
            Some("https://github.com/o/r/pull/7"),
            Some("o/r"),
            false,
            false,
            &BTreeMap::new(),
        );
        assert_eq!(result.outcome, "bound", "{:?}", result.refusal);
        assert_eq!(result.bindings[0].action, "filled_primary");
        assert_eq!(result.bindings[1].action, "appended_additional");
        assert_eq!(result.bound_ids(), vec!["x-aaaa", "x-bbbb"]);
        assert_eq!(entries[0]["pr_number"], 7);
        let additional = &entries[1]["additional_prs"];
        assert_eq!(additional[0]["number"], 7);
        // The pre-existing primary stays put.
        assert_eq!(entries[1]["pr_number"], 2);
    }

    #[test]
    fn an_open_pr_on_a_finished_node_records_as_additional() {
        let mut entries = vec![node(json!({
            "id": "x-aaaa",
            "status": "done",
            "completed_at": "2026-09-12T00:00:00Z",
            "pr_number": 1720,
            "pr_url": "https://github.com/o/r/pull/1720",
            "merge_status": "merged",
        }))];
        let claimed = vec!["x-aaaa".to_string()];
        let result = bind_pr_rows(
            &mut entries,
            &claimed,
            1813,
            Some("https://github.com/o/r/pull/1813"),
            Some("o/r"),
            false,
            true,
            &BTreeMap::new(),
        );
        assert_eq!(result.outcome, "bound", "{:?}", result.refusal);
        assert_eq!(result.bindings[0].action, "appended_additional");
        // The finished row's primary and closure facts stay untouched.
        assert_eq!(entries[0]["pr_number"], 1720);
        assert_eq!(entries[0]["merge_status"], "merged");
        let additional = &entries[0]["additional_prs"];
        assert_eq!(additional[0]["number"], 1813);
        assert_eq!(additional[0]["url"], "https://github.com/o/r/pull/1813");
    }

    #[test]
    fn a_closing_claim_on_a_finished_node_still_skips() {
        let mut entries = vec![node(json!({
            "id": "x-aaaa",
            "status": "done",
            "completed_at": "2026-09-12T00:00:00Z",
            "pr_number": 1720,
            "pr_url": "https://github.com/o/r/pull/1720",
        }))];
        let claimed = vec!["x-aaaa".to_string()];
        let result = bind_pr_rows(
            &mut entries,
            &claimed,
            1813,
            Some("https://github.com/o/r/pull/1813"),
            Some("o/r"),
            false,
            false,
            &BTreeMap::new(),
        );
        assert_eq!(result.outcome, "bound");
        assert_eq!(result.bindings[0].action, "already_done");
        assert!(result.bound_ids().is_empty());
        assert!(entries[0].get("additional_prs").is_none(), "no write");
        assert_eq!(entries[0]["pr_number"], 1720);
    }

    #[test]
    fn a_docs_noted_foreign_ref_does_not_block_the_bind() {
        // The twin the docs step recorded (note "Docs", case-insensitive on
        // purpose) is an ancillary companion, never a claim about the
        // delivery PR: the bind proceeds and the row survives untouched.
        let mut entries = vec![node(json!({
            "id": "x-aaaa",
            "additional_prs": [
                {"number": 9, "url": "https://github.com/other/repo/pull/9", "note": "Docs"},
            ],
        }))];
        let claimed = vec!["x-aaaa".to_string()];
        let result = bind_pr_rows(
            &mut entries,
            &claimed,
            7,
            Some("https://github.com/o/r/pull/7"),
            Some("o/r"),
            false,
            false,
            &BTreeMap::new(),
        );
        assert_eq!(result.outcome, "bound", "{:?}", result.refusal);
        assert_eq!(result.bindings[0].action, "filled_primary");
        let first = entries.first().expect("one row");
        assert_eq!(first["pr_number"], 7);
        let rows = first["additional_prs"].as_array().expect("the twin kept");
        assert_eq!(rows.len(), 1);
        let twin = rows.first().expect("the twin row");
        assert_eq!(twin["number"], 9);
        assert_eq!(twin["note"], "Docs");
    }

    #[test]
    fn a_folded_or_unnoted_foreign_ref_still_refuses() {
        for note in [Some("folded"), None] {
            let mut extra = json!({"number": 9, "url": "https://github.com/other/repo/pull/9"});
            if let Some(note) = note {
                extra["note"] = json!(note);
            }
            let mut entries = vec![node(json!({"id": "x-aaaa", "additional_prs": [extra]}))];
            let claimed = vec!["x-aaaa".to_string()];
            let result = bind_pr_rows(
                &mut entries,
                &claimed,
                7,
                Some("https://github.com/o/r/pull/7"),
                Some("o/r"),
                false,
                false,
                &BTreeMap::new(),
            );
            assert_eq!(result.outcome, "refused", "note {note:?} must refuse");
            assert_eq!(
                result.refusal.as_deref(),
                Some("x-aaaa already carries a other/repo PR ref; this PR is o/r - refusing a cross-repo claim")
            );
            let first = entries.first().expect("one row");
            assert!(first["pr_number"].is_null(), "no partial binding");
        }
    }

    #[test]
    fn a_docs_twin_in_the_primary_slot_refuses_and_names_the_repair() {
        // The 404-hint state: the twin won the primary. The binder cannot
        // prove the primary is docs from shape alone, so it refuses and
        // names the sanctioned repair instead of binding over it.
        let mut entries = vec![node(json!({
            "id": "x-aaaa",
            "pr_number": 288,
            "pr_url": "https://github.com/fnodotdev/fno-web/pull/288",
            "additional_prs": [
                {"number": 288, "url": "https://github.com/fnodotdev/fno-web/pull/288", "note": "docs"},
            ],
        }))];
        let claimed = vec!["x-aaaa".to_string()];
        let result = bind_pr_rows(
            &mut entries,
            &claimed,
            548,
            Some("https://github.com/fnodotdev/fno/pull/548"),
            Some("fnodotdev/fno"),
            false,
            false,
            &BTreeMap::new(),
        );
        assert_eq!(result.outcome, "refused");
        let refusal = result.refusal.as_deref().unwrap_or("");
        assert!(refusal.contains("--repair-docs-twin"), "{refusal}");
        assert!(refusal.contains("docs-twin primary"), "{refusal}");
        let first = entries.first().expect("one row");
        assert_eq!(first["pr_number"], 288, "no partial binding");
    }

    #[test]
    fn the_docs_twin_repair_demotes_a_twin_primary_and_refuses_the_rest() {
        // Happy arm: the primary number sits in additional_prs with note
        // docs, so the demotion clears the primary keys and keeps the twin.
        let mut entries = vec![node(json!({
            "id": "x-aaaa",
            "pr_number": 288,
            "pr_url": "https://github.com/fnodotdev/fno-web/pull/288",
            "additional_prs": [
                {"number": 288, "url": "https://github.com/fnodotdev/fno-web/pull/288", "note": "docs"},
            ],
        }))];
        let result = repair_docs_twin_primary(&mut entries, "x-aaaa");
        assert_eq!(result.action, "repaired");
        let first = entries.first().expect("one row");
        assert!(first["pr_number"].is_null());
        assert!(first["pr_url"].is_null());
        assert!(first["merge_status"].is_null());
        let rows = first["additional_prs"].as_array().expect("the twin kept");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows.first().expect("the twin row")["note"], "docs");
    }

    #[test]
    fn the_repair_proves_a_twin_across_the_transfer_alias() {
        // The exact row the incident left: the primary re-pointed to the
        // transferred docs repo's live name, the twin still carrying the
        // recorded pre-transfer name. The alias map joins the two names,
        // so the proof holds and the repair proceeds.
        let mut entries = vec![node(json!({
            "id": "x-aaaa",
            "pr_number": 288,
            "pr_url": "https://github.com/footnote-sh/fno-web/pull/288",
            "additional_prs": [
                {"number": 288, "url": "https://github.com/fnodotdev/fno-web/pull/288", "note": "docs"},
            ],
        }))];
        let result = repair_docs_twin_primary(&mut entries, "x-aaaa");
        assert_eq!(result.action, "repaired");
        let first = entries.first().expect("one row");
        assert!(first["pr_number"].is_null());
        assert!(first["pr_url"].is_null());
        let rows = first["additional_prs"].as_array().expect("the twin kept");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows.first().expect("the twin row")["note"], "docs");
    }

    #[test]
    fn the_repair_refuses_a_code_primary_and_a_twinless_docs_primary() {
        let cases = [
            (
                json!({
                    "id": "x-aaaa",
                    "pr_number": 548,
                    "pr_url": "https://github.com/fnodotdev/fno/pull/548",
                }),
                "not a proven docs twin",
            ),
            (
                json!({
                    "id": "x-aaaa",
                    "pr_number": 288,
                    "pr_url": "https://github.com/fnodotdev/fno-web/pull/288",
                }),
                "no additional_prs row names it",
            ),
            (
                // A code PR and a docs PR can share a number across repos:
                // the number alone is no proof, the url names the repo too.
                json!({
                    "id": "x-aaaa",
                    "pr_number": 288,
                    "pr_url": "https://github.com/fnodotdev/fno/pull/288",
                    "additional_prs": [
                        {"number": 288, "url": "https://github.com/footnote-sh/fno-web/pull/288", "note": "docs"},
                    ],
                }),
                "not a proven docs twin",
            ),
        ];
        for (row, why) in cases {
            let mut entries = vec![node(row)];
            let result = repair_docs_twin_primary(&mut entries, "x-aaaa");
            assert_eq!(result.action, "refused");
            let detail = result.detail.as_deref().unwrap_or("");
            assert!(detail.contains(why), "{detail}");
        }
    }
}
