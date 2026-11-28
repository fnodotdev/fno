//! The decide door: the `fno inbox decide` / `fno backlog decide`
//! record command, its provenance lanes, and the decide-reindex recovery
//! command, ported from the deleted Python decide family. Shared law-door
//! helpers stay in `law_match`; this module reaches them through
//! `crate::law_match`.

use serde_json::{json, Map, Value};

use crate::decision_trace::{actor_kind_from_authority, emit_span_to, new_span_id, Trace};
use crate::law_match::{
    all_decision_rows, attended_terminal, evidence_repo_root, is_retraction_row, now_iso,
    project_events_journal, text_cap, UNATTRIBUTED_CALLER, WAIVER_SUBJECT_PREFIX,
};

// ---------------------------------------------------------------------------
// The decide door: the `decide` mode carrying the `fno inbox decide` /
// `fno backlog decide` argv. The port of `_record` + `record_decision`
// (cli/src/fno/decide/cli.py, cli/src/fno/decide/__init__.py, deleted): the
// same gate order, the same refusal texts, the same receipt lines, and the
// same exit contract (0 recorded, 1 recorded-but-index-failed or a failed
// write, 2 a bad --authority or graduation, 3 refused). The law door above
// is the chat_attested/operator slice of this door; the decide door adds the
// agent, role, and beastmode lanes with their origin floor, relayed_by
// stamp, coord closure key, and waiver guard.
// ---------------------------------------------------------------------------

/// The closed mail-origin vocabulary (`MAIL_ORIGINS`). Origin is evidence
/// about the channel, never an authority claim.
pub(crate) const MAIL_ORIGINS: &[&str] = &["operator", "peer", "scheduler", "recovery"];

/// The highest authority an origin may carry (`MAX_AUTHORITY_BY_ORIGIN`).
fn max_authority_by_origin(origin: &str) -> &'static str {
    match origin {
        "operator" => "operator",
        _ => "agent",
    }
}

/// The authority lanes the decide CLI accepts: the shared enum less
/// `chat_attested`, which is the standing-decision door's lane and never a
/// decide flag value.
const AUTHORITY_SOURCES: &[&str] = &["operator", "role", "agent", "beastmode"];
const _: () =
    assert!(AUTHORITY_SOURCES.len() + 1 == crate::decision_index::AUTHORITY_SOURCES.len());

/// The caller identity a decide run resolves under: `Ambient` reads the
/// process truth (the ancestry prover); `Forced` injects a handle (`Some`)
/// or a scrubbed identity (`None`), the seam the tests drive where the law
/// door injects a caller. A `Forced` handle holds no team. `Seeded` is the
/// write-path test identity: it carries the full session triple so the
/// provenance capture runs without the ambient prover, whose census walk
/// reads whatever host the suite lands on.
#[derive(Clone, Copy)]
pub(crate) enum DecideIdentity {
    Ambient,
    #[cfg_attr(not(test), expect(dead_code))]
    Forced(Option<&'static str>),
    #[cfg(test)]
    Seeded(&'static str, &'static str),
}

/// The canonical handle of the agent session this process runs as, `None`
/// when the caller is no agent session.
pub(crate) fn ambient_agent_handle() -> Option<String> {
    DecideIdentity::Ambient.agent()
}

impl DecideIdentity {
    /// The full session identity this process resolves as: the session id
    /// and harness the ambient prover names, `None` when the caller is no
    /// agent session or the run injected an identity (a test seam carries a
    /// handle, never a transcript).
    fn session_identity(self) -> Option<(String, String)> {
        match self {
            Self::Ambient => {
                let get = |name: &str| std::env::var(name).ok();
                let ident = crate::spawn_context::resolve_self_identity(
                    &get,
                    None,
                    None,
                    &crate::paths::AgentsHome::from_env(),
                );
                let session = ident.session_id.as_deref().map(str::trim).unwrap_or("");
                let harness = ident.harness.as_deref().map(str::trim).unwrap_or("");
                (!session.is_empty() && !harness.is_empty())
                    .then(|| (session.to_string(), harness.to_string()))
            }
            Self::Forced(_) => None,
            #[cfg(test)]
            Self::Seeded(session, harness) => {
                Some(((*session).to_string(), (*harness).to_string()))
            }
        }
    }

    fn agent(self) -> Option<String> {
        match self {
            Self::Ambient => self
                .session_identity()
                .map(|(session, _)| crate::identity::canonical_handle(&session)),
            Self::Forced(None) => None,
            Self::Forced(Some(h)) => Some((*h).to_string()),
            #[cfg(test)]
            Self::Seeded(session, _) => Some(crate::identity::canonical_handle(session)),
        }
    }

    /// Whether `handle` holds a live team whose territory covers `subject`.
    /// An unreadable registry or graph reads as no cover (fail closed).
    fn role_covers(self, handle: &str, subject: &str) -> bool {
        match self {
            Self::Ambient => ambient_role_covers(handle, subject),
            Self::Forced(_) => false,
            #[cfg(test)]
            Self::Seeded(_, _) => false,
        }
    }
}

fn ambient_role_covers(handle: &str, subject: &str) -> bool {
    let registry = crate::paths::AgentsHome::from_env().registry_json();
    let Ok(teams) = crate::territory::live_teams(&registry) else {
        return false;
    };
    let mine: Vec<crate::territory::Team> = teams
        .into_iter()
        .filter(|t| crate::question_clear::holds_team(std::slice::from_ref(t), handle))
        .collect();
    let Ok(entries) = crate::backlog::decisions_cli::read_graph_entries() else {
        return false;
    };
    let projects = std::env::current_dir()
        .map_err(|e| e.to_string())
        .and_then(|cwd| crate::org_board::scope::project_map(&cwd));
    role_covers_subject(&mine, subject, &entries, &projects)
}

/// The role-scope rule: the caller holds a team, and a subject that names a
/// node (by id or slug) must sit inside one of the caller's territories. A
/// topic subject names no node, so holding a team is enough there.
pub(crate) fn role_covers_subject(
    mine: &[crate::territory::Team],
    subject: &str,
    entries: &[Value],
    projects: &Result<std::collections::HashMap<String, String>, String>,
) -> bool {
    if mine.is_empty() {
        return false;
    }
    let field = |e: &Value, k: &str| e.get(k).and_then(Value::as_str).map(str::to_owned);
    let Some(node) = entries
        .iter()
        .find(|e| {
            field(e, "id").as_deref() == Some(subject)
                || field(e, "slug").as_deref() == Some(subject)
        })
        .and_then(|e| field(e, "id"))
    else {
        return true;
    };
    mine.iter().any(|t| {
        crate::territory::compile_scope_ids(&t.scope, entries, projects)
            .is_ok_and(|ids| ids.contains(&node))
    })
}

/// The authority lanes the origin floor can cap (`enforce_origin_floor`): an
/// ambient agent identity cannot declare an origin above peer.
pub(crate) fn enforce_origin_floor(origin: Option<&str>, id: DecideIdentity) -> Option<String> {
    let origin = origin?;
    if origin == "peer" {
        return Some(origin.to_string());
    }
    if id.agent().is_some() {
        return Some("peer".to_string());
    }
    Some(origin.to_string())
}

/// Who recorded a ruling, and how much of that a reader may trust
/// (`Provenance`, a named struct in Python for the same reason).
pub(crate) struct DecideProvenance {
    pub(crate) decided_by: String,
    pub(crate) authority_source: Option<String>,
    pub(crate) attested_by: Option<String>,
    pub(crate) relayed_by: Option<String>,
}

/// The provenance refusals, each carrying the exact Python error text so the
/// CLI's remedy wording stays one law.
pub(crate) enum DecideRefusal {
    UnknownOrigin(String),
    RefusedAuthority {
        agent: String,
        origin: Option<String>,
    },
    UnattributedAuthority,
}

/// The decide door's parsed argv.
pub(crate) struct DecideDoor {
    subject: Option<String>,
    decision: Option<String>,
    question_id: Option<String>,
    rationale: Option<String>,
    options: Vec<String>,
    supersedes: Option<String>,
    answers_ask: Option<String>,
    class: Option<String>,
    overturns: Option<String>,
    decided_by: Option<String>,
    authority: Option<String>,
    graduation: Option<String>,
    graduation_ref: Option<String>,
    reads: Vec<String>,
    origin: Option<String>,
    expiry: Option<String>,
    /// The session reach the row stamps (`--reach`); derived from the
    /// caller's project when the flag is absent and the subject binds no
    /// wider scope.
    reach: Option<String>,
    /// Who the ruling serves (`--audience`; `--scope` is a one-release alias): `internal`, `customers` or `customer:<persona>`.
    audience: Option<String>,
    /// The goal behind the ask (`--why`); the user's words when the ask came
    /// from him. Required unless `--answers-ask` carries it.
    why: Option<String>,
    /// The persona jobs served (`--job <persona>/<job-slug>`, repeatable);
    /// one per named persona when the scope names personas.
    job: Vec<String>,
    keep: Vec<String>,
}

const DECIDE_USAGE: &str = "usage: fno inbox decide <subject> <decision> [--question-id q] [--rationale s] [--option s]... [--supersedes d-x] [--answers-ask s] [--class c] [--overturns s-or-d] --audience internal|customers|customer:<persona> --why s [--job p/s] [--expiry node|pr|date:<when>|never] [--reach global|project:<slug>|machine:<id>|user:<name>] [--keep d-x]... [--decided-by name] [--authority operator|role|agent|beastmode] [--graduation k] [--graduation-ref r] [--read cmd]... [--origin o]  -- a ruling records in one step: audience and why are required when it lands, and a customer ruling names its persona there too.";

/// The `--reach` value validated at parse: None when the value is not a
/// session reach the scope readers answer.
fn parse_reach_flag(v: &str) -> Option<String> {
    crate::decision_index::is_reach_value(v).then(|| v.to_string())
}

/// The parse refusal for a value that is not a session reach.
fn reach_refusal(v: &str) -> String {
    format!(
        "--reach '{v}' is not a session reach (global, project:<slug>, \
machine:<id>, user:<name>)\n{DECIDE_USAGE}"
    )
}

/// A parse-empty door: every field unset, so the parse loop assigns one
/// flag at a time against a known shape.
fn empty_door() -> DecideDoor {
    DecideDoor {
        subject: None,
        decision: None,
        question_id: None,
        rationale: None,
        options: Vec::new(),
        supersedes: None,
        answers_ask: None,
        class: None,
        overturns: None,
        decided_by: None,
        authority: None,
        graduation: None,
        graduation_ref: None,
        reads: Vec::new(),
        origin: None,
        expiry: None,
        reach: None,
        audience: None,
        why: None,
        job: Vec::new(),
        keep: Vec::new(),
    }
}

pub(crate) fn parse_decide_door(args: &[String]) -> Result<DecideDoor, String> {
    let mut door = empty_door();
    let mut positional = 0usize;
    let mut i = 0usize;
    while i < args.len() {
        let (flag, inline) = match args[i].split_once('=') {
            Some((f, v)) => (f.to_string(), Some(v.to_string())),
            None => (args[i].clone(), None),
        };
        let take = |i: &mut usize| -> Result<String, String> {
            if let Some(v) = &inline {
                return Ok(v.clone());
            }
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value\n{DECIDE_USAGE}"))
        };
        match flag.as_str() {
            "--subject" => {
                let v = take(&mut i)?;
                if door.subject.is_some() {
                    return Err("pass either <subject> or --subject (deprecated), not both".into());
                }
                eprintln!(
                    "warning: --subject is deprecated; use <subject> instead. \
The alias will be removed in a future release."
                );
                door.subject = Some(v);
            }
            "--decision" => {
                let v = take(&mut i)?;
                if door.decision.is_some() {
                    return Err(
                        "pass either <decision> or --decision (deprecated), not both".into(),
                    );
                }
                eprintln!(
                    "warning: --decision is deprecated; use <decision> instead. \
The alias will be removed in a future release."
                );
                door.decision = Some(v);
            }
            "--question-id" => door.question_id = Some(take(&mut i)?),
            "--rationale" => door.rationale = Some(take(&mut i)?),
            "--option" => door.options.push(take(&mut i)?),
            "--supersedes" => door.supersedes = Some(take(&mut i)?),
            "--answers-ask" => door.answers_ask = Some(take(&mut i)?),
            "--class" => door.class = Some(take(&mut i)?),
            "--overturns" => door.overturns = Some(take(&mut i)?),
            "--expiry" => door.expiry = Some(take(&mut i)?),
            "--reach" => {
                let v = take(&mut i)?;
                door.reach = Some(parse_reach_flag(&v).ok_or_else(|| reach_refusal(&v))?);
            }
            "--scope" | "--audience" => door.audience = Some(take(&mut i)?),
            "--why" => door.why = Some(take(&mut i)?),
            "--job" => door.job.push(take(&mut i)?),
            "--keep" => door
                .keep
                .extend(crate::law_reach::split_ids(&take(&mut i)?)),
            "--decided-by" => door.decided_by = Some(take(&mut i)?),
            "--authority" => door.authority = Some(take(&mut i)?),
            "--graduation" => door.graduation = Some(take(&mut i)?),
            "--graduation-ref" => door.graduation_ref = Some(take(&mut i)?),
            "--read" => door.reads.push(take(&mut i)?),
            "--origin" => door.origin = Some(take(&mut i)?),
            f if f.starts_with('-') && f != "-" => {
                return Err(format!("no such option: {f}\n{DECIDE_USAGE}"));
            }
            _ => {
                positional += 1;
                if positional == 1 {
                    door.subject = Some(args[i].clone());
                } else if positional == 2 {
                    door.decision = Some(args[i].clone());
                } else {
                    return Err(format!("too many positional arguments\n{DECIDE_USAGE}"));
                }
            }
        }
        i += 1;
    }
    Ok(door)
}

pub(crate) use crate::decision_index::decision_lane;

/// The claim context receipt (`_render_claim_receipt`): advisory, and a
/// receipt failure cannot undo a ruling.
fn render_decide_claim_receipt(node_id: &str, decided_by: &str) {
    let key = format!("node:{node_id}");
    let caller_label = if decided_by.is_empty() {
        "unknown caller"
    } else {
        decided_by
    };
    let (state, holder, error) = {
        let (state, record) = crate::claims::status(&key, None);
        (
            state.as_str().to_string(),
            record.map(|r| r.holder),
            None::<String>,
        )
    };
    let mut line = format!("decide: claim {key}; claim state: {state}; caller: {caller_label}");
    if state == "free" {
        eprintln!("{line}");
        return;
    }
    if let Some(h) = &holder {
        line.push_str(&format!("; holder: {h}"));
    }
    let mut comparison = "caller comparison unavailable".to_string();
    if (state == "live" || state == "suspect") && holder.is_some() {
        let holder = holder.as_deref().unwrap_or_default();
        if let Some((prefix, holder_session)) = holder.split_once(':') {
            if prefix == "target-session" && !holder_session.is_empty() {
                let mut caller_session = std::env::var("TARGET_SESSION_ID")
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty());
                if caller_session.is_none() {
                    let get = |name: &str| std::env::var(name).ok();
                    let ident = crate::spawn_context::resolve_self_identity(
                        &get,
                        None,
                        None,
                        &crate::paths::AgentsHome::from_env(),
                    );
                    caller_session = ident
                        .session_id
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                }
                if let Some(caller) = caller_session {
                    comparison = if holder_session == caller {
                        "this caller holds the node".to_string()
                    } else {
                        "another session holds the node, not this caller".to_string()
                    };
                }
            }
        }
    }
    if let Some(e) = error {
        line.push_str(&format!("; error: {e}"));
    }
    eprintln!("{line}; {comparison}");
}

/// The machine decision index, beside the ledger (`_decisions_index_path`).
fn decisions_jsonl_path() -> std::path::PathBuf {
    crate::decision_index::default_state_path("decisions.jsonl")
}

/// This repo's project journal, the record door's durability write.
fn decisions_project_journal() -> std::path::PathBuf {
    project_events_journal()
}

/// The machine journal (`paths.global_events_json`), when it resolves.
fn global_events_journal_path() -> Option<std::path::PathBuf> {
    crate::decision_index::default_state_path("events.jsonl")
        .exists()
        .then(|| crate::decision_index::default_state_path("events.jsonl"))
}

/// The row identity `row_key` applies (`decide/__init__.py:814`).
fn decision_index_key(row: &Value) -> (String, String) {
    let etype = row
        .get("_event_type")
        .and_then(Value::as_str)
        .unwrap_or("operator_decision")
        .to_string();
    let id = ["decision_id", "retraction_id", "target_decision_id"]
        .iter()
        .find_map(|k| row.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()))
        .unwrap_or("")
        .to_string();
    (etype, id)
}

/// Compact the raw index: drop damaged lines into a `.corrupt` sibling and
/// rewrite the good ones atomically. Returns the dropped count. The store
/// is the read boundary; this repairs the legacy raw file the Python
/// `_compact_index` repaired.
fn compact_decisions_index() -> usize {
    let path = decisions_jsonl_path();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let mut good: Vec<&str> = Vec::new();
    let mut dropped: Vec<&str> = Vec::new();
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let ok = serde_json::from_str::<Value>(line)
            .map(|v| {
                let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
                let is_type = ty == "operator_decision" || ty == "decision_retracted";
                let data = v.get("data").cloned().unwrap_or(Value::Null);
                let id = if ty == "operator_decision" {
                    data.get("decision_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                } else {
                    data.get("target_decision_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                };
                is_type && !id.is_empty()
            })
            .unwrap_or(false);
        if ok {
            good.push(line);
        } else {
            dropped.push(line);
        }
    }
    if dropped.is_empty() {
        return 0;
    }
    let corrupt = path.with_extension("jsonl.corrupt");
    if let Ok(mut fh) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&corrupt)
    {
        use std::io::Write as _;
        let _ = writeln!(fh, "{}", dropped.join("\n"));
    }
    let tmp = path.with_extension("jsonl.compact");
    if std::fs::write(&tmp, format!("{}\n", good.join("\n"))).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
    dropped.len()
}

/// The decide engine's graduation validator (`validate_graduation` in
/// cli/src/fno/decide/graduation.py, deleted): lowercase-normalized, and
/// the unknown-kind refusal names no value. The law door's twin above
/// carries its own wording; the decide texts are not that law.
fn decide_graduation(kind: Option<&str>, reference: Option<&str>) -> Result<Value, String> {
    let kind = kind.unwrap_or("").trim().to_lowercase();
    let reference = reference.unwrap_or("").trim();
    match kind.as_str() {
        "guidance" => {
            if !reference.is_empty() {
                return Err("guidance takes no graduation reference".to_string());
            }
            Ok(json!({"kind": "guidance"}))
        }
        "enforced" => {
            static ARTIFACT_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
            let re = ARTIFACT_RE.get_or_init(|| {
                regex::Regex::new(r"^(file|test|doc|gate|default):\S(?:.*\S)?$").expect("parses")
            });
            if !re.is_match(reference) {
                return Err(
                    "enforced graduation requires file:, test:, doc:, gate:, or default:"
                        .to_string(),
                );
            }
            Ok(json!({"kind": "enforced", "artifact": reference}))
        }
        "should-be-enforced-but-i-did-not" => {
            static FOLLOW_UP_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
            let re = FOLLOW_UP_RE.get_or_init(|| {
                regex::Regex::new(r"(?i)^node:[a-z][a-z0-9]*-[0-9a-f]+$").expect("parses")
            });
            if !re.is_match(reference) {
                return Err("should-be-enforced-but-i-did-not requires node:<id>".to_string());
            }
            Ok(json!({
                "kind": "should-be-enforced-but-i-did-not",
                "follow_up": reference.to_ascii_lowercase(),
            }))
        }
        _ => Err(
            "graduation must be enforced, guidance, or should-be-enforced-but-i-did-not"
                .to_string(),
        ),
    }
}

/// The decide wrapper: an omitted declaration defaults to guidance.
fn decide_graduation_or_guidance(
    kind: Option<&str>,
    reference: Option<&str>,
) -> Result<Value, String> {
    match (kind, reference) {
        (None, None) => Ok(json!({"kind": "guidance"})),
        _ => decide_graduation(Some(&kind.unwrap_or("")), reference),
    }
}

/// The decide-reindex command, ported from `reindex` (cli/src/fno/decide/
/// __init__.py, deleted): compact damaged rows out of the raw index and
/// recover any journaled row the index store lacks, without minting ids.
/// The machine-wide project-roots walk the Python engine paid is not
/// re-derived: the store's migration absorbed the historical journals
/// machine-wide into graph.db, and a repo running its own decide-reindex
/// recovers its own journal-only row. Counts mirror the Python keys.
pub fn run_decide_reindex(_argv: &[String]) -> i32 {
    // A backfill cannot degrade: an unreadable store must not read as done.
    let graph_path = crate::graph_get::default_graph_path();
    if let Err(e) = crate::decision_index::read_store_rows_raw(&graph_path, &decisions_jsonl_path())
    {
        eprintln!("backlog decide-reindex: failed: {e}");
        return 1;
    }
    let repaired = compact_decisions_index();
    let (rows, _) =
        crate::decision_index::read_store_rows_raw(&graph_path, &decisions_jsonl_path())
            .unwrap_or((Vec::new(), 0));
    let mut known: std::collections::BTreeSet<(String, String)> =
        rows.iter().map(decision_index_key).collect();
    let prior_keys = known.clone();
    let mut counted: std::collections::BTreeSet<(String, String)> = Default::default();
    let mut added = 0usize;
    let mut already = 0usize;
    let mut invalid = 0usize;
    let mut unusable = 0usize;
    let mut journals = vec![decisions_project_journal()];
    if let Some(g) = global_events_journal_path() {
        journals.push(g);
    }
    for journal in journals {
        let text = match crate::event_store::journal_text_checked(
            &journal,
            &crate::event_store::EventQuery::of_types(&["operator_decision", "decision_retracted"]),
        ) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for line in text.lines() {
            let Ok(env) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let ty = env.get("type").and_then(Value::as_str).unwrap_or("");
            if ty != "operator_decision" && ty != "decision_retracted" {
                continue;
            }
            let data = env.get("data").cloned().unwrap_or(Value::Null);
            let id = if ty == "operator_decision" {
                data.get("decision_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            } else {
                data.get("target_decision_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            };
            if id.is_empty() {
                unusable += 1;
                continue;
            }
            let key = (ty.to_string(), id.to_string());
            if known.contains(&key) {
                if prior_keys.contains(&key) && !counted.contains(&key) {
                    counted.insert(key.clone());
                    already += 1;
                }
                continue;
            }
            if let Err(_) = crate::event_store::append_envelope(&decisions_jsonl_path(), line, None)
            {
                invalid += 1;
            } else {
                known.insert(key);
                added += 1;
            }
        }
    }
    // The graph leg last, so a journal-only row the loop above just indexed
    // lands in the table in the same run.
    let (replay, graph_error) =
        match replay_unstored_decisions(&graph_path, &decisions_jsonl_path(), None) {
            Ok(r) => (r, None),
            Err(e) => (ReplayCounts::default(), Some(e)),
        };
    let mut counts = json!({
        "added": added,
        "already": already,
        "repaired": repaired,
        "invalid": invalid,
        "unusable": unusable,
        "total": known.len(),
        "graph_replayed": replay.replayed,
        "graph_refused": replay.refused,
    });
    // The index leg already ran, so its counts print either way; an
    // unreadable graph still fails the command.
    if let Some(e) = &graph_error {
        counts["graph_error"] = json!(e);
    }
    println!("{counts}");
    if let Some(e) = graph_error {
        eprintln!("backlog decide-reindex: the graph replay failed: {e}");
        return 1;
    }
    0
}

/// The recovery every "graph store refused" line names.
pub(crate) const GRAPH_RETRY: &str = "On a machine that shares its backlog, the store \
beat retries the graph write each minute; `fno backlog decide-reindex` retries it now";

/// One replay pass: rows written to the graph decisions table, and rows the
/// store refused again. A refused row stays in the index for the next pass.
#[derive(Debug, Default, PartialEq)]
pub struct ReplayCounts {
    pub replayed: usize,
    pub refused: usize,
}

/// Write each index row the graph decisions table lacks, in index order,
/// with its own id and ts. A door that refused the graph write left the row
/// in the journal and the index only; this is its retry. `cap` bounds the
/// rows one pass writes; a refused row costs no slot, so rows the store
/// keeps refusing never starve the rows behind them. The table read is the
/// db alone, because
/// `read_store_rows` unions the index and would hide the gap. A replayed
/// write is harmless: the insert is `ON CONFLICT(event_id) DO NOTHING`.
pub fn replay_unstored_decisions(
    graph: &std::path::Path,
    index: &std::path::Path,
    cap: Option<usize>,
) -> Result<ReplayCounts, String> {
    let mut counts = ReplayCounts::default();
    let index_present = index.exists() || crate::event_store::store_path(index).exists();
    if !index_present || !crate::backlog::database_path(graph).exists() {
        return Ok(counts);
    }
    let store = crate::backlog::api::Store::new(graph);
    let event_key = |ty: &str, data: &Value| {
        let field = if ty == "operator_decision" {
            "decision_id"
        } else {
            "retraction_id"
        };
        data.get(field)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let mut stored: std::collections::HashSet<String> =
        crate::backlog::api::decisions(&store, None, None)
            .map_err(|e| e.0)?
            .iter()
            .map(|row| event_key(row["_event_type"].as_str().unwrap_or(""), row))
            .collect();
    let text = crate::event_store::journal_text_checked(
        index,
        &crate::event_store::EventQuery::of_types(&["operator_decision", "decision_retracted"]),
    )?;
    for line in text.lines() {
        let Ok(env) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let ty = env.get("type").and_then(Value::as_str).unwrap_or("");
        if ty != "operator_decision" && ty != "decision_retracted" {
            continue;
        }
        let key = event_key(ty, &env["data"]);
        if key.is_empty() || stored.contains(&key) {
            continue;
        }
        if cap.is_some_and(|cap| counts.replayed >= cap) {
            break;
        }
        let write = if ty == "operator_decision" {
            crate::backlog::api::decision_record(&store, env.clone())
        } else {
            crate::backlog::api::decision_retract(&store, env.clone())
        };
        match write {
            Ok(_) => {
                stored.insert(key);
                counts.replayed += 1;
            }
            Err(_) => counts.refused += 1,
        }
    }
    Ok(counts)
}

/// The decide door entry: parse, then the gated write. The argv is the
/// `fno inbox decide` / `fno backlog decide` command line minus its command
/// tokens.
pub fn run_decide_door(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{DECIDE_USAGE}");
        return 0;
    }
    let door = match parse_decide_door(args) {
        Ok(d) => d,
        Err(usage) => {
            eprintln!("fno inbox decide: {usage}");
            return 2;
        }
    };
    decide_door_write(door, DecideIdentity::Ambient)
}

/// The gated decide write: the gate ladder of `_record` + `record_decision`,
/// in the Python order. Exit contract: 0 recorded, 1 failed write or a
/// supersession target the index cannot recover, 2 a bad flag value, 3
/// refused.
pub(crate) fn decide_door_write(mut door: DecideDoor, id: DecideIdentity) -> i32 {
    // Positional subject and decision are required to record.
    let (subject, decision) = match (&door.subject, &door.decision) {
        (Some(s), Some(d)) if !s.trim().is_empty() && !d.trim().is_empty() => {
            (s.trim().to_string(), d.clone())
        }
        _ => {
            eprintln!("decide: subject and decision are required to record");
            return 1;
        }
    };
    // The authority enum binds where the value is authored; the reader stays
    // permissive (rows on disk carry invented spellings).
    if let Some(a) = &door.authority {
        if !AUTHORITY_SOURCES.contains(&a.as_str()) {
            eprintln!(
                "decide: --authority '{a}' is not one of {}. Nothing was recorded. Use 'role' \
for a lead ruling inside its own scope; omit the flag to resolve it from this session.",
                AUTHORITY_SOURCES.join(", ")
            );
            return 2;
        }
    }
    let graduation = match decide_graduation_or_guidance(
        door.graduation.as_deref(),
        door.graduation_ref.as_deref(),
    ) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("backlog decide: refused. {e}. Nothing was recorded.");
            return 2;
        }
    };
    // A class must name a routable hop; the six values are the schema's.
    if let Some(c) = door
        .class
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        if !crate::decision_trace::DECISION_CLASSES.contains(&c) {
            eprintln!(
                "decide: --class '{c}' is not one of {}. Nothing was recorded.",
                crate::decision_trace::DECISION_CLASSES.join(", ")
            );
            return 2;
        }
    }
    // The origin floor binds before the provenance resolution, so the event
    // records the gated value (the same order Python applies).
    let caller_session = id.agent();
    let origin = enforce_origin_floor(door.origin.as_deref(), id);
    let provenance = match resolve_decider_lanes(
        id,
        door.decided_by.as_deref(),
        door.authority.as_deref(),
        origin.as_deref(),
    ) {
        Ok(p) => p,
        Err(DecideRefusal::UnknownOrigin(o)) => {
            eprintln!(
                "decide: refused. mail origin '{o}' is unknown; use one of {}",
                MAIL_ORIGINS.join(", ")
            );
            return 3;
        }
        Err(DecideRefusal::RefusedAuthority { agent, .. }) => {
            return decide_authority_refusal(&agent);
        }
        Err(DecideRefusal::UnattributedAuthority) => return decide_unattributed_refusal(),
    };
    // The role lane is a lead's ruling inside its own scope, and gates read
    // it (the visual gate clears on a role row). An agent session must hold
    // a live team that covers the subject; a caller with no identity and no
    // terminal holds none.
    if provenance.authority_source.as_deref() == Some("role") {
        let allowed = match caller_session.as_deref() {
            Some(handle) => id.role_covers(handle, &subject),
            None => provenance.decided_by != "unattributed-caller",
        };
        if !allowed {
            return decide_role_refusal(&provenance.decided_by);
        }
    }
    // A pr-hold ruling's text is the hold phrase or a release; anything
    // else records a row the merge gate silently ignores. Refuse here,
    // before anything is written, and name both live forms.
    if let Some(message) = hold_text_gate(&subject, &decision) {
        eprintln!("decide: refused. {message}");
        return 3;
    }
    // The explicit reach rides the same user gate the law door applies to
    // its scope stamp: a reach past this machine's own user needs the user
    // certificate, whatever the authority lane. It fires only where the
    // flag takes effect: the door subjects' global pin overrides the flag
    // (a hold is a machine contract that binds by repo and number), and
    // the project derivation is never wide.
    if let Some(v) = door
        .reach
        .as_deref()
        .filter(|_| !crate::decision_index::is_pr_door_subject(&subject))
    {
        if let Some(refusal) = crate::law_match::wide_scope_refusal(v) {
            eprintln!("decide: refused. {refusal}");
            return 3;
        }
    }
    // The evidence gate: a measured claim carries the read that produced it.
    // Exempt when the RESOLVED authority is operator (an attended terminal;
    // the waiver command inherits this).
    let mut read_rows: Option<Vec<Value>> = None;
    if provenance.authority_source.as_deref() != Some("operator") {
        let text = format!(
            "{decision}\n{}",
            door.rationale.as_deref().unwrap_or_default()
        );
        let root = evidence_repo_root();
        let mut runner =
            |cmd: &str, root: &std::path::Path| crate::evidence::shell_run(cmd, root, 20);
        match crate::evidence::check_ruling_evidence(&text, &door.reads, &root, &mut runner) {
            Ok(rows) => read_rows = rows,
            Err(gate) => {
                eprintln!("decide: refused. {} Nothing was recorded.", gate.message);
                return 3;
            }
        }
    }
    // A waiver subject is operator-evidence-only; the check reads the
    // RESOLVED authority, never the caller's claim.
    let waiver_hit = subject == WAIVER_SUBJECT_PREFIX
        || subject.starts_with(&format!("{WAIVER_SUBJECT_PREFIX}:"));
    if waiver_hit && provenance.authority_source.as_deref() != Some("operator") {
        return decide_waiver_refusal(&subject);
    }
    // The correction lane: overturning is the user lane. The gate reads the
    // RESOLVED authority, the same reading the waiver gate takes.
    let overturns = door
        .overturns
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    if overturns.is_some() && provenance.authority_source.as_deref() != Some("operator") {
        eprintln!("decide: --overturns requires --authority operator. Nothing was recorded.");
        return 2;
    }
    // A number names its hash before any check below reads the token.
    for token in [&mut door.supersedes, &mut door.overturns] {
        if let Some(typed) = token.clone() {
            match crate::numbers::resolve('d', &typed) {
                Some(hash) => *token = Some(hash),
                None => {
                    eprintln!(
                        "decide: {}. Nothing was recorded.",
                        crate::numbers::unresolved(&typed)
                    );
                    return 2;
                }
            }
        }
    }
    let overturns = door
        .overturns
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    // --overturns extends --supersedes when the target has a decision id; the
    // recoverability gate below covers the derived link the same way.
    if let (None, Some(t)) = (&door.supersedes, overturns) {
        if t.starts_with("d-") {
            door.supersedes = Some(t.to_string());
        } else if crate::numbers::is_label_key(t) {
            // The supersession gate compares keys, and an older row's key is
            // still its hash. A number no decision holds links nothing.
            door.supersedes = crate::numbers::resolve('d', t).filter(|id| {
                !id.starts_with("q-")
                    && (!crate::numbers::is_label_key(id)
                        || all_decision_rows().map_or(true, |(rows, _)| {
                            rows.iter()
                                .any(|r| decision_row_str(r, "decision_id") == *id)
                        }))
            });
        }
    }
    // Supersession: an unknown target refuses before any write (a transposed
    // id must not read as a silent no-op), and the lane guards mirror the
    // deleted Python's two RefusedAuthority raises.
    if let Some(sup) = &door.supersedes {
        let (rows, _) = all_decision_rows().unwrap_or((Vec::new(), 0));
        let want = sup.trim().to_lowercase();
        let matched: Vec<&Value> = rows
            .iter()
            .filter(|r| {
                !is_retraction_row(r) && decision_row_str(r, "decision_id").to_lowercase() == want
            })
            .collect();
        if matched.is_empty() {
            eprintln!(
                "decide: failed to record: supersession target {sup} is not recoverable from the \
decision index. Run `fno backlog decide-reindex` before retrying."
            );
            return 1;
        }
        let newest = matched
            .into_iter()
            .max_by_key(|r| {
                (
                    decision_row_str(r, "ts").to_string(),
                    decision_row_str(r, "decision_id").to_string(),
                )
            })
            .cloned()
            .unwrap_or_default();
        let lane = decision_lane(&newest);
        let resolved = provenance.authority_source.as_deref().unwrap_or("");
        if lane == "user" && resolved != "operator" && resolved != "chat_attested" {
            return decide_authority_refusal(&provenance.decided_by);
        }
        let target_authority = decision_row_str(&newest, "authority_source");
        if resolved == "chat_attested" && target_authority == "operator" {
            return decide_authority_refusal(&provenance.decided_by);
        }
    }
    if let Some(message) = crate::law_reach::decide_near_refusal(
        &subject,
        &decision,
        "global",
        door.supersedes.as_deref(),
        &door.keep,
    ) {
        eprintln!("decide: refused. {message}\nNothing was recorded.");
        return 2;
    }
    // `--expiry` states the row's end. With none, a coord row or an answer
    // to a question carries the closure key its subject proves; an unproven
    // key stays None, and a standing decision carries none.
    let expiry_ref: Option<Value> = if let Some(spec) = door.expiry.as_deref() {
        match crate::decision_index::parse_expiry(spec, &subject) {
            Ok(e) => Some(e),
            Err(e) => {
                eprintln!("decide: {e}. Nothing was recorded.");
                return 2;
            }
        }
    } else {
        let row = json!({
            "subject": subject,
            "authority_source": provenance.authority_source,
            "question_id": door.question_id,
        });
        if crate::decision_index::expiry_defaults_to_subject(&row) {
            let entries = crate::backlog::decisions_cli::read_graph_entries().unwrap_or_default();
            crate::backlog::decisions_cli::derive_expiry_ref(&row, &entries)
        } else {
            None
        }
    };
    // An answer carries its question's scope and why: the asker already
    // chose the audience and named the goal. Explicit flags override, and
    // the receipt names the carry.
    let carried_question = door
        .answers_ask
        .as_deref()
        .or(door.question_id.as_deref())
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .and_then(|q| {
            let home = crate::paths::AgentsHome::from_env();
            let index = crate::provider_cap::questions_path(&home);
            crate::outstanding::read_open_questions(&index)
                .ok()
                .and_then(|qs| {
                    qs.iter()
                        .find(|row| row.id == q || row.id.ends_with(q))
                        .cloned()
                })
        });
    let carried_scope = carried_question.as_ref().and_then(|q| q.audience.clone());
    let carried_why = carried_question.as_ref().and_then(|q| q.why.clone());
    let carried_from = carried_question.as_ref().map(|q| q.id.clone());
    // The audience has no default: the decider names who the ruling serves.
    let project_id = crate::backlog::law_match::resolve_project(
        None,
        &crate::backlog::law_match::settings_sources(),
    )
    .unwrap_or_else(|_| "fno".into());
    // --audience ask: the who is undecided on purpose. The ruling stores audience?
    // and the receipt says the row holds its meaning until a scope lands.
    // A carried audience? (an unanswered --audience ask on the question) lands the
    // same way: the audience stays undecided through the answer.
    let scope_ask = door.audience.as_deref().map(str::trim) == Some("ask");
    let effective_audience = if scope_ask {
        Some("audience?".to_string())
    } else {
        door.audience.clone().or(carried_scope.clone())
    };
    let audience = if effective_audience.as_deref() == Some("audience?") {
        "audience?".to_string()
    } else {
        match crate::backlog::audience::parse_audience(effective_audience.as_deref(), &project_id) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("decide: refused. {e}. Nothing was recorded.");
                return 2;
            }
        }
    };
    // The why has no default either. An answer inherits the question's goal;
    // a bare ruling states its own.
    let why = match door.why.clone().or(carried_why.clone()) {
        Some(w) if !w.trim().is_empty() => w,
        _ => {
            eprintln!(
                "decide: refused. --why is required: the goal behind the ask, quoted from the user's words when the ask came from him. Nothing was recorded."
            );
            return 2;
        }
    };
    // An answer inherits the question's jobs when the door passed none; an
    // explicit --job wins.
    let job = if door.job.is_empty() {
        carried_question
            .as_ref()
            .map(|q| q.job.clone())
            .unwrap_or_default()
    } else {
        door.job.clone()
    };
    if audience != "audience?" {
        if let Err(e) = crate::backlog::audience::validate_job(&audience, &job, &project_id) {
            eprintln!("decide: refused. {e}. Nothing was recorded.");
            return 2;
        }
    }
    // Mint and envelope. A key appears only when its value is set; the text
    // caps mirror the Python builder (2000 chars).
    let mut data = json!({
        "decision": text_cap(&decision, 2000),
        "graduation": graduation,
    });
    data["subject"] = json!(subject);
    if let Some(map) = data.as_object_mut() {
        map.insert("audience".into(), json!(audience));
        map.insert("why".into(), json!(text_cap(&why, 2000)));
        if !job.is_empty() {
            map.insert("job".into(), json!(job));
        }
    }
    // A carried value always says so on the receipt.
    if let Some(qid) = &carried_from {
        if door.audience.is_none() {
            eprintln!(
                "audience: carried {audience} from {}",
                crate::numbers::display(qid)
            );
        }
        if door.why.is_none() {
            eprintln!("why: carried from {}", crate::numbers::display(qid));
        }
        if door.job.is_empty() && !job.is_empty() {
            eprintln!("job: carried from {}", crate::numbers::display(qid));
        }
    }
    crate::backlog::decision_subject::stamp(&mut data, &subject);
    if let Some(r) = decide_reach(
        &subject,
        door.reach.as_deref(),
        crate::law_match::resolve_project(None, &crate::law_match::settings_sources())
            .ok()
            .map(|slug| format!("project:{slug}")),
    ) {
        if let Some(map) = data.as_object_mut() {
            map.insert("reach".into(), json!(r));
        }
    }
    if let Some(r) = &expiry_ref {
        data["expiry_ref"] = json!(r);
    }
    // The flag takes the number the user sees or the hash: a number token
    // resolves to the hash it names before anything stores or closes it.
    let question_hash = door
        .question_id
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(|q| crate::numbers::resolve_with_pull('q', q).unwrap_or_else(|| q.to_string()));
    let minted = match &question_hash {
        Some(q) => crate::numbers::mint::answer_key(&crate::graph_get::default_graph_path(), q),
        None => crate::numbers::mint::mint_key(),
    };
    let decision_id = match minted {
        Ok(key) => key,
        Err(error) => {
            eprintln!("decide: failed to record: {error}");
            return 1;
        }
    };
    if let (Some(map), Some(id)) = (data.as_object_mut(), &decision_id) {
        map.insert("decision_id".into(), json!(id));
    }
    if let Some(q) = &question_hash {
        data["question_id"] = json!(q);
    }
    if !door.options.is_empty() {
        data["options"] = json!(door.options);
    }
    data["decided_by"] = json!(provenance.decided_by);
    if let Some(a) = &provenance.attested_by {
        data["attested_by"] = json!(a);
    }
    if let Some(r) = &provenance.relayed_by {
        data["relayed_by"] = json!(r);
    }
    if let Some(o) = &origin {
        data["origin"] = json!(o);
    }
    if let Some(a) = &provenance.authority_source {
        data["authority_source"] = json!(a);
    }
    // The session provenance reads the transcript once, after every gate:
    // a refused row records nothing, so it pins nothing and pays no read.
    apply_session_provenance(&mut data, id);
    if let Some(r) = door
        .rationale
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        data["rationale"] = json!(text_cap(r, 2000));
    }
    if let Some(sup) = &door.supersedes {
        data["supersedes"] = json!(sup);
    }
    if let Some(t) = overturns {
        data["overturns"] = json!(t);
    }
    // The self route span rides ahead of the row: the row's parent names it.
    let journal = project_events_journal();
    let mut route_span_id: Option<String> = None;
    let mut pending_route: Option<(Trace, Map<String, Value>)> = None;
    if let Some(ask_ref) = door
        .answers_ask
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        // A span id parents verbatim; any other reference names the ask's
        // bus message, whose endpoints re-derive the ask span. Unresolvable,
        // the route still rides, only parentless.
        let ask = crate::decision_trace::resolve_ask_span(&journal, ask_ref);
        if ask.is_none() {
            eprintln!(
                "decide: answers-ask '{ask_ref}' names no ask span; the route records unparented"
            );
        }
        let trace = Trace {
            trace_id: subject.clone(),
            span_id: new_span_id(),
            parent_span_id: ask,
            actor_session: caller_session.clone(),
            actor_kind: actor_kind_from_authority(provenance.authority_source.as_deref()),
            comms: "mail",
            recipient_session: None,
            recipient_kind: None,
        };
        let mut attrs = Map::new();
        attrs.insert("route".to_string(), json!("self"));
        if let Some(c) = door
            .class
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
        {
            attrs.insert("class".to_string(), json!(c));
        }
        // The span rides ahead of the row (emitted below, before the row's
        // ts mints), so the row's parent is only set when the span landed.
        pending_route = Some((trace, attrs));
    }
    // The correction span: parented at the overturned span; the row's trace
    // names the same parent. Captured here, emitted after the row lands.
    let pending_correction = overturns.as_deref().map(|t| {
        pending_correction_span(
            &subject,
            t,
            caller_session.clone(),
            provenance.authority_source.as_deref(),
        )
    });
    // The route rides ahead of the row: the trace query reads in ts order and
    // a parent must not postdate its child. A failed route emit leaves the
    // row parentless rather than dangling; a failed row write below leaves
    // the route orphaned, reported on stderr.
    if let Some((trace, attrs)) = pending_route.take() {
        match emit_span_to(&journal, "route", &trace, &attrs) {
            Ok(()) => route_span_id = Some(trace.span_id.clone()),
            Err(e) => eprintln!("decide: route span skipped: {e}"),
        }
    }
    // The decision IS the span when it answers, overturns, or closes a
    // question: the row carries the trace envelope, span_id = decision_id,
    // parented at the hop it answers, the span it overturns, or the question
    // it answers. The question parent is the one-record link: the answer
    // decision then reads as its question's number everywhere.
    if door.answers_ask.is_some() || overturns.is_some() || question_hash.is_some() {
        data["trace"] = serde_json::to_value(&Trace {
            trace_id: subject.clone(),
            span_id: decision_id.clone().unwrap_or_default(),
            parent_span_id: route_span_id
                .or_else(|| overturns.map(str::to_string))
                .or_else(|| question_hash.clone()),
            actor_session: caller_session.clone(),
            actor_kind: actor_kind_from_authority(provenance.authority_source.as_deref()),
            comms: "mail",
            recipient_session: None,
            recipient_kind: None,
        })
        .unwrap_or_default();
    }
    if let Some(rows) = &read_rows {
        data["reads"] = json!(rows);
    }
    if let Some(map) = data.as_object_mut() {
        crate::identity::stamp_person(map);
    }
    // Minted after the route span: the row is the child, and its ts must not
    // precede the parent the trace names.
    let ts = now_iso();
    let envelope = json!({"ts": ts, "type": "operator_decision", "source": "target", "data": data});
    let Some(decision_id) = decision_id else {
        let after = crate::numbers::mint::After::Decide {
            question: question_hash.clone(),
        };
        let code = crate::law_match::queue_ruling(envelope, journal.clone(), false, after);
        if code == 0 {
            if let Some((trace, attrs)) = pending_correction {
                if let Err(e) = emit_span_to(&journal, "correction", &trace, &attrs) {
                    eprintln!("decide: correction span skipped: {e}");
                }
            }
            // A hold disarms an armed merge now, not at the drain.
            if let Some(line) = crate::pr_admission::after_queue(
                &subject,
                &decision,
                provenance.authority_source.as_deref(),
            ) {
                eprintln!("{line}");
            }
        }
        return code;
    };
    // Durability first: the project journal. A failed write here records
    // nothing anywhere (the Python generic handler's exit 1).
    if let Err(e) = crate::event_store::append_envelope(&journal, &envelope.to_string(), None) {
        eprintln!("decide: failed to record: {e}");
        return 1;
    }
    // The row is durable: now the correction hop it parents (the route rode
    // ahead of the row above).
    if let Some((trace, attrs)) = pending_correction {
        if let Err(e) = emit_span_to(&journal, "correction", &trace, &attrs) {
            eprintln!("decide: correction span skipped: {e}");
        }
    }
    // Recall second: the machine-wide decision index. The event id names the
    // recovery, because re-running would mint a second id for one ruling.
    let index_path = decisions_jsonl_path();
    if let Err(e) = crate::event_store::append_envelope(&index_path, &envelope.to_string(), None) {
        eprintln!(
            "decide: recorded {} to the project journal, but the \
recall store write failed: {e}. Run `fno backlog decide-reindex` to recover it. \
Do NOT re-run decide; that records it twice.",
            crate::numbers::display(&decision_id)
        );
        return 1;
    }
    // The row is durable in the journal and the recall index. A new
    // operator pr-hold row disarms an armed queue at once, before anything
    // can read the ruling and merge past it. Never fails the record.
    if let Some(line) = crate::pr_admission::after_record(
        &subject,
        &decision,
        provenance.authority_source.as_deref(),
    ) {
        eprintln!("{line}");
    }
    // The graph decisions table is the store `fno backlog decisions` reads
    // first; a refusal degrades to the durable capture, never a lost ruling.
    let graph_path = crate::graph_get::default_graph_path();
    let db_result = crate::backlog::api::decision_record(
        &crate::backlog::api::Store::new(&graph_path),
        envelope.clone(),
    );
    let (node_id, why) = if let Err(e) = &db_result {
        eprintln!(
            "decide: recorded {}, but the graph store refused the ruling: {}. {GRAPH_RETRY}.",
            crate::numbers::display(&decision_id),
            e.0
        );
        (
            None,
            format!("the graph store refused the ruling ({})", e.0),
        )
    } else {
        match crate::backlog::decisions_cli::read_graph_entries() {
            Err(e) => (None, format!("the graph could not be read ({e})")),
            Ok(entries) if entries.is_empty() => (None, "the graph read back no nodes".to_string()),
            Ok(entries) => {
                match crate::backlog::decisions_cli::resolved_twice(&entries, &subject) {
                    Some(n) => (Some(n), String::new()),
                    None => (None, format!("subject '{subject}' names no graph node")),
                }
            }
        }
    };
    eprintln!("{}", reach_receipt_line(&data));
    match &node_id {
        Some(node) => {
            eprintln!(
                "decide: recorded {} on {node}. Recover with: fno backlog decisions {node}",
                crate::numbers::display(&decision_id)
            );
            render_decide_claim_receipt(node, &provenance.decided_by);
        }
        None => {
            eprintln!(
                "decide: recorded {}; no projection was written because {why} \
(the event and the index are the record). Recover with: fno backlog decisions {subject}",
                crate::numbers::display(&decision_id)
            );
        }
    }
    after_decide(
        &journal,
        &decision_id,
        &subject,
        &decision,
        Some(&provenance.decided_by),
        provenance.authority_source.as_deref(),
        question_hash.as_deref(),
    );
    println!("{}", crate::numbers::display(&decision_id));
    0
}

/// The work that follows a recorded ruling, at the door or at the drain.
pub(crate) fn after_decide(
    journal: &std::path::Path,
    decision_id: &str,
    subject: &str,
    decision: &str,
    decided_by: Option<&str>,
    authority: Option<&str>,
    question: Option<&str>,
) {
    crate::law_reach::mail_near_leads(decision_id, subject, decision);
    // The ruling closes the question it names, in the same write: the
    // check-in that prints the answer command reads the close, not the
    // decision. The close is bookkeeping on a durable ruling, so a
    // failed write reports and never fails the record.
    if let Some(qid) = question {
        let index = crate::provider_cap::questions_path(&crate::paths::AgentsHome::from_env());
        match crate::question_clear::close_with_recorded_answer(
            journal,
            &index,
            qid,
            decision,
            decided_by,
            crate::question_intake::QUESTION_CAP,
        ) {
            Ok(crate::question_clear::AnsweredClose::Closed) => eprintln!(
                "decide: closed question {} (decision {})",
                crate::numbers::display(qid),
                crate::numbers::display(decision_id)
            ),
            Ok(crate::question_clear::AnsweredClose::AlreadyClosed) => {}
            Ok(crate::question_clear::AnsweredClose::Unknown) => eprintln!(
                "decide: question {} is not a question id this machine knows; nothing to close",
                crate::numbers::display(qid)
            ),
            Ok(crate::question_clear::AnsweredClose::Peer(host)) => eprintln!(
                "decide: question {} was asked on {host}; close it there",
                crate::numbers::display(qid)
            ),
            Err(e) => eprintln!(
                "decide: recorded {}, but the question close for --question-id {} did not land ({e}); finish it with: fno inbox outstanding clear {}",
                crate::numbers::display(decision_id),
                crate::numbers::display(qid),
                crate::numbers::display(qid)
            ),
        }
    }
    // A lead's ruling on a subject retires the PARALLEL user asks on the
    // same subject (a different question id): two live voices on one
    // subject can disagree, and the durable ruling is the newer voice.
    // Agent-authority rulings are coordination notes, not user-lane
    // commands, so they retire nothing. Peer-origin copies close through
    // their own machine's door. The close is bookkeeping on a durable
    // ruling; a failed write reports and never fails the record.
    if let Err(errors) = supersede_parallel_asks(subject, decision_id, authority, question) {
        for error in errors {
            eprintln!("decide: {error}");
        }
    }
}

/// The decide door's agent-lane refusal: the exact law-door-advice paragraph
/// `backlog_decide` prints for RefusedAuthorityError.
fn decide_authority_refusal(agent: &str) -> i32 {
    eprintln!(
        "backlog decide: refused. This session is agent {agent}, so it cannot record decisions HERE. \
The standing-decision door is open to it and the terms are narrow, so read them before you use it: \
`fno inbox decisions set <subject> <decision> --rationale <why>` (the operator types \
`/fno:decide <the ruling>` for the same thing) records a chat_attested row, never an \
operator row, and it cannot supersede the operator's own decision. That door is for a \
durable rule the OPERATOR asked for. It is not a way to route your own ruling around \
this refusal. Append agent findings without replacing node details with \
`fno backlog note <node> <text>`."
    );
    3
}

/// The pr-hold text gate. None when the text records: the hold phrase
/// (a rationale may follow) or a release, the two forms the merge gate's
/// effects path acts on. Some names both forms when the text would land as
/// a row no gate reads, which is how a hold worded its own way went live
/// while the merge path read the PR as mergeable.
fn hold_text_gate(subject: &str, decision: &str) -> Option<String> {
    if !crate::pr_admission::is_hold_subject(subject) {
        return None;
    }
    if crate::pr_admission::is_hold_text(decision) || crate::pr_admission::is_release_text(decision)
    {
        return None;
    }
    Some(format!(
        "a pr-hold ruling's text is the hold phrase '{}' (a rationale may \
follow) or a release ('release' or 'release: reason'); any other wording \
records a row a merge gate silently ignores. Nothing was recorded.",
        crate::pr_admission::HOLD_DECISION
    ))
}

/// The reach a ruling stamps. A per-PR door subject binds by repo and
/// number, whatever the recorder's cwd, so it pins `global` and an
/// explicit flag never narrows it: a hold stamped to the caller's project
/// would stop binding outside that project. A fleet subject binds every
/// project. An explicit `--reach` wins over derivation everywhere else,
/// and a project that resolves nowhere stamps nothing, so the row reads
/// exactly as unstamped rows always did.
fn decide_reach(subject: &str, flag: Option<&str>, derived: Option<String>) -> Option<String> {
    if crate::decision_index::is_pr_door_subject(subject) {
        return Some("global".to_string());
    }
    if let Some(v) = flag {
        return Some(v.to_string());
    }
    if crate::decision_index::is_fleet_subject(subject) {
        return Some("global".to_string());
    }
    derived
}

/// The one reach line the recorded receipt prints: the row's scope and
/// whether session rules load it. A row decide stamped names its reach;
/// an unstamped row says so and names the reading unstamped rows get,
/// instead of printing the fallback as if decide had stamped it. The
/// lane mapping mirrors `decision_lane` on the authority alone because
/// the row under construction carries no `ts`; every row this door mints
/// sits past the lane cutover, so operator and chat_attested are the
/// user lanes here.
pub(crate) fn reach_receipt_line(row: &Value) -> String {
    let load = match crate::decision_index::authority_of(row) {
        "operator" | "chat_attested" => "session rules load this row".to_string(),
        "agent" | "role" => {
            "session rules do not load this row (agent lane; gates may still read it)".to_string()
        }
        "beastmode" => {
            "session rules do not load this row (grant lane; gates may still read it)".to_string()
        }
        _ => "session rules do not load this row (unattributed lane)".to_string(),
    };
    match crate::decision_index::reach_stamp(row) {
        Some(stamp) => format!("decide: reach {stamp}; {load}."),
        None => format!(
            "decide: no reach stamped because no project resolved here \
(readers treat this row as project:fno); {load}."
        ),
    }
}

/// The role-lane refusal: no live team of the caller covers the subject.
fn decide_role_refusal(caller: &str) -> i32 {
    eprintln!(
        "decide: refused. --authority role is a lead's ruling inside its own scope, and \
{caller} holds no live team whose territory covers this subject. Nothing was recorded. \
Omit --authority to record it as agent, or ask the lead who owns the node to record it."
    );
    3
}

/// The decide door's unattributed refusal (`UnattributedAuthorityError`).
fn decide_unattributed_refusal() -> i32 {
    eprintln!(
        "decide: refused. This process has no session identity and no terminal, so nothing here \
shows the operator ruled. Operator authority is never inherited by silence. Run \
`fno inbox decisions set <subject> <decision> --rationale <why>` from an attended operator \
terminal, or have the operator type `/fno:decide <the ruling>` in chat, which records in \
one step. Append agent findings with `fno backlog note <node> <text>`."
    );
    3
}

/// A string field off a decision row, defaulting empty (`rows_str` in the
/// decisions listing; a private twin here so the door does not reach across
/// the listing module).
fn decision_row_str<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The waiver-subject refusal (`WaiverAuthorityRefusedError`): the only path
/// is the attended coverage-waive command.
fn decide_waiver_refusal(subject: &str) -> i32 {
    eprintln!(
        "decide: refused. '{subject}' is a review-coverage waiver subject: waiver evidence needs \
superuser authority, and a chat-attested row proves only that a session was addressed, \
not that a person reviewed anything. Waivers are recorded by the attended command \
`fno do pr coverage-waive <pr> --reason \"...\"` at an operator terminal; a session a \
harness identifies records nothing there."
    );
    3
}

/// The full provenance resolver (`_resolve_decider`): three states and the
/// third fails closed. `decided_by` is STAMPED, never stated, whenever a
/// session identity resolves; a caller-supplied name rides `relayed_by`.
pub(crate) fn resolve_decider_lanes(
    id: DecideIdentity,
    decided_by: Option<&str>,
    authority_source: Option<&str>,
    origin: Option<&str>,
) -> Result<DecideProvenance, DecideRefusal> {
    if let Some(o) = origin {
        if !MAIL_ORIGINS.contains(&o) {
            return Err(DecideRefusal::UnknownOrigin(o.to_string()));
        }
    }
    let agent = id.agent();
    if let Some(o) = origin {
        if authority_source == Some("operator") && max_authority_by_origin(o) != "operator" {
            return Err(DecideRefusal::RefusedAuthority {
                agent: agent
                    .clone()
                    .unwrap_or_else(|| UNATTRIBUTED_CALLER.to_string()),
                origin: Some(o.to_string()),
            });
        }
    }
    if agent.is_some() && authority_source == Some("operator") {
        return Err(DecideRefusal::RefusedAuthority {
            agent: agent.clone().unwrap(),
            origin: None,
        });
    }
    if let Some(handle) = &agent {
        // State 1. Relayed only when it says something the stamp does not.
        if origin == Some("operator") {
            return Ok(DecideProvenance {
                decided_by: handle.clone(),
                authority_source: Some(authority_source.unwrap_or("agent").to_string()),
                attested_by: None,
                relayed_by: Some(handle.clone()),
            });
        }
        let relayed = decided_by
            .map(str::trim)
            .filter(|d| !d.is_empty() && *d != handle.as_str())
            .map(str::to_string);
        return Ok(DecideProvenance {
            decided_by: handle.clone(),
            authority_source: Some(authority_source.unwrap_or("agent").to_string()),
            attested_by: None,
            relayed_by: relayed,
        });
    }
    if attended_terminal() {
        return Ok(attended_provenance(decided_by, authority_source, origin));
    }
    // State 3, fail closed.
    if authority_source == Some("operator") {
        return Err(DecideRefusal::UnattributedAuthority);
    }
    Ok(DecideProvenance {
        decided_by: UNATTRIBUTED_CALLER.to_string(),
        authority_source: authority_source.map(str::to_string),
        attested_by: None,
        relayed_by: decided_by
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string),
    })
}

/// State 2: a person at a terminal with no session identity; the stated name
/// IS the record. With no stated authority this resolves operator, the lane
/// the law door's `resolve_caller` gives the same caller: a row that reads
/// "decided by operator" must be law, or it cannot lift the hold it answers.
/// A relayed name or a non-operator origin is someone else's ruling, so the
/// superuser lane is never defaulted for it.
fn attended_provenance(
    decided_by: Option<&str>,
    authority_source: Option<&str>,
    origin: Option<&str>,
) -> DecideProvenance {
    let decider = decided_by
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or("operator");
    let origin = origin.filter(|o| !o.is_empty());
    let authority = authority_source.or_else(|| {
        (decider == "operator" && origin.is_none_or(|o| max_authority_by_origin(o) == "operator"))
            .then_some("operator")
    });
    DecideProvenance {
        decided_by: decider.to_string(),
        authority_source: authority.map(str::to_string),
        attested_by: Some(origin.unwrap_or(decider).to_string()),
        relayed_by: None,
    }
}

/// The session provenance a decide row carries: the full session id, the
/// harness, the transcript path, and the id of the user turn the row
/// answers (the user's message for a relayed ruling). The turn pointer is
/// pinned at write time because a transcript only grows: the turn a ruling
/// came from is the last real user turn on disk when the row lands, and no
/// later read can recover which one that was. `None` when this process
/// resolves as no session (an attended terminal or an injected identity),
/// which leaves the row reading exactly as rows before the field did.
fn capture_session_provenance(id: DecideIdentity) -> Option<Value> {
    let (session, harness) = id.session_identity()?;
    let transcript = resolve_session_transcript(&session, &harness);
    let mut out = Map::new();
    out.insert("session_id".into(), json!(session));
    out.insert("harness".into(), json!(harness));
    if let Some(path) = &transcript {
        out.insert("transcript".into(), json!(path.display().to_string()));
    }
    if let Some(turn) = transcript.as_deref().and_then(last_user_turn) {
        let (turn_id, text, ts) = turn;
        out.insert("turn_id".into(), json!(turn_id));
        if !text.is_empty() {
            out.insert(
                "turn_excerpt".into(),
                json!(crate::runtime::operator_turns::excerpt(&text)),
            );
        }
        if let Some(ts) = ts {
            out.insert("turn_ts".into(), json!(ts));
        }
    }
    Some(Value::Object(out))
}

/// The transcript file a session's rows live in, per harness. A harness
/// that keeps no readable transcript (opencode keys a SQLite store) answers
/// `None`; the row still carries the session id and harness.
fn resolve_session_transcript(session: &str, harness: &str) -> Option<std::path::PathBuf> {
    match harness {
        "claude" => crate::claude_drive::find_transcript(session),
        "codex" => crate::codex_store::codex_rollout_path(None, session),
        _ => None,
    }
}

/// The write-site fold: capture the caller's session provenance and stamp it
/// onto the row's data. A named helper so the write test pins the insertion
/// directly, without driving the whole door and its store machinery.
///
/// The `turn_excerpt` quote survives only on the user lanes (operator,
/// chat_attested): those rows answer a user turn, so the quote is the
/// user's words behind the ruling. A role or agent row answers a dispatch
/// or the lead's own ruling, and the newest user turn is usually
/// unrelated; quoting it read as the user speaking. The turn pointer and
/// timestamp stay on every row: which turn was last is factual provenance.
fn apply_session_provenance(data: &mut Value, id: DecideIdentity) {
    let Some(mut sp) = capture_session_provenance(id) else {
        return;
    };
    let user_lane = matches!(
        crate::decision_index::authority_of(data),
        "operator" | "chat_attested"
    );
    if !user_lane {
        if let Some(map) = sp.as_object_mut() {
            map.remove("turn_excerpt");
        }
    }
    if let Some(map) = data.as_object_mut() {
        map.insert("session_provenance".into(), sp);
    }
}

/// The last row in a transcript a person typed: a user turn the classifier
/// admits, so machine shapes (task notifications, command echoes) never
/// stand in for the ruling. Returns the turn id (the row's uuid, else the
/// derived digest the operator-turn queue mints), its classified text and
/// its timestamp.
fn last_user_turn(path: &std::path::Path) -> Option<(String, String, Option<String>)> {
    let f = std::fs::File::open(path).ok()?;
    let mut best: Option<(String, String, Option<String>)> = None;
    for (line_no, line) in std::io::BufRead::lines(std::io::BufReader::new(f)).enumerate() {
        let Ok(line) = line else {
            continue;
        };
        let Ok(rec) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if !crate::provenance::is_user_turn(&rec) {
            continue;
        }
        let Ok(text) = crate::provenance::classify(&crate::provenance::turn_text(&rec)) else {
            continue;
        };
        let turn_id = crate::runtime::operator_turns::turn_id(&rec, &text, line_no);
        let ts = crate::provenance::turn_stamp(&rec).map(str::to_string);
        best = Some((turn_id, text, ts));
    }
    best
}

/// The correction span and its attrs: parented at the overturned span, with
/// the attrs carrying the same id as the `overturns` join key the pushback
/// fold matches against route spans. The operator lane owns corrections and
/// is unreachable under the door's test identities (an agent claiming
/// operator refuses; an unattributed caller refuses), so the unit test
/// drives this constructor directly.
fn pending_correction_span(
    subject: &str,
    target: &str,
    caller_session: Option<String>,
    authority_source: Option<&str>,
) -> (Trace, Map<String, Value>) {
    let mut attrs = Map::new();
    attrs.insert("overturns".to_string(), json!(target));
    (
        Trace {
            trace_id: subject.to_string(),
            span_id: new_span_id(),
            parent_span_id: Some(target.to_string()),
            actor_session: caller_session,
            actor_kind: actor_kind_from_authority(authority_source),
            comms: "mail",
            recipient_session: None,
            recipient_kind: None,
        },
        attrs,
    )
}

/// A lead's ruling on a subject retires the parallel user asks on the same
/// subject (a different question id): two live voices on one subject can
/// disagree, and the durable ruling is the newer voice. Rulings with
/// operator, role, team or beastmode authority retire copies; a plain
/// agent ruling is a coordination note and retires nothing. Peer-origin
/// copies close through their own machine's door. Returns the per-id
/// write errors; the close is bookkeeping on a durable ruling, so a
/// failed write never fails the record.
pub(crate) fn supersede_parallel_asks(
    subject: &str,
    decision_id: &str,
    authority: Option<&str>,
    skip_qid: Option<&str>,
) -> Result<(), Vec<String>> {
    if !matches!(
        authority,
        Some("operator") | Some("role") | Some("team") | Some("beastmode")
    ) {
        return Ok(());
    }
    let index = crate::provider_cap::questions_path(&crate::paths::AgentsHome::from_env());
    let (asked, closed) = crate::question_intake::question_rows(&index);
    let pairs: Vec<(String, String)> = asked
        .iter()
        .filter_map(|(qid, row)| {
            if closed.contains(qid) {
                return None;
            }
            if skip_qid == Some(qid.as_str()) {
                return None;
            }
            if crate::question_bus::peer_origin(row).is_some() {
                return None;
            }
            let row_subject = crate::question_intake::str_field(row, "subject")?.trim();
            (row_subject == subject).then(|| {
                (
                    qid.clone(),
                    format!(
                        "superseded: decision {} rules this subject",
                        crate::numbers::display(decision_id)
                    ),
                )
            })
        })
        .collect();
    if pairs.is_empty() {
        return Ok(());
    }
    let now = crate::claims::now_ms() / 1000;
    let (superseded, errors) =
        crate::question_sweep::close_questions(&index, &pairs, now, "decide-door");
    for qid in &superseded {
        eprintln!(
            "decide: superseded parallel ask {} on this subject (decision {})",
            crate::numbers::display(qid),
            crate::numbers::display(decision_id)
        );
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::law_match::scope_tests::DoorEnv;

    #[test]
    fn attended_decide_with_no_stated_authority_records_law() {
        let lane = |by, auth, origin| attended_provenance(by, auth, origin).authority_source;
        assert_eq!(lane(None, None, None).as_deref(), Some("operator"));
        assert_eq!(
            lane(None, None, Some("operator")).as_deref(),
            Some("operator")
        );
        assert_eq!(lane(None, Some("role"), None).as_deref(), Some("role"));
        assert_eq!(lane(Some("robin"), None, None), None);
        assert_eq!(lane(None, None, Some("scheduler")), None);
        let p = attended_provenance(None, None, None);
        let row = json!({
            "subject": "fb-b44f",
            "decided_by": p.decided_by,
            "authority_source": p.authority_source,
            "ts": now_iso(),
        });
        assert!(crate::decision_index::is_law(&row), "{row}");
    }

    // -------------------------------------------------------------------
    // The decide door: injected-identity unit tests (the parity goldens
    // pin the identity-independent CLI answers; these pin the lanes).
    // -------------------------------------------------------------------

    /// A parsed decide argv.
    fn decide_argv(parts: &[&str]) -> DecideDoor {
        let mut argv: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
        if !argv.iter().any(|a| a == "--audience") {
            argv.extend(["--audience".to_string(), "internal".to_string()]);
        }
        parse_decide_door(&argv).expect("parses")
    }

    /// Save one node into the env's graph so a subject can resolve.
    fn save_node(graph: &std::path::Path, id: &str, slug: &str) {
        let mut connection = crate::backlog::open(graph).expect("opens");
        let node = crate::backlog::model::Node::from_json(&serde_json::json!({
            "id": id,
            "slug": slug,
            "title": "Node",
            "type": "feature",
            "status": "ready",
            "priority": "p2",
        }))
        .expect("node json");
        crate::backlog::nodes::save(&mut connection, &node).expect("saves");
    }

    #[test]
    fn decide_agent_supersede_of_a_law_row_refuses_and_records_nothing() {
        let env = DoorEnv::new();
        crate::paths::pin_test_claims_root(env.0.path());
        // Seed an operator law row through the same stores the door writes.
        let envelope = serde_json::json!({
            "ts": now_iso(),
            "type": "operator_decision",
            "source": "target",
            "data": {
                "decision_id": "d-0aa0aa0a",
                "decision": "The operator owns durable policy.",
                "subject": "file-budget",
                "authority_source": "operator",
                "graduation": {"kind": "guidance"},
            },
        });
        let line = envelope.to_string();
        crate::event_store::append_envelope(&decisions_jsonl_path(), &line, None).expect("index");
        crate::backlog::api::decision_record(
            &crate::backlog::api::Store::new(&crate::graph_get::default_graph_path()),
            envelope,
        )
        .expect("db");
        let before = all_decision_rows().expect("reads").0.len();
        let code = decide_door_write(
            decide_argv(&["x-n1", "Override", "--supersedes", "d-0aa0aa0a"]),
            DecideIdentity::Forced(Some("cl-test1")),
        );
        assert_eq!(code, 3);
        let after = all_decision_rows().expect("reads").0.len();
        assert_eq!(after, before, "nothing recorded on a refused write");
    }

    #[test]
    fn decide_bad_authority_and_graduation_refuse_before_any_write() {
        let env = DoorEnv::new();
        crate::paths::pin_test_claims_root(env.0.path());
        assert_eq!(
            decide_door_write(
                decide_argv(&["x-n1", "do it", "--authority", "lead"]),
                DecideIdentity::Forced(None),
            ),
            2
        );
        assert_eq!(
            decide_door_write(
                decide_argv(&["x-n1", "do it", "--graduation", "banana"]),
                DecideIdentity::Forced(None),
            ),
            2
        );
        assert!(all_decision_rows().is_err(), "no store was ever created");
    }

    #[test]
    fn the_hold_text_gate_admits_only_the_phrase_and_a_release() {
        // Admit arm: the phrase, with or without a rationale.
        for text in [
            "do not merge this pull request",
            "do not merge this pull request: docs gate first",
            "do not merge this pull request. The docs gate reads the #NNN form.",
        ] {
            assert!(
                hold_text_gate("pr-hold:o/r#8", text).is_none(),
                "the phrase records: {text}"
            );
        }
        // Admit arm: the release forms the effects path acts on.
        for text in [
            "release",
            "release: docs gate green",
            "release with two rounds",
        ] {
            assert!(
                hold_text_gate("pr-hold:o/r#8", text).is_none(),
                "a release records: {text}"
            );
        }
        // Pass-through arm: subjects outside the hold door gate nothing.
        assert!(hold_text_gate("release:fno-version", "anything goes").is_none());
        // Subject and release wording tables, folded from the gate crate:
        // the door admits exactly the subjects the effects path parses and
        // the releases it acts on.
        assert!(crate::pr_admission::is_hold_subject("pr-hold:o/r#8"));
        assert!(crate::pr_admission::is_hold_subject("  pr-hold:o/r#8"));
        assert!(!crate::pr_admission::is_hold_subject("pr-hold"));
        assert!(!crate::pr_admission::is_hold_subject("pr-hold:o/r"));
        assert!(!crate::pr_admission::is_hold_subject(
            "pr-hold:o/r#not-a-number"
        ));
        assert!(!crate::pr_admission::is_hold_subject("pr-hold:#8"));
        assert!(!crate::pr_admission::is_hold_subject("merge-freeze:o/r#8"));
        assert!(crate::pr_admission::is_release_text("release"));
        assert!(crate::pr_admission::is_release_text(" release "));
        assert!(crate::pr_admission::is_release_text(
            "release: the docs gate reads the #NNN form"
        ));
        assert!(crate::pr_admission::is_release_text(
            "release with two clean review rounds"
        ));
        assert!(!crate::pr_admission::is_release_text("released"));
        assert!(!crate::pr_admission::is_release_text(
            "do not merge this pull request"
        ));
        assert!(!crate::pr_admission::is_release_text("keep it held"));
        // Refuse arm: near spellings and prose, each naming the phrase.
        for text in [
            "do not merge this docs pull request",
            "hold",
            "keep it held",
            "released",
        ] {
            let refusal =
                hold_text_gate("pr-hold:o/r#8", text).expect("a dead hold wording refuses");
            assert!(
                refusal.contains("do not merge this pull request"),
                "the refusal names the phrase: {refusal}"
            );
            assert!(refusal.contains("Nothing was recorded"), "{refusal}");
        }
    }

    #[test]
    fn a_pr_hold_ruling_refuses_text_that_is_neither_the_phrase_nor_a_release() {
        let env = DoorEnv::new();
        crate::paths::pin_test_claims_root(env.0.path());
        let before = all_decision_rows().map(|r| r.0.len()).unwrap_or(0);
        let code = decide_door_write(
            decide_argv(&[
                "pr-hold:fixture/repo#1",
                "do not merge this docs pull request",
                "--authority",
                "agent",
                "--why",
                "hold gate fixture",
            ]),
            DecideIdentity::Forced(Some("cl-hold1")),
        );
        assert_eq!(code, 3);
        let after = all_decision_rows().map(|r| r.0.len()).unwrap_or(0);
        assert_eq!(after, before, "nothing recorded on a refused hold text");
    }

    #[test]
    fn the_reach_stamp_pins_door_subjects_and_follows_the_flag_or_the_derivation() {
        // A hold binds by repo and number, whatever the recorder's cwd.
        assert_eq!(
            decide_reach("pr-hold:o/r#8", None, Some("project:fno".to_string())),
            Some("global".to_string())
        );
        // An explicit flag never un-pins a door subject.
        assert_eq!(
            decide_reach("merge-freeze:o/r#8@abc", Some("project:x"), None),
            Some("global".to_string())
        );
        // Fleet subjects bind every project; the stamp makes the reader's
        // default explicit.
        assert_eq!(
            decide_reach("model-tier", None, None),
            Some("global".to_string())
        );
        // Elsewhere the flag wins over derivation, and a project that
        // resolves nowhere stamps nothing.
        assert_eq!(
            decide_reach(
                "release:fno",
                Some("project:fnh"),
                Some("project:fno".to_string())
            ),
            Some("project:fnh".to_string())
        );
        assert_eq!(
            decide_reach("release:fno", None, Some("project:fnh".to_string())),
            Some("project:fnh".to_string())
        );
        assert_eq!(decide_reach("release:fno", None, None), None);
    }

    #[test]
    fn a_reach_flag_stamps_the_row_the_caller_names() {
        // The flag validates at parse: a non-reach refuses before any write.
        let argv: Vec<String> = ["--reach", "banana"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(parse_decide_door(&argv).is_err());
        let argv: Vec<String> = ["--reach", "project:fnh"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let door = parse_decide_door(&argv).expect("a reach value parses");
        assert_eq!(door.reach.as_deref(), Some("project:fnh"));
        // The stamped row states its reach in the recorded receipt.
        let env = DoorEnv::new();
        crate::paths::pin_test_claims_root(env.0.path());
        let code = decide_door_write(
            decide_argv(&[
                "reach-topic",
                "The ruling text",
                "--authority",
                "agent",
                "--why",
                "reach fixture",
                "--reach",
                "project:fnh",
            ]),
            DecideIdentity::Forced(Some("cl-reach1")),
        );
        assert_eq!(code, 0);
        let (rows, _) = all_decision_rows().expect("reads");
        let row = rows
            .iter()
            .find(|r| decision_row_str(r, "subject") == "reach-topic")
            .expect("the row landed");
        assert_eq!(crate::decision_index::row_scope(row), "project:fnh");
        // The receipt line, on the recorded row and on both lanes.
        let line = reach_receipt_line(row);
        assert!(line.contains("reach project:fnh"), "{line}");
        assert!(
            line.contains("session rules do not load this row"),
            "{line}"
        );
        let role_row = json!({"authority_source": "role", "reach": "project:fno"});
        let line = reach_receipt_line(&role_row);
        assert!(line.contains("reach project:fno"), "{line}");
        assert!(line.contains("agent lane"), "{line}");
        let operator_row = json!({"authority_source": "operator", "reach": "global"});
        let line = reach_receipt_line(&operator_row);
        assert!(line.contains("reach global"), "{line}");
        assert!(line.contains("session rules load this row"), "{line}");
        assert!(!line.contains("do not load"), "{line}");
        // An unstamped row says so with the cause, and never prints the
        // fallback as a reach decide stamped.
        let unstamped = json!({"authority_source": "role", "subject": "some-topic"});
        let line = reach_receipt_line(&unstamped);
        assert!(
            line.contains("no reach stamped because no project resolved here"),
            "{line}"
        );
        assert!(
            line.contains("readers treat this row as project:fno"),
            "{line}"
        );
    }

    #[test]
    fn a_wide_reach_without_a_user_certificate_refuses() {
        let env = DoorEnv::new();
        crate::paths::pin_test_claims_root(env.0.path());
        let code = decide_door_write(
            decide_argv(&[
                "reach-wide",
                "The ruling text",
                "--authority",
                "agent",
                "--why",
                "wide reach fixture",
                "--reach",
                "global",
            ]),
            DecideIdentity::Forced(Some("cl-reach2")),
        );
        assert_eq!(code, 3);
        let read = all_decision_rows();
        if let Ok((rows, _)) = &read {
            assert!(
                !rows
                    .iter()
                    .any(|r| decision_row_str(r, "subject") == "reach-wide"),
                "nothing recorded on a refused wide reach"
            );
        }
    }

    #[test]
    fn decide_coord_lane_stamps_the_node_closure_key() {
        let env = DoorEnv::new();
        crate::paths::pin_test_claims_root(env.0.path());
        save_node(
            &crate::graph_get::default_graph_path(),
            "x-node1",
            "node-one",
        );
        let code = decide_door_write(
            decide_argv(&[
                "x-node1",
                "Ship it",
                "--authority",
                "agent",
                "--why",
                "ship it",
            ]),
            DecideIdentity::Forced(Some("cl-test2")),
        );
        assert_eq!(code, 0);
        let (rows, _) = all_decision_rows().expect("reads");
        let first = rows.last().expect("a row landed");
        assert_eq!(first["expiry_ref"]["kind"], json!("node"));
        assert_eq!(first["expiry_ref"]["node_id"], json!("x-node1"));
        // AC8: answering the ask in place rides a route span; the row's
        // trace parent names that span. --answers-ask carries the ask's
        // scope and why when the ask is recorded; this ask is unrecorded,
        // so the why states its own.
        let code = decide_door_write(
            decide_argv(&[
                "x-node1",
                "Answer the ask in place",
                "--answers-ask",
                "s-1",
                "--class",
                "none",
                "--why",
                "answer the ask",
            ]),
            DecideIdentity::Forced(Some("cl-test2")),
        );
        assert_eq!(code, 0);
        // The route span lands and the row's trace parent names it.
        let journal = project_events_journal();
        let spans = crate::event_store::query_events(
            &journal,
            &crate::event_store::EventQuery::of_types(&["decision_span"]),
        )
        .unwrap_or_default();
        assert_eq!(spans.len(), 1, "one route span");
        let route = serde_json::from_str::<serde_json::Value>(&spans[0].line).unwrap();
        assert_eq!(route["data"]["span_kind"], "route");
        assert_eq!(route["data"]["route"], "self");
        assert_eq!(route["data"]["class"], "none");
        let parent = route["data"]["trace"]["span_id"]
            .as_str()
            .unwrap()
            .to_string();
        let (rows, _) = all_decision_rows().expect("reads");
        let row = rows.last().expect("a row landed");
        assert_eq!(row["trace"]["parent_span_id"], json!(parent));
        // The route rides ahead of the row: ts order folds the parent first.
        assert!(
            route["ts"].as_str().unwrap() <= row["ts"].as_str().unwrap(),
            "route precedes the row in ts order"
        );
    }

    #[test]
    fn decide_correction_span_attrs_carry_the_overturns_join_key() {
        // The operator lane owns corrections and is unreachable under the
        // door's test identities (an agent claiming operator refuses, an
        // unattributed caller refuses), so the test drives the constructor
        // the write path calls.
        let (trace, attrs) = pending_correction_span(
            "x-node5",
            "s-r1",
            Some("cl-test5".to_string()),
            Some("operator"),
        );
        assert_eq!(attrs["overturns"], json!("s-r1"));
        assert_eq!(trace.parent_span_id.as_deref(), Some("s-r1"));
        assert_eq!(trace.actor_kind, "user");
        assert!(trace.span_id.starts_with("s-"));
    }

    #[test]
    fn decide_records_person_beside_the_session() {
        let env = DoorEnv::new();
        crate::paths::pin_test_claims_root(env.0.path());
        save_node(
            &crate::graph_get::default_graph_path(),
            "x-node4",
            "node-four",
        );
        let code = decide_door_write(
            decide_argv(&[
                "x-node4",
                "Ship it",
                "--authority",
                "agent",
                "--why",
                "ship it",
            ]),
            DecideIdentity::Forced(Some("cl-test4")),
        );
        assert_eq!(code, 0);
        let (rows, _) = all_decision_rows().expect("reads");
        let row = rows.last().expect("a row landed");
        // The person rides beside decided_by; it never replaces the caller.
        assert_eq!(row["person"], json!(crate::identity::person()));
        assert_ne!(row["decided_by"], row["person"], "{row}");
        let machine_id = crate::claims::machine_id();
        if !machine_id.is_empty() {
            assert_eq!(row["machine_id"], json!(machine_id));
        }
    }

    #[test]
    fn decide_with_question_id_closes_the_question_it_names() {
        let env = DoorEnv::new();
        crate::paths::pin_test_claims_root(env.0.path());
        save_node(&crate::graph_get::default_graph_path(), "x-nodeq", "node-q");
        let index = crate::provider_cap::questions_path(&crate::paths::AgentsHome::from_env());
        let ask = json!({
            "ts": now_iso(),
            "type": "operator_question",
            "source": "target",
            "data": {
                "question_id": "q-closeaa1",
                "question": "which lane?",
                "asker": "robin",
                "node": "x-nodeq",
            },
        });
        crate::event_store::append_envelope(&index, &ask.to_string(), None).expect("ask");

        let argv = [
            "x-nodeq",
            "Ship it",
            "--authority",
            "agent",
            "--question-id",
            "q-closeaa1",
            "--why",
            "ship it",
        ];
        let code = decide_door_write(decide_argv(&argv), DecideIdentity::Forced(Some("cl-testq")));
        assert_eq!(code, 0);
        let closes = crate::event_store::journal_text(&index, &["operator_question_closed"]);
        assert!(closes.contains("q-closeaa1"), "{closes}");
        // A rerun writes no second close: the first close is the close.
        let code = decide_door_write(decide_argv(&argv), DecideIdentity::Forced(Some("cl-testq")));
        assert_eq!(code, 0);
        let closes = crate::event_store::journal_text(&index, &["operator_question_closed"]);
        assert_eq!(closes.lines().count(), 1, "{closes}");
        // The number the user sees resolves the same close: the door never
        // asks for the hex, and the decision records the hash it names.
        let graph = crate::backlog::database_path(&crate::graph_get::default_graph_path());
        crate::numbers::sweep(&graph, None, vec!["q-closeaa1".to_string()], "h").expect("mint");
        // One record, one number: the question mints a D number now, and the
        // typed token is that number whatever letter a caller reaches for.
        let qnum = crate::numbers::label("q-closeaa1").expect("numbered");
        let argv = [
            "x-nodeq",
            "Ship it",
            "--authority",
            "agent",
            "--question-id",
            qnum.as_str(),
            "--why",
            "ship it",
        ];
        let code = decide_door_write(decide_argv(&argv), DecideIdentity::Forced(Some("cl-testq")));
        assert_eq!(code, 0);
        let (rows, _) = crate::law_match::all_decision_rows().expect("rows");
        let last = rows.last().expect("decision row");
        assert_eq!(
            last.get("question_id").and_then(Value::as_str),
            Some("q-closeaa1")
        );
        // The receipt mints the decision's D number in step with the record,
        // so the printed id is the short one from the first read.
        let did = last
            .get("decision_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        // The first answer holds its question's number. A later ruling on
        // that question is a record of its own, so it takes another.
        let qlabel = crate::numbers::label("q-closeaa1").expect("numbered");
        assert!(
            rows.iter()
                .any(|r| crate::law_match::row_id(r) == qlabel.as_str()),
            "no answer holds {qlabel}"
        );
        assert_ne!(did, qlabel, "a later ruling reused the question's number");
        // Already closed: the numbered rerun adds no second close.
        let closes = crate::event_store::journal_text(&index, &["operator_question_closed"]);
        assert_eq!(closes.lines().count(), 1, "{closes}");
    }

    #[test]
    fn decide_on_a_peer_copy_names_the_origin_machine_and_writes_no_close() {
        let env = DoorEnv::new();
        crate::paths::pin_test_claims_root(env.0.path());
        save_node(&crate::graph_get::default_graph_path(), "x-nodep", "node-p");
        let index = crate::provider_cap::questions_path(&crate::paths::AgentsHome::from_env());
        let ask = json!({
            "ts": now_iso(),
            "type": "operator_question",
            "source": "daemon",
            "data": {
                "question_id": "q-feedbe22@otherbox",
                "question": "which lane?",
                "asker": "robin",
                "origin_question_id": "q-feedbe22",
                "origin_machine": "otherbox",
            },
        });
        crate::event_store::append_envelope(&index, &ask.to_string(), None).expect("ask");
        let graph = crate::backlog::database_path(&crate::graph_get::default_graph_path());
        crate::numbers::sweep(&graph, None, vec!["q-feedbe22".to_string()], "h").expect("mint");
        let qnum = crate::numbers::label("q-feedbe22").expect("numbered");

        let argv = [
            "x-nodep",
            "Ship it",
            "--authority",
            "agent",
            "--question-id",
            qnum.as_str(),
            "--why",
            "ship it",
        ];
        let code = decide_door_write(decide_argv(&argv), DecideIdentity::Forced(Some("cl-peerq")));
        assert_eq!(code, 0);
        // The decision records the RESOLVED bare id: the number named the
        // copy's original, never the raw token. The copy itself keeps its
        // answer for the origin machine; the close path reports the peer.
        let (rows, _) = crate::law_match::all_decision_rows().expect("rows");
        let last = rows.last().expect("decision row");
        assert_eq!(
            last.get("question_id").and_then(Value::as_str),
            Some("q-feedbe22")
        );
        let closes = crate::event_store::journal_text(&index, &["operator_question_closed"]);
        assert!(
            !closes.contains("q-feedbe22"),
            "a peer copy closed locally: {closes}"
        );
    }

    #[test]
    fn decide_role_lane_needs_a_live_team() {
        let env = DoorEnv::new();
        crate::paths::pin_test_claims_root(env.0.path());
        let role = || decide_argv(&["x-n1", "Approved: PR 7", "--authority", "role"]);
        // A worker with no team cannot mint the lead's lane.
        assert_eq!(
            decide_door_write(role(), DecideIdentity::Forced(Some("cl-role1"))),
            3
        );
        // Nor can a caller with no identity and no terminal.
        assert_eq!(decide_door_write(role(), DecideIdentity::Forced(None)), 3);
        let (rows, _) = all_decision_rows().unwrap_or((Vec::new(), 0));
        assert!(rows.is_empty(), "nothing recorded");
    }

    #[test]
    fn role_scope_covers_only_the_callers_territory() {
        let team = crate::territory::Team {
            scope: "e-1".to_string(),
            level: 2,
            holder: "lead-a".to_string(),
            holder_session: Some("s-lead".to_string()),
        };
        let entries = vec![
            json!({"id": "e-1", "type": "epic", "project": "alpha", "status": "in_progress"}),
            json!({"id": "e-1a", "slug": "inside", "parent": "e-1", "project": "alpha", "status": "ready"}),
            json!({"id": "e-outs", "project": "beta", "status": "ready"}),
        ];
        let projects = Err("no projects".to_string());
        let mine = [team];
        assert!(role_covers_subject(&mine, "e-1a", &entries, &projects));
        assert!(role_covers_subject(&mine, "inside", &entries, &projects));
        assert!(!role_covers_subject(&mine, "e-outs", &entries, &projects));
        assert!(role_covers_subject(
            &mine,
            "release-cadence",
            &entries,
            &projects
        ));
        assert!(!role_covers_subject(
            &[],
            "release-cadence",
            &entries,
            &projects
        ));
    }

    /// One open question row in the test home's questions index.
    fn seed_ask(index: &std::path::Path, qid: &str, subject: &str, asker: &str) {
        crate::provider_cap::append_questions_row(
            index,
            &serde_json::json!({
                "ts": "2026-11-12T01:00:00Z",
                "type": "operator_question",
                "source": "test",
                "data": {
                    "question_id": qid,
                    "question": "which home?",
                    "asker": asker,
                    "session_id": format!("sess-{asker}"),
                    "subject": subject,
                    "node": "x-node1",
                },
            }),
        )
        .expect("seeds");
    }

    #[test]
    fn a_lead_ruling_supersedes_the_parallel_ask_on_its_subject() {
        let env = DoorEnv::new();
        crate::paths::pin_test_claims_root(env.0.path());
        let _agents = crate::claims::EnvVarGuard::set(
            crate::paths::HOME_ENV,
            env.0.path().join("agents").to_str().unwrap(),
        );
        let index = crate::provider_cap::questions_path(&crate::paths::AgentsHome::from_env());
        seed_ask(&index, "q-1a2b3c4d", "crate-home", "atticus");
        seed_ask(&index, "q-2b3c4d5e", "other-subject", "atticus");

        supersede_parallel_asks("crate-home", "d-1a2b3c4d", Some("role"), None)
            .expect("the fold runs");

        let (asked, closed) = crate::question_intake::question_rows(&index);
        assert!(closed.contains("q-1a2b3c4d"), "the parallel ask closed");
        assert!(asked.contains_key("q-1a2b3c4d"));
        assert!(
            !closed.contains("q-2b3c4d5e"),
            "a different subject never closes"
        );
        let close_row = crate::event_store::journal_text(&index, &["operator_question_closed"])
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|row| crate::question_intake::str_field(row, "question_id") == Some("q-1a2b3c4d"))
            .expect("a close row landed");
        assert!(
            crate::question_intake::str_field(&close_row, "reason")
                .unwrap_or_default()
                .contains("rules this subject"),
            "{close_row}"
        );
        assert_eq!(
            crate::question_intake::str_field(&close_row, "answer"),
            Some("")
        );
    }

    #[test]
    fn an_agent_authority_ruling_retires_no_parallel_ask() {
        let env = DoorEnv::new();
        crate::paths::pin_test_claims_root(env.0.path());
        let _agents = crate::claims::EnvVarGuard::set(
            crate::paths::HOME_ENV,
            env.0.path().join("agents").to_str().unwrap(),
        );
        let index = crate::provider_cap::questions_path(&crate::paths::AgentsHome::from_env());
        seed_ask(&index, "q-3c4d5e6f", "crate-home", "atticus");

        supersede_parallel_asks("crate-home", "d-2b3c4d5e", Some("agent"), None)
            .expect("the fold runs");

        let (_, closed) = crate::question_intake::question_rows(&index);
        assert!(
            !closed.contains("q-3c4d5e6f"),
            "a coordination note never retires the user's ask"
        );
    }

    /// A decide run carrying a full session triple pins the session
    /// provenance on the row: the full session id, harness, transcript
    /// path, and the user turn the row answers (the last real user turn
    /// the transcript held when the row landed). The seeded identity drives
    /// the capture deterministically; the write fold stamps the row's data
    /// map directly, so the pin exercises no door or store machinery. The
    /// quote follows the user lanes: an operator row quotes the turn, a
    /// role row keeps the pointer without the quote.
    #[test]
    fn the_session_provenance_quote_follows_the_user_lane() {
        let _env_lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // A claude store the transcript resolver scans, planted in a temp
        // dir so the test never reads the real one.
        let config = tempfile::tempdir().expect("tempdir");
        let projects = config.path().join("projects").join("-repo");
        std::fs::create_dir_all(&projects).expect("mkdir");
        let session = "4d2c3d68-7d5c-4c1e-9a51-2f8e0a9b1c3d";
        std::fs::write(
            projects.join(format!("{session}.jsonl")),
            concat!(
                r#"{"type":"user","uuid":"u-early","timestamp":"2026-11-20T10:00:00.000Z","message":{"role":"user","content":"earlier ask"}}"#,
                "\n",
                r#"{"type":"assistant","uuid":"a-1","timestamp":"2026-11-20T10:00:05.000Z","message":{"role":"assistant","content":[{"type":"text","text":"on it"}]}}"#,
                "\n",
                r#"{"type":"user","uuid":"u-ruling","timestamp":"2026-11-20T10:01:00.000Z","message":{"role":"user","content":"the docs gate reads the #NNN form"}}"#,
                "\n",
            ),
        )
        .expect("writes");
        let _config =
            crate::claims::EnvVarGuard::set("CLAUDE_CONFIG_DIR", config.path().to_str().unwrap());
        let mut operator_data = json!({
            "decision_id": "d-prov0001",
            "subject": "x-provnode",
            "decided_by": "4d2c3d68",
            "authority_source": "operator",
        });
        apply_session_provenance(
            &mut operator_data,
            DecideIdentity::Seeded(session, "claude"),
        );
        let sp = operator_data
            .pointer("/session_provenance")
            .expect("the row carries session provenance");
        assert_eq!(sp["session_id"], json!(session));
        assert_eq!(sp["harness"], json!("claude"));
        let stored = sp["transcript"].as_str().expect("transcript path stored");
        assert!(stored.ends_with(&format!("{session}.jsonl")), "{stored}");
        assert_eq!(sp["turn_id"], json!("u-ruling"));
        assert_eq!(
            sp["turn_excerpt"],
            json!("the docs gate reads the #NNN form")
        );
        assert_eq!(sp["turn_ts"], json!("2026-11-20T10:01:00.000Z"));
        // The role lane keeps the pointer and drops the quote: the newest
        // user turn is usually unrelated to a lead's own ruling.
        let mut role_data = json!({
            "decision_id": "d-prov0002",
            "subject": "x-provnode",
            "decided_by": "4d2c3d68",
            "authority_source": "role",
        });
        apply_session_provenance(&mut role_data, DecideIdentity::Seeded(session, "claude"));
        let sp = role_data
            .pointer("/session_provenance")
            .expect("the row carries session provenance");
        assert_eq!(sp["turn_id"], json!("u-ruling"));
        assert_eq!(sp["turn_ts"], json!("2026-11-20T10:01:00.000Z"));
        assert!(
            sp.as_object()
                .is_none_or(|m| !m.contains_key("turn_excerpt")),
            "a role row quotes no user turn: {sp}"
        );
    }

    /// An injected identity (the test seam, and any caller with no session)
    /// records no session provenance: the row reads with the decided_by
    /// prefix exactly as rows before the field always did.
    #[test]
    fn an_injected_identity_records_no_session_provenance() {
        let env = DoorEnv::new();
        crate::paths::pin_test_claims_root(env.0.path());
        let argv = [
            "x-provold",
            "Record the ruling",
            "--authority",
            "agent",
            "--why",
            "provenance test",
        ];
        let code = decide_door_write(decide_argv(&argv), DecideIdentity::Forced(Some("cl-prov1")));
        assert_eq!(code, 0);
        let (rows, _) = crate::law_match::all_decision_rows().expect("rows");
        let row = rows
            .iter()
            .find(|r| decision_row_str(r, "subject") == "x-provold")
            .expect("the row landed");
        assert!(
            row.pointer("/session_provenance").is_none(),
            "an injected identity pins no session: {row}"
        );
    }
}
