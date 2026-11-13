//! The usage event arm: allowlisted journal rows ship as one PostHog event
//! each, so trend views read fleet usage (spawns, refusals, merges, latency)
//! the way the eval arm's `fno_eval_run` first read eval history. PostHog
//! files OTLP records as logs and its trend insights read events, so this
//! posts to the capture batch route beside the OTLP arm, not through it.
//! Off unless `[telemetry.usage_events] enabled = true` and a token is set,
//! through `token_env` or the `token_file` read; `OTEL_SDK_DISABLED` and
//! `DO_NOT_TRACK` force it off like every other telemetry arm. The event
//! carries an allowlist of event kinds and identifier fields and never free
//! text, a path, a prompt or a title: our own fleet opts in, nobody else.

use crate::otlp_export::{self, ExportState};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const INTERVAL_S: u64 = 300;
const LOOKBACK_S: i64 = 86_400;
const MAX_BATCH: usize = 1000;
const DEFAULT_HOST: &str = "https://us.i.posthog.com";
const DEFAULT_TOKEN_ENV: &str = "FNO_POSTHOG_PROJECT_TOKEN";
const DEFAULT_TOKEN_FILE: &str = "~/.fno/secrets/posthog-project-token";
/// One shared id, because a journal row has no person behind it.
const DISTINCT_ID: &str = "fno-usage";
const MAX_VALUE_LEN: usize = 128;
const MAX_COMMAND_LEN: usize = 120;

/// The journal a kind lives in. Each fact lands in one home: the agents
/// journal (daemon and agents rows), the global journal (target and hook
/// rows from every project), or each registered project's space journal
/// (hook guards, the backlog, mail sends). A kind reads only its home, so a
/// row that one emitter mirrors into a second journal still ships once.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Home {
    Agents,
    Global,
    Space,
}

/// One kind that ships. `key` names the fields that identify the fact when
/// one fact mints many rows (a merge's cleanup request is re-minted by every
/// detector pass): the uuid hashes those values instead of the row id, so a
/// re-mint dedups to one event. `skip_when` is a (field, value) pair that
/// keeps a row home (a guard's `allow` outnumbers its blocks fifty to one
/// and answers no question).
pub(crate) struct Kind {
    pub name: &'static str,
    pub home: Home,
    pub key: &'static [&'static str],
    pub skip_when: Option<(&'static str, &'static str)>,
}

const fn kind(name: &'static str, home: Home) -> Kind {
    Kind {
        name,
        home,
        key: &[],
        skip_when: None,
    }
}

/// The event kinds that ship. An allowlist in code, not config: widening it
/// is a code review, which is the point. Each kind carries at least one
/// allowlisted field.
pub(crate) const KINDS: &[Kind] = &[
    // Sessions: spawns, refusals, first turns, reaps, lead wakes.
    kind("agent_spawned", Home::Agents),
    kind("agent_spawn_refused", Home::Agents),
    kind("agent_spawn_failed", Home::Agents),
    kind("spawn_first_turn", Home::Agents),
    kind("agent_row_reaped", Home::Agents),
    kind("lead_wake", Home::Agents),
    kind("agent_ask_done", Home::Agents),
    kind("consult_call", Home::Agents),
    kind("sandbox_denial", Home::Agents),
    kind("sandbox_ingest", Home::Agents),
    kind("cli_command", Home::Agents),
    // Mail: delivered to a turn, sent through the daemon.
    kind("agent_mail_drained", Home::Agents),
    kind("agent_send_done", Home::Space),
    kind("agent_send_failed", Home::Space),
    // Merges: one event per merge, then the cleanup it ran.
    Kind {
        key: &["request_id"],
        ..kind("merge_cleanup_requested", Home::Agents)
    },
    kind("merge_cleanup_completed", Home::Agents),
    // Targets: start and finish, the stop reason, the review verdict.
    kind("run_recorded", Home::Global),
    kind("termination", Home::Global),
    kind("session_finalized", Home::Global),
    kind("review_attestation", Home::Global),
    kind("verifier_verdict", Home::Global),
    // CI: each push, and each read of that head's checks.
    kind("pr_push_landed", Home::Global),
    kind("ci_cadence_check", Home::Global),
    // Space rows: guard refusals, PRs, node closes, skills, locks.
    Kind {
        skip_when: Some(("decision", "allow")),
        ..kind("guard_decision", Home::Space)
    },
    kind("style_refusal", Home::Space),
    kind("backlog_done_refused", Home::Space),
    kind("backlog_done_forced", Home::Space),
    kind("node_done", Home::Space),
    kind("pr_opened", Home::Space),
    kind("skill_invoked", Home::Space),
    kind("plan_graduated", Home::Space),
    kind("chokepoint_wait_exceeded", Home::Space),
    kind("graph_lock_held", Home::Space),
];

/// True when `name` is a shipped kind.
pub(crate) fn allowlisted(name: &str) -> bool {
    KINDS.iter().any(|k| k.name == name)
}

/// False when the kind's `skip_when` keeps this row home. A row whose data
/// did not parse is kept: its kind still counts.
pub(crate) fn kept(name: &str, data: Option<&Map<String, Value>>) -> bool {
    let skip = KINDS
        .iter()
        .find(|k| k.name == name)
        .and_then(|k| k.skip_when);
    match (skip, data) {
        (Some((field, value)), Some(data)) => {
            data.get(field).and_then(Value::as_str) != Some(value)
        }
        _ => true,
    }
}

/// String fields that ship when the row carries them; `command` keeps its
/// words through its own gate, the rest pass one identifier each.
const STRING_FIELDS: &[&str] = &[
    "command",
    "harness",
    "provider",
    "model",
    "spawned_by_harness",
    "substrate",
    "launch",
    "class",
    "rule",
    "refusal",
    "reason",
    "outcome",
    "status",
    "verdict",
    "reviewer",
    "level",
    "guard",
    "decision",
    "tool",
    "skill",
    "via",
    "surface",
    "delivery",
    "stage",
    "termination_reason",
    "project",
    "node",
    "node_id",
    "graph_node_id",
    "scope",
];

/// Numeric fields that ship when the row carries them.
const INT_FIELDS: &[&str] = &[
    "exit",
    "exit_code",
    "duration_ms",
    "latency_ms",
    "hold_ms",
    "idle_secs",
    "hits",
    "violation_count",
    "pr",
    "pr_number",
    "offset_s",
    "review_round",
    "findings_blocking",
    "findings_nonblocking",
    "reclaimed_bytes",
    "acted",
];

/// Boolean fields that ship when the row carries them.
const BOOL_FIELDS: &[&str] = &["ship", "delivered", "resumable"];

/// Array fields whose every element must pass the identifier gate; one
/// failing element drops the whole field.
const ID_LIST_FIELDS: &[&str] = &["node_ids"];

/// A nested object whose allowlisted fields ship beside the top level ones
/// (`run_recorded` carries its facts under `run`). A top level value wins.
const NESTED: &[&str] = &["run"];

pub enum Resolved {
    Off { forced_off_by: Option<&'static str> },
    Refused(String),
    On(Config),
}

pub struct Config {
    pub url: String,
    pub host: String,
    pub token: String,
}

pub fn enabled(cwd: &Path) -> bool {
    crate::agents_config::config_lookup(cwd, &["telemetry", "usage_events", "enabled"])
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

pub fn resolve(cwd: &Path) -> Resolved {
    if let Some(var) = otlp_export::forced_off() {
        return Resolved::Off {
            forced_off_by: Some(var),
        };
    }
    if !enabled(cwd) {
        return Resolved::Off {
            forced_off_by: None,
        };
    }
    let table = crate::agents_config::config_lookup(cwd, &["telemetry", "usage_events"]);
    let read = |key: &str, default: &str| {
        table
            .as_ref()
            .and_then(|t| t.get(key))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(default)
            .to_string()
    };
    let base = read("host", DEFAULT_HOST);
    if !base.starts_with("https://") {
        return Resolved::Refused(format!(
            "telemetry.usage_events.host must be https, got: {base}"
        ));
    }
    let token_env = read("token_env", DEFAULT_TOKEN_ENV);
    let token_file = read("token_file", DEFAULT_TOKEN_FILE);
    let token = match otlp_export::token_from_env_or_file(&token_env, &token_file) {
        Ok(token) => token,
        Err(reason) => return Resolved::Refused(reason),
    };
    let base = base.trim_end_matches('/');
    Resolved::On(Config {
        url: format!("{base}/batch/"),
        host: base.trim_start_matches("https://").to_string(),
        token: token.trim().to_string(),
    })
}

/// A string ships only when it reads as one identifier: the eval arm's
/// charset, the length cap, and a path refusal (a leading `/` or `~`, or
/// any `..` segment, because a relative walk is a path too).
pub(crate) fn identifier_value(value: &str) -> Option<&str> {
    if value.is_empty() || value.len() > MAX_VALUE_LEN {
        return None;
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/'))
    {
        return None;
    }
    if value.starts_with('/') || value.starts_with('~') || value.contains("..") {
        return None;
    }
    Some(value)
}

/// `command` keeps its spaces (a command line is "fno do pr merge"), so its
/// gate is its one special case: words, digits and word separators only,
/// and any slash drops the whole value, because a slash in a command line
/// is a path or a redirect and neither is an identifier.
fn command_value(value: &str) -> Option<&str> {
    if value.is_empty() || value.len() > MAX_COMMAND_LEN {
        return None;
    }
    if value.chars().any(|c| c == '/' || c == '\\') {
        return None;
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | ' '))
    {
        return None;
    }
    Some(value)
}

/// The PostHog uuid for a row: the event_id hashes to a stable dashed
/// prefix, whatever shape it arrived in (a bare 32-hex id, the store's
/// `evt:`-prefixed sha fallback, or an emitter-supplied id), so a
/// redelivered batch still dedups to one event per row.
pub(crate) fn event_uuid(event_id: &str) -> String {
    let digest = Sha256::digest(event_id.as_bytes());
    let h: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// The journal line envelope, parsed once at the boundary (rust-style R1):
/// a timestamp and the emitter's data object; everything the arm reads
/// lives inside `data`.
#[derive(serde::Deserialize)]
struct Envelope {
    ts: String,
    data: Map<String, Value>,
}

/// Copy the allowlisted fields of `data` into `props`, each through its
/// gate. A key already in `props` stays, so a top level value outranks the
/// same key in a nested object.
fn copy_fields(data: &Map<String, Value>, props: &mut Map<String, Value>) {
    let mut put = |field: &str, value: Value| {
        props.entry(field.to_string()).or_insert(value);
    };
    for field in STRING_FIELDS {
        if let Some(v) = data.get(*field).and_then(Value::as_str) {
            let clean = if *field == "command" {
                command_value(v)
            } else {
                identifier_value(v)
            };
            if let Some(clean) = clean {
                put(*field, json!(clean));
            }
        }
    }
    for field in INT_FIELDS {
        if let Some(v) = data.get(*field).and_then(Value::as_i64) {
            put(*field, json!(v));
        }
    }
    for field in BOOL_FIELDS {
        if let Some(v) = data.get(*field).and_then(Value::as_bool) {
            put(*field, json!(v));
        }
    }
    for field in ID_LIST_FIELDS {
        let Some(items) = data.get(*field).and_then(Value::as_array) else {
            continue;
        };
        let clean: Option<Vec<&str>> = items
            .iter()
            .map(|v| v.as_str().and_then(identifier_value))
            .collect();
        if let Some(clean) = clean {
            put(*field, json!(clean));
        }
    }
}

/// The uuid for a row: the fact key when the kind names one and the row
/// carries every key field, else the row's own event id.
fn row_uuid(kind: &Kind, row: &crate::event_store::EventRow, data: &Map<String, Value>) -> String {
    let parts: Option<Vec<String>> = kind
        .key
        .iter()
        .map(|f| data.get(*f).map(|v| v.to_string()))
        .collect();
    match parts {
        Some(parts) if !parts.is_empty() => {
            event_uuid(&format!("{}:{}", kind.name, parts.join(":")))
        }
        _ => event_uuid(&row.event_id),
    }
}

/// One journal row as one PostHog event, with its uuid. None for a kind off
/// the allowlist, a row its kind keeps home, or a line that is not the
/// envelope shape (query_events already rejected those rows; this is a
/// belt-and-braces read).
fn event(row: &crate::event_store::EventRow, machine: &str) -> Option<(String, Value)> {
    let kind = KINDS.iter().find(|k| k.name == row.r#type)?;
    let parsed: Envelope = serde_json::from_str(&row.line).ok()?;
    let data = &parsed.data;
    if !kept(kind.name, Some(data)) {
        return None;
    }
    let mut props = Map::new();
    props.insert("machine".into(), json!(machine));
    props.insert("$process_person_profile".into(), json!(false));
    copy_fields(data, &mut props);
    for nested in NESTED {
        if let Some(inner) = data.get(*nested).and_then(Value::as_object) {
            copy_fields(inner, &mut props);
        }
    }
    let uuid = row_uuid(kind, row, data);
    Some((
        uuid.clone(),
        json!({
            "event": format!("fno_{}", row.r#type),
            "uuid": uuid,
            "timestamp": parsed.ts,
            "distinct_id": DISTINCT_ID,
            "properties": props,
        }),
    ))
}

/// The rows a beat owes, oldest first: inside the lookback window, on the
/// type allowlist, not yet delivered or dropped, one per uuid, capped at one
/// batch.
fn select(
    rows: Vec<crate::event_store::EventRow>,
    state: &ExportState,
    machine: &str,
) -> Vec<(String, String, Value)> {
    let mut pending: Vec<(i64, (String, Value))> = rows
        .into_iter()
        .filter_map(|row| event(&row, machine).map(|doc| (row.ts_ms, doc)))
        .filter(|(_, (id, _))| !state.knows(id))
        .collect();
    pending.sort_by_key(|(ms, _)| *ms);
    let mut seen = std::collections::HashSet::new();
    pending.retain(|(_, (id, _))| seen.insert(id.clone()));
    pending.truncate(MAX_BATCH);
    pending
        .into_iter()
        .map(|(_, (id, doc))| {
            let ts = doc["timestamp"].as_str().unwrap_or_default().to_string();
            (id, ts, doc)
        })
        .collect()
}

fn state_path(home: &crate::paths::AgentsHome) -> PathBuf {
    home.otlp_export_dir().join("usage_events_state.json")
}

/// The journal files a read walks, one per home: the agents journal, the
/// global journal, and the space journal of every registered project.
pub(crate) struct Journals {
    pub agents: PathBuf,
    pub global: PathBuf,
    pub spaces: Vec<PathBuf>,
}

impl Journals {
    pub(crate) fn of(home: &crate::paths::AgentsHome) -> Self {
        let mut spaces: Vec<PathBuf> = Vec::new();
        for root in crate::backlog::settings::project_roots() {
            if let Some(space) = crate::paths::space_dir_opt(&root) {
                let journal = space.join("events.jsonl");
                if !spaces.contains(&journal) {
                    spaces.push(journal);
                }
            }
        }
        Journals {
            agents: home.events_jsonl(),
            global: crate::daemon::global_events_path(home),
            spaces,
        }
    }

    fn paths(&self, home: Home) -> Vec<&Path> {
        match home {
            Home::Agents => vec![self.agents.as_path()],
            Home::Global => vec![self.global.as_path()],
            Home::Space => self.spaces.iter().map(PathBuf::as_path).collect(),
        }
    }
}

/// Every allowlisted row in `[since_ms, until_ms]`, each kind read from its
/// own home: one bounded SQL read per journal file.
pub(crate) fn rows_between(
    journals: &Journals,
    since_ms: i64,
    until_ms: Option<i64>,
) -> Vec<crate::event_store::EventRow> {
    let mut out = Vec::new();
    for home in [Home::Agents, Home::Global, Home::Space] {
        let types: Vec<String> = KINDS
            .iter()
            .filter(|k| k.home == home)
            .map(|k| k.name.to_string())
            .collect();
        for path in journals.paths(home) {
            let query = crate::event_store::EventQuery {
                types: types.clone(),
                since_ms: Some(since_ms),
                until_ms,
                ..Default::default()
            };
            out.extend(crate::event_store::query_events(path, &query).unwrap_or_default());
        }
    }
    out
}

/// The rows a beat reads, already bounded: the type allowlist and a 24h
/// lookback window, so a store the size of a fleet's is still one bounded
/// SQL read per journal.
fn read_rows(journals: &Journals, now_ms: i64) -> Vec<crate::event_store::EventRow> {
    rows_between(journals, now_ms - LOOKBACK_S * 1000, None)
}

/// One beat. The same delivery rule as the eval arm: a 2xx records the batch,
/// a permanent 4xx drops it, and anything else holds it for the next beat.
fn run_beat(
    cfg: &Config,
    home: &crate::paths::AgentsHome,
    journals: &Journals,
    poster: &mut dyn crate::attention_http::HttpPost,
    now_ms: i64,
) -> otlp_export::BeatReport {
    let path = state_path(home);
    let mut state = otlp_export::load_state_at(&path);
    let machine = crate::claims::hostname();
    let rows = read_rows(journals, now_ms);
    let pending = select(rows, &state, &machine);
    let count = pending.len();
    if pending.is_empty() {
        return otlp_export::BeatReport {
            acted: 0,
            skip: Some("caught_up"),
            detail: vec!["usage events: nothing new in the window".to_string()],
        };
    }
    let headers = [("Content-Type".to_string(), "application/json".to_string())];
    let result = poster.post(&cfg.url, &headers, &batch_body(&cfg.token, &pending));
    let mut report = otlp_export::BeatReport {
        acted: count as u64,
        skip: None,
        detail: Vec::new(),
    };
    if result.delivered() {
        for (id, ts, _) in pending {
            state.delivered.insert(id, ts);
        }
        state.held_error = None;
        report
            .detail
            .push(format!("usage events: delivered {count} to {}", cfg.host));
    } else if result.dropped() {
        for (id, ts, _) in pending {
            state.dropped.insert(id, ts);
        }
        state.held_error = None;
        report.detail.push(format!(
            "usage events: endpoint rejected {count} permanently (status {})",
            result.status.unwrap_or(0)
        ));
    } else {
        state.held_error = Some(match result.status {
            Some(code) => format!("status {code}"),
            None => "no status".to_string(),
        });
        report.acted = 0;
        report.skip = Some("held");
        report
            .detail
            .push(format!("usage events: held {count} for retry"));
    }
    otlp_export::prune(&mut state, now_ms);
    otlp_export::save_state_at(home, &path, &state);
    report
}

fn batch_body(token: &str, pending: &[(String, String, Value)]) -> String {
    let events: Vec<&Value> = pending.iter().map(|(_, _, doc)| doc).collect();
    json!({"api_key": token, "batch": events}).to_string()
}

/// `fno-agents evals events --usage`: print the batch the next beat would post, with
/// the token redacted, and the posture on stderr. It never sends.
pub fn preview_command(_args: &[String]) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(home) = crate::paths::AgentsHome::from_env_opt() else {
        eprintln!("fno-agents evals events --usage: no agents home");
        return 1;
    };
    let state = otlp_export::load_state_at(&state_path(&home));
    let pending = select(
        read_rows(&Journals::of(&home), otlp_export::now_ms()),
        &state,
        &crate::claims::hostname(),
    );
    println!("{}", batch_body("<redacted>", &pending));
    let posture = match resolve(&cwd) {
        Resolved::Off {
            forced_off_by: Some(var),
        } => format!("off - forced off by {var}"),
        Resolved::Off {
            forced_off_by: None,
        } => "off - nothing leaves this machine".to_string(),
        Resolved::Refused(reason) => format!("refused: {reason}"),
        Resolved::On(cfg) => format!(
            "on -> {}, held: {}",
            cfg.host,
            state.held_error.as_deref().unwrap_or("none")
        ),
    };
    eprintln!("usage events: {posture}; {} pending", pending.len());
    0
}

/// The command path and start the `fno` front door handed over on exec
/// (`crates/fno/src/cli_usage.rs`), read once and taken out of this
/// process's environment so a child this process spawns writes no second
/// row under the parent's name.
static CLI_START: std::sync::OnceLock<Option<(String, i64)>> = std::sync::OnceLock::new();

/// Read the handed-over command, if any. `fno-agents` main calls this first.
pub fn take_cli_start() {
    let path = std::env::var("FNO_CLI_PATH").ok();
    let start = std::env::var("FNO_CLI_START_MS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok());
    std::env::remove_var("FNO_CLI_PATH");
    std::env::remove_var("FNO_CLI_START_MS");
    let handed = match (path, start) {
        (Some(path), Some(start))
            if path
                .chars()
                .all(|c| c.is_ascii_lowercase() || matches!(c, '-' | ' ')) =>
        {
            Some((path, start))
        }
        _ => None,
    };
    if CLI_START.set(handed).is_err() {
        eprintln!("fno-agents: the handed-over command was already read");
    }
}

/// Write the `cli_command` row for a handed-over command, then exit.
pub fn exit_cli(code: i32) -> ! {
    if let (Some(Some((path, start))), Some(home)) =
        (CLI_START.get(), crate::paths::AgentsHome::from_env_opt())
    {
        let data = json!({
            "command": path,
            "exit": code,
            "duration_ms": (otlp_export::now_ms() - start).max(0),
        });
        let emitter = crate::runtime::events::EventEmitter::new(home.events_jsonl(), "cli");
        if let Err(e) = emitter.emit("cli_command", &data) {
            eprintln!("fno-agents: the cli_command row did not land: {e}");
        }
    }
    std::process::exit(code)
}

pub struct Arm {
    config_cwd: PathBuf,
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

impl Arm {
    pub fn new(config_cwd: PathBuf) -> Self {
        Self {
            config_cwd,
            last_tick: Mutex::new(None),
            in_flight: Arc::new(AtomicBool::new(false)),
        }
    }
}

fn emit_tick_row(home: &crate::paths::AgentsHome, acted: u64, skip: Option<&str>, detail: &str) {
    let journal = crate::loop_runtime::Journal::new_raw(
        home.events_jsonl(),
        crate::daemon::global_events_path(home),
    );
    crate::tick_ledger::emit_tick(
        &journal,
        "usage_events",
        crate::tick_ledger::SCHED_DAEMON,
        acted,
        skip,
        Some(detail),
        INTERVAL_S,
    );
}

pub fn maybe_tick(arm: &Arm, home: crate::paths::AgentsHome) {
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed().as_secs() < INTERVAL_S) {
            return;
        }
        *last = Some(Instant::now());
    }
    if arm.in_flight.swap(true, Ordering::SeqCst) {
        return;
    }
    let flag = Arc::clone(&arm.in_flight);
    let cwd = arm.config_cwd.clone();
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        match resolve(&cwd) {
            Resolved::Off { forced_off_by } => {
                let why = forced_off_by
                    .map(|v| format!("forced off by {v}"))
                    .unwrap_or_else(|| "telemetry.usage_events.enabled is false".to_string());
                emit_tick_row(&home, 0, Some("disabled"), &why);
            }
            Resolved::Refused(reason) => {
                emit_tick_row(&home, 0, Some("config"), &format!("refused: {reason}"));
            }
            Resolved::On(cfg) => {
                let mut poster = crate::attention_http::CurlPost;
                let report = run_beat(
                    &cfg,
                    &home,
                    &Journals::of(&home),
                    &mut poster,
                    otlp_export::now_ms(),
                );
                emit_tick_row(&home, report.acted, report.skip, &report.detail.join("; "));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attention_http::{HttpPost, PostResult};
    use serde_json::json;

    fn row_line(kind: &str, event_id: &str, data: Value) -> crate::event_store::EventRow {
        let line = json!({
            "ts": "2026-11-02T11:00:00.5Z",
            "type": kind,
            "source": "test",
            "data": data,
        })
        .to_string();
        crate::event_store::EventRow {
            seq: 0,
            event_id: event_id.to_string(),
            ts_ms: 0,
            r#type: kind.to_string(),
            source: "test".to_string(),
            scope: None,
            retention_class: "durable".to_string(),
            reject_reason: None,
            line,
            history_only: false,
            recovery_batch: None,
        }
    }

    fn test_journals(dir: &Path, home: &crate::paths::AgentsHome) -> Journals {
        Journals {
            agents: home.events_jsonl(),
            global: dir.join("global").join("events.jsonl"),
            spaces: vec![dir.join("space").join("events.jsonl")],
        }
    }

    /// The golden map: each kind reads from its own journal and ships as
    /// `fno_<kind>` with only allowlisted fields. A guard allow stays home,
    /// a re-minted merge request ships once, and a nested run's facts ship
    /// beside the top level ones.
    #[test]
    fn each_kind_ships_from_its_home_with_only_allowlisted_fields() {
        let dir = tempfile::tempdir().unwrap();
        let home = crate::paths::AgentsHome::at(dir.path());
        let journals = test_journals(dir.path(), &home);
        let space = journals.spaces.first().cloned().unwrap();
        for path in [&journals.global, &space] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        }
        let at = |path: &PathBuf, source: &str| {
            crate::runtime::events::EventEmitter::new(path.clone(), source.to_string())
        };
        let secret = "secret free text /Users/x";
        let rows: [(&PathBuf, &str, &str, Value); 10] = [
            (
                &journals.agents,
                "cli",
                "cli_command",
                json!({"command": "backlog get", "exit": 0, "duration_ms": 41, "argv": secret}),
            ),
            (
                &journals.agents,
                "agents",
                "merge_cleanup_requested",
                json!({"request_id": "merge-cleanup-ab12", "repo": secret, "pr": 7,
                       "branch": secret, "worktree": null, "node_ids": ["x-aaaa"]}),
            ),
            (
                &journals.agents,
                "agents",
                "merge_cleanup_requested",
                json!({"request_id": "merge-cleanup-ab12", "repo": secret, "pr": 7,
                       "branch": secret, "worktree": "w-second-pass", "node_ids": ["x-aaaa"]}),
            ),
            (
                &journals.global,
                "target",
                "run_recorded",
                json!({"run": {"status": "started", "graph_node_id": "x-aaaa",
                               "model": "sonnet", "title": secret}}),
            ),
            (
                &journals.global,
                "target",
                "review_attestation",
                json!({"reviewer": "code-review", "head_sha": "abc123", "verdict": "pass",
                       "session_id": "s-1", "level": "low", "findings_blocking": 0,
                       "branch": secret}),
            ),
            (
                &space,
                "hook",
                "guard_decision",
                json!({"guard": "git-protection", "decision": "allow", "tool": "Bash"}),
            ),
            (
                &space,
                "hook",
                "guard_decision",
                json!({"guard": "git-protection", "decision": "block", "tool": "Bash"}),
            ),
            (
                &space,
                "hook",
                "skill_invoked",
                json!({"skill": "fno:target", "harness": "claude", "via": "tool"}),
            ),
            (
                &space,
                "target",
                "pr_opened",
                json!({"pr": 141, "node": "x-aaaa", "title": secret}),
            ),
            (
                &space,
                "backlog",
                "node_done",
                json!({"node_id": "x-aaaa", "via": "reconcile", "pr_number": 141}),
            ),
        ];
        for (path, source, kind, data) in rows {
            at(path, source).emit(kind, &data).unwrap();
        }
        // A space-home kind in the agents journal is not read: one home each.
        at(&journals.agents, "hook")
            .emit(
                "guard_decision",
                &json!({"guard": "x", "decision": "block", "tool": "Bash"}),
            )
            .unwrap();

        let rows = read_rows(&journals, otlp_export::now_ms());
        let state = ExportState::default();
        let batch = select(rows, &state, "mac1");
        let text = serde_json::to_string(&batch.iter().map(|b| &b.2).collect::<Vec<_>>()).unwrap();
        assert!(!text.contains("secret"), "free text leaked: {text}");
        let named = |event: &str| -> Vec<&Map<String, Value>> {
            batch
                .iter()
                .filter(|b| b.2.get("event") == Some(&json!(event)))
                .map(|b| b.2.get("properties").and_then(Value::as_object).unwrap())
                .collect()
        };
        let first = |rows: &[&Map<String, Value>], key: &str| -> Value {
            rows.first()
                .and_then(|m| m.get(key))
                .cloned()
                .unwrap_or(Value::Null)
        };
        let cli = named("fno_cli_command");
        assert_eq!(cli.len(), 1);
        assert_eq!(first(&cli, "command"), "backlog get");
        assert_eq!(first(&cli, "duration_ms"), 41);
        let merges = named("fno_merge_cleanup_requested");
        assert_eq!(merges.len(), 1, "a re-mint ships once");
        assert_eq!(first(&merges, "node_ids"), json!(["x-aaaa"]));
        assert_eq!(first(&merges, "pr"), 7);
        let runs = named("fno_run_recorded");
        assert_eq!(first(&runs, "status"), "started");
        assert_eq!(first(&runs, "graph_node_id"), "x-aaaa");
        assert_eq!(first(&named("fno_review_attestation"), "verdict"), "pass");
        let guards = named("fno_guard_decision");
        assert_eq!(guards.len(), 1, "allow stays home, one home read");
        assert_eq!(first(&guards, "decision"), "block");
        assert_eq!(first(&named("fno_skill_invoked"), "skill"), "fno:target");
        assert_eq!(first(&named("fno_pr_opened"), "pr"), 141);
        assert_eq!(first(&named("fno_node_done"), "via"), "reconcile");
        let allowed: Vec<&str> = STRING_FIELDS
            .iter()
            .chain(INT_FIELDS)
            .chain(BOOL_FIELDS)
            .chain(ID_LIST_FIELDS)
            .chain(&["machine", "$process_person_profile"])
            .copied()
            .collect();
        for (_, _, doc) in &batch {
            for key in doc["properties"].as_object().unwrap().keys() {
                assert!(allowed.contains(&key.as_str()), "unlisted field {key}");
            }
        }
    }

    struct Scripted {
        status: Option<u16>,
        bodies: Vec<String>,
    }

    impl HttpPost for Scripted {
        fn post(&mut self, _: &str, _: &[(String, String)], body: &str) -> PostResult {
            self.bodies.push(body.to_string());
            PostResult {
                status: self.status,
                body: String::new(),
                error: None,
            }
        }
    }

    /// A redelivered batch still dedups: every event_id shape (bare hex, the
    /// store's evt-prefixed sha, an emitter-supplied id) maps to one stable
    /// dashed uuid, and different ids map differently.
    #[test]
    fn event_uuid_is_stable_across_id_shapes() {
        let hex = event_uuid(&"a".repeat(32));
        let evt_sha = event_uuid("evt:abc123");
        let emitter_id = event_uuid("run-42-worker-a");
        assert!(hex.chars().filter(|c| *c == '-').count() == 4, "{hex}");
        assert_eq!(hex, event_uuid(&"a".repeat(32)), "same id, same uuid");
        assert_ne!(hex, evt_sha);
        assert_ne!(evt_sha, emitter_id);
    }

    fn scripted(status: Option<u16>) -> Scripted {
        Scripted {
            status,
            bodies: Vec::new(),
        }
    }

    /// A 2xx ships once, a hold retries the same rows, and a kind outside
    /// the type allowlist never ships.
    #[test]
    fn beat_delivers_once_and_holds_on_a_transient_failure() {
        let dir = tempfile::tempdir().unwrap();
        let home = crate::paths::AgentsHome::at(dir.path());
        let cfg = Config {
            url: "https://ph.example/batch/".into(),
            host: "ph.example".into(),
            token: "tok".into(),
        };
        let journals = test_journals(dir.path(), &home);
        let daemon = crate::runtime::events::EventEmitter::new(home.events_jsonl(), "daemon");
        daemon
            .emit("agent_spawned", &json!({"node": "x-aaaa"}))
            .unwrap();
        let consult = crate::runtime::events::EventEmitter::new(home.events_jsonl(), "consult");
        consult
            .emit(
                "consult_call",
                &json!({
                    "session_id": "sess-1",
                    "harness": "claude",
                    "model": "glm-5.3-flash",
                    "question_chars": 10,
                    "context_bytes": 20,
                    "input_tokens": 30,
                    "output_tokens": 40,
                    "cost_usd": 0.5,
                    "exit": 0
                }),
            )
            .unwrap();
        // A kind outside the type allowlist never leaves the machine.
        daemon.emit("daemon_started", &json!({"pid": 1})).unwrap();

        let mut held = scripted(Some(503));
        let report = run_beat(&cfg, &home, &journals, &mut held, otlp_export::now_ms());
        assert_eq!(report.skip, Some("held"));
        assert_eq!(report.acted, 0);
        assert_eq!(held.bodies.len(), 1, "a held beat still posts once");
        let sent: Value = serde_json::from_str(&held.bodies[0]).unwrap();
        assert_eq!(sent["api_key"], "tok");
        assert_eq!(sent["batch"].as_array().unwrap().len(), 2);

        let mut ok = scripted(Some(200));
        let report = run_beat(&cfg, &home, &journals, &mut ok, otlp_export::now_ms());
        assert_eq!(report.acted, 2);
        let sent: Value = serde_json::from_str(&ok.bodies[0]).unwrap();
        let first = &sent["batch"][0];
        assert_eq!(first["event"], "fno_agent_spawned");
        assert_eq!(first["distinct_id"], "fno-usage");
        assert_eq!(first["properties"]["$process_person_profile"], false);
        assert_eq!(first["properties"]["machine"], crate::claims::hostname());

        let report = run_beat(&cfg, &home, &journals, &mut ok, otlp_export::now_ms());
        assert_eq!(report.skip, Some("caught_up"));
        assert_eq!(ok.bodies.len(), 1, "a delivered attempt never resends");
    }

    /// The gates in order: a machine-wide kill switch outranks the config,
    /// stock config (no usage table) stays off, an enabled arm without a
    /// token refuses rather than shipping unauthenticated, and every gate
    /// open resolves to the capture batch route.
    #[test]
    fn resolve_stays_off_until_each_gate_opens() {
        static ENV_LOCK: std::sync::LazyLock<&'static std::sync::Mutex<()>> =
            std::sync::LazyLock::new(crate::claims::test_env_lock);
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let keys = [
            "FNO_CONFIG",
            "FNO_NO_CANONICAL_CONFIG",
            "FNO_GLOBAL_SETTINGS_PATH",
            "FNO_POSTHOG_PROJECT_TOKEN",
            "OTEL_SDK_DISABLED",
            "DO_NOT_TRACK",
        ];
        let saved: Vec<_> = keys.iter().map(|k| (*k, std::env::var_os(k))).collect();
        for key in keys {
            std::env::remove_var(key);
        }
        std::env::set_var("FNO_NO_CANONICAL_CONFIG", "1");
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::env::set_var("FNO_CONFIG", &cfg);

        // The OTLP arm arming alone never arms this one.
        std::fs::write(
            &cfg,
            "[telemetry.otlp]\nenabled = true\nendpoint = \"https://c.example\"\n",
        )
        .unwrap();
        assert!(matches!(
            resolve(dir.path()),
            Resolved::Off {
                forced_off_by: None
            }
        ));

        // Enabled with no token refuses and names the env var.
        let token_path = dir.path().join("absent-token");
        std::fs::write(
            &cfg,
            format!(
                "[telemetry.usage_events]\nenabled = true\ntoken_file = \"{}\"\n",
                token_path.display()
            ),
        )
        .unwrap();
        assert!(
            matches!(resolve(dir.path()), Resolved::Refused(ref m) if m.contains("FNO_POSTHOG_PROJECT_TOKEN"))
        );

        // Token set: every gate open resolves on.
        std::env::set_var("FNO_POSTHOG_PROJECT_TOKEN", "tok");
        assert!(matches!(
            resolve(dir.path()),
            Resolved::On(c) if c.url == "https://us.i.posthog.com/batch/"
        ));

        // The machine-wide kill switch outranks the config.
        std::env::set_var("DO_NOT_TRACK", "1");
        assert!(matches!(
            resolve(dir.path()),
            Resolved::Off {
                forced_off_by: Some("DO_NOT_TRACK")
            }
        ));

        for (key, value) in saved {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }

    /// A command line keeps its words but never carries a path: any slash
    /// drops the whole value, and the charset gate refuses shell syntax.
    #[test]
    fn command_ships_words_never_a_path() {
        let mk = |cmd: &str| {
            let row = row_line(
                "graph_lock_held",
                "b".repeat(32).as_str(),
                json!({"command": cmd, "hold_ms": 4210}),
            );
            event(&row, "mac1").unwrap().1
        };
        let doc = mk("fno do pr merge 12");
        assert_eq!(doc["properties"]["command"], "fno do pr merge 12");
        assert_eq!(doc["properties"]["hold_ms"], 4210);
        for rejected in [
            "/usr/bin/fno status",
            "cat /tmp/secret.txt",
            "fno do pr merge 12 && rm -rf /",
        ] {
            let doc = mk(rejected);
            assert!(
                !doc["properties"]
                    .as_object()
                    .unwrap()
                    .contains_key("command"),
                "a path-bearing or non-identifier command shipped: {rejected}"
            );
        }
        let long = format!("fno {}", "x".repeat(130));
        let doc = mk(&long);
        assert!(
            !doc["properties"]
                .as_object()
                .unwrap()
                .contains_key("command"),
            "over 120 drops"
        );
    }

    /// One token per string field: identifiers ship, prose and paths and
    /// overlong values drop, and the numeric fields pass through as numbers.
    #[test]
    fn identifier_needs_one_token() {
        let mk = |data: Value| {
            let row = row_line("consult_call", "c".repeat(32).as_str(), data);
            event(&row, "mac1").unwrap().1
        };
        let ships = |field: &str, doc: &Value| doc["properties"].get(field).is_some();
        let drops = |field: &str, doc: &Value| doc["properties"].get(field).is_none();

        assert!(ships("reason", &mk(json!({"reason": "conflicting"}))));
        assert!(drops(
            "reason",
            &mk(json!({"reason": "PR state: conflicting with main"}))
        ));
        assert!(ships("node", &mk(json!({"node": "x-aaaa"}))));
        assert!(drops("node", &mk(json!({"node": "/Users/bb16/code/x"}))));
        assert!(ships("harness", &mk(json!({"harness": "claude"}))));
        assert!(drops(
            "harness",
            &mk(json!({"harness": "my harness words"}))
        ));
        assert!(ships("model", &mk(json!({"model": "gpt-6.1-sol"}))));
        assert!(drops("model", &mk(json!({ "model": "a".repeat(200) }))));
        assert!(ships("outcome", &mk(json!({"outcome": "ok"}))));
        assert!(ships("class", &mk(json!({"class": "git_write"}))));
        assert!(ships(
            "rule",
            &mk(json!({"rule": "operation_not_permitted"}))
        ));
        assert_eq!(mk(json!({"exit": 0}))["properties"]["exit"], 0);
        assert_eq!(
            mk(json!({"duration_ms": 4210}))["properties"]["duration_ms"],
            4210
        );
    }
}
