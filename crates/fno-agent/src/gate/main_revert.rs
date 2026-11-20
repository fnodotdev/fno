//! The main_revert arm: when main stays red, revert the culprit merge first
//! and fix forward second. Opt-in per repo (`main_repair.auto_revert`). A red
//! head that outlives `main_repair.revert_after_minutes` starts a walk of
//! main's first-parent chain: follow first parents while the workflow reads
//! fail, stop at green, and refuse on a cancelled, pending or missing run.
//! The oldest PR merge in that red stretch is the culprit: the arm files a
//! `main-repair` node, opens the revert PR the merge gate lets through a red
//! main, and asks the culprit's author to re-land with the fix. What one read
//! cannot prove is refused, never guessed. One `main_revert_check` row per
//! red head is the arm's memory, so a restart never opens a second revert,
//! and an opened revert for one culprit PR also dedupes across later red
//! heads parked over the same culprit.

use crate::backlog::row_facts::RowFacts;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::paths::AgentsHome;

const INTERVAL: Duration = Duration::from_secs(60);
const WINDOW_MS: i64 = 7 * 24 * 3_600_000;
const TIMEOUT: Duration = Duration::from_secs(120);
pub(crate) const CHECK_EVENT: &str = "main_revert_check";
const FINAL: &[&str] = &["opened", "ambiguous", "flaky", "failed", "deduped"];

#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Decision {
    Ambiguous(String),
    Flaky,
    Revert(u64),
}

fn short(sha: &str) -> String {
    sha.chars().take(8).collect()
}

fn merge_pr(subject: &str) -> Option<u64> {
    subject
        .strip_prefix("Merge pull request #")
        .map(|rest| {
            rest.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|digits| digits.parse().ok())
}

/// The culprit candidate in a red stretch: the deepest walked entry whose
/// subject parses as a PR merge, with its sha.
fn oldest_red_merge(stretch: &[(String, String)]) -> Option<(u64, &str)> {
    stretch
        .iter()
        .rev()
        .find_map(|(sha, subject)| merge_pr(subject).map(|pr| (pr, sha.as_str())))
}

/// The red stretch's oldest PR merge is the culprit: an earlier red commit
/// may be the first red, and a cancelled run can hide which one, so the walk
/// refuses any terminator that is not green.
pub(crate) fn decide(
    stretch: &[(String, String)],
    terminator: &str,
    failing: &[String],
    flakes: &HashSet<String>,
) -> Decision {
    if terminator != "pass" {
        return Decision::Ambiguous(format!("the walk reads {terminator}, not green"));
    }
    let Some((pr, _)) = oldest_red_merge(stretch) else {
        return Decision::Ambiguous("the red stretch holds no PR merge".into());
    };
    // main reads red on a cancelled run too (a runner outage): with no
    // failing check, the merge broke nothing.
    if failing.is_empty() {
        return Decision::Ambiguous("no failing check at the red head".into());
    }
    if failing.iter().all(|f| flakes.contains(f)) {
        return Decision::Flaky;
    }
    Decision::Revert(pr)
}

/// When this repo's `head` was first seen red, whether a final outcome
/// already closed it, and which culprit PRs of this slug already carry an
/// opened revert - the set a later red head dedupes against.
pub(crate) fn fold(rows: &[Value], slug: &str, head: &str) -> (Option<i64>, bool, HashSet<u64>) {
    let (mut first, mut closed) = (None::<i64>, false);
    let mut opened = HashSet::new();
    for row in rows {
        let field = |k: &str| row.pointer(&format!("/data/{k}")).and_then(Value::as_str);
        if field("slug") == Some(slug) {
            if field("outcome") == Some("opened") {
                if let Some(pr) = row.pointer("/data/pr").and_then(Value::as_u64) {
                    opened.insert(pr);
                }
            }
            if field("head") == Some(head) {
                let ts = crate::ci_cadence::ts_ms(row).unwrap_or(i64::MAX);
                first = Some(first.map_or(ts, |f| f.min(ts)));
                closed |= FINAL.contains(&field("outcome").unwrap_or(""));
            }
        }
    }
    (first, closed, opened)
}

fn gh_json(root: &Path, path: &str) -> Result<Value, String> {
    let raw = crate::pr_push::gh_api("gh", root, path, &[])?;
    serde_json::from_str(&raw).map_err(|e| format!("unparseable {path}: {e}"))
}

/// The head's red stretch and the bucket that ended the walk: (sha, subject)
/// pairs whose workflow read fail, newest first, then "pass" or a refuse arm
/// ("cancel", "pending", "none", "deeper").
fn read_head(
    root: &Path,
    workflow: &str,
    head: &str,
) -> Result<(Vec<(String, String)>, String), String> {
    let commit = gh_json(root, &format!("repos/{{owner}}/{{repo}}/commits/{head}"))?;
    let subject = commit["commit"]["message"].as_str().unwrap_or("");
    let subject = subject.lines().next().unwrap_or("").to_string();
    let parents = commit["parents"].as_array().cloned().unwrap_or_default();
    // A squash or a direct push has one parent: never a merge to revert.
    let (2, Some(parent)) = (
        parents.len(),
        parents.first().and_then(|p| p["sha"].as_str()),
    ) else {
        return Ok((Vec::new(), "none".into()));
    };
    // The head is red; its ancestors walk while they read fail. One commits
    // page carries the chain, and a step matches the known first-parent sha,
    // never the page's date order.
    let mut stretch = vec![(head.to_string(), subject)];
    let mut expected = parent.to_string();
    let mut page: Option<Vec<Value>> = None;
    loop {
        let path = format!("repos/{{owner}}/{{repo}}/actions/runs?head_sha={expected}&event=push");
        let runs = gh_json(root, &path)?;
        let bucket = runs["workflow_runs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| r["name"].as_str() == Some(workflow))
            .max_by_key(|r| r["created_at"].as_str().unwrap_or("").to_string())
            .map_or("none", crate::pr_push::rest_bucket);
        if bucket != "fail" {
            return Ok((stretch, bucket.to_string()));
        }
        if page.is_none() {
            let fetched = gh_json(
                root,
                &format!("repos/{{owner}}/{{repo}}/commits?sha={head}&per_page=100"),
            )?;
            page = Some(fetched.as_array().cloned().unwrap_or_default());
        }
        let ancestor = page.as_ref().and_then(|c| {
            c.iter()
                .find(|c| c["sha"].as_str() == Some(expected.as_str()))
        });
        let Some(ancestor) = ancestor else {
            // Deeper than one page: the first red is unproven, refused.
            return Ok((Vec::new(), "deeper".into()));
        };
        let ancestor_subject = ancestor["commit"]["message"]
            .as_str()
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        stretch.push((expected.clone(), ancestor_subject));
        expected = ancestor["parents"]
            .as_array()
            .and_then(|p| p.first())
            .and_then(|p| p["sha"].as_str())
            .unwrap_or_default()
            .to_string();
        if expected.is_empty() {
            return Ok((Vec::new(), "none".into()));
        }
    }
}

fn failing_checks(root: &Path, sha: &str) -> Result<Vec<String>, String> {
    let rows = crate::pr_push::read_checks_rows("gh", root, sha)?;
    let latest = crate::check_supersession::latest_per_name(&Value::Array(rows));
    Ok(latest
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["bucket"].as_str() == Some("fail"))
        .filter_map(|r| r["name"].as_str().map(str::to_string))
        .collect())
}

fn run(cwd: &Path, bin: &str, args: &[&str]) -> Result<String, String> {
    match crate::pr_push::run_labeled("main-revert", bin, args, cwd, TIMEOUT)? {
        (true, out, _) => Ok(out.trim().to_string()),
        (false, _, err) => Err(format!("{bin} {}: {}", args.join(" "), err.trim())),
    }
}

/// File the repair node, open the revert PR bound to it, tell the culprit.
/// `head` is the red sha the guards re-check; `culprit` is the merge being
/// reverted, which sits below `head` when the stretch walked.
fn open_revert(
    root: &Path,
    head: &str,
    culprit: &str,
    pr: u64,
    workflow: &str,
) -> Result<Value, String> {
    let title = format!("Revert PR {pr}: main red at {}", short(head));
    let why = format!(
        "main went red in {workflow} at {}, and the first red merge on main's first-parent chain is {}, the merge of PR {pr}.",
        short(head),
        short(culprit)
    );
    let receipt = run(
        root,
        "fno",
        &["backlog", "idea", &title, "-J", "--details", &why],
    )?;
    let node = serde_json::from_str::<Value>(&receipt).ok();
    let node = node.and_then(|v| v["id"].as_str().map(str::to_string));
    let node = node.ok_or_else(|| format!("backlog idea printed no id: {receipt}"))?;
    // Past this point a failure names the filed node, so the row points at
    // the repair node left without a PR.
    let opened = revert_for(root, head, culprit, pr, &node, &title, &why);
    opened.map_err(|e| format!("repair node {node} has no PR: {e}"))
}

fn revert_for(
    root: &Path,
    head: &str,
    culprit: &str,
    pr: u64,
    node: &str,
    title: &str,
    why: &str,
) -> Result<Value, String> {
    let tag = format!("tags+={}", crate::authorized_merge::MAIN_REPAIR_TAG);
    run(root, "fno", &["backlog", "update", node, &tag])?;
    let branch = format!("bugfix/{node}-revert-pr{pr}");
    run(root, "git", &["fetch", "origin", "main"])?;
    // The red verdict is a cached read: a main that moved since may already
    // carry the fix, and a revert on top of it would undo a green main. The
    // guard reads the red head, never the culprit below it.
    let fetched = run(root, "git", &["rev-parse", "origin/main"])?;
    if fetched != head {
        return Err(format!(
            "main moved to {} since the red read",
            short(&fetched)
        ));
    }
    let body = format!(
        "{why} This reverts that merge so main turns green in one CI run. The author of #{pr} re-lands it with the fix.\n\n{}",
        crate::org_board::pr_closure::render([node])
    );
    let url = crate::bank_pr::open_with(root, &branch, title, &body, |tree| {
        // The helper fetches again; a main that moved since the check above
        // is not the head read red.
        if run(tree, "git", &["rev-parse", "HEAD"])? != head {
            return Err("main moved before the revert was cut".into());
        }
        run(tree, "git", &["revert", "-m", "1", "--no-edit", culprit]).map(|_| ())
    })?;
    let ask = format!("{why} The fno daemon opened a revert: {url}. Please re-land this change with the fix in a new PR.");
    let comments = format!("repos/{{owner}}/{{repo}}/issues/{pr}/comments");
    let _ = run(
        root,
        "gh",
        &["api", &comments, "-f", &format!("body={ask}")],
    );
    let owner = crate::authorized_merge::pr_bound_entry(root, pr);
    if let Some(owner) = owner.and_then(|e| RowFacts::id_of(&e).map(str::to_string)) {
        let _ = run(root, "fno", &["backlog", "note", &owner, &ask]);
    }
    Ok(json!({"pr": pr, "node": node, "url": url}))
}

/// One repo: the outcome word and its row fields, `None` when nothing is owed.
fn check_repo(
    root: &Path,
    slug: &str,
    rows: &[Value],
    flakes: &HashSet<String>,
    now_ms: i64,
) -> Option<(&'static str, Map<String, Value>)> {
    let delay =
        crate::agents_config::config_value_deep(root, &["main_repair", "revert_after_minutes"]);
    let delay_ms = delay.and_then(|v| v.as_integer()).unwrap_or(10) * 60_000;
    let token = crate::main_ci::main_ci_reading_cached(root).ok()?;
    let (workflow, head) = crate::main_ci::main_ci_red_run(&token)?;
    let (first, closed, opened) = fold(rows, slug, &head);
    let mut fields = Map::from_iter([
        ("head".into(), json!(head)),
        ("workflow".into(), json!(workflow)),
    ]);
    match first {
        _ if closed => return None,
        None => return Some(("seen", fields)),
        Some(first) if now_ms.saturating_sub(first) < delay_ms => return None,
        Some(_) => {}
    }
    let read = read_head(root, &workflow, &head)
        .and_then(|(stretch, terminator)| Ok((stretch, terminator, failing_checks(root, &head)?)));
    let (outcome, extra) = match read {
        Err(reason) => ("unreadable", json!({"reason": reason})),
        Ok((stretch, terminator, failing)) => match decide(&stretch, &terminator, &failing, flakes)
        {
            Decision::Ambiguous(reason) => ("ambiguous", json!({"reason": reason})),
            Decision::Flaky => ("flaky", json!({"failing": failing})),
            Decision::Revert(pr) if opened.contains(&pr) => {
                // The culprit already carries an opened revert; a newer red
                // head parked over the same culprit is not a new culprit.
                // The row closes the head, so the walk runs once, not every
                // tick.
                ("deduped", json!({"pr": pr}))
            }
            Decision::Revert(pr) => {
                let Some((_, culprit)) = oldest_red_merge(&stretch) else {
                    return None;
                };
                match open_revert(root, &head, culprit, pr, &workflow) {
                    Ok(receipt) => ("opened", receipt),
                    Err(reason) => ("failed", json!({"pr": pr, "reason": reason})),
                }
            }
        },
    };
    fields.extend(extra.as_object().cloned().unwrap_or_default());
    Some((outcome, fields))
}

pub(crate) fn run_pass(home: &AgentsHome) -> Result<(), String> {
    let on = |root: &String| {
        let key = crate::agents_config::config_value_deep(
            Path::new(root),
            &["main_repair", "auto_revert"],
        );
        key.and_then(|v| v.as_bool()) == Some(true)
    };
    let roots = crate::daemon::worktree_sweep::registry_repo_roots(home);
    let roots: Vec<String> = roots.into_iter().filter(on).collect();
    if roots.is_empty() {
        return Ok(());
    }
    let now_ms = crate::ci_cadence::millis_now();
    let path = crate::daemon::global_events_path(home);
    let query = crate::event_store::EventQuery {
        since_ms: Some(now_ms - WINDOW_MS),
        ..crate::event_store::EventQuery::of_types(&[CHECK_EVENT])
    };
    let text = crate::event_store::journal_text_checked(&path, &query)?;
    let rows: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let emitter = crate::events::EventEmitter::new(path.clone(), "daemon");
    let mut slugs = HashSet::new();
    for root in roots {
        let root = Path::new(&root);
        // Two clones of one repo are one main.
        let slug = crate::pr_status::cache::git_slug(root).filter(|s| slugs.insert(s.clone()));
        let Some(slug) = slug else { continue };
        let flakes = crate::ci_cadence::flake_checks(&path, root);
        if let Some((outcome, mut fields)) = check_repo(root, &slug, &rows, &flakes, now_ms) {
            fields.insert("slug".into(), json!(slug));
            fields.insert("outcome".into(), json!(outcome));
            let landed = emitter.emit_fields(CHECK_EVENT, fields);
            landed.map_err(|e| format!("{CHECK_EVENT} row did not land: {e}"))?;
        }
    }
    Ok(())
}

pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        let busy = last.is_some_and(|t| t.elapsed() < INTERVAL);
        if busy || arm.in_flight.swap(true, Ordering::SeqCst) {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        if let Err(error) = run_pass(&home) {
            eprintln!("main-revert: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const MERGE: &str = "Merge pull request #11 from o/feature/x-1";
    const NEWER_MERGE: &str = "Merge pull request #12 from o/feature/x-2";

    fn stretch(entries: &[(&str, &str)]) -> Vec<(String, String)> {
        entries
            .iter()
            .map(|(sha, subject)| (sha.to_string(), subject.to_string()))
            .collect()
    }

    fn decide_with(
        entries: &[(&str, &str)],
        terminator: &str,
        failing: &[&str],
        flakes: &[&str],
    ) -> Decision {
        let failing: Vec<String> = failing.iter().map(|s| s.to_string()).collect();
        let flakes: HashSet<String> = flakes.iter().map(|s| s.to_string()).collect();
        decide(&stretch(entries), terminator, &failing, &flakes)
    }

    #[test]
    fn a_red_merge_over_a_green_parent_is_reverted() {
        assert_eq!(
            decide_with(&[("c2", MERGE)], "pass", &["unit"], &[]),
            Decision::Revert(11)
        );
    }

    #[test]
    fn the_oldest_red_merge_in_a_walked_stretch_is_reverted() {
        // Two merges landed before main's red settled and both runs read
        // red: the culprit is the older one, not the head.
        assert_eq!(
            decide_with(
                &[("c3", NEWER_MERGE), ("c2", MERGE)],
                "pass",
                &["unit"],
                &[]
            ),
            Decision::Revert(11)
        );
    }

    #[test]
    fn the_oldest_red_merge_skips_direct_pushes_on_the_way_down() {
        // The rule names the oldest red MERGE; a red direct push below it is
        // walked past, and the nearest merge above the green ancestor wins.
        assert_eq!(
            decide_with(
                &[("c3", NEWER_MERGE), ("c2", "feat: walk")],
                "pass",
                &["unit"],
                &[]
            ),
            Decision::Revert(12)
        );
    }

    #[test]
    fn what_one_read_cannot_prove_is_refused() {
        // An unsettled walk terminator may hide the first red commit; a red
        // stretch of only direct pushes is no merge to revert.
        let cases = [
            (&[("c2", MERGE)] as &[_], "cancel"),
            (&[("c2", MERGE)], "pending"),
            (&[("c2", MERGE)], "none"),
            (&[("c2", MERGE)], "deeper"),
        ];
        for (entries, terminator) in cases {
            let decision = decide_with(entries, terminator, &["unit"], &[]);
            assert!(
                matches!(decision, Decision::Ambiguous(_)),
                "{terminator} {entries:?}"
            );
        }
        // A red stretch with no merge subject anywhere.
        let no_merge = decide_with(
            &[("c3", "feat: second"), ("c2", "feat: walk")],
            "pass",
            &["unit"],
            &[],
        );
        assert!(matches!(no_merge, Decision::Ambiguous(_)));
        // A red made only of cancelled runs names no failing check.
        let cancelled = decide_with(&[("c2", MERGE)], "pass", &[], &[]);
        assert!(matches!(cancelled, Decision::Ambiguous(_)));
    }

    #[test]
    fn a_red_made_only_of_known_flakes_is_not_reverted() {
        assert_eq!(
            decide_with(&[("c2", MERGE)], "pass", &["e2e"], &["e2e"]),
            Decision::Flaky
        );
        let real = decide_with(&[("c2", MERGE)], "pass", &["e2e", "unit"], &["e2e"]);
        assert_eq!(real, Decision::Revert(11));
    }

    #[test]
    fn the_journal_names_first_sighting_and_closure_per_repo_head() {
        let row = |ts: &str, slug: &str, head: &str, outcome: &str| json!({"ts": ts, "data": {"slug": slug, "head": head, "outcome": outcome}});
        let rows = [
            row("2026-10-26T10:00:00Z", "o/r", "c2", "seen"),
            row("2026-10-26T10:11:00Z", "o/r", "c2", "opened"),
            row("2026-10-26T10:12:00Z", "o/r", "c3", "seen"),
            row("2026-10-26T10:13:00Z", "x/y", "c3", "opened"),
        ];
        let ts = |i: usize| crate::ci_cadence::ts_ms(&rows[i]);
        let (first, closed, opened) = fold(&rows, "o/r", "c2");
        assert_eq!((first, closed), (Some(ts(0)), true));
        assert!(opened.is_empty());
        let (first, closed, opened) = fold(&rows, "o/r", "c3");
        assert_eq!((first, closed), (Some(ts(2)), false));
        assert!(opened.is_empty());
        assert_eq!(fold(&rows, "o/r", "c4"), (None, false, HashSet::new()));
    }

    #[test]
    fn opened_reverts_dedupe_their_culprit_across_heads() {
        // An opened revert for one culprit is visible to every head of the
        // same slug, so a later red head parked over the same culprit skips
        // a second revert; another repo's opened culprit is not ours.
        let row = |slug: &str, head: &str, pr: u64| json!({"ts": "2026-10-26T10:11:00Z", "data": {"slug": slug, "head": head, "outcome": "opened", "pr": pr}});
        let rows = [
            row("o/r", "c2", 11),
            row("o/r", "c3", 12),
            row("x/y", "c9", 11),
        ];
        let (_, _, opened) = fold(&rows, "o/r", "c4");
        assert_eq!(opened, HashSet::from([11, 12]));
        let (_, _, other) = fold(&rows, "x/y", "c4");
        assert_eq!(other, HashSet::from([11]));
    }

    #[test]
    fn a_deduped_head_closes_like_any_final_outcome() {
        let row = json!({"ts": "2026-10-26T10:11:00Z", "data": {"slug": "o/r", "head": "c3", "outcome": "deduped"}});
        let (_, closed, opened) = fold(&[row], "o/r", "c3");
        assert!(closed);
        assert!(opened.is_empty());
    }
}
