//! One shared read of the machine decision store: graph.db decisions plus
//! the legacy `decisions.jsonl` rows the db lacks (the same merge the
//! Python owner `_read_index` applies). The Python command (`list_decisions`)
//! stays the format owner; this module mirrors its flatten and lifecycle
//! derivation for the Rust readers that cannot shell out under fleet load -
//! `prove_it_verdicts` measured ~25s per Python call, and the session-start
//! variant measured 67.0s, which breaks every hook budget the read runs
//! inside.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The Python reader's law-lane cutover (`AUTHORITY_LANE_CUTOVER`,
/// `cli/src/fno/decide/__init__.py`): an operator or chat_attested row at or
/// after this instant is law. Compared as a string, the same rule Python
/// applies.
pub const LAW_LANE_CUTOVER: &str = "2026-08-21T00:00:00Z";

/// A wide law row recorded at or after this instant binds a machine that
/// holds a user certificate only when that certificate's user key signed
/// it. Measured 2026-11-03: the newest unsigned wide row read 23:47:37Z the
/// day before. An older row reads as legacy and binds as it always did.
pub const PROOF_CUTOVER: &str = "2026-11-03T00:00:00Z";

thread_local! {
    /// The rows the last store read on this thread withheld.
    static WITHHELD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many rows the last `read_store_rows` on this thread withheld as
/// unproven. A note for a listing to print, never a filter. Per thread, so
/// a daemon's concurrent reads never cross.
pub fn withheld_unproven() -> usize {
    WITHHELD.with(std::cell::Cell::get)
}

/// The live flattened decisions plus the count of lines that could not join
/// them. `damaged` is a report for the reader, never a filter: the rows are
/// exactly what the Python reader would call live.
#[derive(Debug)]
pub struct Index {
    pub rows: Vec<Value>,
    pub damaged: usize,
}

/// `$FNO_HOME/<name>`, else `$HOME/.fno/<name>`: the same resolution
/// `graph_get::default_graph_path` applies to the graph store.
pub fn default_state_path(name: &str) -> PathBuf {
    if let Some(v) = std::env::var_os("FNO_HOME") {
        return PathBuf::from(v).join(name);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".fno").join(name)
}

/// The decisions journal of the store a graph spelling names: at the state
/// root the anchor lives in (a migrated `db/` twin collapses to its root,
/// where the journal always sits), never the home path and never inside
/// `db/`. For the default graph this is
/// `default_state_path("decisions.jsonl")`.
pub fn graph_journal(graph: &Path) -> PathBuf {
    let anchor = crate::backlog::anchor_path(graph);
    let root = match anchor.parent() {
        Some(p) if p.file_name().is_some_and(|n| n == "db") => p.parent().unwrap_or(p),
        Some(p) => p,
        None => return default_state_path("decisions.jsonl"),
    };
    root.join("decisions.jsonl")
}

/// The cold archive beside the journal: the same state root
/// the journal sits in, named `decisions-archive.jsonl`. The default reads
/// skip it. The citation gate reads it, so every stored id stays resolvable.
pub fn graph_archive(graph: &Path) -> PathBuf {
    graph_journal(graph).with_file_name("decisions-archive.jsonl")
}

/// The archive file for the machine's default store.
pub fn default_archive_path() -> PathBuf {
    default_state_path("decisions-archive.jsonl")
}

/// The archive's rows, flattened the same way the journal's are. A missing
/// or unreadable archive is empty: the archive is a projection, and its
/// absence never breaks a read (the fail-open posture of every reader).
pub fn read_archive_rows(jsonl: &Path) -> (Vec<Value>, usize) {
    let Ok(text) = std::fs::read_to_string(jsonl) else {
        return (Vec::new(), 0);
    };
    flatten_envelopes(&text)
}

/// The archive's row keys: the set every default read skips.
fn archive_skip_keys(jsonl: &Path) -> std::collections::HashSet<(String, String)> {
    read_archive_rows(jsonl).0.iter().map(row_key).collect()
}

/// Apply the store's scope amendments: the newest amendment per target
/// rewrites that row's `reach` stamp (never the legacy `scope` stamp, which
/// carries audience values), then the amendment rows drop out, because they
/// never render as rows. Pure over the given rows.
pub fn apply_scope_amendments(rows: &mut Vec<Value>) {
    let mut newest: BTreeMap<String, ((String, String), String)> = BTreeMap::new();
    for row in rows.iter().filter(|r| {
        r.get(de::EVENT_TYPE).and_then(Value::as_str)
            == Some(crate::backlog::decisions::SCOPE_AMEND_EVENT)
    }) {
        let Some(target) = row.get(de::TARGET).and_then(Value::as_str) else {
            continue;
        };
        if target.is_empty() {
            continue;
        }
        let Some(reach) = row.get(de::REACH).and_then(Value::as_str) else {
            continue;
        };
        if reach.is_empty() {
            continue;
        }
        let rank = (
            row.get(de::TS)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            row.get(de::AMENDMENT_ID)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        );
        let key = target.to_lowercase();
        if newest.get(&key).is_none_or(|(seen, _)| *seen < rank) {
            newest.insert(key, (rank, reach.to_string()));
        }
    }
    if !newest.is_empty() {
        for row in rows.iter_mut() {
            if row.get(de::EVENT_TYPE).and_then(Value::as_str) != Some("operator_decision") {
                continue;
            }
            let id = row
                .get(de::DECISION_ID)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            if let Some((_, reach)) = newest.get(&id) {
                if let Some(obj) = row.as_object_mut() {
                    obj.insert("reach".to_string(), json!(reach));
                }
            }
        }
    }
    rows.retain(|row| {
        row.get(de::EVENT_TYPE).and_then(Value::as_str)
            != Some(crate::backlog::decisions::SCOPE_AMEND_EVENT)
    });
}

/// The envelope flatten: JSONL text into flattened rows plus the damaged
/// count. The index stores event ENVELOPES (`{type, ts, data}`) that the
/// Python reader flattens (data fields at the top plus `_event_type` and
/// the envelope's `ts`). A line that does not parse, or a
/// decision/retraction envelope whose required id field is empty, counts in
/// `damaged`.
fn flatten_envelopes(text: &str) -> (Vec<Value>, usize) {
    let mut rows: Vec<Value> = Vec::new();
    let mut damaged: usize = 0;
    for line in text.lines() {
        let Ok(env) = serde_json::from_str::<Value>(line) else {
            damaged += 1;
            continue;
        };
        let Some(data) = env.get("data").and_then(Value::as_object) else {
            damaged += 1;
            continue;
        };
        let Some(etype) = env.get("type").and_then(Value::as_str) else {
            damaged += 1;
            continue;
        };
        if etype != "operator_decision"
            && etype != "decision_retracted"
            && etype != crate::backlog::decisions::SCOPE_AMEND_EVENT
        {
            continue;
        }
        // The Python reader marks an envelope whose required data field is
        // empty as damaged (discarded), not as a live or retiring row. A
        // scope amend without its target applies to nothing, so its id is
        // the damaged shape, like a retraction with no target.
        let required = match etype {
            "operator_decision" => "decision_id",
            crate::backlog::decisions::SCOPE_AMEND_EVENT => "target_decision_id",
            _ => "target_decision_id",
        };
        if data
            .get(required)
            .and_then(Value::as_str)
            .map(str::is_empty)
            .unwrap_or(true)
        {
            damaged += 1;
            continue;
        }
        let mut flat = Value::Object(data.clone());
        let obj = flat.as_object_mut().expect("just built");
        obj.insert("_event_type".to_string(), json!(etype));
        obj.insert(
            "ts".to_string(),
            env.get("ts").cloned().unwrap_or(Value::Null),
        );
        rows.push(flat);
    }
    (rows, damaged)
}

/// The retirement pass over flattened rows. A `decision_retracted` row
/// retires its `target_decision_id` (newest `(ts, reason)` wins) and a
/// decision whose `supersedes` names another retires that one (newest
/// `(ts, decision_id)` wins). ids compare casefolded, the Python reader's
/// own rule. Rows carry whatever fields the row carried; the `text`-present
/// rule is prove_it's, applied at its call site, because law rows carry
/// `decision` and no `text` at all.
pub fn derive_rows(rows: Vec<Value>, damaged: usize) -> Index {
    derive_rows_with(rows, damaged, true)
}

/// `derive_rows` that keeps date-expired laws, so `law recheck` can write the receipt.
pub fn derive_rows_unguarded(rows: Vec<Value>, damaged: usize) -> Index {
    derive_rows_with(rows, damaged, false)
}

fn derive_rows_with(rows: Vec<Value>, damaged: usize, apply_date_guard: bool) -> Index {
    let is_decision = |row: &Value| {
        matches!(
            row.get("_event_type").and_then(Value::as_str),
            Some("operator_decision")
        )
    };
    let retired = retirement_map(&rows);
    let rows = rows
        .into_iter()
        .filter(is_decision)
        .filter(|row| {
            let id = row
                .get("decision_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            id.is_empty() || !retired.contains_key(&id)
        })
        .filter(|row| !apply_date_guard || !date_guard_fired(row, &chrono::Utc::now().to_rfc3339()))
        .collect();
    Index { rows, damaged }
}

/// True when a `guard: {kind: "date", at}` has passed; event and set guards need `law recheck`.
pub fn date_guard_fired(row: &Value, now_rfc3339: &str) -> bool {
    let Some(guard) = row.get("guard").filter(|g| g.is_object()) else {
        return false;
    };
    if guard.get("kind").and_then(Value::as_str) != Some("date") {
        return false;
    }
    let (Some(at), Ok(now)) = (
        guard.get("at").and_then(Value::as_str),
        chrono::DateTime::parse_from_rfc3339(now_rfc3339),
    ) else {
        return false;
    };
    chrono::DateTime::parse_from_rfc3339(at).is_ok_and(|at| at <= now)
}

/// The retirement map behind `derive_rows`, exposed for readers that render
/// the retired rows instead of dropping them: casefolded target id ->
/// ((ts, tie) rank of the newest retirement, kind "retracted" | "superseded").
pub fn retirement_map(
    rows: &[Value],
) -> std::collections::BTreeMap<String, ((String, String), &'static str)> {
    let rank = |row: &Value, tie: &str| {
        (
            row.get("ts")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            row.get(tie)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        )
    };
    let mut retired: std::collections::BTreeMap<String, ((String, String), &'static str)> =
        Default::default();
    for row in rows
        .iter()
        .filter(|r| r.get("_event_type").and_then(Value::as_str) == Some("decision_retracted"))
    {
        let target = row
            .get("target_decision_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        if target.is_empty() {
            continue;
        }
        let r = rank(row, "reason");
        if retired.get(&target).map_or(true, |prev| prev.0 < r) {
            retired.insert(target, (r, "retracted"));
        }
    }
    for row in rows.iter().filter(|r| {
        matches!(
            r.get("_event_type").and_then(Value::as_str),
            Some("operator_decision")
        )
    }) {
        let target = row
            .get("supersedes")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        if target.is_empty() {
            continue;
        }
        let r = rank(row, "decision_id");
        if retired.get(&target).map_or(true, |prev| prev.0 < r) {
            retired.insert(target, (r, "superseded"));
        }
    }
    retired
}

/// `derive_live` composed of its two halves, kept as the seam the JSONL-file
/// readers and their tests call.
pub fn derive_live(text: &str) -> Index {
    let (rows, damaged) = flatten_envelopes(text);
    derive_rows(rows, damaged)
}

/// The ways a decision-store read fails. One variant today: the store is
/// unreadable, and the message names both legs. The type is what a caller
/// matches (docs/rust-style.md R6); the plain read keeps its grandfathered
/// signature.
#[derive(Debug, thiserror::Error)]
#[error("decision store read failed: {0}")]
pub struct DecisionStoreError(pub String);

impl From<String> for DecisionStoreError {
    fn from(message: String) -> Self {
        Self(message)
    }
}

/// The decision-envelope field names, one definition each (the
/// row_facts key-module pattern): new reads go through these, so a field
/// name has one home and a typo refuses to compile.
pub mod de {
    pub const EVENT_TYPE: &str = "_event_type";
    pub const TYPE: &str = "type";
    pub const DATA: &str = "data";
    pub const DECISION_ID: &str = "decision_id";
    pub const SUBJECT: &str = "subject";
    pub const SCOPE: &str = "scope";
    pub const REACH: &str = "reach";
    pub const PRIOR_REACH: &str = "prior_reach";
    pub const DECISION: &str = "decision";
    pub const AUTHORITY: &str = "authority_source";
    pub const SUPERSEDES: &str = "supersedes";
    pub const TARGET: &str = "target_decision_id";
    pub const RETRACTION_ID: &str = "retraction_id";
    pub const AMENDMENT_ID: &str = "amendment_id";
    pub const TS: &str = "ts";
}

/// The row identity the Python merge uses (`decide/__init__.py:814-819`):
/// event type, then the first non-empty of `decision_id`,
/// `retraction_id`, `amendment_id`, `target_decision_id`. The amendment id
/// sits before the target: two amendments of one law must not share a key,
/// or the journal-vs-db merge drops the newer one as a copy.
pub(crate) fn row_key(row: &Value) -> (String, String) {
    let etype = row
        .get(de::EVENT_TYPE)
        .and_then(Value::as_str)
        .unwrap_or("operator_decision")
        .to_string();
    let id = [
        "decision_id",
        "retraction_id",
        "amendment_id",
        "target_decision_id",
    ]
    .iter()
    .find_map(|k| row.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()))
    .unwrap_or("")
    .to_string();
    (etype, id)
}

/// The store rows: graph.db decisions first, then the JSONL rows the db
/// lacks, keyed the way `_read_index` (`cli/src/fno/decide/__init__.py:800`)
/// merges them. The JSONL half is load-bearing, not vestigial: ruling
/// d-0e9ed907 is in the JSONL and missing from the db. A missing JSONL reads
/// as zero rows, as Python does. No db AND no JSONL is `Err` naming both
/// paths: a caller that cannot read the store must say so, never render an
/// empty list that reads as "no rulings exist". `damaged` counts JSONL
/// lines only, the same source the Python reader reports.
///
/// On a machine that holds a user certificate, the rows then pass
/// `admit_proven`, so every law reader sees only proven wide law.
pub fn read_store_rows(graph: &Path, jsonl: &Path) -> Result<(Vec<Value>, usize), String> {
    let (mut rows, damaged) = read_store_rows_raw(graph, jsonl)?;
    apply_scope_amendments(&mut rows);
    let skip = archive_skip_keys(&graph_archive(graph));
    if !skip.is_empty() {
        rows.retain(|row| !skip.contains(&row_key(row)));
    }
    let withheld = crate::identity::trusted_user_key()
        .map_or(0, |user_key| admit_proven(&mut rows, &user_key));
    WITHHELD.with(|count| count.set(withheld));
    Ok((rows, damaged))
}

/// The physical store rows (no archive skip, no reach apply) plus the
/// archive's rows, deduped by row_key: every stored row a citation may
/// name, so a moved id stays resolvable. Unproven rows keep the same admit
/// pass the plain read applies, so a forged id cannot pass the gate by
/// archiving.
pub fn read_store_rows_everything(
    graph: &Path,
    jsonl: &Path,
) -> Result<(Vec<Value>, usize), DecisionStoreError> {
    let (mut rows, mut damaged) = read_store_rows_raw(graph, jsonl)?;
    let (archived, arch_damaged) = read_archive_rows(&graph_archive(graph));
    damaged += arch_damaged;
    let known: std::collections::HashSet<(String, String)> = rows.iter().map(row_key).collect();
    for row in archived {
        if !known.contains(&row_key(&row)) {
            rows.push(row);
        }
    }
    Ok((rows, damaged))
}

/// Drop the rows a peer could have written in this machine's user's name,
/// returning how many. A wide decision (global, or bound to a machine or
/// user) at or after `PROOF_CUTOVER` stays only when `user_key` proves it.
/// A retraction of a proven wide law, or a decision that supersedes one,
/// needs the same proof, so an unsigned row cannot retire signed law.
pub fn admit_proven(rows: &mut Vec<Value>, user_key: &str) -> usize {
    let kind = |row: &Value| {
        row.get("_event_type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let text = |row: &Value, key: &str| {
        row.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase()
    };
    let proven = |row: &Value| crate::identity::verify_law_row(row).as_deref() == Some(user_key);
    let before = rows.len();
    let mut guarded = std::collections::HashSet::new();
    rows.retain(|row| {
        let wide =
            binds(row_scope(row), String::new, String::new).is_some() || row_scope(row) == "global";
        let fresh = row.get("ts").and_then(Value::as_str).unwrap_or("") >= PROOF_CUTOVER;
        if kind(row) != "operator_decision" || !wide || !fresh {
            return true;
        }
        let keep = proven(row);
        if keep {
            guarded.insert(text(row, "decision_id"));
        }
        keep
    });
    rows.retain(|row| {
        let target = match kind(row).as_str() {
            "decision_retracted" => text(row, "target_decision_id"),
            "operator_decision" => text(row, "supersedes"),
            _ => String::new(),
        };
        !guarded.contains(&target) || proven(row)
    });
    before - rows.len()
}

/// The store rows with no admit pass: the reindex keys its replay on every
/// stored row, withheld ones included, or it would re-append them each run.
pub fn read_store_rows_raw(graph: &Path, jsonl: &Path) -> Result<(Vec<Value>, usize), String> {
    let db = crate::backlog::database_path(graph);
    let db_present = db.exists();
    // The journal's store counts as present: rows committed to decisions.db
    // never touch the file.
    let jsonl_present = jsonl.exists() || crate::event_store::store_path(jsonl).exists();
    if !db_present && !jsonl_present {
        return Err(format!(
            "no decision store: nothing at {} and nothing at {}",
            db.display(),
            jsonl.display()
        ));
    }
    let mut rows = Vec::new();
    if db_present {
        match crate::backlog::api::decisions(&crate::backlog::api::Store::new(graph), None, None) {
            Ok(db_rows) => rows = db_rows,
            Err(e) => {
                if !jsonl_present {
                    return Err(format!(
                        "the decision db at {} is unreadable ({}) and no JSONL sits at {}",
                        db.display(),
                        e.0,
                        jsonl.display()
                    ));
                }
                // Degrade to the JSONL leg; the Err above covers the machine
                // with neither leg readable.
            }
        }
    }
    let known: std::collections::HashSet<(String, String)> = rows.iter().map(row_key).collect();
    let mut damaged = 0usize;
    if jsonl_present {
        // Store-aware: the journal's committed rows (decisions.db since the
        // cutover) plus the unseen live lines, in commit order.
        let text = crate::event_store::journal_text_checked(
            jsonl,
            &crate::event_store::EventQuery::of_types(&[]),
        )?;
        let (jsonl_rows, jsonl_damaged) = flatten_envelopes(&text);
        damaged = jsonl_damaged;
        for row in jsonl_rows {
            if !known.contains(&row_key(&row)) {
                rows.push(row);
            }
        }
    }
    Ok((rows, damaged))
}

/// The store read through the lifecycle derivation: the rows a Python
/// `fno backlog decisions` call renders.
pub fn read_store_live(graph: &Path, jsonl: &Path) -> Result<Index, String> {
    let (rows, damaged) = read_store_rows(graph, jsonl)?;
    let mut index = derive_rows(rows, damaged);
    drop_expired(&mut index, graph);
    Ok(index)
}

/// `read_store_live` over the machine's default paths, resolved the way
/// every other client-side command resolves them.
pub fn default_store_live() -> Result<Index, String> {
    read_store_live(
        &crate::graph_get::default_graph_path(),
        &default_state_path("decisions.jsonl"),
    )
}

/// The newest `law_trigger` row per law from the decision store's journal,
/// keyed by decision_id. Append-only imports: the newest row wins and
/// nothing rewrites a law row. A missing or unreadable journal is `Err`
/// naming the path, the same contract as the store legs.
pub fn read_law_triggers(jsonl: &Path) -> Result<BTreeMap<String, Value>, String> {
    let text = crate::event_store::journal_text_checked(
        jsonl,
        &crate::event_store::EventQuery::of_types(&["law_trigger"]),
    )?;
    let mut best: BTreeMap<String, ((String, usize), Value)> = BTreeMap::new();
    for (i, line) in text.lines().enumerate() {
        let Ok(env) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        // The store applies the type filter, but a journal with no store
        // reads its whole family, so the row type is checked here too.
        if env.get("type").and_then(Value::as_str) != Some("law_trigger") {
            continue;
        }
        let Some(data) = env.get("data").cloned() else {
            continue;
        };
        let Some(id) = data
            .get("decision_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        if id.is_empty() {
            continue;
        }
        let ts = env
            .get("ts")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let rank = (ts, i);
        if best.get(&id).is_none_or(|(seen, _)| *seen < rank) {
            best.insert(id, (rank, data));
        }
    }
    Ok(best.into_iter().map(|(k, (_, v))| (k, v)).collect())
}

/// Read an index file into its live rows: the committed rows of the journal's
/// store plus the unseen live lines. A missing or unreadable file is
/// `Err` naming the path: a caller that cannot read the index must say so,
/// never render an empty list that reads as "no rulings exist".
pub fn read_live(path: &Path) -> Result<Index, String> {
    if !path.exists() && !crate::event_store::store_path(path).exists() {
        return Err(format!(
            "{}: neither the journal nor its store exists",
            path.display()
        ));
    }
    let text = crate::event_store::journal_text_checked(
        path,
        &crate::event_store::EventQuery::of_types(&[]),
    )?;
    Ok(derive_live(&text))
}

/// The authorities a write door may record. Every door checks this one list.
pub const AUTHORITY_SOURCES: &[&str] = &["operator", "chat_attested", "role", "agent", "beastmode"];

/// A stored authority read as its enum value. The old spellings of `role`
/// live in the role migration table; any other value outside the enum reads
/// empty, so it lands in no lane.
pub fn authority_of(row: &Value) -> &'static str {
    authority_in(RowFacts::of(row).authority_source.unwrap_or(""))
}

/// The decision fields the lane and expiry readers use, read once from a
/// row. An absent or null field reads `None`; a row of the wrong shape
/// reads every field `None`.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct RowFacts<'a> {
    #[serde(borrow)]
    authority_source: Option<&'a str>,
    #[serde(borrow)]
    ts: Option<&'a str>,
    #[serde(borrow)]
    decision_id: Option<&'a str>,
    #[serde(borrow)]
    question_id: Option<&'a str>,
    #[serde(borrow)]
    subject: Option<&'a str>,
}

impl<'a> RowFacts<'a> {
    fn of(row: &'a Value) -> Self {
        serde::Deserialize::deserialize(row).unwrap_or_default()
    }
}

fn authority_in(raw: &str) -> &'static str {
    if crate::role_migration::is_legacy_role_authority(raw) {
        return "role";
    }
    AUTHORITY_SOURCES
        .iter()
        .find(|a| **a == raw)
        .copied()
        .unwrap_or("")
}

/// The one authority-to-lane map every reader and door shares.
pub fn decision_lane(row: &Value) -> &'static str {
    let facts = RowFacts::of(row);
    match authority_in(facts.authority_source.unwrap_or("")) {
        "agent" | "role" => "agent",
        "beastmode" => "grant",
        "operator" | "chat_attested" if facts.ts.unwrap_or("") >= LAW_LANE_CUTOVER => "user",
        _ => "unattributed",
    }
}

/// The standing lane: authority `operator` or `chat_attested`, `ts` at or
/// after the cutover. Beastmode is the grant lane and agent/role are
/// coordination; neither is law.
pub fn is_law(row: &Value) -> bool {
    decision_lane(row) == "user"
}

/// A law row that carries the user's own ruling: an operator row, or a
/// chat_attested row that stores the user's words (a `quote`, or a
/// rationale that starts `user in chat:`). Any session can mint a bare
/// chat_attested row, so the words are the evidence that the user spoke.
pub fn is_user_ruling(row: &Value) -> bool {
    if !is_law(row) {
        return false;
    }
    match row.get("authority_source").and_then(Value::as_str) {
        Some("operator") => true,
        Some("chat_attested") => {
            let quoted = row
                .get("quote")
                .and_then(Value::as_str)
                .is_some_and(|q| !q.trim().is_empty());
            let chat_rationale = row
                .get("rationale")
                .and_then(Value::as_str)
                .and_then(|r| r.trim().strip_prefix("user in chat:"))
                .is_some_and(|words| !words.trim().is_empty());
            quoted || chat_rationale
        }
        _ => false,
    }
}

/// Door subjects that name one PR: `merge-grant:<repo>#<n>@<sha>` and kin.
const PR_DOOR_PREFIXES: &[&str] = &["merge-grant:", "merge-freeze:", "pr-hold:"];

/// Whether the subject names one PR through a door prefix. A per-head door
/// row is a machine contract the merge and hold gates read, never a policy
/// law, so the record door's no-conflict gate exempts it: each head's
/// subject is fresh by construction.
pub fn is_pr_door_subject(subject: &str) -> bool {
    let s = subject.trim();
    PR_DOOR_PREFIXES.iter().any(|p| s.starts_with(p))
}

/// The repository-scoped PR a subject names, door prefix and head pin cut
/// first. A bare PR number never proves closure.
pub fn pr_expiry_ref(subject: Option<&str>) -> Option<Value> {
    let mut value = subject?.trim();
    if let Some(rest) = PR_DOOR_PREFIXES.iter().find_map(|p| value.strip_prefix(p)) {
        value = rest.split_once('@').map_or(rest, |(head, _)| head);
    }
    let lower = value.to_ascii_lowercase();
    let rest = if lower.starts_with("https://github.com/") {
        &value[19..]
    } else if lower.starts_with("http://github.com/") {
        &value[18..]
    } else {
        value
    };
    let (repo, number_raw) = rest.split_once("/pull/").or_else(|| rest.split_once('#'))?;
    let segment_ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
    };
    let (owner, name) = repo.split_once('/')?;
    if !segment_ok(owner) || !segment_ok(name) {
        return None;
    }
    let number = number_raw.parse::<i64>().ok()?;
    Some(json!({"kind": "pr", "repository": repo.to_lowercase(), "number": number}))
}

/// Board answers recorded before `--expiry` existed that state a general
/// rule, not one node's call, so they read as `expiry: never`. The store is
/// append-only, so the pin lives here. Classified by lead ruling D3356.
const STANDING_ANSWERS: &[&str] = &[
    "d-8fec973f", // D3199: squash-only PRs toward 0.5.0
    "d-52cd8d76", // D3029: who may raise a lead budget, and by how much
    "d-b181c28f", // D3024: every Anthropic lane gets --advisor opus
    "d-8e33a5c9", // D2965: telemetry is opt-in, grades and counts only
    "d-fefe281d", // D2809: the approval passkey rule, no approve tool
    // The same five once the migration makes the number the key.
    "D3199",
    "D3029",
    "D3024",
    "D2965",
    "D2809",
];

/// Whether a row with no stated expiry ends with its subject: a
/// coordination row, or any answer to a question. A standing decision
/// carries neither and stays live until it is retracted or superseded.
pub fn expiry_defaults_to_subject(row: &Value) -> bool {
    let facts = RowFacts::of(row);
    if STANDING_ANSWERS.contains(&facts.decision_id.unwrap_or("")) {
        return false;
    }
    decision_lane(row) == "agent" || facts.question_id.is_some_and(|q| !q.trim().is_empty())
}

/// A refused door setting (`--expiry`, `--delivery`, `--challenges`): the
/// message names the value and what the flag takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecError(pub String);

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SpecError {}

impl From<SpecError> for String {
    fn from(e: SpecError) -> String {
        e.0
    }
}

/// The `--expiry` value a door stamps: `node[:<id>]`, `pr[:<repo>#<n>]`,
/// `date:<RFC 3339>`, or `never`. A bare `node` or `pr` means the subject's.
pub fn parse_expiry(spec: &str, subject: &str) -> Result<Value, SpecError> {
    let bad = || {
        SpecError(format!(
            "--expiry wants node[:<id>], pr[:<repo>#<n>], date:<RFC 3339> or never, got {spec:?}"
        ))
    };
    let (kind, rest) = spec.split_once(':').unwrap_or((spec, ""));
    match (kind.trim(), rest.trim()) {
        ("never", "") => Ok(json!({"kind": "never"})),
        ("node", "") if !subject.trim().is_empty() => {
            Ok(json!({"kind": "node", "node_id": subject.trim()}))
        }
        ("node", id) if !id.is_empty() => Ok(json!({"kind": "node", "node_id": id})),
        ("pr", "") => pr_expiry_ref(Some(subject)).ok_or_else(|| {
            SpecError(format!(
                "--expiry pr: the subject {subject:?} names no <owner>/<repo>#<n> PR"
            ))
        }),
        ("pr", pr) => pr_expiry_ref(Some(pr)).ok_or_else(bad),
        ("date", at) => chrono::DateTime::parse_from_rfc3339(at)
            .map(|t| json!({"kind": "date", "at": t.with_timezone(&chrono::Utc).to_rfc3339()}))
            .map_err(|_| bad()),
        _ => Err(bad()),
    }
}

/// When a node row is closed, the evidence; `None` while it is open.
pub fn node_closure(node: &Value) -> Option<String> {
    if crate::backlog_ready::node_is_open(node) {
        return None;
    }
    let facts = NodeFacts::of(node);
    let at = match facts.completed_at {
        Some(v) if crate::backlog_ready::truthy(Some(&v)) => v.as_str().map(str::to_string),
        _ => facts
            .supersession
            .and_then(|s| s.verified_at)
            .map(str::to_string),
    };
    let id = facts.id.unwrap_or("");
    Some(format!(
        "node {id} closed at {}",
        at.as_deref().unwrap_or("None")
    ))
}

/// The node fields closure evidence reads, read once from a graph row.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct NodeFacts<'a> {
    #[serde(borrow)]
    id: Option<&'a str>,
    completed_at: Option<Value>,
    #[serde(borrow)]
    supersession: Option<Superseded<'a>>,
}

#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct Superseded<'a> {
    #[serde(borrow)]
    verified_at: Option<&'a str>,
}

impl<'a> NodeFacts<'a> {
    fn of(node: &'a Value) -> Self {
        serde::Deserialize::deserialize(node).unwrap_or_default()
    }
}

/// A stated `expiry_ref`, typed at the read.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Expiry {
    Never,
    Date { at: String },
    Node { node_id: String },
    Pr { repository: String, number: i64 },
}

/// The lifecycle of an expiry that needs no graph: `never` is live, and a
/// passed `date` is expired. `None` for the node and PR kinds.
pub fn timed_lifecycle(
    expiry: &Value,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<(&'static str, Option<String>)> {
    match <Expiry as serde::Deserialize>::deserialize(expiry).ok()? {
        Expiry::Never => Some(("live", None)),
        Expiry::Date { at } => {
            let at = chrono::DateTime::parse_from_rfc3339(&at).ok()?;
            Some(if at <= now {
                ("expired", Some(format!("date {} passed", at.to_rfc3339())))
            } else {
                ("live", None)
            })
        }
        _ => None,
    }
}

/// The node id a row's expiry names by exact id, stated or defaulted.
fn expiry_node_id(row: &Value, seq_prefix: Option<&str>) -> Option<String> {
    if let Some(expiry) = row.get("expiry_ref").filter(|r| r.is_object()) {
        return match <Expiry as serde::Deserialize>::deserialize(expiry) {
            Ok(Expiry::Node { node_id }) => Some(node_id),
            _ => None,
        };
    }
    if !expiry_defaults_to_subject(row) {
        return None;
    }
    let subject = RowFacts::of(row).subject.unwrap_or("").trim();
    crate::law_match::matches_node_id_shape_under(subject, seq_prefix).then(|| subject.to_string())
}

/// Drop every row whose expiry has fired, for the readers that serve live
/// decisions (the rule set, the stage door, the gates). It reads only the
/// node ids the rows name, by exact id, so it stays inside the hook budget.
/// The list reader also resolves fuzzy subjects and PR merges. An unreadable
/// graph keeps every row: the fail-open posture of every reader.
pub fn drop_expired(index: &mut Index, graph: &Path) {
    let now = chrono::Utc::now();
    index.rows.retain(|row| {
        row.get("expiry_ref")
            .filter(|r| r.is_object())
            .and_then(|e| timed_lifecycle(e, now))
            .map_or(true, |(lc, _)| lc != "expired")
    });
    let seq_prefix = super::node_ref::sequential_prefix();
    let ids: std::collections::BTreeSet<String> = index
        .rows
        .iter()
        .filter_map(|row| expiry_node_id(row, seq_prefix.as_deref()))
        .collect();
    if ids.is_empty() || !graph.exists() {
        return;
    }
    let ids: Vec<String> = ids.into_iter().collect();
    let mut closed = std::collections::HashSet::new();
    for batch in ids.chunks(400) {
        let query = crate::backlog::RowQuery {
            filter: crate::backlog::api::NodeFilter {
                id_in: Some(batch.to_vec()),
                ..Default::default()
            },
            ..Default::default()
        };
        let Ok(rows) = crate::graph_store::read_rows_where(graph, &query) else {
            return;
        };
        for node in rows {
            if node_closure(&node).is_some() {
                if let Some(id) = NodeFacts::of(&node).id {
                    closed.insert(id.to_string());
                }
            }
        }
    }
    index.rows.retain(|row| {
        expiry_node_id(row, seq_prefix.as_deref()).map_or(true, |id| !closed.contains(&id))
    });
}

/// The laws of an index, newest first.
pub fn laws_of(mut index: Index) -> Index {
    index.rows.retain(is_law);
    index.rows.sort_by(|a, b| {
        let at = a.get("ts").and_then(Value::as_str).unwrap_or("");
        let bt = b.get("ts").and_then(Value::as_str).unwrap_or("");
        bt.cmp(at)
    });
    index
}

/// The live laws of an index FILE, newest first. The fixture/test path; the
/// store read is `read_store_live` + `laws_of`.
pub fn live_laws(path: &Path) -> Result<Index, String> {
    Ok(laws_of(read_live(path)?))
}

/// Subject prefixes whose laws bind every project: dispatch, model-tier and
/// fleet rulings. A law with one of these subjects and no scope stamp reads
/// `global`, and the record door stamps it so.
const FLEET_SUBJECT_PREFIXES: [&str; 3] = ["dispatch-", "model-", "fleet-"];

pub fn is_fleet_subject(subject: &str) -> bool {
    FLEET_SUBJECT_PREFIXES
        .iter()
        .any(|p| subject.starts_with(p))
}

/// Whether a stored value names a session reach (`global`, `project:`,
/// `machine:`, `user:`), as against an audience (`internal`, `customers`,
/// `customer:<persona>`). Rows recorded before the audience field kept the
/// reach under `scope`, so the reader tells the two apart by shape.
pub fn is_reach_value(v: &str) -> bool {
    v == "global"
        || ["project:", "machine:", "user:", "person:"]
            .iter()
            .any(|p| v.starts_with(p))
}

/// The stamps a decision row carries for audience and reach, typed at the
/// read.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct ScopeStamps<'a> {
    reach: Option<&'a str>,
    scope: Option<&'a str>,
    subject: Option<&'a str>,
}

/// The audience a row serves: `internal`, `customers` or `customer:<persona>`.
/// `None` for a row recorded before the field existed.
pub fn row_audience(row: &Value) -> Option<&str> {
    let stamps: ScopeStamps = serde::Deserialize::deserialize(row).unwrap_or_default();
    stamps.scope.filter(|v| !is_reach_value(v))
}

/// The reach a row reads as: the reach the row carries (`reach_stamp`),
/// else `global` for a fleet subject, else the pre-scope legacy value.
/// Every other row recorded before the field existed was recorded in fno.
pub fn row_scope(row: &Value) -> &str {
    if let Some(stamp) = reach_stamp(row) {
        return stamp;
    }
    let stamps: ScopeStamps = serde::Deserialize::deserialize(row).unwrap_or_default();
    match stamps.subject {
        Some(subject) if is_fleet_subject(subject) => "global",
        _ => "project:fno",
    }
}

/// The reach the row carries: the `reach` stamp, else a legacy `scope`
/// stamp that names a reach. None for a row recorded without a stamp, so
/// a reader can tell a stamped reach from the unstamped fallback.
pub fn reach_stamp(row: &Value) -> Option<&str> {
    let stamps: ScopeStamps = serde::Deserialize::deserialize(row).unwrap_or_default();
    stamps
        .reach
        .or_else(|| stamps.scope.filter(|v| is_reach_value(v)))
}

/// The session-scope test: `global` always governs; a
/// `project:<slug>` row governs only the matching slug; a `machine:<id>` or
/// `user:<name>` (legacy `person:<name>`) row governs only the session that resolves that identity.
pub fn row_in_scope(row: &Value, project: &str) -> bool {
    filters_match(row)
        && match row_scope(row) {
            "global" => true,
            scoped => binds_here(scoped).unwrap_or_else(|| scoped == format!("project:{project}")),
        }
}

/// The identity arms of the scope test: `Some` for a `machine:` or `user:`
/// scope, `None` for every other scope.
pub fn binds_here(scope: &str) -> Option<bool> {
    binds(scope, crate::claims::machine_id, crate::identity::person)
}

/// Identity is read lazily, inside its own arm: `machine_id` can shell out,
/// and most rows are global or project. A session that reads no machine id
/// fails open, the posture every law reader keeps.
fn binds(
    scope: &str,
    machine: impl FnOnce() -> String,
    person: impl FnOnce() -> String,
) -> Option<bool> {
    if let Some(id) = scope.strip_prefix("machine:") {
        let mine = machine();
        return Some(mine.is_empty() || mine == id);
    }
    scope
        .strip_prefix("user:")
        .or_else(|| scope.strip_prefix("person:"))
        .map(|name| name == person())
}

/// The one visibility test every scope filter shares. `None` fails open on
/// the project: it hides no project row. A row bound to another machine or
/// user still hides, because identity needs no project.
pub fn row_visible(row: &Value, project: Option<&str>) -> bool {
    match project {
        Some(project) => row_in_scope(row, project),
        None => filters_match(row) && binds_here(row_scope(row)) != Some(false),
    }
}

/// The row's filters beside its scope; every filter must match. The matcher
/// and the session facts live in `law_filters`.
pub fn filters_match(row: &Value) -> bool {
    crate::law_filters::filters_match(row)
}

/// Keep only the rows the session may see, returning the withheld count.
pub fn retain_in_scope(index: &mut Index, project: Option<&str>) -> usize {
    let before = index.rows.len();
    index.rows.retain(|row| row_visible(row, project));
    before - index.rows.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn envelope(id: &str, ts: &str, authority: &str, subject: &str, extra: &str) -> String {
        format!(
            "{{\"type\":\"operator_decision\",\"ts\":\"{ts}\",\"data\":{{\"decision_id\":\"{id}\",\
             \"subject\":\"{subject}\",\"decision\":\"Ruling.\",\"text\":\"Ruling.\",\
             \"authority_source\":\"{authority}\"{extra}}}}}"
        )
    }

    fn write_index(dir: &Path, lines: &[String]) -> PathBuf {
        let path = dir.join("decisions.jsonl");
        fs::write(&path, lines.join("\n") + "\n").expect("writes");
        path
    }

    #[test]
    fn ac1_hp_live_laws_keeps_the_three_law_rows_newest_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_index(
            dir.path(),
            &[
                // live law, operator, newest of the three
                envelope(
                    "d-aaaa0001",
                    "2026-09-12T00:00:00Z",
                    "operator",
                    "topic-a",
                    "",
                ),
                // live law, chat_attested
                envelope(
                    "d-bbbb0002",
                    "2026-09-10T00:00:00Z",
                    "chat_attested",
                    "topic-b",
                    "",
                ),
                // agent lane: authority agent is coordination, never law
                envelope("d-cccc0003", "2026-09-11T00:00:00Z", "agent", "topic-c", ""),
                // operator but before the cutover: unattributed, not law
                envelope(
                    "d-dddd0004",
                    "2026-08-01T00:00:00Z",
                    "operator",
                    "topic-d",
                    "",
                ),
                // superseded by d-ffff0006 below
                envelope(
                    "d-eeee0005",
                    "2026-09-05T00:00:00Z",
                    "operator",
                    "topic-e",
                    "",
                ),
                // retraction of d-gggg0007 below
                format!(
                    "{{\"type\":\"decision_retracted\",\"ts\":\"2026-09-13T00:00:00Z\",\
                     \"data\":{{\"target_decision_id\":\"d-gggg0007\",\"reason\":\"r\"}}}}"
                ),
                envelope(
                    "d-gggg0007",
                    "2026-09-06T00:00:00Z",
                    "operator",
                    "topic-g",
                    "",
                ),
                // the superseding law, oldest of the three live ones
                envelope(
                    "d-ffff0006",
                    "2026-09-08T00:00:00Z",
                    "operator",
                    "topic-f",
                    ",\"supersedes\":\"d-eeee0005\"",
                ),
            ],
        );
        let index = live_laws(&path).expect("reads");
        let ids: Vec<&str> = index
            .rows
            .iter()
            .filter_map(|r| r.get("decision_id").and_then(Value::as_str))
            .collect();
        assert_eq!(ids, vec!["d-aaaa0001", "d-bbbb0002", "d-ffff0006"]);
    }

    #[test]
    fn ac1_err_missing_path_is_err_and_bad_lines_count_as_damaged() {
        let missing = live_laws(Path::new("/nonexistent/fno/decisions.jsonl"));
        let reason = missing.expect_err("missing path is the Err");
        assert!(
            reason.contains("/nonexistent/fno/decisions.jsonl"),
            "{reason}"
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_index(
            dir.path(),
            &[
                "not json at all".to_string(),
                envelope(
                    "d-aaaa0001",
                    "2026-09-12T00:00:00Z",
                    "operator",
                    "topic-a",
                    "",
                ),
                "{\"type\":\"operator_decision\"}".to_string(),
                envelope(
                    "d-bbbb0002",
                    "2026-09-10T00:00:00Z",
                    "operator",
                    "topic-b",
                    "",
                ),
            ],
        );
        let index = live_laws(&path).expect("valid rows still return");
        assert_eq!(index.rows.len(), 2);
        assert_eq!(index.damaged, 2);
    }

    fn amend(id: &str, target: &str, reach: &str, ts: &str) -> String {
        format!(
            "{{\"type\":\"decision_scope_amended\",\"ts\":\"{ts}\",\"data\":{{\"amendment_id\":\"{id}\",\
             \"target_decision_id\":\"{target}\",\"reach\":\"{reach}\"}}}}"
        )
    }

    #[test]
    fn a_scope_amend_applies_newest_wins_and_strips_itself() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_index(
            dir.path(),
            &[
                envelope(
                    "d-aaaa0001",
                    "2026-09-12T00:00:00Z",
                    "operator",
                    "topic-a",
                    ",\"reach\":\"project:old\"",
                ),
                amend("d-cccc0003", "d-pseudo1", "project:new", "2026-11-24T00:00:00Z"),
                amend("d-dddd0004", "d-aaaa0001", "global", "2026-11-25T00:00:00Z"),
                "{\"type\":\"decision_scope_amended\",\"ts\":\"2026-11-26T00:00:00Z\",\"data\":{\"amendment_id\":\"d-eeee0005\",\"reach\":\"global\"}}".to_string(),
            ],
        );
        let (mut rows, damaged) = flatten_envelopes(&fs::read_to_string(&path).expect("reads"));
        assert_eq!(damaged, 1, "an amend with no target is the damaged shape");
        apply_scope_amendments(&mut rows);
        assert_eq!(rows.len(), 1, "amendment rows never render");
        let row = rows.first().expect("the one row stays");
        assert_eq!(
            row.get(de::DECISION_ID).and_then(Value::as_str),
            Some("d-aaaa0001")
        );
        assert_eq!(
            row.get(de::REACH).and_then(Value::as_str),
            Some("global"),
            "the newest amendment wins"
        );
    }

    #[test]
    fn the_default_read_skips_archive_rows_and_everything_joins_them_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let graph = dir.path().join("graph.json");
        let jsonl = dir.path().join("decisions.jsonl");
        fs::write(
            &jsonl,
            format!(
                "{}\n{}\n",
                envelope(
                    "d-keep0001",
                    "2026-09-12T00:00:00Z",
                    "operator",
                    "topic-keep",
                    ""
                ),
                envelope(
                    "d-gone0002",
                    "2026-09-11T00:00:00Z",
                    "operator",
                    "topic-gone",
                    ""
                ),
            ),
        )
        .expect("writes");
        let archive = graph_archive(&graph);
        fs::write(
            &archive,
            format!(
                "{{\"type\":\"operator_decision\",\"ts\":\"2026-09-11T00:00:00Z\",\"data\":{{\"decision_id\":\"d-gone0002\",\"subject\":\"topic-gone\",\"decision\":\"Ruling.\",\"authority_source\":\"operator\"}}}}\n"
            ),
        )
        .expect("writes");
        let (rows, _) = read_store_rows(&graph, &jsonl).expect("reads");
        let ids: Vec<&str> = rows
            .iter()
            .filter_map(|r| r.get("decision_id").and_then(Value::as_str))
            .collect();
        assert_eq!(ids, vec!["d-keep0001"], "the archived row skips");
        let (all, _) = read_store_rows_everything(&graph, &jsonl).expect("reads");
        let mut all_ids: Vec<&str> = all
            .iter()
            .filter_map(|r| r.get("decision_id").and_then(Value::as_str))
            .collect();
        all_ids.sort();
        assert_eq!(
            all_ids,
            vec!["d-gone0002", "d-keep0001"],
            "the archive joins back"
        );
    }

    #[test]
    fn two_amendments_of_one_law_keep_distinct_row_keys() {
        let a = amend("d-cccc0003", "d-aaaa0001", "global", "2026-11-24T00:00:00Z");
        let b = amend(
            "d-dddd0004",
            "d-aaaa0001",
            "project:fnh",
            "2026-11-25T00:00:00Z",
        );
        let (rows_a, _) = flatten_envelopes(&a);
        let (rows_b, _) = flatten_envelopes(&b);
        let (Some(first), Some(second)) = (rows_a.first(), rows_b.first()) else {
            panic!("each amend flattens to one row");
        };
        assert_ne!(
            row_key(first),
            row_key(second),
            "two amendments of one law must not collide"
        );
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;

    fn law(id: &str, scope: Option<&str>) -> Value {
        let mut row = serde_json::json!({
            "decision_id": id,
            "subject": "topic",
            "decision": "Ruling.",
            "_event_type": "operator_decision",
            "authority_source": "operator",
        });
        if let Some(scope) = scope {
            if let Some(map) = row.as_object_mut() {
                map.insert("scope".into(), serde_json::json!(scope));
            }
        }
        row
    }

    #[test]
    fn a_foreign_project_row_hides_and_the_count_names_it() {
        let mut index = Index {
            rows: vec![
                law("d-1", Some("global")),
                law("d-2", Some("project:demo")),
                law("d-3", Some("project:etl")),
                law("d-4", None),
            ],
            damaged: 0,
        };
        let hidden = retain_in_scope(&mut index, Some("demo"));
        let kept: Vec<&str> = index
            .rows
            .iter()
            .map(|r| r.get("decision_id").and_then(Value::as_str).unwrap_or(""))
            .collect();
        // d-4 carries no scope, so it reads project:fno and hides here too.
        assert_eq!(kept, vec!["d-1", "d-2"]);
        assert_eq!(hidden, 2);
    }
}

#[cfg(test)]
mod proof_tests {
    use super::*;
    use crate::machine_key::{cert_path, mint_cert, own, MachineKey};

    const AFTER: &str = "2026-11-04T00:00:00Z";

    fn certify(root: &Path, user_key: &MachineKey) {
        let line = mint_cert(user_key, "jason", &own().expect("own key").public_key(), 1);
        std::fs::write(cert_path(root), line).expect("writes");
    }

    /// One store envelope; `signed` seals it under the installed certificate.
    fn envelope(kind: &str, ts: &str, mut data: Value, signed: bool) -> String {
        if signed {
            let map = data.as_object_mut().expect("object");
            crate::identity::sign_law_row(map, kind, ts);
        }
        json!({"ts": ts, "type": kind, "data": data}).to_string()
    }

    fn law(id: &str, scope: &str) -> Value {
        json!({
            "decision_id": id, "subject": "topic", "decision": "Ruling.",
            "authority_source": "operator", "scope": scope,
        })
    }

    #[test]
    fn a_certified_machine_reads_only_wide_law_its_user_key_signed() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = crate::claims::EnvVarGuard::set(
            crate::paths::HOME_ENV,
            dir.path().to_str().expect("utf-8"),
        );
        let mine = MachineKey::load_or_create(&dir.path().join("mine.pk8")).expect("key");
        let theirs = MachineKey::load_or_create(&dir.path().join("theirs.pk8")).expect("key");
        certify(dir.path(), &theirs);
        let op = "operator_decision";
        let foreign = envelope(op, AFTER, law("d-0000000b", "global"), true);
        certify(dir.path(), &mine);
        let jsonl = dir.path().join("decisions.jsonl");
        let lines = [
            envelope(op, AFTER, law("d-0000000a", "global"), true),
            foreign,
            envelope(op, AFTER, law("d-0000000c", "global"), false),
            envelope(
                op,
                "2026-11-01T00:00:00Z",
                law("d-0000000d", "global"),
                false,
            ),
            envelope(op, AFTER, law("d-0000000e", "project:demo"), false),
        ];
        std::fs::write(&jsonl, lines.join("\n") + "\n").expect("writes");
        let graph = dir.path().join("graph.json");
        let (rows, _) = read_store_rows(&graph, &jsonl).expect("reads");
        assert_eq!(withheld_unproven(), 2);
        let mut ids: Vec<&str> = rows
            .iter()
            .filter_map(|r| r.get("decision_id").and_then(Value::as_str))
            .collect();
        ids.sort();
        // The signed row, the legacy row and the project row stay.
        assert_eq!(ids, ["d-0000000a", "d-0000000d", "d-0000000e"]);
        // With no certificate the machine reads every row, as before.
        std::fs::remove_file(cert_path(dir.path())).expect("removes");
        assert_eq!(read_store_rows(&graph, &jsonl).expect("reads").0.len(), 5);
        assert_eq!(withheld_unproven(), 0);
    }

    #[test]
    fn one_lane_map_reads_old_role_spellings_and_answers_end_with_their_subject() {
        let at = |authority: &str| json!({"authority_source": authority, "ts": "2027-01-01T00:00:00Z", "subject": "x-aaaa"});
        let legacy = crate::role_migration::LEGACY_ROLE_AUTHORITIES;
        for role in ["role", "agent"].into_iter().chain(legacy.iter().copied()) {
            assert_eq!(decision_lane(&at(role)), "agent", "{role}");
        }
        assert_eq!(decision_lane(&at("operator")), "user");
        assert_eq!(decision_lane(&at("beastmode")), "grant");
        assert_eq!(decision_lane(&at("banana")), "unattributed");
        assert_eq!(authority_of(&at("banana")), "");
        // A standing law has no default end; a question answer ends with
        // its subject in any lane.
        assert!(!expiry_defaults_to_subject(&at("operator")));
        let mut answer = at("operator");
        if let Some(map) = answer.as_object_mut() {
            map.insert("question_id".into(), json!("q-1"));
        }
        assert!(expiry_defaults_to_subject(&answer));
        assert_eq!(
            pr_expiry_ref(Some("merge-grant:fnodotdev/fno#12@abc")),
            Some(json!({"kind": "pr", "repository": "fnodotdev/fno", "number": 12}))
        );
        assert_eq!(
            parse_expiry("never", "x-aaaa").unwrap(),
            json!({"kind": "never"})
        );
        assert_eq!(
            parse_expiry("node", "x-aaaa").unwrap(),
            json!({"kind": "node", "node_id": "x-aaaa"})
        );
        assert!(parse_expiry("pr", "x-aaaa").is_err());
        let past = parse_expiry("date:2020-01-01T00:00:00Z", "").unwrap();
        assert_eq!(
            timed_lifecycle(&past, chrono::Utc::now()).unwrap().0,
            "expired"
        );
        let mut index = Index {
            rows: vec![
                json!({"decision_id": "d-1", "expiry_ref": past}),
                json!({"decision_id": "d-2", "expiry_ref": {"kind": "never"}}),
            ],
            damaged: 0,
        };
        drop_expired(&mut index, Path::new("/nonexistent/graph.db"));
        assert_eq!(index.rows.len(), 1, "a passed date drops; never stays");
    }
}
