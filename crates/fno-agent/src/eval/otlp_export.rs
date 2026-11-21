//! The OTLP export arm: the feed window ships as one OTLP/HTTP JSON logs
//! batch per beat to a user-named collector. Every gate must open before a
//! byte leaves: an opt-in `[telemetry.otlp] enabled = true`, an http(s)
//! endpoint, and a non-empty headers env (auth rides there, never in the
//! config file). Identity fields (ids, kinds, enums) always export; free
//! text only with `log_content`. Delivery is at-least-once keyed on
//! `fno.row.id`, so a redelivered batch dedupes downstream. With
//! `forward_requests = true` the same beat also ships the masked
//! `api_requests` rows the receiver stored, to the same endpoint under the
//! same headers; the key unset means those rows never leave the machine.

use crate::feed::FeedRow;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// How far back each beat reads, regardless of what already shipped: a
/// collector outage older than this forgets its backlog.
const LOOKBACK_S: u64 = 86_400;
/// Rows per post. One beat posts at most one batch; the rest wait.
const MAX_BATCH: usize = 1000;
/// The env var carrying the post headers as `k=v,k2=v2`. Overridable in
/// config (`headers_env`) so two collectors can hold different secrets.
const HEADERS_ENV: &str = "FNO_OTLP_HEADERS";
/// Delivered and dropped ids older than this prune from the state maps.
const STATE_TTL_HOURS: i64 = 48;
/// The arm's cadence; also the tick row's declared interval.
pub const OTLP_INTERVAL_S: u64 = 300;
/// A tick exceeding this emits a stuck row naming the stage.
const OTLP_TICK_BUDGET_S: u64 = 300;

/// The config verdict for one beat. `Refused` means the operator asked for
/// an export the config cannot express: the beat reports it and sends
/// nothing, every beat, until the config heals.
pub enum Resolved {
    Off { forced_off_by: Option<&'static str> },
    Refused(String),
    On(OtlpConfig),
}

/// One beat's export posture, resolved once per tick.
pub struct OtlpConfig {
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    pub log_content: bool,
    pub forward_requests: bool,
    pub host: String,
}

/// The env kill switch that forces every telemetry export off, if one is set.
pub(crate) fn forced_off() -> Option<&'static str> {
    ["OTEL_SDK_DISABLED", "DO_NOT_TRACK"]
        .into_iter()
        .find(|var| {
            let val = std::env::var(var).unwrap_or_default();
            let val = val.trim().to_ascii_lowercase();
            val == "true" || val == "1"
        })
}

/// The PostHog token read shared by the capture-route arms (eval events and
/// usage events). The env var wins while it holds a token; otherwise the
/// token file is read fresh, so a daemon beat picks up a rotated token
/// without a restart. The file must be mode 0600: group or other bits
/// refuse the read.
pub(crate) fn token_from_env_or_file(token_env: &str, token_file: &str) -> Result<String, String> {
    if let Ok(from_env) = std::env::var(token_env) {
        if !from_env.trim().is_empty() {
            return Ok(from_env);
        }
    }
    let path = crate::github_app::expand_tilde(Path::new(token_file));
    let meta = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "token env {token_env} is unset or empty and token file {} is missing",
                path.display()
            ));
        }
        Err(e) => {
            return Err(format!(
                "token env {token_env} is unset or empty and token file {} is unreadable: {e}",
                path.display()
            ));
        }
    };
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "token file {} must be mode 0600 (run chmod 600 {})",
            path.display(),
            path.display()
        ));
    }
    let body = std::fs::read_to_string(&path)
        .map_err(|e| format!("token file {} is unreadable: {e}", path.display()))?;
    let token = body.trim();
    if token.is_empty() {
        return Err(format!("token file {} is empty", path.display()));
    }
    Ok(token.to_string())
}

/// Gates in order: env kill switches first (`OTEL_SDK_DISABLED`,
/// `DO_NOT_TRACK`), because a machine saying "no telemetry" outranks any
/// config file. Then the opt-in flag, then the endpoint scheme, then the
/// headers env: an unset or empty value refuses rather than shipping an
/// unauthenticated export to whatever the endpoint is.
pub fn resolve(cwd: &Path) -> Resolved {
    if let Some(var) = forced_off() {
        return Resolved::Off {
            forced_off_by: Some(var),
        };
    }
    if !crate::agents_config::otlp_export_enabled(cwd) {
        return Resolved::Off {
            forced_off_by: None,
        };
    }
    let table = match crate::agents_config::config_lookup(cwd, &["telemetry", "otlp"]) {
        Some(t) => t,
        None => {
            return Resolved::Off {
                forced_off_by: None,
            }
        }
    };
    let endpoint = table
        .get("endpoint")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if endpoint.is_empty() {
        return Resolved::Refused("telemetry.otlp.endpoint is empty".to_string());
    }
    if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
        return Resolved::Refused(format!(
            "telemetry.otlp.endpoint must be http(s), got: {endpoint}"
        ));
    }
    let mut endpoint = endpoint;
    // The logs path is fixed; a bare collector URL is the whole signal.
    if !endpoint.ends_with("/v1/logs") {
        endpoint.push_str("/v1/logs");
    }
    let headers_env = table
        .get("headers_env")
        .and_then(|v| v.as_str())
        .unwrap_or(HEADERS_ENV)
        .trim()
        .to_string();
    let raw = std::env::var(&headers_env).unwrap_or_default();
    if raw.trim().is_empty() {
        return Resolved::Refused(format!(
            "headers env {headers_env} is unset or empty; the export would be unauthenticated"
        ));
    }
    let mut headers = Vec::new();
    for pair in raw.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let Some((name, value)) = pair.split_once('=') else {
            return Resolved::Refused(format!("headers env entry has no '=': {pair}"));
        };
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() || value.is_empty() {
            return Resolved::Refused(format!("headers env entry has an empty side: {pair}"));
        }
        headers.push((name.to_string(), value.to_string()));
    }
    let log_content = table
        .get("log_content")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let forward_requests = table
        .get("forward_requests")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let rest = endpoint
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(&endpoint);
    let host = rest.split('/').next().unwrap_or(rest).to_string();
    Resolved::On(OtlpConfig {
        endpoint,
        headers,
        log_content,
        forward_requests,
        host,
    })
}

/// sha256 over 0x1f-joined parts, hex. Both id formulas ride this so a
/// delivered id stays stable across beats.
fn sha_hex(parts: &[&str]) -> String {
    let joined = parts.join("\u{1f}");
    let digest = Sha256::digest(joined.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The stable row id: sha256 over the identity fields (kind, ts, node,
/// session, ref, team, holder, name). Title is excluded so a title edit
/// does not resend the row.
pub(crate) fn row_id(row: &FeedRow) -> String {
    sha_hex(&[
        row.kind.as_str(),
        row.ts.as_str(),
        row.node.as_deref().unwrap_or(""),
        row.session_id.as_deref().unwrap_or(""),
        row.r#ref.as_deref().unwrap_or(""),
        row.team.as_deref().unwrap_or(""),
        row.holder.as_deref().unwrap_or(""),
        row.name.as_deref().unwrap_or(""),
    ])
}

/// The stable forward id for one api_requests row, over the stored dedupe
/// key. The `api_request` lead part keeps these digests in their own domain
/// beside the feed ids inside the shared delivered map.
pub(crate) fn request_row_id(dedupe_key: &str) -> String {
    sha_hex(&["api_request", dedupe_key])
}

fn str_attr(key: &str, value: &str) -> Value {
    json!({"key": key, "value": {"stringValue": value}})
}

fn record(row: &FeedRow, log_content: bool, now_nanos: u128) -> Value {
    let time_nanos = crate::event_store::parse_rfc3339_ms(&row.ts)
        .map(|ms| (ms as u128) * 1_000_000)
        .map(|n| n.to_string())
        .unwrap_or_default();
    let mut attrs = vec![
        str_attr("fno.kind", &row.kind),
        str_attr("fno.area", &row.area),
        str_attr("session.id", row.session_id.as_deref().unwrap_or("")),
        str_attr("fno.row.id", &row_id(row)),
        json!({
            "key": "event.name",
            "value": {"stringValue": format!("fno.{}", row.kind)}
        }),
    ];
    for (key, value) in [
        ("fno.node.id", row.node.as_deref()),
        ("fno.harness", row.harness.as_deref()),
        ("fno.model", row.model.as_deref()),
        ("fno.effort", row.effort.as_deref()),
        ("fno.phase", row.phase.as_deref()),
        ("fno.team", row.team.as_deref()),
        ("fno.actor", row.actor.as_deref()),
        ("fno.parent.session.id", row.parent.as_deref()),
        ("fno.ref", row.r#ref.as_deref()),
    ] {
        if let Some(v) = value {
            attrs.push(str_attr(key, v));
        }
    }
    if log_content {
        for (key, value) in [
            ("fno.title", Some(row.title.as_str())),
            ("fno.cwd", row.cwd.as_deref()),
            ("fno.reason", row.reason.as_deref()),
            ("fno.detail", row.detail.as_deref()),
            ("fno.url", row.url.as_deref()),
            ("fno.name", row.name.as_deref()),
            ("fno.holder", row.holder.as_deref()),
            ("fno.owner", row.owner.as_deref()),
            ("fno.lead", row.lead.as_deref()),
        ] {
            if let Some(v) = value {
                attrs.push(str_attr(key, v));
            }
        }
    }
    json!({
        "timeUnixNano": time_nanos,
        "observedTimeUnixNano": now_nanos.to_string(),
        "severityNumber": 9,
        "severityText": "INFO",
        "eventName": format!("fno.{}", row.kind),
        "body": {"stringValue": if log_content { row.title.as_str() } else { row.kind.as_str() }},
        "attributes": attrs,
    })
}

/// The OTLP/HTTP JSON logs request body for one batch: one resource, one
/// scope, one record per row.
pub(crate) fn body(rows: &[&FeedRow], log_content: bool) -> String {
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let records: Vec<Value> = rows
        .iter()
        .map(|r| record(r, log_content, now_nanos))
        .collect();
    logs_doc("fno.feed", records)
}

fn logs_doc(scope_name: &str, records: Vec<Value>) -> String {
    let doc = json!({
        "resourceLogs": [{
            "resource": {"attributes": [
                str_attr("service.name", "fno"),
                str_attr("service.version", env!("CARGO_PKG_VERSION")),
            ]},
            "scopeLogs": [{
                "scope": {"name": scope_name},
                "logRecords": records,
            }],
        }],
    });
    serde_json::to_string(&doc).expect("serializing an owned value never fails")
}

/// The event name one forwarded api_request record carries.
const REQUEST_EVENT_NAME: &str = "fno.api_request";

/// One forwarded api_request record. Every attribute is a field the
/// receiver's redaction already keeps: ids, numbers, model and
/// skill/plugin/agent names. Cost ships as a double in dollars; the token
/// counts and identifiers stay exact.
fn request_record(row: &crate::otel_ingest::Row, now_nanos: u128) -> Value {
    let time_nanos = row
        .ts
        .as_deref()
        .and_then(crate::event_store::parse_rfc3339_ms)
        .map(|ms| (ms as u128) * 1_000_000)
        .map(|n| n.to_string())
        .unwrap_or_default();
    let mut attrs = vec![
        str_attr("session.id", &row.session_id),
        str_attr("fno.row.id", &request_row_id(&row.dedupe_key)),
        json!({
            "key": "event.name",
            "value": {"stringValue": REQUEST_EVENT_NAME}
        }),
    ];
    for (key, value) in [
        ("fno.model", row.model.as_deref()),
        ("skill.name", row.skill_name.as_deref()),
        ("plugin.name", row.plugin_name.as_deref()),
        ("agent.name", row.agent_name.as_deref()),
    ] {
        if let Some(v) = value {
            attrs.push(str_attr(key, v));
        }
    }
    if let Some(micros) = row.cost_usd_micros {
        attrs.push(json!({
            "key": "fno.cost_usd",
            "value": {"doubleValue": micros as f64 / 1_000_000.0}
        }));
    }
    for (key, value) in [
        ("fno.input_tokens", row.input_tokens),
        ("fno.output_tokens", row.output_tokens),
        ("fno.cache_read_tokens", row.cache_read_tokens),
        ("fno.cache_creation_tokens", row.cache_creation_tokens),
    ] {
        if let Some(v) = value {
            attrs.push(json!({"key": key, "value": {"intValue": v}}));
        }
    }
    json!({
        "timeUnixNano": time_nanos,
        "observedTimeUnixNano": now_nanos.to_string(),
        "severityNumber": 9,
        "severityText": "INFO",
        "eventName": REQUEST_EVENT_NAME,
        "body": {"stringValue": REQUEST_EVENT_NAME},
        "attributes": attrs,
    })
}

/// The OTLP logs body for one api_requests batch, under its own scope name
/// so a collector can tell the two sources apart.
fn requests_body(rows: &[&crate::otel_ingest::Row]) -> String {
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let records: Vec<Value> = rows.iter().map(|r| request_record(r, now_nanos)).collect();
    logs_doc("fno.api_requests", records)
}

/// What one beat remembers: the ids already delivered (never resent), the
/// ids the endpoint permanently rejected (never retried), the newest row ts
/// shipped, and the last held error for `status_line`. The 48h prune keeps
/// the maps bounded; the collector side dedupes on `fno.row.id`.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct ExportState {
    #[serde(default)]
    pub(crate) delivered: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) dropped: BTreeMap<String, String>,
    #[serde(default)]
    last_delivered_ts: Option<String>,
    #[serde(default)]
    pub(crate) held_error: Option<String>,
}

impl ExportState {
    pub(crate) fn knows(&self, id: &str) -> bool {
        self.delivered.contains_key(id) || self.dropped.contains_key(id)
    }
}

fn state_path(home: &crate::paths::AgentsHome) -> PathBuf {
    home.otlp_export_dir().join("state.json")
}

fn receipts_path(home: &crate::paths::AgentsHome) -> PathBuf {
    home.otlp_export_dir().join("receipts.jsonl")
}

fn load_state(home: &crate::paths::AgentsHome) -> ExportState {
    load_state_at(&state_path(home))
}

pub(crate) fn load_state_at(path: &Path) -> ExportState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Atomic write: tmp file then rename, the attention save_state shape. The
/// export dir is created on first save; a failed save silently reads as an
/// empty state next beat, and at-least-once delivery absorbs the re-send.
fn save_state(home: &crate::paths::AgentsHome, state: &ExportState) {
    save_state_at(home, &state_path(home), state);
}

pub(crate) fn save_state_at(home: &crate::paths::AgentsHome, path: &Path, state: &ExportState) {
    let dir = home.otlp_export_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let tmp = path.with_extension("json.tmp");
    if let Ok(text) = serde_json::to_string(state) {
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

fn append_receipt(home: &crate::paths::AgentsHome, row: &Value) {
    let dir = home.otlp_export_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(receipts_path(home))
    {
        let _ = writeln!(f, "{row}");
    }
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub(crate) fn prune(state: &mut ExportState, now_ms: i64) {
    let cutoff = now_ms - STATE_TTL_HOURS * 3600 * 1000;
    let live =
        |ts: &String| crate::event_store::parse_rfc3339_ms(ts).is_some_and(|ms| ms >= cutoff);
    state.delivered.retain(|_, ts| live(ts));
    state.dropped.retain(|_, ts| live(ts));
}

/// One beat's outcome for the tick row.
pub(crate) struct BeatReport {
    pub acted: u64,
    pub skip: Option<&'static str>,
    pub detail: Vec<String>,
}

/// The rows a beat owes: dated, inside the lookback window, neither
/// delivered nor dropped, oldest first, capped at MAX_BATCH. run_beat and
/// `preview` share this selection, so a preview prints exactly the feed
/// batch the next beat posts. The detail lines name what selection left behind.
fn select_pending<'a>(
    rows: &'a [FeedRow],
    state: &ExportState,
    now_ms: i64,
) -> (Vec<(String, &'a FeedRow)>, Vec<String>) {
    let mut detail: Vec<String> = Vec::new();
    let cutoff = now_ms - LOOKBACK_S as i64 * 1000;
    let mut undated = 0usize;
    let mut pending: Vec<(String, &FeedRow)> = Vec::new();
    for r in rows {
        let Some(ms) = crate::event_store::parse_rfc3339_ms(&r.ts) else {
            undated += 1;
            continue;
        };
        if ms < cutoff {
            continue;
        }
        let id = row_id(r);
        if state.delivered.contains_key(&id) || state.dropped.contains_key(&id) {
            continue;
        }
        pending.push((id, r));
    }
    if undated > 0 {
        detail.push(format!("otlp: {undated} undated rows skipped"));
    }
    // Oldest first: the export catches up in feed order.
    pending.sort_by_key(|(_, r)| crate::event_store::parse_rfc3339_ms(&r.ts).unwrap_or(i64::MAX));
    if pending.len() > MAX_BATCH {
        detail.push(format!(
            "otlp: {} rows beyond the batch cap wait for the next beat",
            pending.len() - MAX_BATCH
        ));
        pending.truncate(MAX_BATCH);
    }
    (pending, detail)
}

/// One beat: dedupe against state, post the pending window once, record the
/// verdict. At-least-once: a hold (no status, 5xx) ships nothing and
/// retries next beat; a permanent 4xx drops the rows (never retried); a 2xx
/// records delivered. Undated rows never ship: `timeUnixNano` cannot lie
/// about them. Selection is `select_pending`, shared with `preview`.
pub(crate) fn run_beat(
    cfg: &OtlpConfig,
    home: &crate::paths::AgentsHome,
    rows: Vec<FeedRow>,
    poster: &mut dyn crate::attention_http::HttpPost,
    now_ms: i64,
) -> BeatReport {
    let mut state = load_state(home);
    let (pending, mut detail) = select_pending(&rows, &state, now_ms);
    if pending.is_empty() {
        let skip = if rows.is_empty() {
            "no_rows"
        } else {
            "caught_up"
        };
        if skip == "no_rows" {
            detail.push(format!(
                "otlp: nothing in the {}h window",
                LOOKBACK_S / 3600
            ));
        } else {
            detail.push("otlp: every row in the window already shipped".to_string());
        }
        return BeatReport {
            acted: 0,
            skip: Some(skip),
            detail,
        };
    }
    let payload = body(
        &pending.iter().map(|(_, r)| *r).collect::<Vec<_>>(),
        cfg.log_content,
    );
    let result = poster.post(&cfg.endpoint, &cfg.headers, &payload);
    let mut acted: u64 = 0;
    let mut skip: Option<&'static str> = None;
    if result.delivered() {
        for (id, r) in &pending {
            state.delivered.insert(id.clone(), r.ts.clone());
        }
        state.last_delivered_ts = pending.last().map(|(_, r)| r.ts.clone());
        state.held_error = None;
        acted = pending.len() as u64;
        detail.push(format!(
            "otlp: delivered {} record(s) to {}",
            pending.len(),
            cfg.host
        ));
    } else if result.dropped() {
        for (id, r) in &pending {
            state.dropped.insert(id.clone(), r.ts.clone());
        }
        state.held_error = None;
        acted = pending.len() as u64;
        detail.push(format!(
            "otlp: endpoint rejected {} record(s) permanently (status {})",
            pending.len(),
            result.status.unwrap_or(0)
        ));
    } else {
        let body_summary: String = result.body.chars().take(200).collect();
        state.held_error = Some(match result.status {
            Some(code) => format!("status {code}: {body_summary}"),
            None => format!("no status: {body_summary}"),
        });
        skip = Some("held");
        detail.push(format!("otlp: held {} record(s) for retry", pending.len()));
    }
    prune(&mut state, now_ms);
    append_receipt(
        home,
        &json!({
            "ts": now_rfc3339(),
            "scope": "feed",
            "records": pending.len(),
            "status": result.status,
            "first_row_ts": pending.first().map(|(_, r)| r.ts.clone()).unwrap_or_default(),
            "last_row_ts": pending.last().map(|(_, r)| r.ts.clone()).unwrap_or_default(),
        }),
    );
    save_state(home, &state);
    BeatReport {
        acted,
        skip,
        detail,
    }
}

/// The api_requests rows a beat owes: inside the window, neither delivered
/// nor dropped, oldest first, capped at MAX_BATCH. The reader sorted and
/// windowed them; this leg's own memory is the shared ExportState.
fn select_pending_requests<'a>(
    rows: &'a [crate::otel_ingest::Row],
    state: &ExportState,
    now_ms: i64,
) -> (Vec<(String, &'a crate::otel_ingest::Row)>, Vec<String>) {
    let mut detail: Vec<String> = Vec::new();
    let cutoff = now_ms - LOOKBACK_S as i64 * 1000;
    let mut undated = 0usize;
    let mut pending: Vec<(String, &crate::otel_ingest::Row)> = Vec::new();
    for r in rows {
        let Some(ms) =
            r.ts.as_deref()
                .and_then(crate::event_store::parse_rfc3339_ms)
        else {
            undated += 1;
            continue;
        };
        if ms < cutoff {
            continue;
        }
        let id = request_row_id(&r.dedupe_key);
        if state.delivered.contains_key(&id) || state.dropped.contains_key(&id) {
            continue;
        }
        pending.push((id, r));
    }
    if undated > 0 {
        detail.push(format!("otlp: {undated} undated request row(s) skipped"));
    }
    pending.sort_by_key(|(_, r)| {
        r.ts.as_deref()
            .and_then(crate::event_store::parse_rfc3339_ms)
            .unwrap_or(i64::MAX)
    });
    if pending.len() > MAX_BATCH {
        detail.push(format!(
            "otlp: {} request row(s) beyond the batch cap wait for the next beat",
            pending.len() - MAX_BATCH
        ));
        pending.truncate(MAX_BATCH);
    }
    (pending, detail)
}

/// The requests leg of one beat: dedupe against the shared state, post the
/// pending window once, record the verdict. The delivery rule is the feed
/// leg's: a hold ships nothing and retries next beat, a permanent 4xx
/// drops, a 2xx records delivered. otel.db is read by the caller
/// (`maybe_tick`) so tests inject rows without a sqlite fixture.
pub(crate) fn run_requests_beat(
    cfg: &OtlpConfig,
    home: &crate::paths::AgentsHome,
    rows: Vec<crate::otel_ingest::Row>,
    poster: &mut dyn crate::attention_http::HttpPost,
    now_ms: i64,
) -> BeatReport {
    let mut state = load_state(home);
    let (pending, mut detail) = select_pending_requests(&rows, &state, now_ms);
    if pending.is_empty() {
        let skip = if rows.is_empty() {
            "no_rows"
        } else {
            "caught_up"
        };
        if skip == "no_rows" {
            detail.push(format!(
                "otlp: nothing in the {}h window",
                LOOKBACK_S / 3600
            ));
        }
        return BeatReport {
            acted: 0,
            skip: Some(skip),
            detail,
        };
    }
    let payload = requests_body(&pending.iter().map(|(_, r)| *r).collect::<Vec<_>>());
    let result = poster.post(&cfg.endpoint, &cfg.headers, &payload);
    let mut acted: u64 = 0;
    let mut skip: Option<&'static str> = None;
    if result.delivered() {
        for (id, r) in &pending {
            state
                .delivered
                .insert(id.clone(), r.ts.clone().unwrap_or_default());
        }
        state.held_error = None;
        acted = pending.len() as u64;
        detail.push(format!(
            "otlp: forwarded {} api_request record(s) to {}",
            pending.len(),
            cfg.host
        ));
    } else if result.dropped() {
        for (id, r) in &pending {
            state
                .dropped
                .insert(id.clone(), r.ts.clone().unwrap_or_default());
        }
        state.held_error = None;
        acted = pending.len() as u64;
        detail.push(format!(
            "otlp: endpoint rejected {} api_request record(s) permanently (status {})",
            pending.len(),
            result.status.unwrap_or(0)
        ));
    } else {
        let body_summary: String = result.body.chars().take(200).collect();
        state.held_error = Some(match result.status {
            Some(code) => format!("status {code}: {body_summary}"),
            None => format!("no status: {body_summary}"),
        });
        skip = Some("held");
        detail.push(format!(
            "otlp: held {} api_request record(s) for retry",
            pending.len()
        ));
    }
    prune(&mut state, now_ms);
    append_receipt(
        home,
        &json!({
            "ts": now_rfc3339(),
            "scope": "api_requests",
            "records": pending.len(),
            "status": result.status,
            "first_row_ts": pending.first().map(|(_, r)| r.ts.clone().unwrap_or_default()),
            "last_row_ts": pending.last().map(|(_, r)| r.ts.clone().unwrap_or_default()),
        }),
    );
    save_state(home, &state);
    BeatReport {
        acted,
        skip,
        detail,
    }
}

/// What the next beat would post, for `fno agent feed --otlp`:
/// the same resolve, state and selection as run_beat, minus the post and the
/// state write. An off or refused posture still previews, with content attrs
/// off, so an operator sees the shape before opting in.
pub(crate) fn preview(
    cwd: &Path,
    home: &crate::paths::AgentsHome,
    rows: &[FeedRow],
) -> (String, Vec<String>) {
    let log_content = match resolve(cwd) {
        Resolved::On(cfg) => cfg.log_content,
        _ => false,
    };
    let state = load_state(home);
    let (pending, detail) = select_pending(rows, &state, now_ms());
    let payload = body(
        &pending.iter().map(|(_, r)| *r).collect::<Vec<_>>(),
        log_content,
    );
    (payload, detail)
}

/// The one-line posture `fno agent feed --otlp` prints to stderr.
pub(crate) fn status_line(cwd: &Path, home: &crate::paths::AgentsHome) -> String {
    match resolve(cwd) {
        Resolved::Off {
            forced_off_by: Some(var),
        } => format!("otlp export: off - forced off by {var}"),
        Resolved::Off {
            forced_off_by: None,
        } => "otlp export: off - nothing leaves this machine".to_string(),
        Resolved::Refused(reason) => format!("otlp export: refused: {reason}"),
        Resolved::On(cfg) => {
            let state = load_state(home);
            let last = state.last_delivered_ts.as_deref().unwrap_or("never");
            let held = state.held_error.as_deref().unwrap_or("none");
            format!(
                "otlp export: on -> {}, content {}, requests {}, last delivered {}, held: {}",
                cfg.host,
                if cfg.log_content { "on" } else { "off" },
                if cfg.forward_requests { "on" } else { "off" },
                last,
                held
            )
        }
    }
}

/// The arm state mirrors the attention arm: one config cwd, a due check, a
/// one-in-flight gate, a stage label for the stuck row.
pub struct Arm {
    config_cwd: PathBuf,
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
    stage: Arc<Mutex<Option<(&'static str, Instant)>>>,
}

impl Arm {
    pub fn new(config_cwd: PathBuf) -> Self {
        Self {
            config_cwd,
            last_tick: Mutex::new(None),
            in_flight: Arc::new(AtomicBool::new(false)),
            stage: Arc::new(Mutex::new(None)),
        }
    }
}

fn stuck_detail(stage: Option<(&'static str, Instant)>, budget_s: u64) -> Option<String> {
    let (name, started) = stage?;
    let secs = started.elapsed().as_secs();
    (secs > budget_s).then(|| format!("tick still in stage {name} after {secs}s"))
}

fn emit_tick_row(
    home: &crate::paths::AgentsHome,
    acted: u64,
    skip_reason: Option<&str>,
    detail: &str,
) {
    let journal = crate::loop_runtime::Journal::new_raw(
        home.events_jsonl(),
        crate::daemon::global_events_path(home),
    );
    crate::tick_ledger::emit_tick(
        &journal,
        "otlp_export",
        crate::tick_ledger::SCHED_DAEMON,
        acted,
        skip_reason,
        Some(detail),
        OTLP_INTERVAL_S,
    );
}

/// The daemon-facing wrapper, the attention shape: due check, one-in-flight
/// gate, the body off-loop behind `spawn_blocking`.
pub fn maybe_tick(arm: &Arm, home: crate::paths::AgentsHome) {
    let interval = std::time::Duration::from_secs(OTLP_INTERVAL_S);
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < interval) {
            return;
        }
        *last = Some(Instant::now());
    }
    if arm.in_flight.swap(true, Ordering::SeqCst) {
        let stage = arm.stage.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(d) = stuck_detail(stage, OTLP_TICK_BUDGET_S) {
            emit_tick_row(&home, 0, Some("timeout"), &d);
        }
        return;
    }
    let flag = Arc::clone(&arm.in_flight);
    let stage = Arc::clone(&arm.stage);
    let cwd = arm.config_cwd.clone();
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        let started = Instant::now();
        let mark = |name: &'static str| {
            *stage.lock().unwrap_or_else(|e| e.into_inner()) = Some((name, started));
        };
        mark("resolve");
        match resolve(&cwd) {
            Resolved::Off { forced_off_by } => {
                mark("emit");
                let why = forced_off_by
                    .map(|v| format!("forced off by {v}"))
                    .unwrap_or_else(|| "telemetry.otlp.enabled is false".to_string());
                emit_tick_row(&home, 0, Some("disabled"), &why);
            }
            Resolved::Refused(reason) => {
                mark("emit");
                emit_tick_row(
                    &home,
                    0,
                    Some("config"),
                    &format!("otlp export refused: {reason}"),
                );
            }
            Resolved::On(cfg) => {
                mark("collect");
                let now = now_ms();
                let since_s = (now / 1000).saturating_sub(LOOKBACK_S as i64) as u64;
                let (rows, leg_notes) = crate::feed::collect_window(&home, Some(since_s), None);
                mark("post");
                let mut poster = crate::attention_http::CurlPost;
                let mut reports = vec![run_beat(&cfg, &home, rows, &mut poster, now)];
                if cfg.forward_requests {
                    mark("collect_requests");
                    let db = home.otel_dir().join("otel.db");
                    match crate::otel_ingest::requests_in_window(
                        &db,
                        now - LOOKBACK_S as i64 * 1000,
                    ) {
                        Ok(rows) => {
                            mark("post_requests");
                            reports.push(run_requests_beat(&cfg, &home, rows, &mut poster, now));
                        }
                        Err(e) => reports.push(BeatReport {
                            acted: 0,
                            skip: None,
                            detail: vec![format!("otlp: api_requests read failed: {e}")],
                        }),
                    }
                }
                mark("emit");
                // A store-skip note explains a thin window; it reads first.
                let mut detail = leg_notes;
                for report in &reports {
                    detail.extend(report.detail.clone());
                }
                let acted: u64 = reports.iter().map(|r| r.acted).sum();
                let skip = reports.iter().find_map(|r| r.skip);
                emit_tick_row(&home, acted, skip, &detail.join("; "));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::LazyLock<&'static std::sync::Mutex<()>> =
        std::sync::LazyLock::new(crate::claims::test_env_lock);

    /// Snapshot the vars the gate chain reads, so a test can set and restore
    /// them without leaking state to a parallel test.
    fn hermetic_env() -> Vec<(&'static str, Option<std::ffi::OsString>)> {
        [
            "FNO_CONFIG",
            "FNO_NO_CANONICAL_CONFIG",
            "FNO_GLOBAL_SETTINGS_PATH",
            "FNO_OTLP_HEADERS",
            "FNO_TEST_OTLP_HEADERS",
            "OTEL_SDK_DISABLED",
            "DO_NOT_TRACK",
        ]
        .iter()
        .map(|k| (*k, std::env::var_os(k)))
        .collect()
    }

    fn restore_env(saved: &[(&'static str, Option<std::ffi::OsString>)]) {
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }

    /// Write a config file and point $FNO_CONFIG at it: the sole candidate
    /// when set, read as-is.
    fn write_cfg(dir: &Path, toml: &str) {
        std::fs::write(dir.join("config.toml"), toml).unwrap();
        std::env::set_var("FNO_CONFIG", dir.join("config.toml"));
    }

    /// The gate chain decides in a fixed order: kill switches first, then
    /// the enabled flag, then endpoint shape, then headers. Every refusal
    /// names what it read; every kill switch outranks the config file.
    #[test]
    fn resolve_gates() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = hermetic_env();
        std::env::set_var("FNO_NO_CANONICAL_CONFIG", "1");
        std::env::remove_var("FNO_CONFIG");
        std::env::remove_var("FNO_GLOBAL_SETTINGS_PATH");
        std::env::remove_var("OTEL_SDK_DISABLED");
        std::env::remove_var("DO_NOT_TRACK");
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();

        // No config: off, with no switch to blame.
        assert!(
            matches!(
                resolve(cwd),
                Resolved::Off {
                    forced_off_by: None
                }
            ),
            "no config reads off with no switch named"
        );

        // enabled = true: past the enabled gate; headers unset refuses
        // rather than shipping an unauthenticated export.
        write_cfg(
            cwd,
            "[telemetry.otlp]\nenabled = true\nendpoint = \"https://collector.example/v1/logs\"\n",
        );
        assert!(
            matches!(resolve(cwd), Resolved::Refused(ref msg) if msg.contains("FNO_OTLP_HEADERS")),
            "unset headers refuse the unauthenticated export"
        );

        // A kill switch outranks the enabled config and names itself.
        std::env::set_var("DO_NOT_TRACK", "1");
        assert!(
            matches!(
                resolve(cwd),
                Resolved::Off {
                    forced_off_by: Some("DO_NOT_TRACK")
                }
            ),
            "DO_NOT_TRACK outranks the enabled config"
        );
        std::env::set_var("OTEL_SDK_DISABLED", "true");
        std::env::remove_var("DO_NOT_TRACK");
        assert!(
            matches!(
                resolve(cwd),
                Resolved::Off {
                    forced_off_by: Some("OTEL_SDK_DISABLED")
                }
            ),
            "OTEL_SDK_DISABLED outranks the enabled config"
        );

        // The full chain passes: endpoint, host, headers, content gate.
        std::env::remove_var("OTEL_SDK_DISABLED");
        std::env::set_var("FNO_OTLP_HEADERS", "Authorization=Bearer t, X-One=1");
        match resolve(cwd) {
            Resolved::On(cfg) => {
                assert_eq!(cfg.endpoint, "https://collector.example/v1/logs");
                assert_eq!(cfg.host, "collector.example");
                assert!(!cfg.log_content);
                assert_eq!(
                    cfg.headers,
                    vec![
                        ("Authorization".to_string(), "Bearer t".to_string()),
                        ("X-One".to_string(), "1".to_string())
                    ]
                );
            }
            Resolved::Off { .. } | Resolved::Refused(_) => {
                panic!("enabled with headers resolves on")
            }
        }

        // A non-http endpoint refuses.
        write_cfg(
            cwd,
            "[telemetry.otlp]\nenabled = true\nendpoint = \"ftp://collector.example\"\n",
        );
        assert!(
            matches!(resolve(cwd), Resolved::Refused(_)),
            "a non-http endpoint refuses"
        );

        // A header entry with no '=' refuses.
        write_cfg(
            cwd,
            "[telemetry.otlp]\nenabled = true\nendpoint = \"https://collector.example/v1/logs\"\n",
        );
        std::env::set_var("FNO_OTLP_HEADERS", "novalue");
        assert!(
            matches!(resolve(cwd), Resolved::Refused(ref msg) if msg.contains("novalue")),
            "a header entry without '=' refuses"
        );

        // A headers_env override reroutes where headers come from.
        write_cfg(
            cwd,
            "[telemetry.otlp]\nenabled = true\nendpoint = \"https://collector.example/v1/logs\"\nheaders_env = \"FNO_TEST_OTLP_HEADERS\"\n",
        );
        std::env::remove_var("FNO_OTLP_HEADERS");
        assert!(
            matches!(resolve(cwd), Resolved::Refused(ref msg) if msg.contains("FNO_TEST_OTLP_HEADERS")),
            "the override env is the one that gets read"
        );
        std::env::set_var("FNO_TEST_OTLP_HEADERS", "a=b");
        assert!(matches!(resolve(cwd), Resolved::On(_)));

        restore_env(&saved);
    }

    /// The forward key rides the same gate chain and defaults off, so a
    /// config that never names it forwards nothing.
    #[test]
    fn forward_requests_resolves_off_by_default() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = hermetic_env();
        std::env::set_var("FNO_NO_CANONICAL_CONFIG", "1");
        std::env::remove_var("FNO_CONFIG");
        std::env::remove_var("FNO_GLOBAL_SETTINGS_PATH");
        std::env::remove_var("OTEL_SDK_DISABLED");
        std::env::remove_var("DO_NOT_TRACK");
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        write_cfg(
            cwd,
            "[telemetry.otlp]\nenabled = true\nendpoint = \"https://collector.example/v1/logs\"\n",
        );
        std::env::set_var("FNO_OTLP_HEADERS", "a=b");
        match resolve(cwd) {
            Resolved::On(cfg) => {
                assert!(!cfg.forward_requests, "the key unset reads off");
            }
            _ => panic!("enabled with headers resolves on"),
        }
        write_cfg(
            cwd,
            "[telemetry.otlp]\nenabled = true\nendpoint = \"https://collector.example/v1/logs\"\nforward_requests = true\n",
        );
        match resolve(cwd) {
            Resolved::On(cfg) => assert!(cfg.forward_requests),
            _ => panic!("the key set reads on"),
        }
        restore_env(&saved);
    }

    /// The two id formulas digest different part shapes (the feed id joins
    /// eight fields, the request id two), so a request id can never equal a
    /// feed id inside the shared delivered map, and a redelivered request
    /// row keeps its id.
    #[test]
    fn request_ids_stay_in_their_own_domain() {
        let id = request_row_id("rid:abc");
        assert_eq!(id, request_row_id("rid:abc"), "stable across beats");
        assert_ne!(id, request_row_id("rid:xyz"));
        assert_ne!(id, request_row_id("sid:s:7:t"), "each row its own id");
    }

    fn request_row(dedupe: &str, ts: Option<&str>) -> crate::otel_ingest::Row {
        crate::otel_ingest::Row {
            dedupe_key: dedupe.to_string(),
            session_id: "s-1".to_string(),
            ts: ts.map(str::to_string),
            model: Some("claude-sonnet-5".to_string()),
            cost_usd_micros: Some(3_050_000),
            input_tokens: Some(100),
            output_tokens: Some(50),
            cache_read_tokens: None,
            cache_creation_tokens: None,
            skill_name: Some("fno:target".to_string()),
            plugin_name: None,
            agent_name: None,
        }
    }

    struct Stub {
        status: Option<u16>,
        bodies: Vec<String>,
    }

    impl crate::attention_http::HttpPost for Stub {
        fn post(&mut self, _: &str, _: &[(String, String)], body: &str) -> PostResult {
            self.bodies.push(body.to_string());
            PostResult {
                status: self.status,
                body: String::new(),
                error: None,
            }
        }
    }

    fn cfg() -> OtlpConfig {
        OtlpConfig {
            endpoint: "https://collector.example/v1/logs".to_string(),
            headers: vec![("Authorization".to_string(), "Bearer t".to_string())],
            log_content: false,
            forward_requests: true,
            host: "collector.example".to_string(),
        }
    }

    fn test_home() -> (tempfile::TempDir, crate::paths::AgentsHome) {
        let tmp = tempfile::tempdir().unwrap();
        let home = crate::paths::AgentsHome::at(tmp.path().join("agents"));
        (tmp, home)
    }

    /// The requests leg ships the row's identity and numbers under its own
    /// scope: the OTLP doc names the api_requests scope, the record carries
    /// the event name, session, cost, tokens and safe names, and nothing
    /// else. Absent fields stay absent.
    #[test]
    fn requests_body_ships_identity_and_numbers() {
        let row = request_row("rid:shape", Some("2026-11-20T11:00:00Z"));
        let doc: Value = serde_json::from_str(&requests_body(&[&row])).unwrap();
        let scope = &doc["resourceLogs"][0]["scopeLogs"][0];
        assert_eq!(scope["scope"]["name"], "fno.api_requests");
        let rec = &scope["logRecords"][0];
        assert_eq!(rec["eventName"], "fno.api_request");
        let attrs = |k: &str| -> Value {
            rec["attributes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["key"] == k)
                .map(|a| a["value"].clone())
                .unwrap_or(Value::Null)
        };
        assert_eq!(attrs("session.id")["stringValue"], "s-1");
        assert_eq!(attrs("fno.cost_usd")["doubleValue"], 3.05);
        assert_eq!(attrs("fno.input_tokens")["intValue"], 100);
        assert_eq!(attrs("skill.name")["stringValue"], "fno:target");
        assert_eq!(attrs("fno.model")["stringValue"], "claude-sonnet-5");
        assert_eq!(
            attrs("fno.row.id")["stringValue"],
            request_row_id("rid:shape")
        );
        assert_eq!(
            attrs("plugin.name"),
            Value::Null,
            "absent fields stay absent"
        );
    }

    /// The requests leg lives by the feed leg's delivery rule: an undated or
    /// out-of-window row never ships, a 2xx records delivered (the redelivery
    /// then reads caught_up), a permanent 4xx drops, and a hold keeps the
    /// rows pending with the error recorded.
    #[test]
    fn requests_beat_lives_by_the_delivery_rule() {
        let (tmp, home) = test_home();
        let now = crate::event_store::parse_rfc3339_ms("2026-11-20T12:00:00Z").unwrap();
        let fresh = "2026-11-20T11:00:00Z";
        let rows = vec![
            request_row("rid:old", Some("2026-11-19T00:00:00Z")),
            request_row("rid:undated", None),
            request_row("rid:fresh", Some(fresh)),
        ];

        // Selection: only the in-window, undelivered, dated row ships.
        let state = load_state(&home);
        let (pending, detail) = select_pending_requests(&rows, &state, now);
        assert_eq!(pending.len(), 1, "cutoff and undated filters hold");
        assert_eq!(pending[0].1.dedupe_key, "rid:fresh");
        assert!(
            detail.iter().any(|d| d.contains("undated")),
            "the undated skip is named: {detail:?}"
        );

        // A 2xx records delivered; the redelivery reads caught_up.
        let mut poster = Stub {
            status: Some(200),
            bodies: Vec::new(),
        };
        let report = run_requests_beat(&cfg(), &home, rows.clone(), &mut poster, now);
        assert_eq!(report.acted, 1);
        let state = load_state(&home);
        assert_eq!(state.delivered.len(), 1);
        let mut poster = Stub {
            status: Some(200),
            bodies: Vec::new(),
        };
        let report = run_requests_beat(&cfg(), &home, rows.clone(), &mut poster, now);
        assert_eq!(report.skip, Some("caught_up"));

        // A permanent 4xx drops the row for good.
        let mut poster = Stub {
            status: Some(400),
            bodies: Vec::new(),
        };
        let rows2 = vec![request_row("rid:drop", Some(fresh))];
        let report = run_requests_beat(&cfg(), &home, rows2, &mut poster, now);
        assert_eq!(report.acted, 1);
        let state = load_state(&home);
        assert!(state.dropped.contains_key(&request_row_id("rid:drop")));

        // A hold ships nothing, keeps the row pending, and names the error.
        let mut poster = Stub {
            status: None,
            bodies: Vec::new(),
        };
        let rows3 = vec![request_row("rid:hold", Some(fresh))];
        let report = run_requests_beat(&cfg(), &home, rows3, &mut poster, now);
        assert_eq!(report.acted, 0);
        let state = load_state(&home);
        assert!(state.held_error.is_some());
        assert!(
            !state.delivered.contains_key(&request_row_id("rid:hold")),
            "held rows stay pending"
        );
        let _ = tmp;
    }
}
