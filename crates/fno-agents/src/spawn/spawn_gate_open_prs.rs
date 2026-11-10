//! The open-PR cap: the spawn gate holds a new `/fno:target` seat while the
//! repo has more than `agents.max_open_prs` open PRs.
//!
//! 36 open PRs once put 64 CI runs in the queue against 3 running, so every
//! PR waited on every other. Work on a node that already has a PR (fix,
//! review, merge) is exempt: it shrinks the pile instead of growing it. A node
//! holds a PR when its graph row links one, or when an open PR's head branch
//! carries the node id (a worker that never recorded the link still owns it).

use std::path::Path;

use serde_json::Value;

use crate::spawn_gate::{GateInput, Refusal, EXIT_OPEN_PR_CAP};

/// The decision core, pure so a test needs no GitHub. `0` disables; an
/// unreadable count passes (fail open, as the disk term does).
pub(crate) fn open_pr_term(open: Option<usize>, cap: u32) -> Option<String> {
    let open = open?;
    (cap > 0 && open > cap as usize)
        .then(|| format!("{open} open PRs is above the max_open_prs cap {cap}"))
}

/// Only a seat that starts new delivery work counts: its seed is the target
/// command and its node has no PR yet.
fn is_new_target_seat(input: &GateInput, node_has_pr: impl Fn(&str) -> bool) -> bool {
    // The `execute` phase also holds fix, tdd and do seats; only the target
    // command opens new delivery work.
    let command = input
        .seed
        .as_deref()
        .and_then(|s| s.split_whitespace().next())
        .and_then(crate::provider::parse_command_token)
        .map(|(command, _)| command);
    if command != Some("target") {
        return false;
    }
    !input.node.as_deref().is_some_and(node_has_pr)
}

fn node_has_pr(node: &str) -> bool {
    let rows = crate::graph_store::read_rows_where(
        &crate::graph_get::default_graph_path(),
        &crate::backlog::RowQuery {
            filter: crate::backlog::api::NodeFilter {
                id_in: Some([node.to_string()].into_iter().collect()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap_or_default();
    rows.iter().any(|row| {
        let set = |key: &str| row.get(key).is_some_and(|v| !v.is_null() && v != "");
        set("pr_number") || set("pr_url")
    })
}

/// The open PR count plus each open PR's head branch.
pub(crate) struct OpenPrs {
    count: usize,
    heads: Vec<String>,
}

impl OpenPrs {
    #[cfg(test)]
    fn counted(count: usize) -> Self {
        OpenPrs {
            count,
            heads: Vec::new(),
        }
    }

    /// Branches are `feature/<node>-<slug>`; the trailing dash keeps `x-1`
    /// from matching `x-1d`.
    fn holds_node(&self, node: &str) -> bool {
        let prefix = format!("feature/{node}-");
        self.heads.iter().any(|h| h.starts_with(&prefix))
    }
}

fn count_open_prs(cwd: &Path) -> Result<OpenPrs, String> {
    let pages = crate::pr_push::gh_api_pages_conditional(
        "gh",
        cwd,
        "repos/{owner}/{repo}/pulls?state=open&per_page=100",
    )?;
    let prs: Vec<&Value> = pages.iter().filter_map(Value::as_array).flatten().collect();
    let heads = prs
        .iter()
        .filter_map(|pr| pr.pointer("/head/ref").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    Ok(OpenPrs {
        count: prs.len(),
        heads,
    })
}

/// The gate's read. Hard like the disk axis: `--force` does not lift it,
/// because a forced seat still opens a PR into the same CI queue.
pub(crate) fn open_pr_gate(config_cwd: &Path, input: &GateInput) -> Result<(), Refusal> {
    gate_with(config_cwd, input, node_has_pr, count_open_prs)
}

fn gate_with(
    config_cwd: &Path,
    input: &GateInput,
    has_pr: impl Fn(&str) -> bool,
    count: impl Fn(&Path) -> Result<OpenPrs, String>,
) -> Result<(), Refusal> {
    let cap = crate::agents_config::max_open_prs(config_cwd);
    if cap == 0 || !is_new_target_seat(input, has_pr) {
        return Ok(());
    }
    let prs = match count(config_cwd) {
        Ok(prs) => Some(prs),
        Err(e) => {
            eprintln!(
                "{} open-PR count unreadable, passing: {e}",
                crate::spawn_gate::NOTE
            );
            None
        }
    };
    if let (Some(prs), Some(node)) = (&prs, input.node.as_deref()) {
        if prs.holds_node(node) {
            return Ok(());
        }
    }
    let open = prs.as_ref().map(|p| p.count);
    let Some(term) = open_pr_term(open, cap) else {
        return Ok(());
    };
    eprintln!(
        "spawn-gate: {term}; holding the new target seat. Merge or close PRs, then respawn; \
         fix, review and merge work on a node that already holds an open PR still runs. To change the limit: set \
         agents.max_open_prs in ~/.fno/config.toml (0 disables)."
    );
    Err(Refusal::with_receipt(
        EXIT_OPEN_PR_CAP,
        serde_json::json!({
            "status": "refused",
            "reason": "open_pr_cap",
            "open_prs": open,
            "max_open_prs": cap,
            "remedy": "merge or close open PRs; a spawn for a node that already holds an open PR (graph pr link, or an open feature/<node>-* branch) is exempt",
        }),
    )
    .ev("axis", serde_json::json!("open_prs")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(seed: &str, node: Option<&str>) -> GateInput {
        GateInput {
            name: "t-x-1".to_string(),
            substrate: "thread".to_string(),
            flags: Default::default(),
            route_provider: None,
            node: node.map(str::to_string),
            account: None,
            caller_session: None,
            succession_scope: None,
            holder_pid: None,
            seed: Some(seed.to_string()),
            session_phase: None,
        }
    }

    #[test]
    fn term_names_the_count_and_the_cap() {
        let term = open_pr_term(Some(36), 20).expect("above cap holds");
        assert!(
            term.contains("36 open PRs") && term.contains("cap 20"),
            "{term}"
        );
        assert_eq!(open_pr_term(Some(20), 20), None, "at the cap passes");
        assert_eq!(open_pr_term(Some(99), 0), None, "0 disables");
        assert_eq!(open_pr_term(None, 20), None, "unreadable fails open");
    }

    #[test]
    fn new_target_seat_over_cap_is_refused_with_the_count() {
        let td = tempfile::tempdir().unwrap();
        let refusal = gate_with(
            td.path(),
            &input("/fno:target x-1", Some("x-1")),
            |_| false,
            |_| Ok(OpenPrs::counted(36)),
        )
        .unwrap_err();
        assert_eq!(refusal.exit_code, EXIT_OPEN_PR_CAP);
        let receipt = refusal.receipt.expect("receipt");
        assert_eq!(receipt["reason"], "open_pr_cap");
        assert_eq!(receipt["open_prs"], 36);
        assert_eq!(receipt["max_open_prs"], 20);
    }

    #[test]
    fn work_on_a_node_with_a_pr_and_non_target_seeds_still_run() {
        let td = tempfile::tempdir().unwrap();
        let over = |_: &Path| Ok(OpenPrs::counted(36));
        assert!(gate_with(
            td.path(),
            &input("/fno:target x-1", Some("x-1")),
            |_| true,
            over
        )
        .is_ok());
        assert!(gate_with(
            td.path(),
            &input("/fno:fix ci", Some("x-1")),
            |_| false,
            over
        )
        .is_ok());
        assert!(gate_with(
            td.path(),
            &input("/fno:target x-1", Some("x-1")),
            |_| false,
            |_| Ok(OpenPrs::counted(20))
        )
        .is_ok());
    }

    #[test]
    fn a_node_with_an_open_branch_pr_is_exempt_without_a_graph_link() {
        let td = tempfile::tempdir().unwrap();
        let open = |heads: &[&str]| {
            let heads: Vec<String> = heads.iter().map(|h| h.to_string()).collect();
            move |_: &Path| {
                Ok(OpenPrs {
                    count: 36,
                    heads: heads.clone(),
                })
            }
        };
        let seat = input("/fno:target x-77", Some("x-77"));
        let held = open(&["feature/x-77-mux-adds-remote-host"]);
        assert!(gate_with(td.path(), &seat, |_| false, held).is_ok());
        let other = open(&["feature/x-77b-other", "feature/x-1-a"]);
        assert!(gate_with(td.path(), &seat, |_| false, other).is_err());
    }

    #[test]
    fn an_unreadable_count_passes() {
        let td = tempfile::tempdir().unwrap();
        let down = |_: &Path| Err("gh down".to_string());
        assert!(gate_with(td.path(), &input("/fno:target x-1", None), |_| false, down).is_ok());
    }
}
