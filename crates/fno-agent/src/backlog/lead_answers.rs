//! The lead check-in's scope-answer readings: which user decisions already
//! govern this team's nodes, and why each quiet worker is quiet.
//!
//! The answered reading reads every answered question page in the questions
//! directory (state path `questions`), matching on NODE membership in the
//! team scope, so a ruling recorded on another lead's page still shows.
//! The quiet reading reads each quiet scope worker with the peek reader,
//! in process, printing its last RESULT line, help block or watching tag; a
//! lead reads by how long ago its transcript last moved. Only a failed
//! read says READER FAILED; a row never blanks.

use std::collections::BTreeSet;
use std::path::Path;

use crate::lead_checkin::OwnTranscript::*;

use serde_json::{json, Value};

/// How many peek records one worker's tail reads: deep enough that a RESULT
/// several tool-calls back still shows, small enough that a beat reading a
/// full org stays cheap.
const PEEK_RECORDS: usize = 40;

/// The rendered report cap, so one worker stays one line.
const REPORT_CAP_CHARS: usize = 200;

/// The team scope's node ids, every status, straight from the graph. The
/// same compile the org fold runs, so a reading and the board cannot
/// disagree about what is in scope.
pub(crate) fn scope_node_ids(
    graph: &Path,
    cwd: &Path,
    scope: &str,
    level: Option<i64>,
) -> Result<BTreeSet<String>, String> {
    let entries = crate::graph_store::read_rows_where(
        graph,
        &crate::backlog::RowQuery {
            fields: Some(
                ["id", "type", "parent", "project"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            ),
            with_blockers: true,
            ..Default::default()
        },
    )
    .map_err(|error| crate::backlog::api::ApiError(error.to_string()))
    .map_err(|e| format!("graph unreadable: {}", e.0))?;
    let level = level.ok_or("team level unresolved")?;
    let projects = crate::org_board::project_map(cwd);
    crate::org_fold::compile_forced(scope, &entries, &projects, level)
}

/// Every readable question page in one directory, `(stem, text)`, conflict
/// markers excluded: the shape both question folds read. `pub(crate)` so the
/// merge gates read the same pages the lead check-in reads.
pub(crate) fn read_question_pages(dir: &std::path::Path) -> Result<Vec<(String, String)>, String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("questions folder {} unreadable: {e}", dir.display()))?;
    let mut pages: Vec<(String, String)> = Vec::new();
    for path in entries.flatten().map(|e| e.path()) {
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if crate::attention_file::has_conflict_markers(&text) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        pages.push((stem.to_string(), text));
    }
    Ok(pages)
}

/// The answered reading: user decisions for scope nodes, from EVERY
/// answered page in the questions directory, any team's page.
pub(crate) fn answered_reading(cwd: &Path, scope_ids: &BTreeSet<String>) -> Result<Value, String> {
    let dir = crate::state_path::resolve("questions", cwd)
        .ok_or("the questions state path did not resolve")?;
    let pages = read_question_pages(&dir)?;
    Ok(json!({"rows": answered_from_pages(&pages, scope_ids)}))
}

/// The pure fold: answered pages whose governing node is in scope, oldest
/// ruling first. The team that asked is irrelevant here; the node carries
/// the scope.
fn answered_from_pages(pages: &[(String, String)], scope_ids: &BTreeSet<String>) -> Vec<Value> {
    let mut rows: Vec<(u64, Value)> = Vec::new();
    for (stem, text) in pages {
        let Some((front, _)) = crate::attention_file::parse_page(text) else {
            continue;
        };
        if front.status != "answered" {
            continue;
        }
        if !crate::attention_arm::stem_names_id(stem, &front.question_id) {
            continue;
        }
        // The governing node: blocks lead, the node field follows; a page
        // that names neither cannot be scope-matched.
        let node = front
            .blocks
            .first()
            .cloned()
            .filter(|b| !b.is_empty() && b != "none")
            .or_else(|| Some(front.node.clone()).filter(|n| !n.is_empty() && n != "none"));
        let Some(node) = node else {
            continue;
        };
        if !scope_ids.contains(&node) {
            continue;
        }
        let ts = front
            .answered_at
            .clone()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| front.asked_at.clone());
        let epoch = chrono::DateTime::parse_from_rfc3339(&ts)
            .map(|d| d.timestamp().max(0) as u64)
            .unwrap_or(0);
        rows.push((
            epoch,
            json!({
                "node": node,
                "question_id": front.question_id,
                "question": front.title,
                "answer": one_line(front.answer.as_deref().unwrap_or(""), REPORT_CAP_CHARS),
                "ts": ts,
                "epoch": epoch,
            }),
        ));
    }
    rows.sort_by_key(|(epoch, _)| *epoch);
    rows.into_iter().map(|(_, v)| v).collect()
}

/// The quiet reading: why each quiet scope worker is quiet, one row per
/// worker, at most `MAX_ORG_ROWS` peeks per beat.
pub(crate) fn quiet_reading(
    top: Option<&Value>,
    scope_ids: &BTreeSet<String>,
) -> Result<Value, String> {
    let payload = top.ok_or("the workers reading failed; quiet workers are unreadable")?;
    let rows_all = payload_workers(payload)?;
    let quiet = quiet_scope_workers(rows_all, scope_ids);
    let total = quiet.len();
    let rows: Vec<Value> = quiet
        .into_iter()
        .take(crate::lead_checkin::MAX_ORG_ROWS)
        .map(|(name, node, w)| {
            let line = quiet_line(w, || peek_last_report(&name));
            json!({"worker": name, "node": node, "line": line})
        })
        .collect();
    Ok(json!({"quiet": total, "read": rows.len(), "rows": rows}))
}

/// The quiet rows in scope, `(worker name, node id, row)`, payload order. A
/// worker's node comes off the row when the join resolved, else off its
/// name; a row that names no scope node - foreign workers - stays out.
fn quiet_scope_workers<'a>(
    workers: &'a [Value],
    scope_ids: &BTreeSet<String>,
) -> Vec<(String, String, &'a Value)> {
    let mut rows = Vec::new();
    for w in workers {
        if crate::lead_checkin::s_str(w, "status") != Some("quiet") {
            continue;
        }
        let Some(name) = crate::lead_checkin::s_str(w, "name") else {
            continue;
        };
        let Some(node) = row_scope_node(w) else {
            continue;
        };
        if !scope_ids.contains(&node) {
            continue;
        }
        rows.push((name.to_string(), node, w));
    }
    rows
}

/// The two top-row fields a quiet line reads. A lead row carries a role.
#[derive(serde::Deserialize, Default)]
struct QuietRow {
    role: Option<String>,
    status_age_s: Option<f64>,
}

/// One quiet row's line. A lead reports in prose and never emits RESULT, so
/// its line is how long ago its transcript last moved. A worker's line is
/// its own last report; a tail with none names that and the quiet time, and
/// only a transcript the reader could not read says READER FAILED.
fn quiet_line(w: &Value, peek: impl FnOnce() -> Tail) -> String {
    use serde::Deserialize;
    let row = QuietRow::deserialize(w).unwrap_or_default();
    let quiet = row
        .status_age_s
        .map(|s| format!("{} min", (s / 60.0) as i64))
        .unwrap_or_else(|| "an unknown time".into());
    if let Some(role) = row.role.filter(|r| !r.is_empty()) {
        return format!("lead ({role}), last active {quiet} ago");
    }
    match peek() {
        Tail::Report(report) => report,
        Tail::Silent => format!(
            "no RESULT, help or watching tag in the last {PEEK_RECORDS} records; quiet {quiet}"
        ),
        Tail::Unreadable(e) => format!("READER FAILED {e}"),
    }
}

/// The node id a payload worker row answers for: the joined node when the
/// row carries one, else the id its spawn name embeds.
fn row_scope_node(w: &Value) -> Option<String> {
    crate::lead_checkin::s_str(w, "node")
        .map(str::to_string)
        .or_else(|| crate::lead_checkin::s_str(w, "name").and_then(node_from_name))
}

/// The node id a spawned worker's name carries (`t-x-9999-glm`): the first
/// `x-` run of 4 to 8 hex chars bounded by non-alphanumerics.
pub(crate) fn node_from_name(node_name: &str) -> Option<String> {
    let mut from = 0usize;
    while let Some(pos) = node_name[from..].find("x-") {
        let abs = from + pos + 2;
        let end = node_name[abs..]
            .find(|c: char| !c.is_ascii_alphanumeric())
            .map(|p| abs + p)
            .unwrap_or(node_name.len());
        let id = &node_name[abs..end];
        if (4..=8).contains(&id.len()) && id.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Some(format!("x-{id}"));
        }
        from = abs;
    }
    None
}

/// Idea-status children under the scope's epics, counted. A child holds a
/// non-empty `contained_in` or `parent` naming a scope node, is not
/// terminal, and still reads `idea`. The ready selection refuses every
/// contained child, so an epic-heavy scope reads as clean while it holds
/// the most unplanned work it has ever had; the check-in counts them as
/// the unplanned work they are, never dispatch rows - a contained child
/// rides its epic.
pub(crate) fn scope_idea_children<'a>(
    rows: &'a [Value],
    scope_ids: &'a BTreeSet<String>,
) -> impl Iterator<Item = &'a Value> {
    rows.iter().filter(move |r| {
        crate::lead_checkin::s_str(r, "status") == Some("idea")
            && !crate::org_board::is_terminal(r)
            && row_holder(r).is_some_and(|holder| scope_ids.contains(holder))
    })
}

/// The most idea rows a beat carries: the count stays whole, the rows feed
/// the idle offers and the batch groups.
const IDEA_ROWS_CAP: usize = 40;

/// The node id the row hangs from: `contained_in` first, then `parent`.
fn row_holder(r: &Value) -> Option<&str> {
    crate::lead_checkin::s_str(r, "contained_in")
        .filter(|s| !s.is_empty())
        .or_else(|| crate::lead_checkin::s_str(r, "parent").filter(|s| !s.is_empty()))
}

/// The beat-facing reader: one graph read for the count and the rows that
/// make it, scoped by the same compile every other scope reading shares.
/// Rows sort by priority then creation, so the first rows are the ones a
/// lead should start.
/// The idea-children reading over rows the caller already read: the beat
/// hands it the one graph read it took at the top. Archived children stay
/// out of the count, same as the store-side `include_archived: false` this
/// reading asks for at the store.
pub(crate) fn idea_children_over(entries: &[Value], scope_ids: &BTreeSet<String>) -> Value {
    let rows: Vec<Value> = entries
        .iter()
        .filter(|e| {
            crate::backlog::row_facts::RowFacts::of(e).is_some_and(|facts| {
                facts
                    .archived_at()
                    .value()
                    .is_none_or(|v| v.is_null() || v == &Value::Bool(false))
            })
        })
        .cloned()
        .collect();
    let mut kids: Vec<&Value> = scope_idea_children(&rows, scope_ids).collect();
    let count = kids.len();
    kids.sort_by_key(|r| {
        (
            crate::lead_checkin::s_str(r, "priority")
                .unwrap_or("p9")
                .to_string(),
            crate::lead_checkin::s_str(r, "created_at")
                .unwrap_or("")
                .to_string(),
        )
    });
    kids.truncate(IDEA_ROWS_CAP);
    json!({ "count": count, "rows": kids })
}

/// How many payload worker rows run inside the scope: the in-scope live
/// worker count the idle-scope attention reads. Machine-wide `live_workers`
/// cannot answer it - the bug this fixes read 4 of 32 workers live while
/// this scope held zero.
pub(crate) fn scope_live_worker_count(rows: &[Value], scope_ids: &BTreeSet<String>) -> usize {
    rows.iter()
        .filter(|w| row_scope_node(w).is_some_and(|node| scope_ids.contains(&node)))
        .count()
}

/// What one quiet worker's tail holds: its last report, no report, or a
/// read that failed with its reason.
enum Tail {
    Report(String),
    Silent,
    Unreadable(String),
}

/// The worker's own last word, read in process from its transcript tail:
/// the same records a peek renders, with no interpreter per worker. A worker
/// with no transcript yet (a pane row before reconcile backfills its session)
/// falls back to the pane screen, read in process the way peek reads it.
fn peek_last_report(handle: &str) -> Tail {
    let texts = match crate::session_truth::tail_texts(handle, PEEK_RECORDS) {
        Ok(texts) => texts,
        Err(_) => match crate::peek::pane_texts(handle, PEEK_RECORDS) {
            Ok(texts) => texts,
            Err(e) => return Tail::Unreadable(e),
        },
    };
    last_report(&texts).map_or(Tail::Silent, Tail::Report)
}

/// The last report in a record tail, newest record winning; inside one
/// record a RESULT line outranks a help block, and a help block outranks a
/// watching tag (a parked worker's live report). Records the harness or
/// control plane injected drop first: a skill body quotes every protocol
/// tag, and neither it nor a tool result is ever the worker's report.
fn last_report(texts: &[String]) -> Option<String> {
    let mut report: Option<String> = None;
    for text in texts {
        if crate::session_truth::injected_record_text(text) {
            continue;
        }
        if let Some(l) = last_result_line(text) {
            report = Some(l);
        } else if let Some(h) = help_line(text) {
            report = Some(h);
        } else if let Some(w) = watching_line(text) {
            report = Some(w);
        }
    }
    report
}

/// The last attributed `<watching ...>` tag in one record, named as the wait
/// it declares. A bare `<watching>` is prose naming the protocol. A
/// backticked tag still counts: workers emit it that way, and the stop gate
/// reads it the same way.
fn watching_line(text: &str) -> Option<String> {
    let mut found = None;
    let mut from = 0usize;
    while let Some(rel) = text[from..].find("<watching") {
        let start = from + rel;
        let Some(gt) = text[start..].find('>') else {
            break;
        };
        let tag = &text[start..start + gt + 1];
        if crate::distress::carries_an_attribute(&tag["<watching".len()..tag.len() - 1]) {
            found = Some(tag);
        }
        from = start + "<watching".len();
    }
    let tag = found?;
    let attr = |k: &str| crate::loopcheck::parse_xml_attr(tag, k).filter(|v| !v.is_empty());
    let mut line = format!("watching {}", attr("reason").unwrap_or_else(|| "-".into()));
    if let Some(pr) = attr("pr") {
        line.push_str(&format!(" on PR {pr}"));
    }
    if let Some(timeout) = attr("timeout") {
        line.push_str(&format!(" (timeout {timeout})"));
    }
    Some(one_line(&line, REPORT_CAP_CHARS))
}

/// The last `RESULT:` line in one record's text.
fn last_result_line(text: &str) -> Option<String> {
    text.lines()
        .filter_map(|l| {
            l.find("RESULT:")
                .map(|i| one_line(&l[i..], REPORT_CAP_CHARS))
        })
        .last()
}

/// One real `<help ...>...</help>` block flattened to a line; an unclosed
/// block runs to the end of the text (the tail may have cut it). Detection
/// is the shared distress parser, so a tag the record merely quotes - in
/// inline code, a fence, or a bare prose mention - is not a report.
fn help_line(text: &str) -> Option<String> {
    let (start, tag_end) = crate::distress::help_open_span(text)?;
    let end = text[tag_end..]
        .find("</help>")
        .map(|i| tag_end + i + "</help>".len())
        .unwrap_or(text.len());
    Some(one_line(&text[start..end], REPORT_CAP_CHARS))
}

/// Flatten to one display line: whitespace runs become single spaces,
/// then a char-safe cap.
fn one_line(text: &str, cap: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let end = flat
        .char_indices()
        .nth(cap)
        .map(|(i, _)| i)
        .unwrap_or(flat.len());
    let cut = &flat[..end];
    if cut.len() < flat.len() {
        format!("{cut}...")
    } else {
        cut.to_string()
    }
}

/// The held reading: this team's open questions, read from the page
/// frontmatter the attention arm froze at write time. The folder comes from
/// items.json's `questions_dir`, so a lead in any project reads the one
/// folder the daemon writes. A missing or stale items.json is a failed
/// reading, never `held: none`: the arm being down must not read as "no
/// questions".
pub(crate) fn held_reading(scope: &str) -> Result<Value, String> {
    let rows = held_rows_for(scope).map_err(|e| e.to_string())?;
    let (quiet, due): (Vec<Value>, Vec<Value>) = rows
        .into_iter()
        .partition(|r| r.pointer("/quiet").is_some_and(|q| !q.is_null()));
    let seen_file = crate::attention_arm::attention_dir()
        .ok()
        .map(|d| d.join("seen-held.json"));
    let seen = seen_file
        .as_deref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|raw| crate::hook::prompt::parse_seen(&raw))
        .unwrap_or_default();
    let (shown, folded, next) = fold_reported(due, &seen, scope);
    if let Some(path) = seen_file.as_deref().filter(|_| next != seen) {
        crate::hook::prompt::write_seen(path, &next);
    }
    Ok(json!({"open": shown.len(), "quiet": quiet.len() + folded, "rows": shown}))
}

/// The fields `fold_reported` reads from one due row, typed once.
#[derive(serde::Deserialize, Default)]
struct DueFields {
    question_id: Option<String>,
    window: Option<i64>,
    overdue: Option<String>,
}

/// A due question is said once per 24h window. Rows whose window the record
/// already holds fold into the quiet count; a lost record only repeats a
/// question once. Other scopes' keys pass through untouched.
fn fold_reported(
    due: Vec<Value>,
    seen: &std::collections::HashMap<String, i64>,
    scope: &str,
) -> (Vec<Value>, usize, std::collections::HashMap<String, i64>) {
    let prefix = format!("{scope}|");
    let mut next: std::collections::HashMap<String, i64> = seen
        .iter()
        .filter(|(k, _)| !k.starts_with(&prefix))
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    let mut shown = Vec::new();
    let mut folded = 0;
    for row in due {
        let fields: DueFields = serde_json::from_value(row.clone()).unwrap_or_default();
        let Some(qid) = fields.question_id else {
            shown.push(row);
            continue;
        };
        let key = format!("{prefix}{qid}");
        let window = fields.window.unwrap_or(0);
        let recorded = seen.get(&key).copied();
        // A take-recommended default is a work order, not news: it lists
        // until the lead records it, whatever session reads the beat.
        let work_order = fields
            .overdue
            .is_some_and(|o| o.starts_with("overdue: take"));
        if work_order || recorded.is_none_or(|r| window > r) {
            next.insert(key, window);
            shown.push(row);
        } else {
            next.insert(key, recorded.unwrap_or(window));
            folded += 1;
        }
    }
    (shown, folded, next)
}

/// The held reads' typed error (rust-style R6): one seam, one enum;
/// `Display` keeps the String-shaped callers whole.
pub(crate) enum HeldReadError {
    Index(String),
    Stale(u64),
    Dir,
    Pages(String),
}

impl std::fmt::Display for HeldReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeldReadError::Index(e) => write!(f, "{e}"),
            HeldReadError::Stale(age) => {
                write!(
                    f,
                    "items.json is {age}s old; the attention arm is not beating"
                )
            }
            HeldReadError::Dir => write!(f, "items.json names no questions_dir"),
            HeldReadError::Pages(e) => write!(f, "{e}"),
        }
    }
}

/// Every open question id for the scope, quiet ones included: the exact set
/// the wake arm's park recheck compares against the beat's stamp,
/// so the arm's verdict and the stamp share one source and shape. The rows
/// deserialize once at this boundary; a row without an id drops out.
pub(crate) fn held_open_ids(scope: &str) -> Result<Vec<String>, HeldReadError> {
    #[derive(serde::Deserialize, Default)]
    struct HeldIdRow {
        #[serde(default)]
        question_id: Option<String>,
    }
    let value = Value::Array(held_rows_for(scope)?);
    let rows: Vec<HeldIdRow> = serde_json::from_value(value).unwrap_or_default();
    Ok(rows.into_iter().filter_map(|r| r.question_id).collect())
}

/// The shared head of the held reads: the attention arm's index, the
/// staleness gate, the pages read, and the per-scope fold. A missing or
/// stale items.json is a failed read, never `held: none`.
fn held_rows_for(scope: &str) -> Result<Vec<Value>, HeldReadError> {
    let index_path = crate::attention_arm::attention_dir()
        .map_err(|e| HeldReadError::Index(e.to_string()))?
        .join("items.json");
    let raw = std::fs::read_to_string(&index_path).map_err(|_| {
        HeldReadError::Index("the attention arm has not written items.json".to_string())
    })?;
    let index: Value = serde_json::from_str(&raw)
        .map_err(|e| HeldReadError::Index(format!("items.json unreadable: {e}")))?;
    let as_of = index.get("as_of").and_then(Value::as_u64).unwrap_or(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let age = now.saturating_sub(as_of);
    if age > 600 {
        return Err(HeldReadError::Stale(age));
    }
    let dir = index
        .get("questions_dir")
        .and_then(Value::as_str)
        .filter(|d| !d.is_empty())
        .ok_or(HeldReadError::Dir)?;
    let mut pages = read_question_pages(std::path::Path::new(dir)).map_err(HeldReadError::Pages)?;
    pages.sort_by(|a, b| a.0.cmp(&b.0));
    let items = serde_json::from_value::<CachedItems>(index)
        .unwrap_or_default()
        .items;
    Ok(held_from_pages(&pages, scope, &items, now as i64))
}

/// The fields of the arm's cached items that the held fold reads.
#[derive(serde::Deserialize, Default)]
struct CachedItems {
    #[serde(default)]
    items: Vec<CachedItem>,
}

#[derive(serde::Deserialize, Default)]
pub(crate) struct CachedItem {
    id: Option<String>,
    created_at: Option<String>,
    deadline: Option<String>,
    on_silence: Option<String>,
    snoozed_until: Option<String>,
    class: Option<String>,
    recommendation: Option<CachedRecommendation>,
}

#[derive(serde::Deserialize, Default)]
struct CachedRecommendation {
    option: Option<u64>,
}

/// Whether every node of a page's frozen team sits inside the check-in's
/// scope. A lead promoted to a wider scope keeps the team string its open
/// pages were written under, so an exact match hides those questions. Subset
/// (not any-overlap) keeps a sibling lead's partly shared page off this line.
fn team_within(team: &str, scope: &str) -> bool {
    let members: BTreeSet<&str> = scope
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let nodes: Vec<&str> = team
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    !nodes.is_empty() && nodes.iter().all(|node| members.contains(node))
}

/// The pure fold: open pages whose question_id the stem names and whose
/// team sits inside this check-in's scope, oldest first. Rows carry
/// what the render needs to name the decide command. Each row joins the
/// arm's cached item by id: `quiet` is `snoozed` or `deadline` while the
/// question waits on either, and `overdue` names what a past deadline means
/// (the words the escalation reader uses). A page with no cached item reads
/// as due, so a missing join never hides a question.
pub(crate) fn held_from_pages(
    pages: &[(String, String)],
    scope: &str,
    items: &[CachedItem],
    now: i64,
) -> Vec<Value> {
    let mut rows: Vec<(u64, Value)> = Vec::new();
    for (stem, text) in pages {
        let Some((front, _)) = crate::attention_file::parse_page(text) else {
            continue;
        };
        if front.status != "open" {
            continue;
        }
        if !crate::attention_arm::stem_names_id(stem, &front.question_id) {
            continue;
        }
        if !team_within(&front.team, scope) {
            continue;
        }
        let epoch = chrono::DateTime::parse_from_rfc3339(&front.asked_at)
            .map(|d| d.timestamp().max(0) as u64)
            .unwrap_or(0);
        let node = front
            .blocks
            .first()
            .cloned()
            .filter(|b| !b.is_empty() && b != "none")
            .or_else(|| Some(front.node.clone()).filter(|n| !n.is_empty() && n != "none"));
        let item = items
            .iter()
            .find(|i| i.id.as_deref() == Some(front.question_id.as_str()));
        let state = item.map(|i| {
            crate::attention::quiet_from(
                i.created_at.as_deref().unwrap_or(&front.asked_at),
                i.deadline.as_deref(),
                i.snoozed_until.as_deref(),
                now,
            )
        });
        let quiet = match &state {
            Some(crate::attention::Quiet::Snoozed(_)) => Some("snoozed"),
            Some(crate::attention::Quiet::BeforeDeadline(_)) => Some("deadline"),
            _ => None,
        };
        let overdue = item
            .filter(|_| matches!(state, Some(crate::attention::Quiet::Due { .. })))
            .map(|i| {
                let recommended = i.recommendation.as_ref().and_then(|r| r.option);
                match (i.class.as_deref(), i.on_silence.as_deref(), recommended) {
                    (Some("irreversible"), _, _) => "overdue: waits (irreversible)".to_string(),
                    (_, Some("take-recommended"), Some(n)) if n >= 1 => {
                        format!("overdue: take option {n} and record it")
                    }
                    _ => "overdue: waits (on_silence wait)".to_string(),
                }
            });
        let window = match state {
            Some(crate::attention::Quiet::Due { epoch }) => Some(epoch),
            _ => None,
        };
        rows.push((
            epoch,
            json!({
                "node": node,
                "question_id": front.question_id,
                "question": front.title,
                "ts": front.asked_at,
                "epoch": epoch,
                "quiet": quiet,
                "overdue": overdue,
                "window": window,
            }),
        ));
    }
    rows.sort_by_key(|(epoch, _)| *epoch);
    rows.into_iter().map(|(_, v)| v).collect()
}

/// The workers payload, the `fno agent top --json --subagents` answer read
/// in process, shared by the workers and quiet readings.
pub(crate) fn fetch_workers_payload() -> Result<Value, String> {
    Ok(crate::agents_top::payload(true, false))
}

pub(crate) fn overdue_watches_reading() -> Result<Value, String> {
    let rows = crate::watch_expiry::overdue(&crate::paths::AgentsHome::from_env())?;
    Ok(json!({
        "rows": rows.into_iter().map(|watch| json!({
            "session_id": watch.session_id,
            "overdue_ms": watch.overdue_ms,
        })).collect::<Vec<_>>(),
    }))
}

/// The finished background subagents this session still holds, read from its
/// own transcript. Claude is the only harness with per-agent transcript
/// files a hold reader can pair; every other harness names why it reads
/// unmeasured, never a silent zero.
pub(crate) fn r_subagents() -> Result<Value, String> {
    let transcript = match crate::lead_checkin::own_transcript() {
        Ok(Text {
            harness: "claude",
            path: Some(path),
            ..
        }) => path,
        Ok(Text { harness, .. }) => {
            return Ok(crate::lead_checkin::unmeasured_value(&format!(
                "{harness} keeps no per-agent transcripts a hold reader can pair"
            )));
        }
        Ok(Unmeasured { reason, .. }) => {
            return Ok(crate::lead_checkin::unmeasured_value(&reason));
        }
        Err(e) => return Err(e),
    };
    crate::subagent_hold::reading(
        &transcript,
        std::time::SystemTime::now(),
        crate::subagent_hold::live_threshold(),
    )
}

/// The payload's worker rows, behind the same positive-predicate guard the
/// summary runs: a top payload whose census failed carries no rows, and a
/// reading that answered from it would read as a quiet zero.
pub(crate) fn payload_workers(payload: &Value) -> Result<&[Value], String> {
    let predicate = payload
        .get("predicate")
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .trim();
    let workers = payload.get("workers").and_then(|w| w.as_array());
    match (predicate.is_empty(), workers) {
        (false, Some(w)) => Ok(w),
        _ => Err("the top payload carries no positive predicate".into()),
    }
}

/// The summary the beat journals, folded from the shared top payload, so
/// the workers and quiet readings cost one `top` call between them. The
/// workers reading journals `live_subagents`, the rows in the payload's
/// `subagents` array whose verdict reads `active`; a payload with no
/// `subagents` array journals null, never 0.
pub(crate) fn workers_summary(payload: &Value) -> Result<Value, String> {
    let workers = payload_workers(payload)?;
    let live_subagents = payload
        .get("subagents")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter(|r| r.get("verdict").and_then(Value::as_str) == Some("active"))
                .count()
        });
    let mut oldest: Option<(f64, String)> = None;
    for w in workers {
        let age = w.get("status_age_s").and_then(|a| a.as_f64());
        let handle = w
            .get("handle")
            .and_then(|h| h.as_str())
            .or_else(|| crate::lead_checkin::s_str(w, "name"))
            .unwrap_or("");
        if let Some(age) = age {
            if oldest.as_ref().map(|(a, _)| age > *a).unwrap_or(true) {
                oldest = Some((age, handle.to_string()));
            }
        }
    }
    let (age, handle) = oldest.ok_or_else(|| "every status_age_s is null".to_string())?;
    Ok(json!({
        "live_workers": workers.len(),
        "oldest_worker_seen": format!("{}s {}", age as i64, handle),
        "live_subagents": live_subagents,
    }))
}

/// The subagents render block: the active count rides the workers line;
/// this block names the finished background subagents this session still
/// holds, oldest first, each with its TaskStop remedy, and fails loudly.
pub(crate) fn subagent_lines(readings: &[crate::lead_checkin::Reading]) -> Vec<String> {
    let mut lines = Vec::new();
    let failed = readings.iter().find(|r| r.name == "subagents" && !r.ok);
    if let Some(r) = failed {
        lines.push(format!("READER FAILED subagents: {}", r.error));
        return lines;
    }
    if let Some(reason) = readings
        .iter()
        .find(|r| r.name == "subagents")
        .and_then(|r| r.value.get("unmeasured").and_then(Value::as_str))
    {
        return vec![format!("subagents: unmeasured ({reason})")];
    }
    let held = readings
        .iter()
        .find(|r| r.name == "subagents")
        .and_then(|r| r.value.get("held").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    if held.is_empty() {
        lines.push("subagents: this session holds none finished".into());
        return lines;
    }
    let label = |row: &Value| {
        row.get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| crate::lead_checkin::dash(row.get("id")))
    };
    let oldest = held
        .iter()
        .max_by_key(|row| row.get("idle_secs").and_then(Value::as_u64).unwrap_or(0));
    let age = oldest
        .and_then(|row| row.get("idle_secs").and_then(Value::as_u64))
        .unwrap_or(0);
    let remedies = held
        .iter()
        .map(|row| format!("TaskStop {}", label(row)))
        .collect::<Vec<_>>()
        .join(", ");
    lines.push(format!(
        "subagents: this session holds {} finished and unstopped, oldest {}s {}; {}",
        held.len(),
        age,
        label(oldest.expect("held is non-empty")),
        remedies
    ));
    lines
}

/// The rows an `ok` rows-carrying reading holds, or its failure reason.
/// A reading the fixture lacks reads as absent-and-ok: the render's
/// `none` line, never a failure.
fn reading_rows(
    readings: &[crate::lead_checkin::Reading],
    name: &str,
) -> (Option<String>, Value, Vec<Value>) {
    match readings.iter().find(|r| r.name == name) {
        Some(r) if !r.ok => (Some(r.error.clone()), Value::Null, Vec::new()),
        Some(r) => {
            let rows = r
                .value
                .get("rows")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            (None, r.value.clone(), rows)
        }
        None => (None, Value::Null, Vec::new()),
    }
}

/// The held render block: the decide and clear commands one line per question.
pub(crate) fn held_lines(readings: &[crate::lead_checkin::Reading]) -> Vec<String> {
    let (error, value, rows) = reading_rows(readings, "held");
    let mut lines = Vec::new();
    if let Some(e) = error {
        lines.push(format!("READER FAILED held: {e}"));
        return lines;
    }
    let quiet = value.pointer("/quiet").and_then(Value::as_u64).unwrap_or(0);
    let quiet_line = format!(
        "held: {quiet} quiet (snoozed, before their deadline, or already reported this window; fno inbox outstanding lists them)"
    );
    // This team's questions: the rows carry node, question id and ask
    // time; a row with a node names the decide command, a row with none names
    // the clear command.
    if rows.is_empty() {
        lines.push("held: none".into());
        if quiet > 0 {
            lines.push(quiet_line);
        }
        return lines;
    }
    lines.push(format!("held: {} question(s) for this team", rows.len()));
    // The probe arm's last check per question: the held line names when a
    // machine last read each ask, so a lead never reports a question as
    // waiting without a fresh reading.
    let home = crate::paths::AgentsHome::from_env();
    let cache = crate::question_probe::cache_path_for(&home)
        .map(|path| crate::question_probe::load_cache(&path))
        .unwrap_or_default();
    let now = crate::claims::now_ms() / 1000;
    for row in rows.iter().take(crate::lead_checkin::MAX_ORG_ROWS) {
        let qid = crate::lead_checkin::dash(row.get("question_id"));
        let shown = crate::numbers::display(&qid);
        let suffix =
            crate::question_probe::check_suffix(cache.get(crate::numbers::canonical(&qid)), now);
        let overdue = row
            .pointer("/overdue")
            .and_then(Value::as_str)
            .map(|o| format!(" - {o}"))
            .unwrap_or_default();
        let suffix = format!("{suffix}{overdue}");
        let node = row
            .get("node")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty() && *n != "none");
        match node {
            Some(n) => lines.push(format!(
                "  {n} on question {shown}; answer with: fno backlog decide {n} \"<ruling>\" --question-id {shown}{suffix}"
            )),
            None => lines.push(format!(
                "  question {shown}: {} If this team asked it: fno inbox outstanding clear {shown} --answer \"<answer>\" --authority role{suffix}",
                crate::question_clear::chat_answer_line(&qid)
            )),
        }
    }
    if quiet > 0 {
        lines.push(quiet_line);
    }
    lines
}

/// The answered render block: one ruling per line, scoped by node.
pub(crate) fn answered_lines(readings: &[crate::lead_checkin::Reading]) -> Vec<String> {
    let (error, _value, rows) = reading_rows(readings, "answered");
    let mut lines = Vec::new();
    if let Some(e) = error {
        lines.push(format!("READER FAILED answered: {e}"));
        return lines;
    }
    if rows.is_empty() {
        lines.push("answered: none in scope".into());
        return lines;
    }
    lines.push(format!(
        "answered: {} user decision(s) in scope",
        rows.len()
    ));
    for row in rows.iter().take(crate::lead_checkin::MAX_ORG_ROWS) {
        lines.push(format!(
            "  {} ({}): {}",
            crate::lead_checkin::dash(row.get("node")),
            crate::lead_checkin::dash(row.get("question_id")),
            crate::lead_checkin::dash(row.get("answer"))
        ));
    }
    let hidden = rows.len().saturating_sub(crate::lead_checkin::MAX_ORG_ROWS);
    if hidden > 0 {
        lines.push(format!("  ... {hidden} more rows cut"));
    }
    lines
}

/// The quiet render block: one line per quiet worker naming its own last
/// word, READER FAILED included.
pub(crate) fn quiet_lines(readings: &[crate::lead_checkin::Reading]) -> Vec<String> {
    let (error, value, rows) = reading_rows(readings, "quiet_workers");
    let mut lines = Vec::new();
    if let Some(e) = error {
        lines.push(format!("READER FAILED quiet_workers: {e}"));
        return lines;
    }
    if rows.is_empty() {
        lines.push("quiet: none in scope".into());
        return lines;
    }
    let total = value
        .get("quiet")
        .and_then(Value::as_u64)
        .unwrap_or(rows.len() as u64);
    let read = value
        .get("read")
        .and_then(Value::as_u64)
        .unwrap_or(rows.len() as u64);
    if read < total {
        lines.push(format!("quiet: {total} worker(s) in scope, {read} read"));
    } else {
        lines.push(format!("quiet: {total} worker(s) in scope"));
    }
    for row in rows.iter().take(crate::lead_checkin::MAX_ORG_ROWS) {
        lines.push(format!(
            "  {} ({}): {}",
            crate::lead_checkin::dash(row.get("worker")),
            crate::lead_checkin::dash(row.get("node")),
            crate::lead_checkin::dash(row.get("line"))
        ));
    }
    let hidden = rows.len().saturating_sub(crate::lead_checkin::MAX_ORG_ROWS);
    if hidden > 0 {
        lines.push(format!("  ... {hidden} more rows cut"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_lines_name_the_last_check_and_survive_an_unreadable_cache() {
        // A home whose cache file is absent: every row degrades to the
        // not-machine-checkable suffix and the reading never fails. The
        // DoorEnv lock serializes the env mutation against the other
        // env-touching tests.
        let _env = crate::law_match::scope_tests::DoorEnv::new();
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join(".fno").join("agents");
        std::fs::create_dir_all(&home).expect("mkdir");
        let prior = std::env::var_os("FNO_AGENT_HOME");
        std::env::set_var("FNO_AGENT_HOME", &home);
        let lines = held_lines(&[crate::lead_checkin::Reading::took(
            "held",
            serde_json::json!({"rows": [
                {"question_id": "q-held1", "node": "x-node"},
                {"question_id": "q-held2"}
            ]}),
        )]);
        match prior {
            Some(v) => std::env::set_var("FNO_AGENT_HOME", v),
            None => std::env::remove_var("FNO_AGENT_HOME"),
        }
        assert_eq!(lines.len(), 3, "{lines:?}");
        let row_line = |n: usize| {
            lines
                .get(n)
                .expect("row line present")
                .ends_with("(not machine-checkable)")
        };
        assert!(row_line(1), "{lines:?}");
        assert!(row_line(2), "{lines:?}");
        a_due_question_lists_once_per_window_and_other_scopes_keep_their_record();
    }

    fn a_due_question_lists_once_per_window_and_other_scopes_keep_their_record() {
        let row = |window: u64| json!({"question_id": "q-1", "window": window});
        let other = std::collections::HashMap::from([("other|q-9".to_string(), 3)]);
        let (shown, folded, seen) = fold_reported(vec![row(0)], &other, "lead");
        assert_eq!((shown.len(), folded), (1, 0), "first sight lists");
        assert_eq!(seen.get("other|q-9"), Some(&3));
        let (shown, folded, seen) = fold_reported(vec![row(0)], &seen, "lead");
        assert_eq!(
            (shown.len(), folded),
            (0, 1),
            "same window folds into quiet"
        );
        let (shown, folded, _) = fold_reported(vec![row(1)], &seen, "lead");
        assert_eq!((shown.len(), folded), (1, 0), "a newer window lists again");
        let order = json!({"question_id": "q-1", "window": 0, "overdue": "overdue: take option 2 and record it"});
        let (shown, folded, _) = fold_reported(vec![order], &seen, "lead");
        assert_eq!(
            (shown.len(), folded),
            (1, 0),
            "a default to take always lists"
        );
    }

    fn texts(items: &[&str]) -> Vec<String> {
        items.iter().map(|t| t.to_string()).collect()
    }

    /// A tag the record merely quotes never reads as the worker's help: a
    /// re-arm note in inline code, the recovery nudge's own bare `<help>`,
    /// and a skill injection quoting the protocol whole. The worker's own
    /// RESULT after an injected record still reads. A parked worker's
    /// watching tag is its report, and a lead reads by its transcript's age:
    /// neither healthy session renders READER FAILED.
    #[test]
    fn the_quiet_line_names_the_real_state_and_never_a_quoted_help() {
        let rearm = "Re-arming; `<help class=\"held\">` only if this wedges";
        assert_eq!(last_report(&texts(&[rearm])), None);
        let nudge = "control: quiet 15 min on x-9, what blocks you? continue or emit <help>. [quiet-recovery:sid:work:nudge]";
        assert_eq!(last_report(&texts(&[nudge])), None);
        let skill = "<command-message>fno:target</command-message>\n<command-name>/fno:target</command-name>\nOn red CI emit <help class=\"stuck\" reason=\"ci\"> and stop.";
        assert_eq!(last_report(&texts(&[skill])), None);
        let tail = texts(&[skill, "RESULT: SUCCESS shipped"]);
        assert_eq!(
            last_report(&tail).as_deref(),
            Some("RESULT: SUCCESS shipped"),
            "the injected record drops; the worker's own RESULT reads"
        );
        let parked = "Held on a user look.\n`<watching reason=\"ci\" pr=\"4081\" timeout=\"15m\">`";
        assert_eq!(
            last_report(&texts(&[parked])).as_deref(),
            Some("watching ci on PR 4081 (timeout 15m)")
        );
        assert_eq!(last_report(&texts(&["I end on <watching> tags."])), None);
        let lead = json!({"role": "L2 x-team", "status_age_s": 1260.0});
        let never = || -> Tail { panic!("a lead is never peeked") };
        assert_eq!(
            quiet_line(&lead, never),
            "lead (L2 x-team), last active 21 min ago"
        );
        let worker = json!({"role": null, "status_age_s": 600.0});
        let none = quiet_line(&worker, || Tail::Silent);
        assert!(
            !none.contains("READER FAILED") && none.ends_with("quiet 10 min"),
            "{none}"
        );
        let failed = quiet_line(&worker, || Tail::Unreadable("transcript unreadable".into()));
        assert_eq!(failed, "READER FAILED transcript unreadable");
    }

    fn open_page(question_id: &str, team: &str) -> (String, String) {
        let text = format!(
            "---\nquestion_id: {question_id}\nstatus: open\nasked_at: 2026-10-20T15:32:09Z\nteam: {team}\nnode: x-0c2a\n---\nbody\n"
        );
        (format!("{question_id}-ask"), text)
    }

    /// A lead re-scoped with x-d02f added still sees the questions its pages
    /// were written under, and a lead whose scope shares no node does not.
    #[test]
    fn a_re_scoped_lead_still_sees_its_open_questions() {
        let old_team = "x-0c2a,x-8b8d,x-b720,x-c530,x-d742,x-e8fd,x-eff0,x-f1d3";
        let pages = vec![
            open_page("q-63c90fdc", old_team),
            open_page("q-499a7db0", old_team),
            open_page("q-other01", "x-zz01,x-zz02"),
        ];
        let widened = format!("{old_team},x-d02f");
        let rows = held_from_pages(&pages, &widened, &[], 0);
        let ids: Vec<&str> = rows
            .iter()
            .filter_map(|r| r["question_id"].as_str())
            .collect();
        assert_eq!(ids, ["q-63c90fdc", "q-499a7db0"], "{rows:?}");
        let other = held_from_pages(&pages, "x-zz01", &[], 0);
        assert_eq!(other.len(), 1, "{other:?}");
        let mut partly = pages.clone();
        partly.push(open_page("q-partly1", "x-0c2a,x-zz09"));
        let rows = held_from_pages(&partly, &widened, &[], 0);
        assert_eq!(
            rows.len(),
            2,
            "a page with a node outside the scope stays off: {rows:?}"
        );
    }
}
