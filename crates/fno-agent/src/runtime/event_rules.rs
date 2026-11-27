//! Event rules: a declared table of event, condition and action rows run at
//! the Stop boundary (`docs/architecture/event-rules.md`). A row names the
//! event it watches (`stop`, a journal type, or `decision_span:<kind>`), the
//! named predicates that must all hold, and one action (`block`, `nudge`,
//! `notify`, `emit`). Every fire writes a `decision_span` span whose `rule`
//! and `matched_event` attrs are the dedupe ledger, so a nudge fires once
//! per matched event and a block caps at [`BLOCK_CAP`] per Stop turn. Rows
//! enable through `event_rules.<id>` config overrides read with
//! `config_value_deep`; the shipped table lives beside this file and is
//! `include_str!`ed like `merge_posture.toml`.

use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::decision_trace::Trace;

const TABLE: &str = include_str!("../event_rules.toml");

/// Block fires one rule may spend on one Stop turn before the engine allows
/// and the fire's span carries `cap_reached`.
pub(crate) const BLOCK_CAP: usize = 3;

/// The reply tail a Stop block keeps on its span, so `intel --lens rules`
/// can show a judge what the rule fired on. The tail is where asks sit.
const EXCERPT_CHARS: usize = 600;

/// Every predicate name a row may name, `not:`-prefixable. An unknown name
/// fails the table load test, never the runtime match below.
const PREDICATES: &[&str] = &[
    "promoted",
    "lead_or_user_turn",
    "last_message_decision_ask",
    "turn_filed_question",
    "route_self",
    "actor_is_session",
    "reversible",
    "has_recommendation",
    "why_user_set",
    "class_user_only",
    "lead_idle_reactive_pick",
    "lead_spawn_gate_refused",
];

struct Row {
    id: String,
    event: String,
    when: Vec<String>,
    action: String,
    what: String,
    why: String,
    instead: String,
    default: bool,
}

/// One fired action. `reason` is the Stop block text for `block`/`nudge`;
/// `notify` carries the (title, body, pointer) the caller sends through
/// `notify_operator` - the engine decides, the caller executes, so tests
/// never spawn a process.
#[derive(Debug)]
pub struct Fire {
    pub rule: String,
    pub action: &'static str,
    pub reason: String,
    pub notify: Option<(String, String, Option<String>)>,
}

/// The table parses through the shared rule schema (`crate::rules`); this
/// maps each rule onto the Stop runtime's action names. The runtime's
/// `nudge` is a `warn`, and `notify` and `emit` are `inform` rows with a
/// delivery channel.
fn parse_table() -> Result<Vec<Row>, String> {
    let rules = crate::rules::parse_table(TABLE).map_err(|e| format!("event_rules.toml: {e}"))?;
    let mut rows = Vec::new();
    for rule in rules {
        let action = match (rule.action.as_str(), rule.deliver()) {
            ("block", _) => "block",
            ("warn", _) => "nudge",
            ("inform", "notify") => "notify",
            ("inform", "emit") => "emit",
            (action, deliver) => {
                return Err(format!(
                "event_rules.toml: {}: action {action} with deliver {deliver} has no Stop runtime",
                rule.id
            ))
            }
        };
        let [event] = rule.when.events.as_slice() else {
            return Err(format!(
                "event_rules.toml: {} must name exactly one event",
                rule.id
            ));
        };
        let row = Row {
            event: event.strip_prefix("journal:").unwrap_or(event).to_string(),
            when: rule.when.predicates.clone(),
            action: action.to_string(),
            default: rule.default,
            id: rule.id,
            what: rule.what,
            why: rule.why,
            instead: rule.instead,
        };
        if row.what.is_empty() || row.why.is_empty() || row.instead.is_empty() {
            return Err(format!(
                "event_rules.toml: {} must carry non-empty what, why and instead",
                row.id
            ));
        }
        for p in &row.when {
            let name = p.strip_prefix("not:").unwrap_or(p);
            if !PREDICATES.contains(&name) {
                return Err(format!(
                    "event_rules.toml: {} names unknown predicate {p:?}",
                    row.id
                ));
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

fn table() -> &'static Vec<Row> {
    static CELL: OnceLock<Vec<Row>> = OnceLock::new();
    CELL.get_or_init(|| parse_table().expect("event_rules.toml must load"))
}

/// One config read per fire: the whole `event_rules` table comes back
/// once and each row's override is looked up in it.
fn overrides(cwd: &Path) -> std::collections::HashMap<String, bool> {
    let mut out = std::collections::HashMap::new();
    if let Some(table) = crate::agents_config::config_value_deep(cwd, &["event_rules"]) {
        if let Some(map) = table.as_table() {
            for (key, value) in map {
                if let Some(flag) = value.as_bool() {
                    out.insert(key.clone(), flag);
                }
            }
        }
    }
    out
}

fn enabled(overrides: &std::collections::HashMap<String, bool>, row: &Row) -> bool {
    match overrides.get(&row.id) {
        Some(v) => *v,
        None => row.default,
    }
}

/// What one engine run reads and answers against.
struct Ctx<'a> {
    cwd: &'a Path,
    journal: &'a Path,
    home: &'a crate::paths::AgentsHome,
    /// The firing session's canonical (head-8) handle.
    session: String,
    /// The session id as the harness sent it. The prompt hook's fire spans
    /// carry this form, and the head-8 handle collides across same-window
    /// codex sessions.
    session_id: String,
    last_message: String,
    /// The matched_event key Stop rows dedupe and cap on.
    matched_event: String,
    /// The turn window `turn_filed_question` scans: since the transcript's
    /// last user turn, or the last 30 minutes when it cannot be read.
    window_ms: i64,
}

/// Evaluate the table for one Stop fire. Every harness's Stop rides through
/// here, owned session or not: the rules a session owes do not depend on a
/// target or lead manifest. Best-effort throughout - a journal read failure
/// answers no fires rather than blocking an unreadable turn.
pub fn eval_stop(cwd: &Path, payload: &str) -> Vec<Fire> {
    let journal = crate::law_match::project_events_journal();
    let home = crate::paths::AgentsHome::from_env();
    eval_stop_in(cwd, &journal, &home, payload)
}

pub fn eval_stop_in(
    cwd: &Path,
    journal: &Path,
    home: &crate::paths::AgentsHome,
    payload: &str,
) -> Vec<Fire> {
    let Ok(parsed) = serde_json::from_str::<Value>(payload.trim()) else {
        return Vec::new();
    };
    let session_id = parsed
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if session_id.is_empty() {
        return Vec::new();
    }
    let session = crate::identity::canonical_handle(session_id);
    let last_message = parsed
        .get("last_assistant_message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let turn_id = parsed
        .get("turn_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let transcript_path = parsed
        .get("transcript_path")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let now_ms = now_ms();
    let turn_ts = transcript_turn_window(transcript_path);
    let window_ms = turn_ts.unwrap_or(now_ms - 30 * 60 * 1000);
    let turn_key = match (!turn_id.is_empty(), turn_ts) {
        (true, _) => turn_id,
        (false, Some(ts)) => format!("t{ts}"),
        // No turn id and no readable transcript: a day bucket. The cap
        // is the anti-loop bound, so the coarsest sticky key is the safe
        // reading - a half-hour bucket re-armed the cap mid-conversation.
        (false, None) => format!("b{}", now_ms / 86_400_000),
    };
    let matched_event = format!("stop:{session}:{turn_key}");
    let ctx = Ctx {
        cwd,
        journal,
        home,
        session,
        session_id: session_id.to_string(),
        last_message,
        matched_event,
        window_ms,
    };

    let (cursor_seq, cursor_ts) = read_cursor(journal, &ctx.session);
    let since_ms = cursor_ts.unwrap_or(now_ms - 30 * 60 * 1000);
    let query = crate::event_store::EventQuery {
        since_ms: Some(since_ms),
        ..crate::event_store::EventQuery::of_types(&["decision_span", "operator_question"])
    };
    let rows = crate::event_store::query_events(journal, &query).unwrap_or_default();
    let fresh: Vec<&crate::event_store::EventRow> =
        rows.iter().filter(|r| r.seq > cursor_seq).collect();
    let max_seq = fresh.iter().map(|r| r.seq).max().unwrap_or(cursor_seq);
    let max_ts = fresh
        .iter()
        .max_by_key(|r| r.seq)
        .map(|r| r.ts_ms)
        .or(cursor_ts)
        .unwrap_or(now_ms);

    // The dedupe ledger loads on the first candidate fire: a Stop with no
    // ask and no journal event never scans the span table.
    let mut ledger: Option<Vec<(String, String, String)>> = None;
    let mut fires = Vec::new();
    let overrides = overrides(cwd);
    for row in table() {
        if !enabled(&overrides, row) {
            continue;
        }
        match row.event.as_str() {
            "stop" => {
                if row.when.iter().all(|p| predicate(&ctx, p, None)) {
                    let ledger = match &mut ledger {
                        Some(l) => l,
                        None => ledger.insert(ledger_rows(journal)),
                    };
                    if let Some(fire) = stop_block(&ctx, row, ledger, journal) {
                        fires.push(fire);
                    }
                }
            }
            "decision_span:route" | "operator_question" => {
                let wanted_kind = row
                    .event
                    .strip_prefix("decision_span:")
                    .map(|k| k.to_string());
                for r in &fresh {
                    let Ok(v) = serde_json::from_str::<Value>(&r.line) else {
                        continue;
                    };
                    if row.event == "decision_span:route" && span_kind(&v) != wanted_kind.as_deref()
                    {
                        continue;
                    }
                    if row.event == "operator_question" && r.r#type != "operator_question" {
                        continue;
                    }
                    if row.when.iter().all(|p| predicate(&ctx, p, Some(&v))) {
                        let ledger = match &mut ledger {
                            Some(l) => l,
                            None => ledger.insert(ledger_rows(journal)),
                        };
                        if let Some(fire) = event_action(&ctx, row, &v, ledger, journal) {
                            fires.push(fire);
                        }
                    }
                }
            }
            // Unknown events fail the table load test; this arm is unreachable.
            _ => {}
        }
    }
    write_judge_request(journal, &ctx);
    if max_seq > cursor_seq || cursor_ts.is_none() {
        write_cursor(journal, &ctx.session, max_seq, max_ts);
    }
    fires
}

/// The block action on a Stop row: repeat while the condition holds, capped
/// at [`BLOCK_CAP`] fires per turn key, the last one carrying `cap_reached`.
fn stop_block(
    ctx: &Ctx,
    row: &Row,
    ledger: &mut Vec<(String, String, String)>,
    journal: &Path,
) -> Option<Fire> {
    let prior = ledger
        .iter()
        .filter(|(r, m, k)| r == &row.id && m == &ctx.matched_event && k == "block")
        .count();
    if prior >= BLOCK_CAP {
        return None;
    }
    let mut attrs = Map::new();
    if prior + 1 >= BLOCK_CAP {
        attrs.insert("cap_reached".to_string(), json!(true));
    }
    attrs.insert(
        "excerpt".to_string(),
        json!(crate::fnod::tail(&ctx.last_message, EXCERPT_CHARS)),
    );
    // fnod scores the same message beside the keyword verdict. Advice only:
    // the keyword answer already fired this block, and the span carries both
    // so `intel --lens rules` can compare them.
    if row.when.iter().any(|p| p == "last_message_decision_ask") {
        attrs.insert("keyword_ask".to_string(), json!(true));
        attrs.extend(crate::fnod::is_ask(ctx.cwd, &ctx.last_message));
    }
    write_fire_span(
        journal,
        ctx,
        "block",
        &row.id,
        &ctx.matched_event,
        attrs,
        "none",
        None,
        "chat",
    );
    let mut reason = block_reason(row);
    if row.id == LEAD_IDLE_PICK_RULE {
        if let Some((beat, _)) = idle_beat(ctx) {
            if let Some(line) = beat.data.pick_line.as_deref() {
                reason.push_str(&format!(" Your beat printed: {line}"));
            }
        }
    }
    Some(Fire {
        rule: row.id.clone(),
        action: "block",
        reason,
        notify: None,
    })
}

/// The notify/nudge actions on a journal-event row: once per matched event,
/// deduped on the span ledger (`rule` + `matched_event`).
fn event_action(
    ctx: &Ctx,
    row: &Row,
    event_row: &Value,
    ledger: &mut Vec<(String, String, String)>,
    journal: &Path,
) -> Option<Fire> {
    let data = event_row.get("data")?;
    let trace = data.get("trace")?;
    let event_id = trace.get("span_id").and_then(Value::as_str)?;
    if ledger.iter().any(|(r, m, _)| r == &row.id && m == event_id) {
        return None;
    }
    let trace_id = trace
        .get("trace_id")
        .and_then(Value::as_str)
        .unwrap_or("none");
    let comms = comms_of(trace.get("comms").and_then(Value::as_str).unwrap_or("chat"));
    let reason = block_reason(row);
    match row.action.as_str() {
        "notify" => {
            let actor = trace
                .get("actor_session")
                .and_then(Value::as_str)
                .unwrap_or(&ctx.session);
            let rec = data
                .get("recommendation")
                .and_then(Value::as_str)
                .unwrap_or("none");
            let body = format!(
                "{actor} decided {trace_id} itself: {rec}. Reverse with fno inbox decide --overturns {event_id} --authority operator."
            );
            write_fire_span(
                journal,
                ctx,
                "notify",
                &row.id,
                event_id,
                Map::new(),
                trace_id,
                Some(event_id.to_string()),
                comms,
            );
            Some(Fire {
                rule: row.id.clone(),
                action: "notify",
                reason,
                notify: Some((
                    format!("event rule {}", row.id),
                    body,
                    Some("fno inbox outstanding".to_string()),
                )),
            })
        }
        "nudge" => {
            write_fire_span(
                journal,
                ctx,
                "nudge",
                &row.id,
                event_id,
                Map::new(),
                trace_id,
                Some(event_id.to_string()),
                comms,
            );
            Some(Fire {
                rule: row.id.clone(),
                action: "nudge",
                reason,
                notify: None,
            })
        }
        // `emit` rows only mark their event seen (the cursor is the record);
        // `block` is a Stop-event action. Neither reaches here today.
        _ => None,
    }
}

/// The fixed predicate match. Unknown names never reach the wildcard: the
/// table load refuses them, so the wildcard is a safe default, not a seam.
fn predicate(ctx: &Ctx, name: &str, ev: Option<&Value>) -> bool {
    let (negated, name) = match name.strip_prefix("not:") {
        Some(rest) => (true, rest),
        None => (false, name),
    };
    let value = match name {
        "promoted" => {
            crate::decision_trace::actor_kind_in(ctx.home, Some(&ctx.session), "hook") == "lead"
        }
        // A promoted lead, or any session the user typed into this turn (a
        // mux-witnessed Enter in the turn window, the same witness the
        // conversation hold arms from). A dispatched worker has neither:
        // its queued turn is the dispatch prompt, not the operator talking.
        "lead_or_user_turn" => {
            crate::decision_trace::actor_kind_in(ctx.home, Some(&ctx.session), "hook") == "lead"
                || crate::operator_witness::submitted_since(
                    ctx.journal,
                    &ctx.session,
                    ctx.window_ms,
                    now_ms(),
                )
        }
        "last_message_decision_ask" => decision_ask(&ctx.last_message),
        "turn_filed_question" => turn_filed_question(ctx),
        "route_self" => {
            ev.and_then(|e| e.get("data"))
                .and_then(|d| d.get("route"))
                .and_then(Value::as_str)
                == Some("self")
        }
        "actor_is_session" => ev.is_some_and(|e| actor_is_session(ctx, e)),
        "reversible" => ev
            .and_then(|e| e.get("data"))
            .and_then(|d| d.get("context"))
            .and_then(|c| c.get("reversible"))
            .and_then(Value::as_str)
            .is_some_and(|v| v.eq_ignore_ascii_case("yes")),
        "has_recommendation" => ev
            .and_then(|e| e.get("data"))
            .and_then(|d| d.get("context"))
            .and_then(|c| c.get("recommendation"))
            .is_some_and(|r| r.as_object().is_some_and(|m| !m.is_empty())),
        "why_user_set" => ev
            .and_then(|e| e.get("data"))
            .and_then(|d| d.get("why_user"))
            .and_then(Value::as_str)
            .is_some_and(|v| !v.trim().is_empty()),
        "class_user_only" => ev.is_some_and(|e| class_user_only(ctx, e)),
        "lead_idle_reactive_pick" => idle_beat(ctx).is_some_and(|(row, _)| idle_beat_blocks(&row)),
        "lead_spawn_gate_refused" => idle_beat(ctx).is_some_and(|(row, journals)| {
            let Some(node) = row.data.pick_node.as_deref() else {
                return false;
            };
            let Some(since_ms) = row.ts_ms() else {
                return false;
            };
            gate_refused_since(&journals, node, since_ms)
        }),
        _ => false,
    };
    negated != value
}

/// The rule id whose block reason names the beat's actual pick line.
const LEAD_IDLE_PICK_RULE: &str = "lead_idle_pick_unanswered";

/// The beat row's typed read: the keys the idle-pick rules consume. Every
/// field defaults, so a beat whose data carries another shape reads all
/// None and never blocks (the unmeasured-beat fail-open).
#[derive(Deserialize, Default)]
#[serde(default)]
struct BeatRow {
    ts: Option<String>,
    data: BeatFacts,
}

/// The beat-data keys the block reads on.
#[derive(Deserialize, Default)]
#[serde(default)]
struct BeatFacts {
    scope_live_workers: Option<u64>,
    reactive: Option<bool>,
    pick_offered: Option<String>,
    pick: Option<String>,
    pick_node: Option<String>,
    pick_line: Option<String>,
}

impl BeatRow {
    /// The row's top-level ts as epoch millis, for the refusal comparison.
    fn ts_ms(&self) -> Option<i64> {
        self.ts
            .as_deref()
            .and_then(|t| t.parse::<chrono::DateTime<chrono::Utc>>().ok())
            .map(|t| t.timestamp_millis())
    }
}

/// A refused-spawn row's typed read: the row wraps its facts under `data`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct RefusalRow {
    data: RefusalData,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RefusalData {
    node: Option<String>,
    argv: Option<Vec<String>>,
}

impl RefusalData {
    /// The node match or the argv match: a seat pick names a scope, not a
    /// node, so the promote token in the refused argv is its evidence.
    fn matches(&self, node: &str) -> bool {
        self.node.as_deref() == Some(node)
            || self
                .argv
                .as_deref()
                .is_some_and(|argv| argv.iter().any(|a| a == node))
    }
}

/// The newest `lead_checkin` beat row for the scope this session holds, with
/// the journals it was read from. `None` when the session holds no live team
/// or no beat was journaled: an unmeasured beat never blocks a stop.
fn idle_beat(ctx: &Ctx) -> Option<(BeatRow, Vec<PathBuf>)> {
    let teams = crate::territory::live_teams(&ctx.home.registry_json()).ok()?;
    let scope = teams
        .iter()
        .find(|t| {
            t.holder_session
                .as_deref()
                .is_some_and(|s| crate::identity::canonical_handle(s) == ctx.session)
        })?
        .scope
        .clone();
    let journals = crate::runtime::lead_history::default_journals(ctx.cwd);
    let beat =
        crate::runtime::lead_history::previous_beat(&journals, &scope, Some(&ctx.session), false)
            .ok()??;
    let row: BeatRow = serde_json::from_value(beat).unwrap_or_default();
    Some((row, journals))
}

/// The idle-beat facts the rule blocks on: zero live scope workers, a pick
/// still on offer with no `--pick` answer, and the beat's own `reactive` tag
/// saying the last offer went untaken. Every key must read: a beat whose
/// worker reading failed carries no count and never blocks.
fn idle_beat_blocks(row: &BeatRow) -> bool {
    let d = &row.data;
    d.scope_live_workers == Some(0)
        && d.reactive == Some(true)
        && d.pick_offered
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
        && d.pick
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
}

/// A spawn the gate refused for `node` after `since_ms`, in any journal the
/// beat reads. `agent_spawn_refused` is the one feed-visible refusal row that
/// names the node (`spawn_gate.rs` emits it at the single refusal site). The
/// row's argv matches too: a seat pick names a scope, not a node, so the
/// promote token in the refused argv is the seat-pick's evidence.
fn gate_refused_since(journals: &[PathBuf], node: &str, since_ms: i64) -> bool {
    let query = crate::event_store::EventQuery::of_types(&["agent_spawn_refused"]);
    journals.iter().any(|j| {
        crate::event_store::query_events(j, &query)
            .unwrap_or_default()
            .iter()
            .any(|r| {
                r.ts_ms > since_ms
                    && serde_json::from_str::<RefusalRow>(&r.line)
                        .unwrap_or_default()
                        .data
                        .matches(node)
            })
    })
}

/// A worker ask addressed to its lead (the `Approval:` template), or an ask
/// sentence with its options: the numbered list right under the ask line, or
/// the options inline on the ask's own line. An ask word in one paragraph and
/// a status list in another is a report, not an ask.
fn decision_ask(text: &str) -> bool {
    if text
        .lines()
        .any(|l| l.trim_start().starts_with("Approval:"))
    {
        return true;
    }
    let lines: Vec<&str> = text.lines().collect();
    lines.iter().enumerate().any(|(i, line)| {
        if crate::repeated_asks::asks_in(line).is_empty() {
            return false;
        }
        if numbered_options(line) >= 2 {
            return true;
        }
        let blank = |l: &&&str| l.trim().is_empty();
        let item = |l: &&&str| is_numbered_item(l);
        !is_numbered_item(line)
            && (lines[i + 1..]
                .iter()
                .skip_while(blank)
                .take_while(item)
                .count()
                >= 2
                // "Options: 1. x 2. y" then "Which do you want?" is an ask too.
                || (line.trim_end().ends_with('?')
                    && lines[..i]
                        .iter()
                        .rev()
                        .skip_while(blank)
                        .take_while(item)
                        .count()
                        >= 2)
                // A closing either-or question carries its options in the
                // ask line itself: "Should I merge now or wait for codex?"
                // has no numbered list to lean on.
                || (line.trim_end().ends_with('?') && either_or(line)))
    })
}

/// ` or ` between alternatives in the ask line itself. The marker is the
/// spaced word, so "for", "work" and "order" never match; a question that
/// names no second alternative stays out (a report may end in a question).
fn either_or(line: &str) -> bool {
    line.to_lowercase().contains(" or ")
}

fn is_numbered_item(line: &str) -> bool {
    let line = line.trim_start();
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0
        && line[digits..]
            .strip_prefix(['.', ')'])
            .is_some_and(|rest| rest.starts_with(char::is_whitespace))
}

fn numbered_options(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i < bytes.len() {
        let at_start = i == 0 || bytes[i - 1].is_ascii_whitespace();
        if at_start && bytes[i].is_ascii_digit() {
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j < bytes.len()
                && (bytes[j] == b'.' || bytes[j] == b')')
                && bytes.get(j + 1).is_some_and(u8::is_ascii_whitespace)
            {
                count += 1;
                i = j;
            }
        }
        i += 1;
    }
    count
}

/// The journal already holds something this session filed or routed since
/// the turn began: an `operator_question` it asked, or a `decision_span`
/// route span it acted on. That is the ask the chat message points at. A
/// message that names a question still open on the board points at a filed
/// ask too, whoever filed it: a successor lead relays its predecessor's.
fn turn_filed_question(ctx: &Ctx) -> bool {
    names_open_question(ctx) || session_filed_this_turn(ctx)
}

/// The `q-xxxxxxxx` hash ids the text names.
fn hash_qids(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    for (i, _) in text.match_indices("q-") {
        let boundary = |b: Option<&u8>| !b.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'-');
        let id = text.get(i..i + 10).unwrap_or_default();
        if id.len() == 10
            && id[2..].bytes().all(|b| b.is_ascii_hexdigit())
            && boundary(i.checked_sub(1).and_then(|j| bytes.get(j)))
            && boundary(bytes.get(i + 10))
        {
            out.push(id);
        }
    }
    out
}

/// The `Q123`/`D45` short labels the text names, with the same boundary rule
/// as [`hash_qids`]. Printers show the label beside the hash, so a reply
/// points at a filed question through either shape; a `@store` tail stops
/// the scan and the bare label resolves in the current store.
fn short_labels(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    for (i, b) in bytes.iter().enumerate() {
        if !matches!(b, b'Q' | b'q' | b'D' | b'd') {
            continue;
        }
        let boundary = |j: Option<&u8>| !j.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'-');
        if !boundary(i.checked_sub(1).and_then(|j| bytes.get(j))) {
            continue;
        }
        let digits = bytes
            .get(i + 1..)
            .map(|rest| rest.iter().take_while(|b: &&u8| b.is_ascii_digit()).count())
            .unwrap_or(0);
        let labelled = (1..=7).contains(&digits)
            && bytes
                .get(i + 1 + digits)
                .is_none_or(|b| !b.is_ascii_alphanumeric() && *b != b'-');
        if labelled {
            if let Some(label) = text.get(i..i + 1 + digits) {
                out.push(label.to_string());
            }
        }
    }
    out
}

/// The question ids `text` names, as the board keys them: `q-xxxxxxxx`
/// hashes plus short labels resolved over this machine's number table.
/// A label nothing holds resolves to nothing and names nothing.
fn named_qids(text: &str) -> Vec<String> {
    let mut out: Vec<String> = hash_qids(text).into_iter().map(str::to_string).collect();
    for token in short_labels(text) {
        if let Some(key) = crate::numbers::resolve('q', &token) {
            // A reply can name the hash and its label; one board id, one row.
            if !out.contains(&key) {
                out.push(key);
            }
        }
    }
    out
}

fn names_open_question(ctx: &Ctx) -> bool {
    !open_questions_named(&ctx.last_message, ctx.home, ctx.journal, ctx.cwd, None).is_empty()
}

/// The questions `text` names that are still open on the board, keyed by
/// their `q-xxxxxxxx` hash (a short label resolves to its hash first).
/// `asked_by` narrows the answer to questions that session itself asked:
/// its canonical handle must equal the row's `asker` or `session_id`. A
/// needs-input binding is a session-id fact, never a mention match - a turn
/// that merely NAMES another session's open question is not waiting on the
/// user over it. `None` keeps every asker (the relay posture).
pub(crate) fn open_questions_named(
    text: &str,
    home: &crate::paths::AgentsHome,
    journal: &Path,
    cwd: &Path,
    asked_by: Option<&str>,
) -> Vec<String> {
    let qids = named_qids(text);
    if qids.is_empty() {
        return Vec::new();
    }
    let fno_dir = home
        .root()
        .parent()
        .map_or_else(|| std::path::PathBuf::from(".fno"), Path::to_path_buf);
    let mut journals = vec![journal.to_path_buf()];
    // Resolving the space dir can panic with no hermetic state root; the
    // Stop path fails open to the journals it can read.
    if let Ok(family) = std::panic::catch_unwind(|| crate::needs::question_journals(&fno_dir, cwd))
    {
        journals.extend(family);
    }
    // The rows stay in the fold (not just their ids) so the asker binding
    // can read them; a question can land in any one journal of the family.
    let mut asked = serde_json::Map::new();
    let mut closed = std::collections::HashSet::new();
    for journal in &journals {
        let (a, c) = crate::question_intake::question_rows(journal);
        asked.extend(a);
        closed.extend(c);
    }
    qids.iter()
        .filter(|q| {
            asked.contains_key(*q)
                && !closed.contains(*q)
                && asked_by
                    .is_none_or(|sid| asked.get(*q).is_some_and(|row| row_asked_by(row, sid)))
        })
        .cloned()
        .collect()
}

/// One JSON field read, keyed through a variable so the string-key read
/// ratchet (docs/rust-style.md R1) counts this helper's single read, never
/// each call site.
fn row_field<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key)
}

/// The question row's field: nested under `data` first, then top level,
/// the same order `question_intake`'s own field reader uses.
fn row_data_field<'a>(row: &'a Value, key: &str) -> Option<&'a Value> {
    row_field(row, "data")
        .and_then(|d| row_field(d, key))
        .or_else(|| row_field(row, key))
}

/// True when a question row names `session_id` as its asker: the row's
/// canonical `asker` handle or `session_id` resolves to the session's own
/// short id. A row that names no asker binds to nobody. Both field shapes
/// read: nested under `data` (the live envelope) or top level, the same
/// order `question_intake`'s own field reader uses.
fn row_asked_by(row: &Value, session_id: &str) -> bool {
    let own = crate::identity::canonical_handle(session_id);
    let asker = row_data_field(row, "asker");
    let session = row_data_field(row, "session_id");
    [asker, session]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|h| crate::identity::canonical_handle(h) == own)
}

/// [`open_questions_named`] for a Stop payload's last assistant message,
/// read from the process environment the way [`eval_stop`] reads it.
pub(crate) fn stop_open_questions(
    cwd: &Path,
    last_message: &str,
    asked_by: Option<&str>,
) -> Vec<String> {
    let journal = crate::law_match::project_events_journal();
    let home = crate::paths::AgentsHome::from_env();
    open_questions_named(last_message, &home, &journal, cwd, asked_by)
}

fn session_filed_this_turn(ctx: &Ctx) -> bool {
    let query = crate::event_store::EventQuery {
        since_ms: Some(ctx.window_ms),
        ..crate::event_store::EventQuery::of_types(&["operator_question", "decision_span"])
    };
    let Ok(rows) = crate::event_store::query_events(ctx.journal, &query) else {
        return false;
    };
    rows.iter().any(|row| {
        let Ok(v) = serde_json::from_str::<Value>(&row.line) else {
            return false;
        };
        match row.r#type.as_str() {
            "operator_question" => actor_is_session(ctx, &v),
            _ => span_kind(&v) == Some("route") && actor_is_session(ctx, &v),
        }
    })
}

/// The row is this session's doing: its `asker` or its trace actor resolves
/// to the same canonical handle.
fn actor_is_session(ctx: &Ctx, row: &Value) -> bool {
    let data = row.get("data");
    let asker = data.and_then(|d| d.get("asker")).and_then(Value::as_str);
    let actor = data
        .and_then(|d| d.get("trace"))
        .and_then(|t| t.get("actor_session"))
        .and_then(Value::as_str);
    [asker, actor]
        .into_iter()
        .flatten()
        .any(|h| crate::identity::canonical_handle(h) == ctx.session)
}

/// The question's escalate route span declared one of the four user classes.
/// No parent span, or a parent whose class is absent, reads as not
/// user-only: the question never claimed a class, so the escape check fires.
fn class_user_only(ctx: &Ctx, row: &Value) -> bool {
    let parent = row
        .get("data")
        .and_then(|d| d.get("trace"))
        .and_then(|t| t.get("parent_span_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let Some(parent) = parent else {
        return false;
    };
    let Ok(spans) = crate::event_store::query_events(
        ctx.journal,
        &crate::event_store::EventQuery::of_types(&["decision_span"]),
    ) else {
        return false;
    };
    for s in spans {
        let Ok(v) = serde_json::from_str::<Value>(&s.line) else {
            continue;
        };
        let span_id = v
            .get("data")
            .and_then(|d| d.get("trace"))
            .and_then(|t| t.get("span_id"))
            .and_then(Value::as_str);
        if span_id != Some(parent.as_str()) {
            continue;
        }
        let class = v
            .get("data")
            .and_then(|d| d.get("class"))
            .and_then(Value::as_str)
            .unwrap_or("none");
        return crate::question_ladder::rules_for(ctx.cwd)
            .unwrap_or_default()
            .user_only(class);
    }
    false
}

fn span_kind(row: &Value) -> Option<&str> {
    row.get("data")
        .and_then(|d| d.get("span_kind"))
        .and_then(Value::as_str)
}

fn comms_of(comms: &str) -> &'static str {
    match comms {
        "mail" => "mail",
        "question" => "question",
        _ => "chat",
    }
}

fn block_reason(row: &Row) -> String {
    format!(
        "rule {}: {}. Why: {}. Do instead: {}.",
        row.id, row.what, row.why, row.instead
    )
}

/// Every span already attributed to a rule: (rule, matched_event, span_kind).
/// The ledger the actions dedupe and cap on - a journal read, not a second
/// store.
fn ledger_rows(journal: &Path) -> Vec<(String, String, String)> {
    let Ok(spans) = crate::event_store::query_events(
        journal,
        &crate::event_store::EventQuery::of_types(&["decision_span"]),
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for s in spans {
        let Ok(v) = serde_json::from_str::<Value>(&s.line) else {
            continue;
        };
        let data = v.get("data");
        let rule = data.and_then(|d| d.get("rule")).and_then(Value::as_str);
        let matched = data
            .and_then(|d| d.get("matched_event"))
            .and_then(Value::as_str);
        let kind = span_kind(&v).unwrap_or_default();
        if let (Some(rule), Some(matched)) = (rule, matched) {
            out.push((rule.to_string(), matched.to_string(), kind.to_string()));
        }
    }
    out
}

/// One fire's journal row. The span IS the ledger entry: the next run reads
/// it back instead of keeping state anywhere else.
fn write_fire_span(
    journal: &Path,
    ctx: &Ctx,
    kind: &str,
    rule: &str,
    matched_event: &str,
    mut attrs: Map<String, Value>,
    trace_id: &str,
    parent_span_id: Option<String>,
    comms: &'static str,
) {
    attrs.insert("rule".to_string(), json!(rule));
    attrs.insert("matched_event".to_string(), json!(matched_event));
    let trace = Trace {
        trace_id: trace_id.to_string(),
        span_id: crate::decision_trace::new_span_id(),
        parent_span_id,
        actor_session: Some(ctx.session.clone()),
        actor_kind: crate::decision_trace::actor_kind_in(ctx.home, Some(&ctx.session), "hook"),
        comms,
        recipient_session: None,
        recipient_kind: None,
    };
    if let Err(e) = crate::decision_trace::emit_span_to(journal, kind, &trace, &attrs) {
        eprintln!("event_rules: {rule} span skipped: {e}");
    }
}

// ── the shadow judge's request row ───────────────────────────────────────────

/// How many behavioural laws one request row keeps. The cap keeps one pass
/// per Stop small; `judge_dropped` counts the rest.
const JUDGE_RULE_CAP: usize = 3;
/// The reply tail a request row keeps for the judge, which reads it later.
const JUDGE_TAIL_CHARS: usize = 4_000;
/// Past this size the judge file rotates and keeps one older copy, so a row
/// per Stop never grows without end.
const JUDGE_ROTATE_BYTES: u64 = 64 * 1024 * 1024;

/// One row of the judge file. A request is written at each Stop. The
/// scorer appends a verdict for each question it answers about a request.
#[derive(Debug, Deserialize)]
pub struct JudgeRow {
    pub ts: String,
    /// `request` or `verdict`.
    pub phase: String,
    pub span_id: String,
    #[serde(default)]
    pub session: String,
    /// The Stop turn's key, `stop:<session>:<turn>`.
    pub matched_event: String,
    #[serde(default)]
    pub machine: String,
    /// `off`, `refused`, `down` or `up`, on a request.
    #[serde(default)]
    pub fnod: Option<String>,
    /// The behavioural laws a request keeps, at most [`JUDGE_RULE_CAP`].
    #[serde(default)]
    pub rules: Vec<String>,
    #[serde(default)]
    pub judge_dropped: usize,
    #[serde(default)]
    pub judge_state: usize,
    /// The reply tail, on a request written while fnod listens.
    #[serde(default)]
    pub excerpt: Option<String>,
    /// The request a verdict answers, and the label it judged under.
    #[serde(default)]
    pub judge_of: Option<String>,
    #[serde(default)]
    pub judge: Option<String>,
    #[serde(default)]
    pub verdict: Option<String>,
    #[serde(default)]
    pub escalate: Option<String>,
    #[serde(default)]
    pub trained: Option<bool>,
    #[serde(default)]
    pub fnod_error: Option<String>,
    #[serde(default)]
    pub fnod_ms: Option<f64>,
    #[serde(default)]
    pub fnod_is_ask: Option<f64>,
}

/// The shadow judge's request row, one per Stop: the behavioural laws this
/// turn's prompt picked, how many state laws it picked, whether fnod
/// listens here, and the reply tail when it does. The scorer
/// (`fno-agent intel --lens rules --score`) reads these rows off the Stop
/// path, so Stop itself asks fnod nothing. Best-effort: a lost row costs a
/// verdict and nothing else.
fn write_judge_request(journal: &Path, ctx: &Ctx) {
    // Read on the first picked law only: a Stop that picked none pays no
    // config read.
    let mut tags: Option<crate::fnod_tags::Tags> = None;
    let mut rules: Vec<String> = Vec::new();
    let (mut state, mut dropped) = (0usize, 0usize);
    for law in picked_laws(journal, ctx) {
        let tags = tags.get_or_insert_with(|| crate::fnod_tags::Tags::read(ctx.cwd));
        match tags.get(&law) {
            crate::fnod_tags::Tag::Behavioural => {
                if rules.len() < JUDGE_RULE_CAP {
                    rules.push(law);
                } else {
                    dropped += 1;
                }
            }
            crate::fnod_tags::Tag::State => state += 1,
            crate::fnod_tags::Tag::Untagged => {}
        }
    }
    // The shadow judge takes no default url. Until config names fnod, the
    // row reads off and this path opens no connection.
    let url = crate::fnod::named_url(ctx.cwd).unwrap_or_default();
    let fnod = if url.trim().is_empty() {
        "off"
    } else if !crate::fnod::loopback(&url) {
        "refused"
    } else if crate::fnod::listening(&url) {
        "up"
    } else {
        "down"
    };
    let tail =
        (fnod == "up").then(|| crate::fnod::tail(&ctx.last_message, JUDGE_TAIL_CHARS).to_string());
    let row = json!({
        "ts": crate::events::now_rfc3339(),
        "phase": "request",
        "span_id": crate::decision_trace::new_span_id(),
        "session": ctx.session,
        "matched_event": ctx.matched_event,
        "machine": crate::claims::hostname(),
        "fnod": fnod,
        "rules": rules,
        "judge_dropped": dropped,
        "judge_state": state,
        "excerpt": tail,
    });
    append_judge_rows(journal, std::slice::from_ref(&row));
}

/// A rule fire span as `rules_source` writes it: the fields the pick reads.
#[derive(Deserialize)]
struct FireSpan {
    data: FireData,
}

#[derive(Deserialize)]
struct FireData {
    span_kind: String,
    event: String,
    law: Option<String>,
    #[serde(default)]
    trace: FireTrace,
}

#[derive(Default, Deserialize)]
struct FireTrace {
    actor_session: Option<String>,
}

/// The laws the prompt and skill hooks picked for this turn: this
/// session's `rule` fire spans since the turn began, each law once. Without
/// a readable transcript the turn window is 30 minutes, so an older turn's
/// picks can join.
fn picked_laws(journal: &Path, ctx: &Ctx) -> Vec<String> {
    let query = crate::event_store::EventQuery {
        since_ms: Some(ctx.window_ms - 2_000),
        ..crate::event_store::EventQuery::of_types(&["decision_span"])
    };
    let mut laws: Vec<String> = Vec::new();
    for row in crate::event_store::query_events(journal, &query).unwrap_or_default() {
        let Ok(fire) = serde_json::from_str::<FireSpan>(&row.line) else {
            continue;
        };
        let fire = fire.data;
        if fire.span_kind != "rule"
            || !matches!(fire.event.as_str(), "prompt" | "skill")
            || fire.trace.actor_session.as_deref() != Some(ctx.session_id.as_str())
        {
            continue;
        }
        if let Some(law) = fire.law {
            if !laws.iter().any(|held| held == &law) {
                laws.push(law);
            }
        }
    }
    laws
}

/// The engine's own state folder beside the journal: the per-session
/// cursors and the judge rows. Beside a state root (it holds `db/`) the
/// layout table places the folder, so it never lands at the root's top.
fn state_dir(journal: &Path) -> PathBuf {
    let parent = journal
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    if parent.join("db").is_dir() {
        return crate::state_layout::place(&parent, "event-rules");
    }
    parent.join("event-rules")
}

/// The judge rows, oldest first: the rotated copy, then the live file. A
/// damaged line is skipped.
pub fn judge_rows(journal: &Path) -> Vec<JudgeRow> {
    let live = state_dir(journal).join("judge.jsonl");
    [live.with_extension("jsonl.1"), live]
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .flat_map(|text| text.lines().map(str::to_string).collect::<Vec<_>>())
        .filter_map(|line| serde_json::from_str::<JudgeRow>(&line).ok())
        .collect()
}

/// Append judge rows, oldest last. Past the size bound the live file
/// rotates first and keeps one older copy.
pub fn append_judge_rows(journal: &Path, rows: &[Value]) {
    let live = state_dir(journal).join("judge.jsonl");
    if let Some(dir) = live.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    if std::fs::metadata(&live).is_ok_and(|m| m.len() > JUDGE_ROTATE_BYTES) {
        std::fs::rename(&live, live.with_extension("jsonl.1")).ok();
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&live)
    else {
        return;
    };
    for row in rows {
        let mut line = row.to_string();
        line.push('\n');
        if let Err(e) = std::io::Write::write_all(&mut file, line.as_bytes()) {
            eprintln!("event_rules: judge row skipped: {e}");
        }
    }
}

// ── the per-session cursor ──────────────────────────────────────────────────

/// Where this session's engine left off: the seq and ts of the newest
/// journal row it processed. Session-keyed files in a subfolder, per the
/// state-root inventory; a missing or malformed cursor reads as a first
/// fire and the engine starts 30 minutes back.
fn cursor_path(journal: &Path, session: &str) -> PathBuf {
    let dir = state_dir(journal);
    let key = if session.is_empty() {
        "anon".to_string()
    } else {
        session.to_string()
    };
    dir.join(format!("{key}.cursor"))
}

fn read_cursor(journal: &Path, session: &str) -> (i64, Option<i64>) {
    let Ok(text) = std::fs::read_to_string(cursor_path(journal, session)) else {
        return (0, None);
    };
    let mut parts = text.split_whitespace();
    let seq = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (seq, parts.next().and_then(|s| s.parse().ok()))
}

fn write_cursor(journal: &Path, session: &str, seq: i64, ts_ms: i64) {
    let path = cursor_path(journal, session);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    std::fs::write(path, format!("{seq} {ts_ms}\n")).ok();
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The last user turn's timestamp in a harness transcript, when the file
/// carries one. ponytail: reads the whole transcript each fire, as
/// repeated_asks does; seek to a tail window if the read ever shows.
fn transcript_turn_window(path: &str) -> Option<i64> {
    if path.is_empty() {
        return None;
    }
    let raw = std::fs::read_to_string(path).ok()?;
    for line in raw.lines().rev() {
        if !line.contains("user") {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let is_user = row.get("type").and_then(Value::as_str) == Some("user")
            || crate::provenance::is_user_turn(&row);
        if !is_user {
            continue;
        }
        if let Some(ts) =
            find_timestamp(&row).and_then(|s| crate::event_store::parse_rfc3339_ms(&s))
        {
            return Some(ts);
        }
    }
    None
}

fn find_timestamp(v: &Value) -> Option<String> {
    match v {
        Value::Object(m) => {
            if let Some(s) = m.get("timestamp").and_then(Value::as_str) {
                return Some(s.to_string());
            }
            m.values().find_map(find_timestamp)
        }
        _ => None,
    }
}

/// `fno-agent hook rules --event stop` - the transport entry adapters call
/// beside their own loop-check when their Stop cannot carry the rules
/// (opencode, pi, agy). The Stop payload rides stdin; a block or nudge
/// prints the same JSON the stop gate prints and the adapter relays the
/// reason to its session.
pub fn run_hook(args: &[String]) -> i32 {
    let mut event = String::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--event" => event = it.next().cloned().unwrap_or_default(),
            other => {
                eprintln!("fno-agent hook rules: unknown argument {other}");
                return 2;
            }
        }
    }
    match event.as_str() {
        "stop" => {}
        "prompt" | "skill" | "pre-tool" => return crate::hook::rules::run(&event),
        _ => {
            eprintln!("fno-agent hook rules: --event takes stop, prompt, skill or pre-tool");
            return 2;
        }
    }
    let payload = crate::hook::read_stdin();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd = std::fs::canonicalize(&cwd).unwrap_or(cwd);
    let fires = eval_stop(&cwd, &payload);
    for fire in &fires {
        if let Some((title, body, pointer)) = &fire.notify {
            crate::operator_notice::notify_operator(title, body, pointer.as_deref());
        }
    }
    if let Some(fire) = fires
        .iter()
        .find(|f| f.action == "block" || f.action == "nudge")
    {
        println!(
            "{}",
            serde_json::json!({"decision": "block", "reason": fire.reason})
        );
        return 0;
    }
    println!("{{}}");
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SESSION: &str = "ses_kd63be01";

    /// One engine test's world: a temp agents home (its registry roles the
    /// test session when asked), a temp journal, and the canonical handle.
    struct Rig {
        _dir: tempfile::TempDir,
        home: crate::paths::AgentsHome,
        journal: PathBuf,
        cwd: PathBuf,
        session: String,
    }

    impl Rig {
        fn new(promoted: bool) -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let home_dir = dir.path().join("agents-home");
            let cwd = dir.path().join("project");
            std::fs::create_dir_all(&home_dir).unwrap();
            std::fs::create_dir_all(&cwd).unwrap();
            std::env::set_var("FNO_AGENT_HOME", &home_dir);
            let session = crate::identity::canonical_handle(SESSION);
            if promoted {
                let registry = json!({
                    "schema_version": crate::state::REGISTRY_SCHEMA_VERSION,
                    "agents": [{
                        "name": "lead-rig",
                        "status": "live",
                        "role_scope": "e-63be",
                        "role_level": 2,
                        "created_at": "2026-10-05T00:00:00Z",
                        "cwd": home_dir.display().to_string(),
                        "harness_session_id": session,
                    }],
                });
                std::fs::write(
                    home_dir.join("registry.json"),
                    serde_json::to_string(&registry).unwrap(),
                )
                .unwrap();
            }
            Rig {
                _dir: dir,
                home: crate::paths::AgentsHome::from_env(),
                // The idle-pick predicates read default_journals(cwd), whose
                // agents-home entry resolves here: one file serves the engine
                // read and the predicate read.
                journal: home_dir.join("events.jsonl"),
                cwd,
                session,
            }
        }

        fn payload(&self, last: &str) -> String {
            json!({
                "session_id": SESSION,
                "last_assistant_message": last,
                "transcript_path": "",
                "cwd": self.cwd.display().to_string(),
            })
            .to_string()
        }

        fn seed(&self, envelope: Value) {
            crate::event_store::append_envelope(&self.journal, &envelope.to_string(), None)
                .unwrap();
        }

        /// A route span this session wrote, as mail-record and the intake
        /// write them.
        fn route_span(&self, span_id: &str, route: &str, class: Option<&str>) {
            let mut data = json!({
                "span_kind": "route",
                "route": route,
                "trace": {
                    "trace_id": "x-node1",
                    "span_id": span_id,
                    "actor_session": self.session,
                    "actor_kind": "worker",
                    "comms": "mail",
                },
            });
            if let Some(cls) = class {
                data["class"] = json!(cls);
            }
            self.seed(json!({
                "ts": crate::events::now_rfc3339(),
                "type": "decision_span",
                "source": "test",
                "data": data,
            }));
        }

        /// An operator_question this session asked, optional escalate route
        /// parent carrying `class`.
        fn question(&self, span_id: &str, parent: Option<(&str, &str)>) {
            let mut data = json!({
                "question_id": span_id,
                "question": "which lane?",
                "asker": self.session,
                "why_user": "the operator asked for the pick",
                "context": {
                    "reversible": "yes",
                    "recommendation": {"option": 1, "why": "it reverses cleanly"},
                },
                "trace": {
                    "trace_id": "x-node1",
                    "span_id": span_id,
                    "actor_session": self.session,
                    "actor_kind": "worker",
                    "comms": "question",
                },
            });
            if let Some((route_id, class)) = parent {
                self.route_span(route_id, "escalate", Some(class));
                data["trace"]["parent_span_id"] = json!(route_id);
            }
            self.seed(json!({
                "ts": crate::events::now_rfc3339(),
                "type": "operator_question",
                "source": "test",
                "data": data,
            }));
        }

        /// The `operator_submit` row the mux writes when it witnesses the
        /// user's Enter in the session pane. `session` is the payload's
        /// session id string; the store matches it through the canonical
        /// handle, so the seed canonicalizes the same way.
        fn operator_submit(&self, session: &str, submit_ms: i64) {
            self.seed(json!({
                "ts": crate::events::now_rfc3339(),
                "type": "operator_submit",
                "source": "daemon",
                "data": {
                    "mux_session": "main", "pane": 7, "via": "pane",
                    "submit_ms": submit_ms,
                    "resolution": "ok",
                    "harness_session": crate::identity::canonical_handle(session),
                }
            }));
        }

        fn spans(&self) -> Vec<Value> {
            crate::event_store::query_events(
                &self.journal,
                &crate::event_store::EventQuery::of_types(&["decision_span"]),
            )
            .unwrap()
            .iter()
            .map(|r| serde_json::from_str::<Value>(&r.line).unwrap())
            .collect()
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            std::env::remove_var("FNO_AGENT_HOME");
        }
    }

    /// The shadow judge's request row: the turn's prompt pick split by tag,
    /// fnod's state on this machine, and the reply tail only when fnod
    /// listens. The Stop answer does not change.
    fn judge_request_leg() {
        let rig = Rig::new(false);
        std::fs::create_dir_all(rig.cwd.join(".fno")).unwrap();
        let fire = |law: &str, event: &str, session: &str| {
            json!({"ts": crate::events::now_rfc3339(), "type": "decision_span", "source": "test",
                "data": {"span_kind": "rule", "rule": format!("law-{law}"), "law": law, "event": event,
                    "trace": {"trace_id": "none", "span_id": crate::decision_trace::new_span_id(),
                        "actor_session": session}}})
        };
        rig.seed(fire("d-behav001", "prompt", SESSION));
        rig.seed(fire("d-behav001", "skill", SESSION));
        rig.seed(fire("d-state001", "prompt", SESSION));
        // Another session's pick, and a pre-tool fire, are not this turn's.
        rig.seed(fire("d-theirs1", "prompt", "ses_other111"));
        rig.seed(fire("d-pretool1", "pre-tool", SESSION));
        let file = rig.cwd.join("judge-tags.toml");
        std::fs::write(
            &file,
            "d-behav001 = \"behavioural\"\nd-state001 = \"state\"\n",
        )
        .unwrap();
        let tags = format!("[fnod.judge]\ntags_file = \"{}\"\n", file.display());

        // fnod listens: the row keeps the tail and the behavioural law once.
        let (url, _listener) = crate::fnod::tests::serve_once("{}", std::time::Duration::ZERO);
        std::fs::write(
            rig.cwd.join(".fno/config.toml"),
            format!("[fnod]\nurl = \"{url}\"\n{tags}"),
        )
        .unwrap();
        let fires = eval_stop_in(
            &rig.cwd,
            &rig.journal,
            &rig.home,
            &rig.payload("wave 1 merged clean"),
        );
        assert!(fires.is_empty(), "the judge changes no Stop answer");
        let [row] = <[JudgeRow; 1]>::try_from(judge_rows(&rig.journal)).unwrap();
        assert_eq!(
            (row.phase.as_str(), row.fnod.as_deref()),
            ("request", Some("up"))
        );
        assert_eq!(row.rules, vec!["d-behav001"]);
        assert_eq!((row.judge_state, row.judge_dropped), (1, 0));
        assert_eq!(row.excerpt.as_deref(), Some("wave 1 merged clean"));
        assert!(row
            .matched_event
            .starts_with(&format!("stop:{}:", rig.session)));

        // fnod is absent: the row says down and keeps no tail.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        std::fs::write(
            rig.cwd.join(".fno/config.toml"),
            format!("[fnod]\nurl = \"{url}\"\n{tags}"),
        )
        .unwrap();
        eval_stop_in(
            &rig.cwd,
            &rig.journal,
            &rig.home,
            &rig.payload("Still working."),
        );
        let rows = judge_rows(&rig.journal);
        let [_, row] = <[JudgeRow; 2]>::try_from(rows).unwrap();
        assert_eq!(row.fnod.as_deref(), Some("down"));
        assert!(row.excerpt.is_none(), "{row:?}");
        assert_eq!(row.rules, vec!["d-behav001"]);

        // fnod is not named: the row says off and keeps no tail.
        std::fs::write(rig.cwd.join(".fno/config.toml"), &tags).unwrap();
        eval_stop_in(
            &rig.cwd,
            &rig.journal,
            &rig.home,
            &rig.payload("Still working."),
        );
        let [_, _, row] = <[JudgeRow; 3]>::try_from(judge_rows(&rig.journal)).unwrap();
        assert_eq!(row.fnod.as_deref(), Some("off"));
        assert!(row.excerpt.is_none(), "{row:?}");
    }

    fn stop_stamp_binds_the_question_to_the_asking_session_leg() {
        // The caller holds test_env_lock: the lock is a plain std Mutex, so
        // a second acquire here deadlocks the test process (the 30m CI cap
        // hung on exactly this). It runs only from r1_blocks, which locks.
        let rig = Rig::new(false);
        // One question this session asked; one another session asked; one
        // stamped by nobody. The turn's summary names all three.
        rig.question("q-aaaabbbb", None);
        rig.seed(json!({
            "ts": crate::events::now_rfc3339(),
            "type": "operator_question",
            "source": "test",
            "data": json!({
                "question_id": "q-ccccdddd",
                "question": "another worker's ask",
                "asker": "zzzz9999",
            }),
        }));
        rig.seed(json!({
            "ts": crate::events::now_rfc3339(),
            "type": "operator_question",
            "source": "test",
            "data": json!({
                "question_id": "q-00001111",
                "question": "an unattributed ask",
            }),
        }));
        let text = "q-aaaabbbb q-ccccdddd q-00001111 all stay open";
        // No filter: the relay posture keeps every asker.
        let any = open_questions_named(text, &rig.home, &rig.journal, &rig.cwd, None);
        assert_eq!(any.len(), 3, "{any:?}");
        // The stamp path: only the questions THIS session asked bind.
        let mine = open_questions_named(
            text,
            &rig.home,
            &rig.journal,
            &rig.cwd,
            Some("ses_kd63be01"),
        );
        assert_eq!(mine, vec!["q-aaaabbbb".to_string()], "{mine:?}");
        // The other session's full id binds its own row through the same
        // canonical-handle comparison.
        let theirs = open_questions_named(
            text,
            &rig.home,
            &rig.journal,
            &rig.cwd,
            Some("zzzz9999-full-uuid"),
        );
        assert_eq!(theirs, vec!["q-ccccdddd".to_string()], "{theirs:?}");
    }

    #[test]
    fn r1_blocks_unfiled_chat_asks_allows_filed_and_caps() {
        let lock = crate::claims::test_env_lock();
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        let rig = Rig::new(true);
        let ask = "1. merge 2. hold. Your call?";
        let (url, fnod) = crate::fnod::tests::serve_once(
            r#"{"answers":{"is_ask":{"type":"noul","noul":0.12}}}"#,
            std::time::Duration::ZERO,
        );
        std::fs::create_dir_all(rig.cwd.join(".fno")).unwrap();
        std::fs::write(
            rig.cwd.join(".fno/config.toml"),
            format!("[fnod]\nurl = \"{url}\"\ntimeout_ms = 2000\n"),
        )
        .unwrap();

        // AC3: the block names the rule and the ask command, and the journal
        // holds one block span. The span carries the keyword verdict and
        // fnod's score side by side, and fnod's low score changes nothing.
        let fires = eval_stop_in(&rig.cwd, &rig.journal, &rig.home, &rig.payload(ask));
        let block = fires
            .iter()
            .find(|f| f.rule == "chat_ask_unfiled")
            .expect("the chat ask blocks Stop");
        assert!(block.reason.contains("rule chat_ask_unfiled"));
        assert!(block.reason.contains("fno inbox outstanding ask"));
        let spans = rig.spans();
        assert_eq!(spans.len(), 1, "one block span for one fire");
        assert_eq!(spans[0]["data"]["span_kind"], "block");
        assert_eq!(spans[0]["data"]["excerpt"], ask);
        assert_eq!(spans[0]["data"]["rule"], "chat_ask_unfiled");
        assert_eq!(spans[0]["data"]["keyword_ask"], true);
        assert_eq!(spans[0]["data"]["fnod_is_ask"], 0.12);
        fnod.join().unwrap();

        // AC4: the same ask with a question filed this turn allows.
        rig.question("q-rig01", None);
        let fires = eval_stop_in(&rig.cwd, &rig.journal, &rig.home, &rig.payload(ask));
        assert!(
            !fires.iter().any(|f| f.rule == "chat_ask_unfiled"),
            "a filed question clears the rule"
        );

        // A plain status message never fires the row.
        let fires = eval_stop_in(
            &rig.cwd,
            &rig.journal,
            &rig.home,
            &rig.payload("wave 1 merged clean"),
        );
        assert!(fires.is_empty(), "no ask, no fire");
        r1_unpromoted_and_cap_legs();
        nonlead_user_turn_blocks_on_either_or_ask_leg();
        dispatched_worker_without_user_turn_still_passes_leg();
        every_harness_stop_payload_shape_reaches_the_rule_leg();
        r2_and_r3_legs();
        stop_stamp_binds_the_question_to_the_asking_session_leg();
        judge_request_leg();
        idle_pick_legs();
    }

    #[test]
    fn status_reports_are_not_decision_asks() {
        // 2026-10-12: the ASK marker "confirm" substring-matched "confirmed",
        // so a numbered status report blocked Stop as an unfiled ask. The
        // confirm asks that remain are the pointed ones.
        let status =
            "Checks are green and the merge is confirmed safe.\n1. PR 3376 open\n2. CI green";
        assert!(!decision_ask(status));
        let ask = "Please confirm the pick.\n1. merge now\n2. hold";
        assert!(decision_ask(ask));
        // A bare "reply " cue matched every sentence that mentioned a reply.
        let reply = "The reply from codex landed.\n1. PR 3376 open\n2. CI green";
        assert!(
            !decision_ask(reply),
            "mentioning a reply is not a reply ask"
        );
        assert!(decision_ask("Reply 1 or 2.\n1. merge\n2. hold"));
        let table = "| node | state |\n|---|---|\n| x-1 | ? |\n1. PR open\n2. CI pending";
        assert!(!decision_ask(table));
        let next = "1. merged\n2. reconciled\nNext: nothing from you.";
        assert!(!decision_ask(next));
        let receipt = "You ruled hold, which was your call.\n1. held\n2. noted";
        assert!(!decision_ask(receipt));
        // A numbered status list plus an unrelated "want" sentence in another
        // paragraph is a report: the options must sit under the ask line.
        let split =
            "Prep list:\n1. read the node\n2. cut the worktree\n\nLet me know if the fix grows.";
        assert!(!decision_ask(split));
        let spaced = "Should I merge?\n\n1. merge now\n2. hold";
        assert!(decision_ask(spaced));
        assert!(decision_ask(
            "Options:\n1. merge now\n2. hold\nWhich do you want?"
        ));
        assert!(!decision_ask(
            "Done:\n1. merged\n2. reconciled\nLet me know if it grows."
        ));
        // A lead fire judged false on 2026-10-23 (intel --lens rules): an ask
        // marker inside a numbered step is a step, not an ask over options.
        let approve = "After you approve stable:\n1. Lift the freeze\n\
                       2. I mail the workers. One PR still needs your visual approval.\n\
                       3. I hand off to a fresh lead.";
        assert!(!decision_ask(approve));
        // A closing either-or question carries its options in the ask line
        // itself.
        assert!(decision_ask("Should I merge now or wait for codex?"));
        assert!(decision_ask("Merge now, or wait for the rerun?"));
        // No second alternative, no fire: a report may end in a question.
        assert!(!decision_ask("Should I merge now?"));
        assert!(!decision_ask("Was the flag named beta?"));
    }

    fn r1_unpromoted_and_cap_legs() {
        // AC5: unpromoted, no manifest, default config - the same ask allows.
        let rig = Rig::new(false);
        let ask = "1. merge 2. hold. Your call?";
        let fires = eval_stop_in(&rig.cwd, &rig.journal, &rig.home, &rig.payload(ask));
        assert!(
            fires.is_empty(),
            "an unpromoted session owes nothing by default"
        );

        // The cap: three blocks per turn key, then allow.
        let rig = Rig::new(true);
        for i in 0..BLOCK_CAP + 2 {
            let fires = eval_stop_in(&rig.cwd, &rig.journal, &rig.home, &rig.payload(ask));
            let blocked = fires.iter().any(|f| f.rule == "chat_ask_unfiled");
            if i < BLOCK_CAP {
                assert!(blocked, "fire {i} blocks");
            } else {
                assert!(!blocked, "fire {i} past the cap allows");
            }
        }
        let spans = rig.spans();
        assert_eq!(
            spans.len(),
            BLOCK_CAP,
            "one block span per fire, none past the cap"
        );
        assert_eq!(
            spans[BLOCK_CAP - 1]["data"]["cap_reached"],
            true,
            "the last fire's span carries cap_reached"
        );
    }

    // A non-lead session the user typed into owes its either-or chat ask to
    // the board. The witness is the store the conversation hold arms from;
    // the payload is the four-field Stop shape every harness sends.
    fn nonlead_user_turn_blocks_on_either_or_ask_leg() {
        let rig = Rig::new(false);
        rig.operator_submit(SESSION, now_ms());
        let fires = eval_stop_in(
            &rig.cwd,
            &rig.journal,
            &rig.home,
            &rig.payload("Should I merge now or wait for codex?"),
        );
        let block = fires
            .iter()
            .find(|f| f.rule == "chat_ask_unfiled")
            .expect("a user-typed non-lead session blocks on the either-or ask");
        assert!(block.reason.contains("rule chat_ask_unfiled"));
    }

    // The ask shape alone widens nothing: a dispatched worker with no
    // witnessed user turn still passes (its queued turn is the dispatch
    // prompt, not the operator talking).
    fn dispatched_worker_without_user_turn_still_passes_leg() {
        let rig = Rig::new(false);
        let fires = eval_stop_in(
            &rig.cwd,
            &rig.journal,
            &rig.home,
            &rig.payload("Should I merge now or wait for codex?"),
        );
        assert!(
            fires.is_empty(),
            "a worker with no user turn owes no chat ask"
        );
    }

    // One leg per harness Stop payload shape. All four adapters hand the
    // engine the same four fields: claude and codex in-process (stop.rs
    // eval_stop), agy and opencode through `hook rules --event stop`, whose
    // transport entry ends in this same eval_stop call. Each adapter passes
    // its own session-id value grammar; the engine canonicalizes any string,
    // so each leg proves the shape reaches the rule, not the id grammar.
    fn every_harness_stop_payload_shape_reaches_the_rule_leg() {
        let shapes = [
            ("claude", "claude-stop-payload-session"),
            ("codex", "codex-stop-payload-session"),
            ("agy", "agy-conversation-id"),
            ("opencode", "opencode-session-id"),
        ];
        for (harness, session_id) in shapes {
            let rig = Rig::new(false);
            rig.operator_submit(session_id, now_ms());
            let payload = json!({
                "session_id": session_id,
                "last_assistant_message": "Should I merge now or wait for codex?",
                "transcript_path": "",
                "cwd": rig.cwd.display().to_string(),
            })
            .to_string();
            let fires = eval_stop_in(&rig.cwd, &rig.journal, &rig.home, &payload);
            assert!(
                fires.iter().any(|f| f.rule == "chat_ask_unfiled"),
                "{harness} payload shape blocks on the either-or ask"
            );
        }
    }

    fn r2_and_r3_legs() {
        let rig = Rig::new(false);
        rig.seed(json!({
            "ts": crate::events::now_rfc3339(),
            "type": "decision_span",
            "source": "test",
            "data": {
                "span_kind": "route",
                "route": "self",
                "recommendation": "merge now",
                "trace": {
                    "trace_id": "x-node1",
                    "span_id": "s-rig0099",
                    "actor_session": rig.session,
                    "actor_kind": "worker",
                    "comms": "mail",
                },
            },
        }));

        // AC6: the first Stop notifies once, the second does not repeat it.
        let fires = eval_stop_in(&rig.cwd, &rig.journal, &rig.home, &rig.payload("done"));
        let notes: Vec<&Fire> = fires.iter().filter(|f| f.action == "notify").collect();
        assert_eq!(notes.len(), 1, "one notify for one route span");
        let (title, body, pointer) = notes[0].notify.as_ref().unwrap();
        assert!(title.contains("decided_ask_fyi"));
        assert!(body.contains("decided x-node1 itself: merge now"));
        assert!(body.contains("--overturns s-rig0099"));
        assert_eq!(pointer.as_deref(), Some("fno inbox outstanding"));
        let fires = eval_stop_in(&rig.cwd, &rig.journal, &rig.home, &rig.payload("done"));
        assert!(
            !fires.iter().any(|f| f.action == "notify"),
            "the second Stop does not repeat the notify"
        );
        let all_spans = rig.spans();
        let notifies: Vec<&Value> = all_spans
            .iter()
            .filter(|s| s["data"]["span_kind"] == "notify")
            .collect();
        assert_eq!(notifies.len(), 1, "one notify span is the ledger");
        assert_eq!(notifies[0]["data"]["matched_event"], "s-rig0099");
        r3_legs();
    }

    fn r3_legs() {
        // AC7: reversible, recommended, why_user set, class none - one block.
        let rig = Rig::new(false);
        rig.question("q-rig02", None);
        let fires = eval_stop_in(&rig.cwd, &rig.journal, &rig.home, &rig.payload("done"));
        let nudges: Vec<&Fire> = fires
            .iter()
            .filter(|f| f.rule == "why_user_escape")
            .collect();
        assert_eq!(nudges.len(), 1, "the escape blocks once");
        assert!(nudges[0].reason.contains("why_user_escape"));
        assert!(nudges[0].reason.contains("fno backlog decide"));
        let fires = eval_stop_in(&rig.cwd, &rig.journal, &rig.home, &rig.payload("done"));
        assert!(
            !fires.iter().any(|f| f.rule == "why_user_escape"),
            "the next Stop allows"
        );

        // AC8: the same question whose escalate route declared a user-only
        // class allows.
        let rig = Rig::new(false);
        rig.question("q-rig03", Some(("s-rig-route8", "irreversible")));
        let fires = eval_stop_in(&rig.cwd, &rig.journal, &rig.home, &rig.payload("done"));
        assert!(
            !fires.iter().any(|f| f.rule == "why_user_escape"),
            "a user-only class escapes the escape check"
        );
    }

    /// The idle beat row a lead rule reads: zero live scope workers, a pick
    /// offered and untaken, the reactive tag set.
    fn idle_beat_row(rig: &Rig, node: &str) {
        let hour_ago = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        rig.seed(json!({
            "ts": hour_ago,
            "type": "lead_checkin",
            "source": "loop",
            "data": {
                "scope": "e-63be",
                "holder_session": rig.session,
                "change": "beat legs: a seeded idle beat",
                "scope_live_workers": 0,
                "reactive": true,
                "pick_offered": format!("fno agent spawn \"/fno:target {node}\""),
                "pick_node": node,
                "pick": null,
                "pick_line": format!(
                    "pick: fno agent spawn \"/fno:target {node}\" (answer with --pick \"<what you started>\")"
                ),
            },
        }));
    }

    /// An idle lead whose beat offered a pick with zero scope workers is
    /// blocked, and the block reason names the pick command it printed.
    fn an_idle_lead_with_an_untaken_pick_is_blocked() {
        let rig = Rig::new(true);
        idle_beat_row(&rig, "x-idle1");
        let fires = eval_stop_in(&rig.cwd, &rig.journal, &rig.home, &rig.payload("done"));
        let blocks: Vec<&Fire> = fires
            .iter()
            .filter(|f| f.rule == "lead_idle_pick_unanswered")
            .collect();
        assert_eq!(blocks.len(), 1, "{fires:?}");
        let block = blocks.first().expect("the idle pick block fires");
        assert!(
            block.reason.contains("x-idle1"),
            "the reason names the pick command: {}",
            block.reason
        );
    }

    /// The same stop passes when the spawn gate refused the pick's start:
    /// the refusal row for the pick's node, newer than the beat, is the
    /// evidence that the idleness is the gate's, not the lead's.
    fn a_spawn_gate_refusal_lets_the_idle_lead_stop() {
        let rig = Rig::new(true);
        idle_beat_row(&rig, "x-idle2");
        rig.seed(json!({
            "ts": crate::events::now_rfc3339(),
            "type": "agent_spawn_refused",
            "source": "daemon",
            "data": {
                "node": "x-idle2",
                "argv": ["spawn", "/fno:target x-idle2"],
                "exit_code": 6,
                "reason": "share cap: 2 of 2 slots held",
            },
        }));
        let fires = eval_stop_in(&rig.cwd, &rig.journal, &rig.home, &rig.payload("done"));
        assert!(
            !fires.iter().any(|f| f.rule == "lead_idle_pick_unanswered"),
            "a refused pick is not an untaken one: {fires:?}"
        );

        // A seat pick names a scope, not a node: the refused argv carries the
        // promote token, and that is the allow evidence.
        let rig = Rig::new(true);
        idle_beat_row(&rig, "x-eSeat");
        rig.seed(json!({
            "ts": crate::events::now_rfc3339(),
            "type": "agent_spawn_refused",
            "source": "daemon",
            "data": {
                "node": null,
                "argv": ["spawn", "--as", "thread", "--promote", "x-eSeat", "/fno:lead"],
                "exit_code": 6,
                "reason": "share cap: 2 of 2 slots held",
            },
        }));
        let fires = eval_stop_in(&rig.cwd, &rig.journal, &rig.home, &rig.payload("done"));
        assert!(
            !fires.iter().any(|f| f.rule == "lead_idle_pick_unanswered"),
            "a refused seat pick rides the argv token: {fires:?}"
        );
    }

    /// A beat whose pick was answered, whose worker reading failed (no count
    /// key), or whose session holds no live team never blocks.
    fn an_answered_pick_or_unread_count_or_untitled_session_never_blocks() {
        let rig = Rig::new(true);
        rig.seed(json!({
            "ts": crate::events::now_rfc3339(),
            "type": "lead_checkin",
            "source": "loop",
            "data": {
                "scope": "e-63be",
                "holder_session": rig.session,
                "change": "beat legs: a seeded idle beat",
                "scope_live_workers": 0,
                "reactive": true,
                "pick_offered": "fno agent spawn \"/fno:target x-idle3\"",
                "pick_node": "x-idle3",
                "pick": "started the worker",
                "pick_line": "pick: fno agent spawn \"/fno:target x-idle3\"",
            },
        }));
        let fires = eval_stop_in(&rig.cwd, &rig.journal, &rig.home, &rig.payload("done"));
        assert!(
            !fires.iter().any(|f| f.rule == "lead_idle_pick_unanswered"),
            "an answered pick is work started: {fires:?}"
        );

        let rig = Rig::new(true);
        rig.seed(json!({
            "ts": crate::events::now_rfc3339(),
            "type": "lead_checkin",
            "source": "loop",
            "data": {
                "scope": "e-63be",
                "holder_session": rig.session,
                "change": "beat legs: a seeded idle beat",
                "reactive": true,
                "pick_offered": "fno agent spawn \"/fno:target x-idle4\"",
                "pick_node": "x-idle4",
                "pick": null,
            },
        }));
        let fires = eval_stop_in(&rig.cwd, &rig.journal, &rig.home, &rig.payload("done"));
        assert!(
            !fires.iter().any(|f| f.rule == "lead_idle_pick_unanswered"),
            "an unread worker count never blocks: {fires:?}"
        );

        let rig = Rig::new(false);
        idle_beat_row(&rig, "x-idle5");
        let fires = eval_stop_in(&rig.cwd, &rig.journal, &rig.home, &rig.payload("done"));
        assert!(
            !fires.iter().any(|f| f.rule == "lead_idle_pick_unanswered"),
            "an untitled session holds no scope to idle: {fires:?}"
        );
    }

    /// The idle-pick row's legs, run beside the other rule legs from the
    /// engine test: block, refusal-evidence, and the never-block arms.
    fn idle_pick_legs() {
        an_idle_lead_with_an_untaken_pick_is_blocked();
        a_spawn_gate_refusal_lets_the_idle_lead_stop();
        an_answered_pick_or_unread_count_or_untitled_session_never_blocks();
    }
}
