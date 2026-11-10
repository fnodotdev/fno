//! The `fno do ci tier` commands: the operator surface for the ci_tier arm.
//! `tier` prints the arm's newest pass row for this repo; `tier promote <pr>`
//! moves a PR up the tier's rank by bumping its bound node's priority one
//! step toward p0. The arm's rules live in ci_tier.rs and
//! docs/architecture/pr-heal.md, "The CI tier"; membership is rank-derived
//! (main-repair, not draft, node priority, then the older PR), so a priority
//! bump is the only honest promote.

use crate::backlog::row_facts::RowFacts;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The window one pass row answers for. The arm passes every 3 minutes; a
/// row older than a day describes a tier that no longer exists.
pub(crate) const SHOW_WINDOW_MS: i64 = 24 * 3_600_000;

/// One rank step toward p0. `None` at p0: nothing promotes past it. Legacy
/// priority words parse through the same vocabulary the node rows carry.
pub(crate) fn next_priority(current: &str) -> Option<String> {
    use crate::backlog::model::Priority;
    let parsed = Priority::parse(current).ok()?;
    let next = match parsed {
        Priority::P3 => Priority::P2,
        Priority::P2 => Priority::P1,
        Priority::P1 => Priority::P0,
        Priority::P0 => return None,
    };
    Some(next.as_str().to_string())
}

/// The show answer from the config state and the newest pass row. `row` is a
/// raw `ci_tier_pass` row (its `data` carries slots, gates, active, ranked,
/// cancelled, rerun); `None` means nothing landed under the window on this
/// machine, which the note words (a keeper on another machine never writes
/// this journal).
pub(crate) fn show_payload(enabled: bool, row: Option<&Value>, slug: &str, now_ms: i64) -> Value {
    let Some(row) = row else {
        let note = if enabled {
            "no ci_tier_pass row landed in the last 24 hours; the CI-tier keeper has not passed \
             on this machine (a keeper on another machine never writes this journal)"
        } else {
            "ci_tier.enabled is false on this machine; no keeper runs for this repo here"
        };
        return json!({"enabled": enabled, "slug": slug, "last_pass": Value::Null, "note": note});
    };
    let ts = crate::ci_cadence::ts_ms(row).unwrap_or(0);
    let age = now_ms - ts;
    let mut fields = serde_json::Map::new();
    fields.insert("age_ms".into(), json!(age));
    fields.insert("live".into(), json!(age < crate::ci_tier::FRESH_MS));
    fields.insert("ts".into(), json!(crate::ci_tier::word(row, "/ts")));
    if let Some(data) = row.get("data") {
        for key in ["slots", "gates", "active", "ranked", "cancelled", "rerun"] {
            fields.insert(key.into(), data.get(key).cloned().unwrap_or(Value::Null));
        }
    }
    json!({
        "enabled": enabled,
        "slug": slug,
        "last_pass": Value::Object(fields),
    })
}

/// The promote decision from what the command can prove before it writes:
/// the machine's tier state and the bound node's current priority (absent
/// reads as p2, the defaulted rank). `Ok` carries (from, to).
pub(crate) fn promote_plan(
    enabled: bool,
    node_id: &str,
    priority: Option<&str>,
) -> Result<(String, String), String> {
    if !enabled {
        return Err(
            "refusing: the CI tier is off on this machine (ci_tier.enabled = false), so there is \
             no tier to promote into. Turn it on with [ci_tier] enabled = true in .fno/config.toml."
                .to_string(),
        );
    }
    let from = priority.unwrap_or("p2");
    let Some(to) = next_priority(from) else {
        return Err(format!(
            "refusing: node {node_id} already ranks p0; nothing promotes past it"
        ));
    };
    Ok((from.to_string(), to))
}

/// The command: `fno do ci tier` shows, `fno do ci tier promote <pr>` bumps.
/// Exit 0 answered, 2 usage, 3 a domain refusal, 4 a read or write failure.
pub fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return 0;
    }
    match args {
        [] => show(),
        [word, rest @ ..] if word == "promote" => match rest {
            [] => {
                eprintln!("fno do ci tier promote: a PR number is required");
                2
            }
            rest => match promote_operand(rest) {
                Ok(pr) => promote(pr),
                Err(code) => code,
            },
        },
        _ => {
            print_usage();
            2
        }
    }
}

/// The all-digits PR operand; anything else (or nothing) is a usage refusal.
fn promote_operand(rest: &[String]) -> Result<u64, i32> {
    let mut operand = None;
    for arg in rest {
        if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_digit()) {
            if operand.is_some() {
                eprintln!("fno do ci tier promote: one PR operand only, got {rest:?}");
                return Err(2);
            }
            operand = arg.parse::<u64>().ok();
        } else {
            eprintln!("fno do ci tier promote: unexpected argument {arg:?}");
            return Err(2);
        }
    }
    operand.ok_or(2)
}

fn print_usage() {
    println!("Usage: fno do ci tier [promote <pr>]");
    println!();
    if let Some(wording) = crate::ci_front::spelling_help("tier") {
        println!("    {wording}");
    }
}

/// `fno do ci tier`: the tier state from this machine's config and journal.
fn show() -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let enabled = crate::ci_tier::tier_on(&cwd);
    let Some(slug) = crate::pr_status::cache::git_slug(&cwd) else {
        eprintln!("fno do ci tier: cannot read this checkout's repo slug; run it from a checkout");
        return 4;
    };
    let Some(home) = crate::paths::AgentsHome::from_env_opt() else {
        eprintln!("fno do ci tier: no agents home; the tier journal is unreadable here");
        return 4;
    };
    let path = crate::daemon::global_events_path(&home);
    match crate::ci_tier::newest_pass_row(&path, &slug, SHOW_WINDOW_MS) {
        Ok(row) => {
            let now = crate::ci_cadence::millis_now();
            println!("{}", show_payload(enabled, row.as_ref(), &slug, now));
            0
        }
        Err(e) => {
            eprintln!("fno do ci tier: {e}");
            4
        }
    }
}

/// `fno do ci tier promote <pr>`: resolve the PR to its one bound node, then
/// bump its priority one step. The refusal order is cheap-first: the local
/// tier state refuses before any network read.
fn promote(pr: u64) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if !crate::ci_tier::tier_on(&cwd) {
        let Err(refusal) = promote_plan(false, "", None) else {
            unreachable!("an off tier always refuses")
        };
        eprintln!("fno do ci tier promote: {refusal}");
        return 3;
    }
    let pull = match read_pull(&cwd, pr) {
        Ok(pull) => pull,
        Err(e) if e.contains("404") => {
            eprintln!("fno do ci tier promote: PR {pr} does not exist in this repo (HTTP 404)");
            return 3;
        }
        Err(e) => {
            eprintln!("fno do ci tier promote: {e}");
            return 4;
        }
    };
    let store = crate::backlog::api::Store::new(&crate::org_board::scope::graph_json_path(&cwd));
    let entries = match crate::backlog::api::rows(&store) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("fno do ci tier promote: the graph read failed: {}", e.0);
            return 4;
        }
    };
    let shaped = json!({
        "number": pr,
        "headRefName": crate::ci_tier::word(&pull, "/head/ref"),
        "url": crate::ci_tier::word(&pull, "/html_url"),
        "body": pull.get("body").cloned().unwrap_or(Value::Null),
    });
    let verdict = crate::org_board::prs::pr_binding_verdicts([&shaped], &entries)
        .into_iter()
        .find(|v| v.number == pr as i64);
    let Some(v) = verdict else {
        eprintln!("fno do ci tier promote: the PR resolved no binding verdict");
        return 4;
    };
    let Some(node_id) = v.node_id.clone() else {
        eprintln!(
            "fno do ci tier promote: refusing: PR {pr} binds no single node ({}); \
             the tier ranks bound nodes' priority, so promote needs one node",
            v.verdict
        );
        return 3;
    };
    let priority = entries
        .iter()
        .find(|e| RowFacts::id_of(e).as_deref() == Some(node_id.as_str()))
        .and_then(|e| e.get("priority").and_then(Value::as_str));
    let (from, to) = match promote_plan(true, &node_id, priority) {
        Ok(plan) => plan,
        Err(refusal) => {
            eprintln!("fno do ci tier promote: {refusal}");
            return 3;
        }
    };
    let input = crate::backlog::api::NodeUpdateInput {
        priority: Some(to.clone()),
        ..Default::default()
    };
    match crate::backlog::api::node_update(&store, &node_id, input) {
        Ok(payload) if payload.success => {
            println!(
                "{}",
                json!({"pr": pr, "node": node_id, "from": from, "to": to,
                    "note": "the next ci_tier pass (within 3 minutes) recomputes the tier; \
                             fno do ci tier shows the arrangement"})
            );
            0
        }
        Ok(payload) => {
            eprintln!(
                "fno do ci tier promote: the node update refused: success={} version={}",
                payload.success, payload.version
            );
            4
        }
        Err(e) => {
            eprintln!("fno do ci tier promote: the node update failed: {}", e.0);
            4
        }
    }
}

/// The PR row `tier promote` binds against: one REST read.
fn read_pull(cwd: &Path, pr: u64) -> Result<Value, String> {
    let raw = crate::pr_push::gh_api(
        "gh",
        cwd,
        &format!("repos/{{owner}}/{{repo}}/pulls/{pr}"),
        &[],
    )?;
    serde_json::from_str(&raw).map_err(|e| format!("gh api pulls/{pr} unparseable: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_bumps_one_step_and_stops_at_p0() {
        assert_eq!(next_priority("p3").as_deref(), Some("p2"));
        assert_eq!(next_priority("p2").as_deref(), Some("p1"));
        assert_eq!(next_priority("p1").as_deref(), Some("p0"));
        assert_eq!(next_priority("p0"), None);
        // The legacy words migrate the same way; an unknown word is None.
        assert_eq!(next_priority("medium").as_deref(), Some("p1"));
        assert_eq!(next_priority("urgent"), None);
    }

    #[test]
    fn show_reads_the_pass_row_into_the_answer() {
        let row = json!({"ts": "2026-11-08T12:00:00Z", "data": {
            "slots": 5, "gates": 1,
            "active": [4504, 4499],
            "ranked": [{"pr": 4504, "branch": "a"}, {"pr": 4499, "branch": "b"}],
            "cancelled": [], "rerun": []}});
        let now = crate::ci_cadence::ts_ms(&row).unwrap();
        let payload = show_payload(true, Some(&row), "fnodotdev/fno", now);
        assert_eq!(payload.get("enabled"), Some(&json!(true)));
        assert_eq!(payload.get("slug"), Some(&json!("fnodotdev/fno")));
        let last_pass = payload.get("last_pass").unwrap();
        assert_eq!(last_pass.get("slots"), Some(&json!(5)));
        assert_eq!(last_pass.get("active"), Some(&json!([4504, 4499])));
        assert_eq!(last_pass.get("age_ms"), Some(&json!(0)));
        assert_eq!(last_pass.get("live"), Some(&json!(true)));
        assert_eq!(payload.get("note"), None);
    }

    #[test]
    fn an_old_row_reads_not_live_and_an_absent_row_names_this_machine() {
        let row = json!({"ts": "2026-11-08T12:00:00Z", "data": {"slots": 6, "gates": 0,
            "active": [1], "ranked": [], "cancelled": [], "rerun": []}});
        let ts = crate::ci_cadence::ts_ms(&row).unwrap();
        let payload = show_payload(true, Some(&row), "s", ts + 2 * 3_600_000);
        assert_eq!(payload.pointer("/last_pass/live"), Some(&json!(false)));
        // Enabled with no row: the keeper has not passed here; the note
        // carries the cross-machine caveat.
        let payload = show_payload(true, None, "s", ts);
        assert_eq!(payload.get("last_pass"), Some(&Value::Null));
        let note = payload.get("note").and_then(Value::as_str).unwrap();
        assert!(note.contains("keeper"), "{note}");
        assert!(note.contains("another machine"), "{note}");
        // Disabled with no row: say the config state plainly.
        let payload = show_payload(false, None, "s", ts);
        assert!(payload
            .get("note")
            .and_then(Value::as_str)
            .unwrap()
            .contains("enabled"));
    }

    #[test]
    fn promote_refuses_an_off_tier_and_p0_and_bumps_one_step() {
        let refusal = promote_plan(false, "x-aaaa", Some("p2")).unwrap_err();
        assert!(refusal.contains("ci_tier.enabled"), "{refusal}");
        let refusal = promote_plan(true, "x-aaaa", Some("p0")).unwrap_err();
        assert!(
            refusal.contains("p0") && refusal.contains("x-aaaa"),
            "{refusal}"
        );
        // An absent priority reads as the defaulted p2.
        assert_eq!(
            promote_plan(true, "x-aaaa", None).unwrap(),
            ("p2".into(), "p1".into())
        );
        assert_eq!(
            promote_plan(true, "x-aaaa", Some("p3")).unwrap(),
            ("p3".into(), "p2".into())
        );
    }

    #[test]
    fn the_usage_lines_name_both_commands() {
        let _ = run(&[]);
        // A bare promote without an operand is a usage refusal (exit 2).
        // The off-tier refusal (exit 3) is promote_plan's own unit test
        // above: running it here would read the ambient checkout's config,
        // and an env with the tier on turns this leg into a live read.
        assert_eq!(run(&["promote".to_string()]), 2);
        assert_eq!(run(&["promote".to_string(), "nope".to_string()]), 2);
    }
}
