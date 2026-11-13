//! The attention ledger: the orchestration tax fno's own user pays, one ISO
//! week at a time (node x-5a74). Per merged PR it counts the questions filed
//! against the user's attention, the asks repeated on one subject, the merged
//! work later reverted, and the hours from node creation to merge.
//!
//! Reads only existing stores - the vault question files, the graph store,
//! the events journals, and local git history. No new store, no new daemon.
//! Missing or unreadable stores degrade to zeroed numbers with a note (the
//! feed.rs contract: a read is a report, never a refusal).

use chrono::{DateTime, Datelike, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The week the ledger reports: `2026-W46` plus its Monday 00:00 UTC start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub label: String,
    pub start: DateTime<Utc>,
}

/// One filed question the window covers, from a vault question page.
#[derive(Debug, Clone)]
pub struct QuestionRow {
    pub node: String,
    pub title: String,
    pub asked: DateTime<Utc>,
}

/// One merged node the window covers, from the graph store.
#[derive(Debug, Clone)]
pub struct MergedNode {
    pub id: String,
    pub created: DateTime<Utc>,
    pub completed: DateTime<Utc>,
}

/// The ledger itself. Hours carry one decimal; `None` means no merged PRs in
/// the window, so no ratio and no cycle numbers exist.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Ledger {
    pub week: String,
    pub questions_filed: usize,
    pub merged_prs: usize,
    pub questions_per_merged_pr: Option<f64>,
    pub repeated_subjects: usize,
    pub reverts: usize,
    pub cycle_hours_median: Option<f64>,
    pub cycle_hours_p90: Option<f64>,
    pub guard_refusals: usize,
    pub operator_queue_depth: Option<usize>,
    /// Degradation notes: a store that was missing or unreadable, so a
    /// number reads as a floor rather than a count.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// The hard-failure shape. Every store read degrades to a note instead, so
/// only an unusable read (the graph store cannot open at all) stops here.
#[derive(Debug)]
pub enum LedgerError {
    Io(String),
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::Io(why) => write!(f, "attention ledger io: {why}"),
        }
    }
}

/// The ISO week holding `now`: label and Monday 00:00 UTC start.
pub fn week_window(now: DateTime<Utc>) -> Window {
    let monday = now.date_naive() - Duration::days(now.weekday().num_days_from_monday() as i64);
    let start = monday
        .and_hms_opt(0, 0, 0)
        .map(|d| d.and_utc())
        .unwrap_or_else(|| now);
    Window {
        label: format!("{}-W{:02}", now.iso_week().year(), now.iso_week().week()),
        start,
    }
}

/// The subject key a question groups under for the repeated-ask count: the
/// node it names, or the first 60 chars of its folded title when the page
/// names no node.
fn subject_key(row: &QuestionRow) -> String {
    if row.node.trim().is_empty() || row.node == "none" {
        let folded: String = row.title.chars().fold(String::new(), |mut acc, c| {
            if c.is_alphanumeric() {
                acc.push(c.to_ascii_lowercase());
            }
            acc
        });
        folded.chars().take(60).collect()
    } else {
        format!("node:{}", row.node.trim())
    }
}

/// The pure core: everything the ledger states comes from these inputs.
pub fn fold(
    window: &Window,
    questions: &[QuestionRow],
    merged: &[MergedNode],
    refusals: usize,
    reverts: usize,
    queue_depth: Option<usize>,
) -> Ledger {
    let in_week = |ts: &DateTime<Utc>| *ts >= window.start;
    let filed: Vec<&QuestionRow> = questions.iter().filter(|q| in_week(&q.asked)).collect();
    let merged_in: Vec<&MergedNode> = merged.iter().filter(|m| in_week(&m.completed)).collect();

    let mut subjects: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for q in &filed {
        *subjects.entry(subject_key(q)).or_insert(0) += 1;
    }
    let repeated = subjects.values().filter(|n| **n >= 2).count();

    let mut cycle: Vec<f64> = merged_in
        .iter()
        .map(|m| (m.completed - m.created).num_seconds() as f64 / 3600.0)
        .collect();
    cycle.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pick = |rank: usize| -> Option<f64> {
        if cycle.is_empty() {
            return None;
        }
        let idx = rank.saturating_sub(1).min(cycle.len() - 1);
        Some((cycle[idx] * 10.0).round() / 10.0)
    };
    let median = if cycle.is_empty() {
        None
    } else if cycle.len() % 2 == 1 {
        pick(cycle.len() / 2 + 1)
    } else {
        let a = cycle[cycle.len() / 2 - 1];
        let b = cycle[cycle.len() / 2];
        Some(((a + b) / 2.0 * 10.0).round() / 10.0)
    };
    let p90 = pick((cycle.len() as f64 * 0.9).ceil() as usize);

    Ledger {
        week: window.label.clone(),
        questions_filed: filed.len(),
        merged_prs: merged_in.len(),
        questions_per_merged_pr: match merged_in.len() {
            0 => None,
            n => Some((filed.len() as f64 / n as f64 * 10.0).round() / 10.0),
        },
        repeated_subjects: repeated,
        reverts,
        cycle_hours_median: median,
        cycle_hours_p90: p90,
        guard_refusals: refusals,
        operator_queue_depth: queue_depth,
        notes: Vec::new(),
    }
}

/// One checkin/CLI line for the ledger. Zero merged PRs prints the raw asks
/// and no ratio, never a division artifact.
pub fn render_line(ledger: &Ledger) -> String {
    let ratio = match ledger.questions_per_merged_pr {
        Some(r) => format!("{r} questions/merged PR"),
        None => "no merged PRs".to_string(),
    };
    let queue = match ledger.operator_queue_depth {
        Some(d) => format!(", operator queue {d}"),
        None => String::new(),
    };
    let cycle = match (ledger.cycle_hours_median, ledger.cycle_hours_p90) {
        (Some(m), Some(p)) => format!(", cycle {m}h median / {p}h p90"),
        _ => String::new(),
    };
    format!(
        "attention ledger {}: {} ({} asks, {} merged), {} repeated subject(s), {} revert(s), {} guard refusal(s){}{queue}",
        ledger.week,
        ratio,
        ledger.questions_filed,
        ledger.merged_prs,
        ledger.repeated_subjects,
        ledger.reverts,
        ledger.guard_refusals,
        cycle,
        queue,
    )
}

/// The checkin reading payload: the numbers plus the pre-rendered line, so
/// the render block reads one typed field and no string-keyed JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadingPayload {
    pub week: String,
    pub line: String,
    pub ledger: Ledger,
}

impl Ledger {
    pub fn reading_payload(&self) -> ReadingPayload {
        ReadingPayload {
            week: self.week.clone(),
            line: render_line(self),
            ledger: self.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct NodeRow {
    #[serde(default)]
    id: String,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    completed_at: String,
    #[serde(default)]
    merge_status: String,
}

#[derive(Debug, Deserialize)]
struct EventRow {
    #[serde(default)]
    ts: String,
    #[serde(rename = "type", default)]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct QuestionPage {
    #[serde(default)]
    asked_at: String,
    #[serde(default)]
    node: String,
    #[serde(default)]
    title: String,
}

fn parse_ts(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Question pages under `dir` (flat) and `dir/done`, parsed through the
/// attention-file reader the vault pages already use.
fn question_rows(dir: &Path, notes: &mut Vec<String>) -> Vec<QuestionRow> {
    let mut rows = Vec::new();
    if !dir.is_dir() {
        notes.push(format!("question files missing at {}", dir.display()));
        return rows;
    }
    let mut pages = vec![dir.to_path_buf()];
    let done = dir.join("done");
    if done.is_dir() {
        pages.push(done);
    }
    for page_dir in &pages {
        let Ok(entries) = std::fs::read_dir(page_dir) else {
            notes.push(format!("question dir unreadable: {}", page_dir.display()));
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                notes.push(format!("question page unreadable: {}", path.display()));
                continue;
            };
            let Some((front, _)) = crate::runtime::attention_file::parse_page(&text) else {
                continue;
            };
            let page: QuestionPage = QuestionPage {
                asked_at: front.asked_at,
                node: front.node,
                title: front.title,
            };
            let Some(asked) = parse_ts(&page.asked_at) else {
                continue;
            };
            rows.push(QuestionRow {
                node: page.node,
                title: page.title,
                asked,
            });
        }
    }
    rows
}

/// Nodes with merge evidence, parsed at the boundary. The projection reads
/// only the four fields the ledger needs.
fn merged_nodes(notes: &mut Vec<String>) -> Vec<MergedNode> {
    let home = crate::paths::AgentsHome::from_env();
    let root = home
        .root()
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".fno"));
    let graph = crate::state_layout::graph_anchor(&root);
    let query = crate::backlog::RowQuery {
        fields: Some(
            ["id", "created_at", "completed_at", "merge_status"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        ),
        ..Default::default()
    };
    let rows =
        match crate::graph_store::read_rows_where(&crate::backlog::database_path(&graph), &query) {
            Ok(rows) => rows,
            Err(e) => {
                notes.push(format!("graph store unreadable: {e}"));
                return Vec::new();
            }
        };
    rows.into_iter()
        .filter_map(|v| serde_json::from_value::<NodeRow>(v).ok())
        .filter(|r| r.merge_status == "merged")
        .filter_map(
            |r| match (parse_ts(&r.created_at), parse_ts(&r.completed_at)) {
                (Some(created), Some(completed)) => Some(MergedNode {
                    id: r.id,
                    created,
                    completed,
                }),
                _ => None,
            },
        )
        .collect()
}

/// Guard refusals in the window: events whose type ends `_refused`, read
/// through the same journals the lead history read takes.
fn refusal_count(window: &Window, cwd: &Path, notes: &mut Vec<String>) -> usize {
    let journals = crate::lead_history::default_journals(cwd);
    let mut count = 0usize;
    for journal in &journals {
        let raw = match crate::event_store::journal_text_checked(
            &journal,
            &crate::event_store::EventQuery::of_types(&[]),
        ) {
            Ok(raw) => raw,
            Err(_) => continue,
        };
        for line in raw.lines() {
            let Ok(v) = serde_json::from_str::<EventRow>(line.trim()) else {
                continue;
            };
            if v.kind.ends_with("_refused") {
                if let Some(ts) = parse_ts(&v.ts) {
                    if ts >= window.start {
                        count += 1;
                    }
                }
            }
        }
    }
    if count == 0 {
        // A zero may be a true zero or an unreadable journal; the notes only
        // name a read that produced nothing at all.
        let any_journal = journals
            .iter()
            .any(|j| crate::event_store::store_path(j).exists() || j.exists());
        if !any_journal {
            notes.push("event journals missing".to_string());
        }
    }
    count
}

/// Reverts on the project repo's main: commits whose subject carries the
/// git revert marker, in the window. Lower bound by construction (the
/// research method's own caveat): squash reverts and unresolvable targets
/// are missed.
fn revert_count(window: &Window, cwd: &Path, notes: &mut Vec<String>) -> usize {
    let since = window.start.to_rfc3339();
    for git_ref in ["origin/main", "main", "HEAD"] {
        let out = Command::new("git")
            .args([
                "-C",
                &cwd.to_string_lossy(),
                "log",
                git_ref,
                "--since",
                &since,
                "--grep",
                "This reverts commit",
                "--format",
                "%H",
            ])
            .output();
        match out {
            Ok(out) if out.status.success() => {
                return String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .count();
            }
            Ok(out) => notes.push(format!(
                "git log {git_ref} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            Err(e) => notes.push(format!("git log {git_ref} failed: {e}")),
        }
    }
    0
}

/// Current operator turn-queue depth, as context on the line. `None` when
/// the queue cannot be resolved (no transcript for this session).
fn queue_depth(cwd: &Path) -> Option<usize> {
    let home = crate::paths::AgentsHome::from_env();
    let get = |key: &str| std::env::var(key).ok();
    let resolved = crate::operator_turns::resolve_operator_session(&get, &home, cwd).ok()?;
    let now = Utc::now().timestamp_millis() as f64 / 1000.0;
    let outcome = crate::operator_turns::read_queue(
        &resolved.session,
        &resolved.transcript,
        &resolved.capture_dir,
        now,
    )
    .ok()?;
    Some(outcome.payload.depth)
}

/// The composed read. Only an unusable graph store is a hard error; every
/// other gap degrades to a note on the ledger.
pub fn compute(cwd: &Path) -> Result<Ledger, LedgerError> {
    let now = Utc::now();
    let window = week_window(now);
    let mut notes: Vec<String> = Vec::new();
    let questions = question_rows(&crate::gate::escalation::questions_dir(cwd), &mut notes);
    let merged = merged_nodes(&mut notes);
    let refusals = refusal_count(&window, cwd, &mut notes);
    let reverts = revert_count(&window, cwd, &mut notes);
    let mut ledger = fold(
        &window,
        &questions,
        &merged,
        refusals,
        reverts,
        queue_depth(cwd),
    );
    ledger.notes = notes;
    Ok(ledger)
}

/// The vault reports dir: `<vault>/internal/<project>/reports` with the
/// space-level fallback the questions dir contract uses.
pub fn reports_dir(cwd: &Path) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    crate::gate::escalation::vault_dir_with_home(cwd, home.as_deref(), "reports")
}

fn report_name(window: &Window) -> String {
    format!("attention-ledger-{}.md", window.label)
}

/// The weekly report body: the numbers, their windows, and the lower-bound
/// caveats the measurement method itself states.
pub fn report_markdown(ledger: &Ledger) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Attention ledger {}\n\n", ledger.week));
    out.push_str(&render_line(ledger));
    out.push_str("\n\n## Numbers\n\n");
    out.push_str(&format!("- questions filed: {}\n", ledger.questions_filed));
    out.push_str(&format!("- merged PRs: {}\n", ledger.merged_prs));
    match ledger.questions_per_merged_pr {
        Some(r) => out.push_str(&format!("- questions per merged PR: {r}\n")),
        None => out.push_str("- questions per merged PR: none (no merged PRs)\n"),
    }
    out.push_str(&format!(
        "- repeated subjects: {}\n",
        ledger.repeated_subjects
    ));
    out.push_str(&format!("- reverts: {}\n", ledger.reverts));
    match (ledger.cycle_hours_median, ledger.cycle_hours_p90) {
        (Some(m), Some(p)) => out.push_str(&format!("- cycle hours median / p90: {m} / {p}\n")),
        _ => out.push_str("- cycle hours: none (no merged PRs)\n"),
    }
    out.push_str(&format!("- guard refusals: {}\n", ledger.guard_refusals));
    match ledger.operator_queue_depth {
        Some(d) => out.push_str(&format!(
            "- operator queue depth (now, not windowed): {d}\n"
        )),
        None => out.push_str("- operator queue depth: unmeasured\n"),
    }
    out.push_str("\n## Method and caveats\n\n");
    out.push_str("- Window: the ISO week, Monday 00:00 UTC to the run time.\n");
    out.push_str("- Merge time is the node completed_at stamp reconcile sets when it resolves MERGED (one sweep with merge_status).\n");
    out.push_str("- Reverts are a lower bound: commits whose subject carries the git revert marker on main; squash reverts and unresolvable targets are missed.\n");
    out.push_str("- Question pages come from the vault questions dir; a page with no parseable asked_at is skipped.\n");
    for note in &ledger.notes {
        out.push_str(&format!("- {note}\n"));
    }
    out
}

/// Write the week's report into `dir`, overwriting the same week's file.
/// Returns the path written.
pub fn write_report_into(dir: &Path, ledger: &Ledger) -> Result<PathBuf, LedgerError> {
    std::fs::create_dir_all(dir).map_err(|e| LedgerError::Io(e.to_string()))?;
    let path = dir.join(format!("attention-ledger-{}.md", ledger.week));
    std::fs::write(&path, report_markdown(ledger))
        .map_err(|e| LedgerError::Io(format!("report write failed: {e}")))?;
    Ok(path)
}

/// The beat's report write: only when this week's file is missing, so the
/// lead checkin keeps the weekly report current without a daemon and never
/// rewrites a file a human may be reading.
pub fn write_report_if_missing(cwd: &Path, ledger: &Ledger) -> Result<PathBuf, LedgerError> {
    let dir = reports_dir(cwd);
    let path = dir.join(format!("attention-ledger-{}.md", ledger.week));
    if path.exists() {
        return Ok(path);
    }
    write_report_into(&dir, ledger)
}

/// The checkin lines the ledger contributes: one line for an ok reading,
/// the named failure line for a failed one, nothing without a reading.
pub fn checkin_lines(readings: &[crate::lead_checkin::Reading]) -> Vec<String> {
    let Some(reading) = readings.iter().find(|r| r.name == "attention_ledger") else {
        return Vec::new();
    };
    if !reading.ok {
        return vec![format!("READER FAILED attention_ledger: {}", reading.error)];
    }
    match serde_json::from_value::<ReadingPayload>(reading.value.clone()) {
        Ok(p) => vec![p.line],
        Err(_) => Vec::new(),
    }
}

/// The beat's best-effort report write: only when this week's file is
/// missing, so the weekly report stays current with no new daemon and a
/// failed write never blocks the beat.
pub fn beat_write(readings: &[crate::lead_checkin::Reading], cwd: &Path) {
    let Some(payload) = readings
        .iter()
        .find(|r| r.name == "attention_ledger")
        .filter(|r| r.ok)
        .and_then(|r| serde_json::from_value::<ReadingPayload>(r.value.clone()).ok())
    else {
        return;
    };
    if let Err(e) = write_report_if_missing(cwd, &payload.ledger) {
        eprintln!("lead-checkin: attention ledger report write failed: {e}");
    }
}

/// `fno-agents attention-ledger [--json|-J] [--report]`
pub fn run_cli(args: &[String]) -> i32 {
    let mut as_json = false;
    let mut want_report = false;
    for arg in args {
        match arg.as_str() {
            "--json" | "-J" => as_json = true,
            "--report" => want_report = true,
            other => {
                eprintln!(
                    "Usage: fno inbox attention-ledger [--json|-J] [--report] (unknown argument: {other})"
                );
                return 2;
            }
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match compute(&cwd) {
        Ok(ledger) => {
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&ledger).unwrap_or_default()
                );
            } else {
                println!("{}", render_line(&ledger));
                for note in &ledger.notes {
                    eprintln!("note: {note}");
                }
            }
            if want_report {
                match write_report_into(&reports_dir(&cwd), &ledger) {
                    Ok(path) => println!("report: {}", path.display()),
                    Err(e) => {
                        eprintln!("attention-ledger: {e}");
                        return 1;
                    }
                }
            }
            0
        }
        Err(e) => {
            eprintln!("attention-ledger: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
    }

    /// The fixture window: ISO week 46 of 2026, Mon 2026-11-09 00:00 UTC.
    fn window() -> Window {
        week_window(ts(2026, 11, 11, 12))
    }

    fn q(node: &str, title: &str, asked: DateTime<Utc>) -> QuestionRow {
        QuestionRow {
            node: node.to_string(),
            title: title.to_string(),
            asked,
        }
    }

    fn m(id: &str, created: DateTime<Utc>, completed: DateTime<Utc>) -> MergedNode {
        MergedNode {
            id: id.to_string(),
            created,
            completed,
        }
    }

    /// AC1: the fold counts windowed questions and merged nodes and states
    /// the ratio; a page dated before the window stays out.
    #[test]
    fn fold_counts_windowed_questions_and_merged_nodes() {
        let w = window();
        let questions = vec![
            q("x-1", "first ask", ts(2026, 11, 10, 9)),
            q("", "node-less ask", ts(2026, 11, 10, 10)),
            q("x-2", "another ask", ts(2026, 11, 10, 11)),
            q("x-1", "last week ask", ts(2026, 11, 5, 9)),
        ];
        let merged = vec![m("x-9", ts(2026, 11, 5, 8), ts(2026, 11, 10, 15))];
        let l = fold(&w, &questions, &merged, 2, 1, Some(1));
        assert_eq!(l.questions_filed, 3);
        assert_eq!(l.merged_prs, 1);
        assert_eq!(l.questions_per_merged_pr, Some(3.0));
        // created 11-05 08:00 to completed 11-10 15:00 = 127.0h
        assert_eq!(l.cycle_hours_median, Some(127.0));
        assert_eq!(l.cycle_hours_p90, Some(127.0));
        assert_eq!(l.guard_refusals, 2);
        assert_eq!(l.reverts, 1);
    }

    /// Zero merged PRs state no ratio and no cycle numbers, never a division
    /// artifact.
    #[test]
    fn fold_zero_merged_states_no_ratio() {
        let w = window();
        let l = fold(&w, &[q("x-1", "ask", ts(2026, 11, 10, 9))], &[], 0, 0, None);
        assert_eq!(l.questions_filed, 1);
        assert_eq!(l.merged_prs, 0);
        assert_eq!(l.questions_per_merged_pr, None);
        assert_eq!(l.cycle_hours_median, None);
        assert_eq!(l.cycle_hours_p90, None);
    }

    /// AC2: repeated subjects group by node, and node-less questions group
    /// by folded title; two asks on one subject count once.
    #[test]
    fn fold_groups_repeated_subjects_by_node_then_title() {
        let w = window();
        let questions = vec![
            q("x-1", "first ask", ts(2026, 11, 10, 9)),
            q("x-1", "asked again", ts(2026, 11, 10, 10)),
            q("", "Duplicate Subject Here", ts(2026, 11, 10, 11)),
            q("", "duplicate subject here", ts(2026, 11, 10, 12)),
            q("x-3", "single ask", ts(2026, 11, 10, 13)),
            q("x-3", "last week", ts(2026, 11, 4, 9)),
        ];
        let l = fold(&w, &questions, &[], 0, 0, None);
        assert_eq!(l.repeated_subjects, 2);
    }

    /// AC5+AC6: the report lands at the ISO-week name, names the four
    /// metrics, and a second write overwrites the same file.
    #[test]
    fn report_write_is_idempotent_per_week() {
        let w = window();
        let ledger = fold(
            &w,
            &[q("x-1", "ask", ts(2026, 11, 10, 9))],
            &[],
            1,
            0,
            Some(0),
        );
        let dir = tempfile::tempdir().unwrap();
        let first = write_report_into(dir.path(), &ledger).unwrap();
        assert_eq!(
            first.file_name().unwrap().to_str().unwrap(),
            "attention-ledger-2026-W46.md"
        );
        let second = write_report_into(dir.path(), &ledger).unwrap();
        assert_eq!(first, second);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names.len(), 1);
        let body = std::fs::read_to_string(&first).unwrap();
        for metric in [
            "questions per merged PR",
            "repeated subjects",
            "reverts",
            "cycle hours",
        ] {
            assert!(body.contains(metric), "report missing: {metric}");
        }
    }
}
