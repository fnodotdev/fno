//! Per-PR attention for the doctor door: the human cost per merged
//! PR in a day window - user turns, questions asked, wait time - beside the
//! per-day guardrail (reverts, hours main was red). The store readers live
//! in `attention_ledger`; nothing here re-reads a store this crate already
//! reads. Missing transcripts and failed reads degrade to notes and floors,
//! never zeroes that look measured.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::attention_ledger::{
    merged_nodes, parse_ts, question_rows, revert_rows, round1, MergedNode, QuestionRow,
};

/// One completed workflow-run row from `gh run list --branch main`, typed at
/// the boundary. Red spans reconstruct from these stamps; no history store
/// exists, so this read is the whole measurement.
#[derive(Debug, Deserialize)]
pub struct MainRun {
    #[serde(default)]
    pub conclusion: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

/// Why the gh read failed, typed so no string error joins the crate.
#[derive(Debug)]
pub enum RunsError {
    Gh(String),
}

/// One merged PR's attention row. A `None` turn count is a missing
/// transcript floor, printed as unmeasured, never a zero that looks read.
#[derive(Debug, Clone, Serialize)]
pub struct PerPrAttention {
    pub node: String,
    pub pr: Option<i64>,
    pub user_turns: Option<usize>,
    pub questions_asked: usize,
    pub questions_open: usize,
    pub wait_hours_median: Option<f64>,
    pub wait_open_age_hours: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// One UTC day's guardrail line.
#[derive(Debug, Clone, Serialize)]
pub struct GuardrailDay {
    pub day: String,
    pub reverts: usize,
    pub red_main_hours: f64,
}

/// The composed report the doctor door prints.
#[derive(Debug, Clone, Serialize)]
pub struct PerPrReport {
    pub days: u32,
    pub rows: Vec<PerPrAttention>,
    pub guardrail: Vec<GuardrailDay>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// The pure per-PR attribution fold: attribute windowed questions to merged
/// nodes, wait from the page stamps (answered minus asked, median; an open
/// question carries its age as a floor), and attach the turn counts the
/// transcript scan measured. A node with no questions still prints its row.
pub fn fold_per_pr_inputs(
    now: DateTime<Utc>,
    window_start: DateTime<Utc>,
    merged: &[MergedNode],
    questions: &[QuestionRow],
    turn_counts: &BTreeMap<String, Option<usize>>,
) -> Vec<PerPrAttention> {
    merged
        .iter()
        .map(|m| {
            let mut notes = Vec::new();
            let attributed: Vec<&QuestionRow> = questions
                .iter()
                .filter(|q| {
                    !q.node.trim().is_empty()
                        && q.node.trim() == m.node.trim()
                        && q.asked >= window_start
                })
                .collect();
            let mut waits: Vec<f64> = Vec::new();
            let mut open_age: Option<f64> = None;
            let mut open = 0usize;
            for q in &attributed {
                match q.answered {
                    Some(a) if a >= q.asked => {
                        waits.push((a - q.asked).num_seconds() as f64 / 3600.0)
                    }
                    _ => {
                        open += 1;
                        let age = (now - q.asked).num_seconds().max(0) as f64 / 3600.0;
                        open_age = Some(open_age.map_or(age, |v: f64| v.max(age)));
                    }
                }
            }
            waits.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let median = match waits.len() {
                0 => None,
                n if n % 2 == 1 => waits.get(n / 2).copied(),
                n => match (waits.get(n / 2 - 1), waits.get(n / 2)) {
                    (Some(a), Some(b)) => Some((a + b) / 2.0),
                    _ => None,
                },
            }
            .map(round1);
            let user_turns = match turn_counts.get(&m.node) {
                Some(c) => *c,
                None => {
                    notes.push("turns unmeasured: no session transcript resolved".to_string());
                    None
                }
            };
            PerPrAttention {
                node: m.node.clone(),
                pr: m.pr,
                user_turns,
                questions_asked: attributed.len(),
                questions_open: open,
                wait_hours_median: median,
                wait_open_age_hours: open_age.map(round1),
                notes,
            }
        })
        .collect()
}

/// The pure per-PR compose: everything the report states comes from these
/// inputs, so the gh-failure arm tests without a network.
pub fn compose_per_pr(
    now: DateTime<Utc>,
    days: u32,
    questions: &[QuestionRow],
    merged: &[MergedNode],
    turn_counts: &BTreeMap<String, Option<usize>>,
    reverts: &[(String, DateTime<Utc>)],
    runs: Result<Vec<MainRun>, RunsError>,
) -> PerPrReport {
    let window_start = now - Duration::days(days as i64);
    let rows = fold_per_pr_inputs(now, window_start, merged, questions, turn_counts);
    let mut notes: Vec<String> = Vec::new();
    let red: Vec<(String, f64)> = match runs {
        Ok(runs) => red_span_hours(&runs),
        Err(RunsError::Gh(why)) => {
            notes.push(format!("main runs unreadable: {why}"));
            Vec::new()
        }
    };
    let mut revert_per_day: BTreeMap<String, usize> = BTreeMap::new();
    for (_, ts) in reverts {
        if *ts >= window_start {
            *revert_per_day
                .entry(ts.format("%Y-%m-%d").to_string())
                .or_insert(0) += 1;
        }
    }
    let mut guardrail = Vec::new();
    let mut day = window_start.date_naive();
    let today = now.date_naive();
    while day <= today {
        let key = day.format("%Y-%m-%d").to_string();
        guardrail.push(GuardrailDay {
            day: key.clone(),
            reverts: revert_per_day.remove(&key).unwrap_or(0),
            red_main_hours: red
                .iter()
                .find(|(d, _)| *d == key)
                .map(|(_, h)| *h)
                .unwrap_or(0.0),
        });
        day = match day.succ_opt() {
            Some(d) => d,
            None => break,
        };
    }
    notes.extend(rows.iter().flat_map(|r| r.notes.iter().cloned()));
    PerPrReport {
        days,
        rows,
        guardrail,
        notes,
    }
}

/// Red-main hours per UTC day from completed workflow runs: red starts when
/// a failure run completes and ends when the next success completes. Matrix
/// workflows re-run one subject, so same-instant rows order failures first,
/// which only widens a span - the hours stay a floor by construction.
pub fn red_span_hours(runs: &[MainRun]) -> Vec<(String, f64)> {
    let mut ends: Vec<(DateTime<Utc>, bool)> = runs
        .iter()
        .filter_map(|r| {
            let end = parse_ts(&r.updated_at)?;
            let failed = r.conclusion == "failure";
            let ok = r.conclusion == "success";
            if !(failed || ok) {
                return None;
            }
            Some((end, failed))
        })
        .collect();
    ends.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    let mut per_day: BTreeMap<String, f64> = BTreeMap::new();
    let mut red_since: Option<DateTime<Utc>> = None;
    for (end, failed) in ends {
        match (red_since, failed) {
            (None, true) => red_since = Some(end),
            (Some(start), false) => {
                add_red_span(&mut per_day, start, end);
                red_since = None;
            }
            _ => {}
        }
    }
    per_day
        .into_iter()
        .map(|(day, hours)| (day, round1(hours)))
        .collect()
}

/// One red span, split at UTC midnight so each day carries its own hours.
fn add_red_span(per_day: &mut BTreeMap<String, f64>, start: DateTime<Utc>, end: DateTime<Utc>) {
    if end <= start {
        return;
    }
    let mut cursor = start;
    while cursor < end {
        let next_day = cursor
            .date_naive()
            .succ_opt()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|t| t.and_utc())
            .unwrap_or(end);
        let boundary = next_day.min(end);
        *per_day
            .entry(cursor.format("%Y-%m-%d").to_string())
            .or_insert(0.0) += (boundary - cursor).num_seconds() as f64 / 3600.0;
        cursor = boundary;
    }
}

/// The composed read: the ledger's store readers, the transcript scan, and
/// the gh read. Only a cwd whose stores all fail prints an empty report;
/// every other gap degrades to a note.
pub fn compute_per_pr(cwd: &Path, days: u32, now: DateTime<Utc>) -> PerPrReport {
    let mut notes: Vec<String> = Vec::new();
    let qdir = crate::gate::escalation::questions_dir(cwd);
    let questions = question_rows(&qdir, &mut notes);
    let merged = merged_nodes(&mut notes);
    let mut turn_counts: BTreeMap<String, Option<usize>> = BTreeMap::new();
    for node in &merged {
        if !turn_counts.contains_key(&node.node) {
            turn_counts.insert(
                node.node.clone(),
                window_user_turns(node, now - Duration::days(days as i64), &mut notes),
            );
        }
    }
    let reverts = revert_rows(
        &super::attention_ledger::Window {
            label: String::new(),
            start: now - Duration::days(days as i64),
        },
        cwd,
        &mut notes,
    );
    let runs = main_runs(run_limit(days));
    compose_per_pr(now, days, &questions, &merged, &turn_counts, &reverts, runs)
}

/// The gh page size: enough runs to cover the window's days, bounded so a
/// long window cannot walk the rate limit.
fn run_limit(days: u32) -> usize {
    (days as usize * 40).clamp(30, 200)
}

/// Completed main-branch workflow runs via the gh CLI, typed at the
/// boundary. A failure carries the trimmed stderr in a typed error.
fn main_runs(limit: usize) -> Result<Vec<MainRun>, RunsError> {
    // Both streams pipe: wait_with_output only collects what is piped, so
    // gh's own progress lines never leak into this read's output.
    let out = std::process::Command::new("gh")
        .args([
            "run",
            "list",
            "--branch",
            "main",
            "--limit",
            &limit.to_string(),
            "--json",
            "conclusion,createdAt,updatedAt",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|child| child.wait_with_output());
    let out = match out {
        Ok(out) => out,
        Err(e) => return Err(RunsError::Gh(format!("spawn failed: {e}"))),
    };
    if !out.status.success() {
        return Err(RunsError::Gh(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| RunsError::Gh(format!("json unreadable: {e}")))
}

/// User turns in the window across a merged node's claude sessions: each
/// transcript line the provenance reader calls a user turn, stamped in the
/// window. The transcript root follows CLAUDE_CONFIG_DIR, the same alias
/// rule the account lanes use. A missing transcript or a non-claude harness
/// yields None plus a note; a None is a floor, never a zero.
fn window_user_turns(
    node: &MergedNode,
    window_start: DateTime<Utc>,
    notes: &mut Vec<String>,
) -> Option<usize> {
    if node.sessions.is_empty() {
        notes.push(format!("{}: no sessions recorded", node.node));
        return None;
    }
    let project_dirs = crate::spawn::claude_ask::ClaudeHome::from_env().project_dirs();
    let munged = node.cwd.replace('/', "-");
    let mut total = 0usize;
    let mut resolved = 0usize;
    for session in &node.sessions {
        if session.harness != "claude" {
            notes.push(format!(
                "{}: harness {} turns unmeasured",
                node.node, session.harness
            ));
            continue;
        }
        // The line shape is heterogeneous across harnesses, so each row is
        // read through the provenance helpers instead of a typed struct,
        // the same idiom the operator-turn scan uses.
        let path = project_dirs
            .iter()
            .map(|root| {
                root.join(&munged)
                    .join(format!("{}.jsonl", session.session_id))
            })
            .find(|candidate| candidate.is_file());
        let Some(path) = path else {
            notes.push(format!(
                "{}: transcript missing across {} project root(s)",
                node.node,
                project_dirs.len()
            ));
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            notes.push(format!(
                "{}: transcript unreadable at {}",
                node.node,
                path.display()
            ));
            continue;
        };
        resolved += 1;
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                continue;
            };
            if !crate::runtime::provenance::is_user_turn(&v) {
                continue;
            }
            let Some(ts) = crate::runtime::provenance::turn_ts_epoch(&v) else {
                continue;
            };
            if ts >= window_start.timestamp() as f64 {
                total += 1;
            }
        }
    }
    (resolved > 0).then_some(total)
}

/// The doctor door's output: one line per merged PR, then the guardrail
/// days, then the notes. `None` prints unmeasured; nothing prints a zero
/// that a missing read produced.
pub fn render_per_pr(report: &PerPrReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("attention per PR, last {} day(s):\n", report.days));
    for row in &report.rows {
        let pr = match row.pr {
            Some(n) => n.to_string(),
            None => "none".to_string(),
        };
        let turns = match row.user_turns {
            Some(t) => t.to_string(),
            None => "unmeasured".to_string(),
        };
        let wait = match row.wait_hours_median {
            Some(w) => format!("{w}h"),
            None => "none".to_string(),
        };
        let open_age = match row.wait_open_age_hours {
            Some(a) => format!(", open age {a}h"),
            None => String::new(),
        };
        let row_notes = if row.notes.is_empty() {
            String::new()
        } else {
            format!(" [{}]", row.notes.join("; "))
        };
        out.push_str(&format!(
            "  PR {pr} ({}): {turns} user turn(s), {} question(s) ({} open), wait {wait}{open_age}{row_notes}\n",
            row.node, row.questions_asked, row.questions_open,
        ));
    }
    out.push_str("guardrail:\n");
    for day in &report.guardrail {
        out.push_str(&format!(
            "  {}: {} revert(s), {}h main red\n",
            day.day, day.reverts, day.red_main_hours
        ));
    }
    for note in &report.notes {
        out.push_str(&format!("note: {note}\n"));
    }
    out
}

/// The `--per-pr` arm of the attention-ledger door: compose and print.
/// Exit 0 even when reads degrade; the notes carry what was missed.
pub fn run_per_pr(cwd: &Path, days: u32, as_json: bool) -> i32 {
    let now = Utc::now();
    let report = compute_per_pr(cwd, days, now);
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else {
        println!("{}", render_per_pr(&report));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
    }

    fn q(node: &str, asked: DateTime<Utc>, answered: Option<DateTime<Utc>>) -> QuestionRow {
        QuestionRow {
            node: node.to_string(),
            title: "ask".to_string(),
            asked,
            answered,
        }
    }

    fn m(node: &str, pr: Option<i64>) -> MergedNode {
        MergedNode {
            created: ts(2026, 11, 10, 8),
            completed: ts(2026, 11, 10, 15),
            node: node.to_string(),
            pr,
            sessions: Vec::new(),
            cwd: String::new(),
        }
    }

    /// AC1-HP: rows attribute per node, wait reads the page stamps (answered
    /// minus asked, median), an open question carries its age as a floor,
    /// and an unattributed question lands on no row.
    #[test]
    fn fold_per_pr_attributes_and_measures_wait() {
        let now = ts(2026, 11, 20, 12);
        let questions = vec![
            q("x-1", ts(2026, 11, 15, 9), Some(ts(2026, 11, 15, 10))),
            q("x-1", ts(2026, 11, 16, 9), Some(ts(2026, 11, 16, 12))),
            q("x-1", ts(2026, 11, 17, 9), None),
            q("x-2", ts(2026, 11, 15, 9), None),
            q("", ts(2026, 11, 15, 9), None),
        ];
        let merged = vec![m("x-1", Some(157)), m("x-9", Some(158))];
        let mut turns = BTreeMap::new();
        turns.insert("x-1".to_string(), Some(12usize));
        let rows = fold_per_pr_inputs(now, ts(2026, 11, 13, 0), &merged, &questions, &turns);
        assert_eq!(rows.len(), 2);
        let x1 = rows.iter().find(|r| r.node == "x-1").unwrap();
        assert_eq!(x1.pr, Some(157));
        assert_eq!(x1.user_turns, Some(12));
        assert_eq!(x1.questions_asked, 3);
        assert_eq!(x1.questions_open, 1);
        assert_eq!(x1.wait_hours_median, Some(2.5));
        assert_eq!(x1.wait_open_age_hours, Some(75.0));
        let x9 = rows.iter().find(|r| r.node == "x-9").unwrap();
        assert_eq!(x9.questions_asked, 0);
        assert_eq!(x9.wait_hours_median, None);
    }

    /// Red spans: a failure's completion until the next success's completion
    /// lands on the failure's day; a span crossing midnight splits per day.
    #[test]
    fn red_span_hours_splits_at_midnight() {
        let runs = vec![
            MainRun {
                conclusion: "success".into(),
                created_at: String::new(),
                updated_at: "2026-11-10T09:00:00Z".into(),
            },
            MainRun {
                conclusion: "failure".into(),
                created_at: String::new(),
                updated_at: "2026-11-10T10:00:00Z".into(),
            },
            MainRun {
                conclusion: "success".into(),
                created_at: String::new(),
                updated_at: "2026-11-11T01:30:00Z".into(),
            },
        ];
        let red = red_span_hours(&runs);
        assert_eq!(red.len(), 2);
        assert_eq!(red.first(), Some(&("2026-11-10".to_string(), 14.0)));
        assert_eq!(red.get(1), Some(&("2026-11-11".to_string(), 1.5)));
    }

    /// AC1-EDGE: a failed gh read degrades to a note with the guardrail
    /// still printed day by day, and the report carries no invented hours.
    #[test]
    fn compose_degrades_gh_failure_to_a_note() {
        let now = ts(2026, 11, 20, 12);
        let merged = vec![m("x-1", Some(157))];
        let report = compose_per_pr(
            now,
            2,
            &[],
            &merged,
            &BTreeMap::new(),
            &[],
            Err(RunsError::Gh("rate limited".to_string())),
        );
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.guardrail.len(), 3);
        assert!(report
            .notes
            .iter()
            .any(|n| n.contains("main runs unreadable: rate limited")));
        assert!(report.guardrail.iter().all(|d| d.red_main_hours == 0.0));
    }
}
