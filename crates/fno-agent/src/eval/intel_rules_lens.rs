//! `fno-agent intel --lens rules`: how often each event rule fires, and how
//! often it fires wrongly. Each rule is a guess about a failure, and each
//! fire is a `decision_span` row carrying `rule`, so the counts are a journal
//! read. A judge marks sampled fires in a labels file, and the lens reads the
//! file back as a false-fire rate. Where fnod scored a fire (`fnod_is_ask`),
//! the yes and no columns split the fires by its advisory answer.
//!
//! The shadow judge adds a section. Every Stop writes one request row to the
//! engine's judge file; `--score` asks fnod about the unscored rows, off the
//! Stop path; and the section prints coverage per machine and day, verdict
//! counts with the share a labels file calls wrong, the ask pair beside the
//! keyword lint, and the laws with no tag.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

const DAY_MS: i64 = 86_400_000;
/// fnod's is_ask probability at or above this reads as yes.
const FNOD_YES: f64 = 0.5;

pub struct Fire {
    pub span_id: String,
    pub ts: String,
    pub session: String,
    /// What a judge reads: a block's reply tail, else the matched event (a
    /// question id, a span id, or the `stop:<session>:<turn>` key).
    pub evidence: Option<String>,
    /// fnod's advisory is_ask probability, when the fire was scored.
    pub fnod: Option<f64>,
}

pub struct RuleRow {
    pub rule: String,
    pub kind: String,
    pub fires: usize,
    pub sessions: usize,
    pub last_7d: usize,
    pub labeled: usize,
    pub false_fires: usize,
    pub fnod_yes: usize,
    pub fnod_no: usize,
    /// Unlabeled fires first, newest first, so each labeling pass moves on.
    pub sample: Vec<Fire>,
}

/// One `{"span_id": "s-...", "verdict": "true"|"false"}` per line. A true
/// verdict means the fire was right. A later line for the same span wins.
pub fn parse_labels(text: &str) -> Result<HashMap<String, bool>, String> {
    let mut out = HashMap::new();
    for (n, line) in text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let v: Value = serde_json::from_str(line).unwrap_or(Value::Null);
        let verdict = v["verdict"]
            .as_str()
            .filter(|s| ["true", "false"].contains(s));
        match (v["span_id"].as_str(), verdict.map(|s| s == "true")) {
            (Some(span), Some(verdict)) => out.insert(span.to_string(), verdict),
            _ => {
                return Err(format!(
                    "labels line {}: needs span_id and a verdict of \"true\" or \"false\"",
                    n + 1
                ))
            }
        };
    }
    Ok(out)
}

/// Fold `decision_span` rows (ts_ms, line) into one row per rule and span
/// kind, most fires first. A `sample` of `None` keeps every fire.
pub fn fold(
    rows: &[(i64, String)],
    labels: &HashMap<String, bool>,
    now_ms: i64,
    sample: Option<usize>,
) -> Vec<RuleRow> {
    let mut by_rule: BTreeMap<(String, String), Vec<(i64, Fire)>> = BTreeMap::new();
    for (ts_ms, line) in rows {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let data = &v["data"];
        let Some(rule) = data["rule"].as_str() else {
            continue;
        };
        let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
        let fire = Fire {
            span_id: text(&data["trace"]["span_id"]),
            ts: text(&v["ts"]),
            session: text(&data["trace"]["actor_session"]),
            evidence: data["excerpt"]
                .as_str()
                .or(data["matched_event"].as_str())
                .map(str::to_string),
            fnod: data["fnod_is_ask"].as_f64(),
        };
        let key = (rule.to_string(), text(&data["span_kind"]));
        by_rule.entry(key).or_default().push((*ts_ms, fire));
    }
    let mut out: Vec<RuleRow> = by_rule
        .into_iter()
        .map(|((rule, kind), mut fires)| {
            let verdicts: Vec<bool> = fires
                .iter()
                .filter_map(|(_, f)| labels.get(&f.span_id).copied())
                .collect();
            let sessions: BTreeSet<&str> = fires.iter().map(|(_, f)| f.session.as_str()).collect();
            let sessions = sessions.len();
            let last_7d = fires
                .iter()
                .filter(|(ts, _)| now_ms - ts <= 7 * DAY_MS)
                .count();
            let fnod_yes = fires
                .iter()
                .filter(|(_, f)| f.fnod.is_some_and(|p| p >= FNOD_YES))
                .count();
            let fnod_no = fires
                .iter()
                .filter(|(_, f)| f.fnod.is_some_and(|p| p < FNOD_YES))
                .count();
            fires.sort_by_key(|(ts, f)| (labels.contains_key(&f.span_id), -ts));
            let total = fires.len();
            RuleRow {
                rule,
                kind,
                fires: total,
                sessions,
                last_7d,
                labeled: verdicts.len(),
                false_fires: verdicts.iter().filter(|v| !**v).count(),
                fnod_yes,
                fnod_no,
                sample: fires
                    .into_iter()
                    .take(sample.unwrap_or(total))
                    .map(|(_, f)| f)
                    .collect(),
            }
        })
        .collect();
    out.sort_by(|a, b| b.fires.cmp(&a.fires).then_with(|| a.rule.cmp(&b.rule)));
    out
}

/// How many requests one scorer run scores, so one lens run cannot sit on
/// fnod for hours. The rest wait for the next run.
const SCORE_CAP: usize = 2_000;
/// Failed passes in a row that end a scorer run.
const SCORE_GIVE_UP: usize = 3;
/// The one Stop text lint. Its blocks are one half of the ask pair.
const ASK_LINT: &str = "chat_ask_unfiled";

/// The shadow judge's rows in one window. A request row is written at each
/// Stop. The scorer appends a verdict row per question it answers.
#[derive(Default)]
pub struct Judge {
    pub requests: usize,
    pub scored: usize,
    pub skipped: usize,
    pub pending: usize,
    /// Skipped requests by the fnod state their row read.
    pub skipped_by: BTreeMap<String, usize>,
    /// (machine, day) to its counts.
    pub coverage: BTreeMap<(String, String), Covered>,
    /// The judge's label (a question id or a D number) to its verdicts.
    pub verdicts: BTreeMap<String, Verdicts>,
    /// Every scored `is_ask` pass's `fnod_ms`, in file order.
    pub ms: Vec<f64>,
    /// Stop keys where the keyword ask lint blocked, and where the judge
    /// read an ask.
    pub keyword: BTreeSet<String>,
    pub judge_ask: BTreeSet<String>,
    pub both: usize,
    pub keyword_only: usize,
    pub judge_only: usize,
    /// Verdicts no labels file marks, oldest first, for the next labeling pass.
    pub sample: Vec<Answer>,
}

/// One machine's requests on one day.
#[derive(Default)]
pub struct Covered {
    pub requests: usize,
    pub scored: usize,
    /// Requests written while fnod was off, refused or down. They carry no
    /// reply tail, so no later run can score them.
    pub skipped: usize,
}

/// One judge label's verdicts.
#[derive(Default)]
pub struct Verdicts {
    /// A verdict word to its count. A failed answer reads `error`.
    pub counts: BTreeMap<String, usize>,
    /// Verdict rows a labels file marks, and how many of those it calls wrong.
    pub labeled: usize,
    pub wrong: usize,
}

/// One verdict row with the reply tail the judge read.
pub struct Answer {
    pub span_id: String,
    pub ts: String,
    pub session: String,
    pub judge: String,
    pub verdict: String,
    pub evidence: String,
}

/// A `decision_span` block, as the event-rule engine writes it: the fields
/// the ask pair joins on.
#[derive(serde::Deserialize)]
struct BlockSpan {
    data: BlockData,
}

#[derive(serde::Deserialize)]
struct BlockData {
    rule: String,
    span_kind: String,
    matched_event: String,
}

/// A scorable request row: what its verdict rows join on, and the reply a
/// person reads to label one.
struct Request {
    span_id: String,
    stop_key: String,
    cell: (String, String),
    excerpt: Option<String>,
}

/// Fold the journal's `decision_span` rows and the judge file's rows, each
/// (ts_ms, line) and already inside the window. `labels` marks a verdict
/// row by span id, and true means the judge was right. The ask pair joins
/// the keyword lint's blocks and the judge's `is_ask` yes verdicts on the
/// stop key.
pub fn fold_judge(
    spans: &[(i64, String)],
    judge: &[crate::event_rules::JudgeRow],
    labels: &HashMap<String, bool>,
) -> Judge {
    let mut out = Judge::default();
    for (_, line) in spans {
        let Ok(block) = serde_json::from_str::<BlockSpan>(line) else {
            continue;
        };
        if block.data.rule == ASK_LINT && block.data.span_kind == "block" {
            out.keyword.insert(block.data.matched_event);
        }
    }
    // The scorable requests, in file order, each found by its span id, and
    // the ones a verdict names.
    let mut scorable: Vec<Request> = Vec::new();
    let mut seats: BTreeMap<&str, usize> = BTreeMap::new();
    let mut scored: BTreeSet<String> = BTreeSet::new();
    for row in judge {
        if row.phase == "request" {
            out.requests += 1;
            let skipped = row.fnod.as_deref() != Some("up");
            let cell = (row.machine.clone(), row.ts.chars().take(10).collect());
            let counted = out.coverage.entry(cell.clone()).or_default();
            counted.requests += 1;
            counted.skipped += usize::from(skipped);
            out.skipped += usize::from(skipped);
            if skipped {
                *out.skipped_by
                    .entry(row.fnod.clone().unwrap_or_else(|| "none".to_string()))
                    .or_insert(0) += 1;
            }
            if !skipped {
                seats.insert(row.span_id.as_str(), scorable.len());
                scorable.push(Request {
                    span_id: row.span_id.clone(),
                    stop_key: row.matched_event.clone(),
                    cell,
                    excerpt: row.excerpt.clone(),
                });
            }
            continue;
        }
        let Some(request) = row
            .judge_of
            .as_deref()
            .and_then(|id| seats.get(id).copied())
            .and_then(|at| scorable.get(at))
        else {
            continue;
        };
        if scored.insert(request.span_id.clone()) {
            if let Some(counted) = out.coverage.get_mut(&request.cell) {
                counted.scored += 1;
            }
        }
        let label = row.judge.clone().unwrap_or_default();
        let verdict = if row.fnod_error.is_some() {
            "error".to_string()
        } else {
            row.verdict.clone().unwrap_or_default()
        };
        let counts = out.verdicts.entry(label.clone()).or_default();
        *counts.counts.entry(verdict.clone()).or_insert(0) += 1;
        if label == "is_ask" {
            if let Some(ms) = row.fnod_ms {
                out.ms.push(ms);
            }
            if row.verdict.as_deref() == Some("yes") {
                out.judge_ask.insert(request.stop_key.clone());
            }
        }
        if verdict == "error" {
            continue;
        }
        match labels.get(&row.span_id).copied() {
            Some(right) => {
                counts.labeled += 1;
                counts.wrong += usize::from(!right);
            }
            None => out.sample.push(Answer {
                span_id: row.span_id.clone(),
                ts: row.ts.clone(),
                session: row.session.clone(),
                judge: label,
                verdict,
                evidence: request.excerpt.clone().unwrap_or_default(),
            }),
        }
    }
    out.scored = scored.len();
    out.pending = scorable.len() - scored.len();
    out.both = out.keyword.intersection(&out.judge_ask).count();
    out.keyword_only = out.keyword.len() - out.both;
    out.judge_only = out.judge_ask.len() - out.both;
    out
}

/// A count map as `key n` pairs, two spaces between.
fn tallies(counts: &BTreeMap<String, usize>) -> String {
    counts
        .iter()
        .map(|(key, n)| format!("{key} {n}"))
        .collect::<Vec<_>>()
        .join("  ")
}

/// The judge section, one line per string. `untagged` is the count of live
/// laws with no tag, or None when the store could not be read.
pub fn judge_lines(j: &Judge, days: u64, untagged: Option<usize>) -> Vec<String> {
    if j.requests == 0 {
        return vec![format!("judge rows: no judge rows in {days} days")];
    }
    let mut out = vec![format!(
        "judge rows (days {days}): requests {}, scored {}, skipped {}, pending {}",
        j.requests, j.scored, j.skipped, j.pending
    )];
    out.push("  coverage (machine, day)".to_string());
    for ((machine, day), counted) in &j.coverage {
        let pending = counted.requests - counted.scored - counted.skipped;
        out.push(format!(
            "  {machine} {day}: {} requests, {} scored, {} skipped, {pending} pending",
            counted.requests, counted.scored, counted.skipped
        ));
    }
    if !j.skipped_by.is_empty() {
        out.push(format!(
            "  skipped while fnod was: {}",
            tallies(&j.skipped_by)
        ));
    }
    for (label, verdicts) in &j.verdicts {
        out.push(format!("  judge:{label}: {}", tallies(&verdicts.counts)));
        if verdicts.labeled > 0 {
            out.push(format!(
                "    labeled {}, wrong {}",
                verdicts.labeled, verdicts.wrong
            ));
        }
    }
    if !j.ms.is_empty() {
        out.push(format!(
            "  fnod_ms p50 {} p90 {}",
            crate::scoreboard_provider::percentile(&j.ms, 50).unwrap_or(0.0),
            crate::scoreboard_provider::percentile(&j.ms, 90).unwrap_or(0.0)
        ));
    }
    out.push(format!(
        "  ask pair by stop key: both {}, keyword-only {}, judge-only {}",
        j.both, j.keyword_only, j.judge_only
    ));
    out.push(match untagged {
        Some(0) => "untagged laws: 0".to_string(),
        Some(n) => format!("untagged laws: {n} (tag them in the file fnod.judge.tags_file names)"),
        None => "untagged laws: the decision store is unreadable".to_string(),
    });
    out
}

/// The judge file's rows inside the window.
fn judge_window(journal: &Path, since_ms: Option<i64>) -> Vec<crate::event_rules::JudgeRow> {
    crate::event_rules::judge_rows(journal)
        .into_iter()
        .filter(|row| {
            crate::event_store::parse_rfc3339_ms(&row.ts)
                .is_some_and(|ts| since_ms.is_none_or(|since| ts >= since))
        })
        .collect()
}

/// A law row's fields the scorer and the untagged count read.
#[derive(serde::Deserialize)]
struct Law {
    decision_id: String,
    /// Missing on a row that names no text. The law still counts as untagged.
    #[serde(default)]
    decision: String,
}

/// The live laws, or None when the decision store cannot be read.
fn live_laws() -> Option<Vec<Law>> {
    let mut index = crate::decision_index::default_store_live().ok()?;
    index.rows.retain(crate::decision_index::is_law);
    Some(
        index
            .rows
            .iter()
            .filter_map(|row| serde_json::from_value(row.clone()).ok())
            .collect(),
    )
}

fn untagged_count(cwd: &Path, laws: &[Law]) -> usize {
    let tags = crate::fnod_tags::Tags::read(cwd);
    laws.iter()
        .filter(|law| tags.get(&law.decision_id) == crate::fnod_tags::Tag::Untagged)
        .count()
}

/// Score the window's scorable request rows that carry no verdict, and
/// append one verdict row per question. `laws` are the live law rows, read
/// for each law's text, and `labels` maps a law id to its D number. One
/// pass per request asks `is_ask` and `rule_obeyed` for each of its laws. A
/// law the store no longer holds is not asked. A pass fnod fails whole
/// writes no row, so the request waits for the next run and a down server
/// costs no verdict. Answers how many requests were scored.
fn score(
    cwd: &Path,
    journal: &Path,
    judge: &[crate::event_rules::JudgeRow],
    laws: &[Law],
    labels: &HashMap<String, String>,
) -> usize {
    let url = crate::fnod::configured_url(cwd);
    if url.trim().is_empty() {
        return 0;
    }
    let scored: BTreeSet<String> = judge
        .iter()
        .filter_map(|row| row.judge_of.clone())
        .collect();
    let timeout = std::time::Duration::from_millis(crate::fnod::SCORE_TIMEOUT_MS);
    let (mut done, mut misses) = (0usize, 0usize);
    for row in judge.iter().filter(|row| {
        row.phase == "request"
            && row.fnod.as_deref() == Some("up")
            && !scored.contains(&row.span_id)
    }) {
        let mut asked: Vec<(String, crate::fnod::Question)> = vec![(
            "is_ask".to_string(),
            crate::fnod::Question::configured(cwd, "is_ask"),
        )];
        let mut rules: Vec<Value> = Vec::new();
        for id in &row.rules {
            let Some(law) = laws.iter().find(|law| law.decision_id == *id) else {
                continue;
            };
            let label = labels.get(id).cloned().unwrap_or_else(|| id.clone());
            rules.push(json!({"decision": label, "text": law.decision}));
            let question = crate::fnod::rule_question(id, &label);
            asked.push((label, question));
        }
        let mut state = Map::new();
        state.insert("message".to_string(), json!(row.excerpt));
        if !rules.is_empty() {
            state.insert("rules".to_string(), json!(rules));
        }
        let names: Vec<String> = asked.iter().map(|(name, _)| name.clone()).collect();
        let questions: Vec<crate::fnod::Question> = asked.into_iter().map(|(_, q)| q).collect();
        let answers = crate::fnod::ask_many(&url, timeout, Value::Object(state), &questions);
        // A pass fnod failed whole leaves no row. An error verdict would
        // read as scored, and nobody would ask this request again.
        if answers.iter().all(|d| d.error.is_some()) {
            misses += 1;
            if misses >= SCORE_GIVE_UP {
                break;
            }
            continue;
        }
        misses = 0;
        let rows: Vec<Value> = names
            .iter()
            .zip(&questions)
            .zip(answers)
            .map(|((name, q), d)| {
                json!({
                    "ts": crate::events::now_rfc3339(),
                    "phase": "verdict",
                    "span_id": crate::decision_trace::new_span_id(),
                    "judge_of": row.span_id,
                    "judge": name,
                    "session": row.session,
                    "matched_event": row.matched_event,
                    "machine": row.machine,
                    "verdict": d.label,
                    "confidence": d.confidence,
                    "escalate": d.escalate.as_str(),
                    "trained": q.trained,
                    "fnod_ms": d.ms,
                    "fnod_is_ask": d.noul,
                    "fnod_error": d.error,
                })
            })
            .collect();
        crate::event_rules::append_judge_rows(journal, &rows);
        done += 1;
        if done >= SCORE_CAP {
            break;
        }
    }
    done
}

pub(crate) fn run_lens(
    days: u64,
    sample: &crate::intel_insights::SampleRequest,
    labels_path: Option<&str>,
    score_now: bool,
) -> i32 {
    let labels = match labels_path.map(|p| {
        std::fs::read_to_string(p)
            .map_err(|e| format!("{p}: {e}"))
            .and_then(|t| parse_labels(&t))
    }) {
        None => HashMap::new(),
        Some(Ok(l)) => l,
        Some(Err(e)) => {
            eprintln!("fno-agent intel --lens rules: {e}");
            return 2;
        }
    };
    let now_ms = crate::claims::now_ms();
    // Fire spans land in the project journal (emit_span's target).
    let journal = crate::law_match::project_events_journal();
    let since = (days > 0).then(|| now_ms - days as i64 * DAY_MS);
    let query = crate::event_store::EventQuery {
        since_ms: since,
        ..crate::event_store::EventQuery::of_types(&["decision_span"])
    };
    let rows = match crate::event_store::query_events(&journal, &query) {
        Ok(rows) => rows
            .into_iter()
            .map(|r| (r.ts_ms, r.line))
            .collect::<Vec<_>>(),
        Err(e) => {
            eprintln!("fno-agent intel --lens rules: {}: {e}", journal.display());
            return 1;
        }
    };
    let sample = match sample {
        crate::intel_insights::SampleRequest::None => Some(5),
        crate::intel_insights::SampleRequest::N(n) => Some(*n),
        crate::intel_insights::SampleRequest::All => None,
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let laws = live_laws();
    if score_now {
        let n = laws.as_ref().map_or(0, |laws| {
            score(
                &cwd,
                &journal,
                &judge_window(&journal, since),
                laws,
                &crate::numbers::labels(),
            )
        });
        println!("scored {n} judge request(s)");
    }
    let report = fold(&rows, &labels, now_ms, sample);
    println!("rule lens (days {days}): {} rule(s)", report.len());
    println!(
        "  {:<26} {:<7} {:>6} {:>8} {:>6} {:>7} {:>6} {:>8} {:>7}",
        "rule", "kind", "fires", "sessions", "last7d", "labeled", "false", "fnod_yes", "fnod_no"
    );
    for r in &report {
        let rate = match r.labeled {
            0 => String::new(),
            n => format!(" ({}%)", r.false_fires * 100 / n),
        };
        println!(
            "  {:<26} {:<7} {:>6} {:>8} {:>6} {:>7} {:>6} {:>8} {:>7}{rate}",
            r.rule,
            r.kind,
            r.fires,
            r.sessions,
            r.last_7d,
            r.labeled,
            r.false_fires,
            r.fnod_yes,
            r.fnod_no
        );
    }
    let untagged = laws.as_ref().map(|laws| untagged_count(&cwd, laws));
    let judge = fold_judge(&rows, &judge_window(&journal, since), &labels);
    for line in judge_lines(&judge, days, untagged) {
        println!("{line}");
    }
    for r in report.iter().filter(|r| !r.sample.is_empty()) {
        println!("sample {} ({}):", r.rule, r.kind);
        for f in &r.sample {
            let evidence = f.evidence.as_deref().unwrap_or("(no evidence)");
            let evidence = evidence.replace('\n', " / ");
            let fnod = f.fnod.map(|p| format!(" fnod {p:.2}")).unwrap_or_default();
            println!("  {} {} {}{fnod}: {evidence}", f.span_id, f.ts, f.session);
        }
    }
    if !judge.sample.is_empty() {
        println!("sample judge (verdicts no labels file marks):");
        for v in judge.sample.iter().take(sample.unwrap_or(usize::MAX)) {
            let evidence = v.evidence.replace('\n', " / ");
            println!(
                "  {} {} {} judge={} {}: {evidence}",
                v.span_id, v.ts, v.session, v.judge, v.verdict
            );
        }
    }
    println!("label: append {{\"span_id\":\"s-...\",\"verdict\":\"true\"|\"false\"}} lines to a file, then pass --labels <file>");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(
        days_ago: i64,
        rule: &str,
        id: &str,
        session: &str,
        excerpt: Option<&str>,
    ) -> (i64, String) {
        let mut data = serde_json::json!({"span_kind": "block", "rule": rule, "excerpt": excerpt,
            "trace": {"span_id": id, "actor_session": session}});
        // Fires s-1 and s-3 carry fnod's advisory score; s-2 was unscored.
        match id {
            "s-1" => data["fnod_is_ask"] = serde_json::json!(0.9),
            "s-3" => data["fnod_is_ask"] = serde_json::json!(0.1),
            _ => {}
        }
        (
            30 * DAY_MS - days_ago * DAY_MS,
            serde_json::json!({"type": "decision_span", "data": data}).to_string(),
        )
    }

    #[test]
    fn rule_lens_counts_fires_reads_judge_rows_and_scores_pending_ones() {
        let rows = vec![
            span(1, "chat_ask_unfiled", "s-1", "aaaa0001", Some("Which one?")),
            span(2, "chat_ask_unfiled", "s-2", "aaaa0001", None),
            span(20, "chat_ask_unfiled", "s-3", "bbbb0002", None),
            span(1, "decided_ask_fyi", "s-4", "bbbb0002", None),
            // A span with no rule is a route or ask span, not a fire.
            (
                0,
                r#"{"type":"decision_span","data":{"span_kind":"route"}}"#.to_string(),
            ),
        ];
        let labels = parse_labels(
            "{\"span_id\":\"s-1\",\"verdict\":\"true\"}\n\n{\"span_id\":\"s-3\",\"verdict\":\"false\"}\n",
        )
        .unwrap();
        let report = fold(&rows, &labels, 30 * DAY_MS, Some(2));
        assert_eq!(report.len(), 2);
        let chat = &report[0];
        assert_eq!(chat.rule, "chat_ask_unfiled");
        assert_eq!((chat.fires, chat.sessions, chat.last_7d), (3, 2, 2));
        assert_eq!((chat.labeled, chat.false_fires), (2, 1));
        assert_eq!((chat.fnod_yes, chat.fnod_no), (1, 1));
        // The unlabeled fire leads the sample so the next pass labels it.
        let ids: Vec<&str> = chat.sample.iter().map(|f| f.span_id.as_str()).collect();
        assert_eq!(ids, vec!["s-2", "s-1"]);
        assert_eq!(chat.sample[1].evidence.as_deref(), Some("Which one?"));
        assert_eq!(fold(&rows, &labels, 30 * DAY_MS, None)[0].sample.len(), 3);
        assert!(parse_labels("{\"span_id\":\"s-1\",\"verdict\":\"maybe\"}").is_err());
        judge_leg();
        scorer_leg();
    }

    /// The judge rows of a window, as the file holds them.
    fn parse(rows: &[Value]) -> Vec<crate::event_rules::JudgeRow> {
        rows.iter()
            .filter_map(|row| serde_json::from_value(row.clone()).ok())
            .collect()
    }

    /// The judge leg of the owner test: the section reads coverage per
    /// machine and day, verdict counts per judge with the labeled share a
    /// labels file calls wrong, and the ask pair beside the keyword lint.
    /// With no row it names the window.
    fn judge_leg() {
        assert_eq!(
            judge_lines(&fold_judge(&[], &[], &HashMap::new()), 7, None),
            vec!["judge rows: no judge rows in 7 days"]
        );
        let request = |machine: &str, day: &str, fnod: &str, key: &str, id: &str| {
            json!({"ts": format!("{day}T10:00:00Z"), "phase": "request", "span_id": id,
                "session": "s", "matched_event": key, "machine": machine, "fnod": fnod,
                "excerpt": "done"})
        };
        let verdict = |judge: &str, value: &str, id: &str, key: &str, ms: i64| {
            json!({"ts": "2026-11-24T10:00:00Z", "phase": "verdict",
                "span_id": format!("s-v-{id}-{judge}"),
                "judge_of": id, "judge": judge, "verdict": value, "matched_event": key,
                "fnod_ms": ms})
        };
        let judge = vec![
            request("iMac2D2", "2026-11-24", "down", "stop:a:1", "s-r1"),
            request("m2-mbp", "2026-11-23", "down", "stop:b:9", "s-r4"),
            request("m2-mbp", "2026-11-24", "up", "stop:b:1", "s-r2"),
            request("m2-mbp", "2026-11-24", "up", "stop:b:2", "s-r3"),
            request("m2-mbp", "2026-11-24", "up", "stop:b:3", "s-r5"),
            request("m2-mbp", "2026-11-24", "up", "stop:b:4", "s-r6"),
            verdict("is_ask", "yes", "s-r2", "stop:b:1", 900),
            verdict("D3241", "obeys", "s-r2", "stop:b:1", 600),
            verdict("is_ask", "no", "s-r3", "stop:b:2", 300),
            verdict("is_ask", "yes", "s-r5", "stop:b:3", 600),
        ];
        let block = |key: &str| {
            (
                0,
                json!({"type": "decision_span", "data": {"span_kind": "block",
                    "rule": "chat_ask_unfiled", "matched_event": key,
                    "trace": {"span_id": "s-k1", "actor_session": "aaaa0001"}}})
                .to_string(),
            )
        };
        // A person marks the D3241 verdict right and one is_ask no wrong.
        let labels = HashMap::from([
            ("s-v-s-r2-D3241".to_string(), true),
            ("s-v-s-r3-is_ask".to_string(), false),
        ]);
        let j = fold_judge(
            &[block("stop:b:1"), block("stop:b:2")],
            &parse(&judge),
            &labels,
        );
        assert_eq!((j.requests, j.scored, j.skipped, j.pending), (6, 3, 2, 1));
        assert_eq!((j.both, j.keyword_only, j.judge_only), (1, 1, 1));
        assert_eq!(j.ms, vec![900.0, 300.0, 600.0]);
        let marked: Vec<String> = j
            .sample
            .iter()
            .map(|a| format!("{}/{}", a.judge, a.verdict))
            .collect();
        assert_eq!(marked, ["is_ask/yes", "is_ask/yes"]);
        assert!(j.sample.iter().all(|a| a.evidence == "done"));
        let lines = judge_lines(&j, 7, Some(4));
        let text = lines.join("\n");
        for wanted in [
            "judge rows (days 7): requests 6, scored 3, skipped 2, pending 1",
            "iMac2D2 2026-11-24: 1 requests, 0 scored, 1 skipped, 0 pending",
            "m2-mbp 2026-11-23: 1 requests, 0 scored, 1 skipped, 0 pending",
            "m2-mbp 2026-11-24: 4 requests, 3 scored, 0 skipped, 1 pending",
            "skipped while fnod was: down 2",
            "judge:is_ask: no 1  yes 2\n    labeled 1, wrong 1",
            "judge:D3241: obeys 1\n    labeled 1, wrong 0",
            "fnod_ms p50 600 p90 900",
            "ask pair by stop key: both 1, keyword-only 1, judge-only 1",
            "untagged laws: 4 (tag them in the file fnod.judge.tags_file names)",
        ] {
            assert!(text.contains(wanted), "missing {wanted:?} in:\n{text}");
        }
        assert!(judge_lines(&j, 7, Some(0))
            .join("\n")
            .contains("untagged laws: 0"));
    }

    /// The scorer leg of the owner test: it asks fnod once per request, in
    /// one pass, and every verdict row names its request. A scored request
    /// is left alone on the next run, and a request fnod failed whole stays
    /// pending.
    fn scorer_leg() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".fno")).unwrap();
        let config = |url: &str| {
            std::fs::write(
                dir.path().join(".fno/config.toml"),
                format!("[fnod]\nurl = \"{url}\"\n"),
            )
            .unwrap();
        };
        let reply = r#"{"answers":{"is_ask":{"type":"noul","noul":0.3},"d-test0001":{"type":"choice","choice":"obeys","probabilities":{"obeys":0.8,"breaks":0.1,"not_applicable":0.1}}}}"#;
        let (url, server) = crate::fnod::tests::serve_once(reply, std::time::Duration::ZERO);
        config(&url);
        let journal = dir.path().join("events.jsonl");
        let request = |id: &str, key: &str| {
            json!({"ts": "2026-11-24T10:00:00Z", "phase": "request", "span_id": id,
                "session": "s", "matched_event": key, "machine": "m2-mbp", "fnod": "up",
                "rules": ["d-test0001"], "judge_dropped": 0, "judge_state": 0,
                "excerpt": "Read the status, then merged."})
        };
        crate::event_rules::append_judge_rows(&journal, &[request("s-req00001", "stop:s:b1")]);
        let laws = vec![Law {
            decision_id: "d-test0001".to_string(),
            decision: "Read state before you state it.".to_string(),
        }];
        let window = judge_window(&journal, None);
        assert_eq!(
            score(dir.path(), &journal, &window, &laws, &HashMap::new()),
            1
        );
        let sent: Value = serde_json::from_str(&server.join().unwrap()).unwrap();
        assert_eq!(
            sent.pointer("/state/message"),
            Some(&json!("Read the status, then merged."))
        );
        assert_eq!(
            sent.pointer("/state/rules/0/text"),
            Some(&json!("Read state before you state it."))
        );
        let rows = crate::event_rules::judge_rows(&journal);
        let verdicts: Vec<&crate::event_rules::JudgeRow> =
            rows.iter().filter(|r| r.phase == "verdict").collect();
        let [is_ask, law] = <[&crate::event_rules::JudgeRow; 2]>::try_from(verdicts).unwrap();
        for v in [is_ask, law] {
            assert_eq!(v.judge_of.as_deref(), Some("s-req00001"));
            assert_eq!(v.matched_event, "stop:s:b1");
            assert_eq!(v.machine, "m2-mbp");
        }
        assert_eq!(
            (is_ask.judge.as_deref(), is_ask.verdict.as_deref()),
            (Some("is_ask"), Some("no"))
        );
        assert_eq!(
            (law.judge.as_deref(), law.verdict.as_deref()),
            (Some("d-test0001"), Some("obeys"))
        );
        assert_eq!(law.escalate.as_deref(), Some("model"));
        assert_eq!(
            score(
                dir.path(),
                &journal,
                &judge_window(&journal, None),
                &laws,
                &HashMap::new()
            ),
            0
        );

        // fnod is down: no verdict lands, so the request waits for a later run.
        crate::event_rules::append_judge_rows(&journal, &[request("s-req00002", "stop:s:b2")]);
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        config(&format!("http://{}", closed.local_addr().unwrap()));
        drop(closed);
        let window = judge_window(&journal, None);
        let none = HashMap::new();
        assert_eq!(score(dir.path(), &journal, &window, &laws, &none), 0);
        let j = fold_judge(&[], &window, &HashMap::new());
        assert_eq!((j.scored, j.pending), (1, 1));

        let (url, server) = crate::fnod::tests::serve_once(reply, std::time::Duration::ZERO);
        config(&url);
        assert_eq!(score(dir.path(), &journal, &window, &laws, &none), 1);
        server.join().unwrap();
        let j = fold_judge(&[], &judge_window(&journal, None), &HashMap::new());
        assert_eq!((j.scored, j.pending), (2, 0));
    }
}
