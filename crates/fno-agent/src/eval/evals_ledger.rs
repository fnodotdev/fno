//! The learnings ledger: one append-only row per eval-caused change in
//! `<state>/evals/learnings.jsonl`. Three writers feed it. The weekly leg
//! settles each closed go node once its after-window ends. The propose loop
//! and a persona edit record their own proof through `--record`.
//!
//! A go node's score is the weekly failure count of its cluster's signal
//! kind: the window before the close against the window after it. A persona
//! row is accuracy on its labeled set, and the ledger refuses one that does
//! not rise, so a persona edit lands only with a gain.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The before and after windows around a go node's close.
const WINDOW_DAYS: i64 = 7;

#[derive(Debug, thiserror::Error)]
pub(crate) enum LedgerError {
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Io(String),
}

impl From<String> for LedgerError {
    fn from(e: String) -> Self {
        LedgerError::Io(e)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChangeKind {
    GoNode,
    Pr,
    Persona,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Outcome {
    Improved,
    NoChange,
    Worse,
    Unmeasured,
}

/// One eval-caused change. An unknown field reads null, never absent.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct Learning {
    pub change: String,
    pub kind: ChangeKind,
    pub trace_ids: Vec<String>,
    pub golden: Option<String>,
    pub go_node: Option<String>,
    pub score_before: Option<f64>,
    pub pr: Option<String>,
    pub score_after: Option<f64>,
    /// What the two scores measure.
    pub score: Option<String>,
    pub outcome: Outcome,
    pub recorded_at: Option<String>,
}

fn path(state: &Path) -> PathBuf {
    crate::evals::state_evals_dir(state).join("learnings.jsonl")
}

pub(crate) fn read(state: &Path) -> Vec<Learning> {
    std::fs::read_to_string(path(state))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn outcome(before: f64, after: f64, lower_is_better: bool) -> Outcome {
    match after.partial_cmp(&before) {
        Some(std::cmp::Ordering::Less) if lower_is_better => Outcome::Improved,
        Some(std::cmp::Ordering::Greater) if !lower_is_better => Outcome::Improved,
        Some(std::cmp::Ordering::Less | std::cmp::Ordering::Greater) => Outcome::Worse,
        _ => Outcome::NoChange,
    }
}

/// Validate one row and append it under the ledger lock. A change already
/// recorded is skipped (Ok(false)), so a rerun writes nothing.
pub(crate) fn append(state: &Path, mut row: Learning) -> Result<bool, LedgerError> {
    if row.change.trim().is_empty() {
        return Err(LedgerError::Refused(
            "a learnings row needs a change id".into(),
        ));
    }
    if row.kind == ChangeKind::Persona {
        let (Some(before), Some(after)) = (row.score_before, row.score_after) else {
            return Err(LedgerError::Refused(
                "a persona row needs score_before and score_after (accuracy on its labeled set)"
                    .into(),
            ));
        };
        if after <= before {
            return Err(LedgerError::Refused(format!(
                "persona accuracy did not rise ({before} -> {after}); the edit does not land"
            )));
        }
        row.outcome = Outcome::Improved;
    }
    let file = path(state);
    let io = |e: std::io::Error| LedgerError::Io(format!("{}: {e}", file.display()));
    std::fs::create_dir_all(crate::evals::state_evals_dir(state)).map_err(io)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(file.with_extension("lock"))
        .map_err(io)?;
    lock.lock().map_err(io)?;
    if read(state).iter().any(|r| r.change == row.change) {
        return Ok(false);
    }
    row.recorded_at
        .get_or_insert_with(|| Utc::now().to_rfc3339());
    let line = serde_json::to_string(&row).map_err(|e| LedgerError::Io(e.to_string()))?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .and_then(|mut f| f.write_all(format!("{line}\n").as_bytes()))
        .map_err(io)?;
    Ok(true)
}

/// The row for one closed go node, from its cluster marker and evidence lines
/// (the fixer's `cluster: layer=L kind=K` and `session S span R` shape).
fn go_row(
    id: &str,
    details: &str,
    pr: Option<&str>,
    closed: DateTime<Utc>,
    traces: &[Value],
) -> Learning {
    let kind = details
        .lines()
        .find_map(|l| l.strip_prefix("cluster: "))
        .and_then(|c| c.split_whitespace().find_map(|p| p.strip_prefix("kind=")))
        .unwrap_or("unknown");
    let sessions: BTreeSet<&str> = details
        .lines()
        .filter_map(|l| l.strip_prefix("session "))
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    let mut trace_ids: Vec<String> = traces
        .iter()
        .filter(|t| {
            crate::evals_labels::fact_str(&t["session_id"]).is_some_and(|s| sessions.contains(s))
        })
        .filter_map(|t| t["id"].as_str().map(str::to_string))
        .collect();
    trace_ids.sort_unstable();
    let window = Duration::days(WINDOW_DAYS);
    let count = |from: DateTime<Utc>, to: DateTime<Utc>| {
        traces
            .iter()
            .filter(|t| t["signal_kind"].as_str() == Some(kind))
            .filter_map(|t| DateTime::parse_from_rfc3339(t["created_at"].as_str()?).ok())
            .filter(|at| *at >= from && *at < to)
            .count() as f64
    };
    let before = count(closed - window, closed);
    let after = count(closed, closed + window);
    Learning {
        change: id.to_string(),
        kind: ChangeKind::GoNode,
        trace_ids,
        golden: None,
        go_node: Some(id.to_string()),
        score_before: Some(before),
        pr: pr.map(str::to_string),
        score_after: Some(after),
        score: Some(format!("{kind} failures per {WINDOW_DAYS} days")),
        outcome: outcome(before, after, true),
        recorded_at: None,
    }
}

/// Record every go node whose after-window has closed and has no row yet.
pub(crate) fn sweep(state: &Path, now: DateTime<Utc>) -> Result<usize, LedgerError> {
    let graph = crate::backlog::settings::graph_path();
    let nodes =
        crate::graph_store::read_rows(&graph).map_err(|e| LedgerError::Io(e.to_string()))?;
    let traces = crate::evals_labels::load_traces(state);
    let mut recorded = 0;
    for node in nodes {
        let text = |field: &str| node.pointer(field).and_then(Value::as_str);
        let Some(id) = text("/id").filter(|id| id.starts_with("go-")) else {
            continue;
        };
        let Some(closed) = text("/completed_at")
            .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
            .map(|at| at.with_timezone(&Utc))
        else {
            continue;
        };
        if closed + Duration::days(WINDOW_DAYS) > now {
            continue;
        }
        let details = text("/details").unwrap_or("");
        let row = go_row(id, details, text("/pr_url"), closed, &traces);
        recorded += usize::from(append(state, row)?);
    }
    Ok(recorded)
}

/// `evals report --learnings [--record <row.json>] [--json]`.
pub fn command(args: &[String]) -> i32 {
    let state = crate::evals_candidates::state_root();
    if let Some(file) = crate::evals::flag_lookup(args, "--record") {
        let row = std::fs::read(&file)
            .map_err(|e| LedgerError::Io(format!("{file}: {e}")))
            .and_then(|b| {
                serde_json::from_slice::<Learning>(&b)
                    .map_err(|e| LedgerError::Refused(format!("{file}: {e}")))
            });
        return match row.and_then(|row| append(&state, row)) {
            Ok(true) => {
                println!("learnings: recorded");
                0
            }
            Ok(false) => {
                println!("learnings: change already recorded; nothing written");
                0
            }
            Err(e) => {
                eprintln!("learnings: {e}");
                1
            }
        };
    }
    let rows = read(&state);
    if crate::json_output::requested(args) {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"rows": rows})).unwrap_or_default()
        );
        return 0;
    }
    let num = |s: Option<f64>| s.map_or("null".to_string(), |v| v.to_string());
    for row in &rows {
        println!(
            "{}\t{:?}\t{} -> {}\t{:?}\t{}",
            row.change,
            row.kind,
            num(row.score_before),
            num(row.score_after),
            row.outcome,
            row.pr.as_deref().unwrap_or("-"),
        );
    }
    println!(
        "{} learnings rows in {}",
        rows.len(),
        path(&state).display()
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(id: &str, session: &str, kind: &str, at: &str) -> Value {
        json!({"id": id, "session_id": {"value": session, "basis": "event"},
            "signal_kind": kind, "created_at": at})
    }

    #[test]
    fn evals_ledger_scores_a_go_node_and_gates_a_persona_edit_on_a_gain() {
        let closed = DateTime::parse_from_rfc3339("2026-11-10T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let details = "cluster: layer=harness kind=no_progress\nsession s1 span t-s1/a\nsession s2 span t-s2/b";
        let traces = vec![
            trace("t1", "s1", "no_progress", "2026-11-05T00:00:00Z"),
            trace("t2", "s2", "no_progress", "2026-11-06T00:00:00Z"),
            trace("t3", "s3", "no_progress", "2026-11-12T00:00:00Z"),
            trace("t4", "s4", "revert", "2026-11-12T00:00:00Z"),
        ];
        let row = go_row(
            "go-1",
            details,
            Some("https://example.test/pr/9"),
            closed,
            &traces,
        );
        assert_eq!(row.trace_ids, ["t1", "t2"]);
        assert_eq!((row.score_before, row.score_after), (Some(2.0), Some(1.0)));
        assert_eq!(row.outcome, Outcome::Improved);
        assert_eq!(row.pr.as_deref(), Some("https://example.test/pr/9"));
        assert_eq!(row.golden, None);
        let line = serde_json::to_value(&row).unwrap();
        assert_eq!(line.pointer("/golden"), Some(&Value::Null), "{line}");
        assert_eq!(line.pointer("/go_node"), Some(&json!("go-1")), "{line}");

        // Append lands once, and a persona edit without a gain is refused.
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        let base: Learning = serde_json::from_value(json!({"change": "c1", "kind": "pr",
            "trace_ids": [], "golden": "g", "score_before": 0.4, "pr": "u",
            "score_after": 0.6, "outcome": "improved"}))
        .unwrap();
        assert!(append(state, base.clone()).unwrap());
        assert!(!append(state, base).unwrap());
        let mut persona: Learning =
            serde_json::from_value(json!({"change": "persona-new-user@abc",
            "kind": "persona", "trace_ids": [], "score_before": 0.8, "score_after": 0.8,
            "outcome": "unmeasured"}))
            .unwrap();
        let refused = append(state, persona.clone()).unwrap_err().to_string();
        assert!(refused.contains("did not rise"), "{refused}");
        persona.score_after = Some(0.85);
        assert!(append(state, persona).unwrap());
        let rows = read(state);
        assert_eq!(rows.len(), 2);
        let last = rows.last().unwrap();
        assert_eq!(last.outcome, Outcome::Improved);
        assert!(last.recorded_at.is_some());
    }
}
