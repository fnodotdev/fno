//! The opt-in daily usage report for every fno user (law D3283). One
//! `fno_usage_report` event per UTC day: counts of allowlisted journal kinds,
//! harnesses and models, plus version, channel, OS, arch and a random install
//! id. Nothing else, and never a hostname, path, title or free text.
//!
//! Nothing leaves before a yes. Consent lives in a state file that only the
//! terminal door (`fno doctor telemetry on`) and the setup wizard write, so
//! no `fno config set` from an agent can switch it on; config can only force
//! it off. Only official release builds carry the PostHog key, embedded at
//! build time, so a source or crates.io build never sends. `show` prints the
//! exact body the next send posts, and each send keeps its body under
//! `sent/<day>.json` for audit.

use crate::attention_http::HttpPost;
use crate::otlp_export;
use crate::paths::AgentsHome;
use crate::usage_events;
use chrono::{Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Raise this when a field joins the report: a yes given to an older schema
/// holds the send until the person says yes again.
pub const SCHEMA: u32 = 1;
pub const INTERVAL_S: u64 = 3600;
const URL: &str = "https://us.i.posthog.com/batch/";
const EVENT: &str = "fno_usage_report";
const KEEP_SENT: usize = 30;
const DOCS: &str = "https://github.com/fnodotdev/fno/blob/main/docs/telemetry.md";

/// The public project key, set only by the release workflow.
fn build_key() -> Option<&'static str> {
    option_env!("FNO_POSTHOG_PUBLIC_KEY")
        .map(str::trim)
        .filter(|k| !k.is_empty())
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Report {
    pub schema: u32,
    pub day: String,
    pub install_id: String,
    pub fno_version: String,
    pub channel: String,
    pub os: String,
    pub arch: String,
    pub events: BTreeMap<String, u64>,
    pub harness: BTreeMap<String, u64>,
    pub model: BTreeMap<String, u64>,
}

pub struct Facts {
    pub install_id: String,
    pub fno_version: String,
    pub channel: String,
    pub os: String,
    pub arch: String,
}

fn ident(value: &str) -> String {
    usage_events::identifier_value(value.trim())
        .unwrap_or("unknown")
        .to_string()
}

#[derive(Deserialize)]
struct Envelope {
    data: Map<String, Value>,
}

/// The pure builder: one day's rows in, one report out. A harness or model
/// value that fails the identifier gate is dropped from its map; the row's
/// kind still counts.
pub fn build(day: &str, rows: &[crate::event_store::EventRow], facts: &Facts) -> Report {
    let mut report = Report {
        schema: SCHEMA,
        day: day.to_string(),
        install_id: ident(&facts.install_id),
        fno_version: ident(&facts.fno_version),
        channel: ident(&facts.channel),
        os: ident(&facts.os),
        arch: ident(&facts.arch),
        events: BTreeMap::new(),
        harness: BTreeMap::new(),
        model: BTreeMap::new(),
    };
    for row in rows {
        if !usage_events::allowlisted(&row.r#type) {
            continue;
        }
        let env = serde_json::from_str::<Envelope>(&row.line).ok();
        if !usage_events::kept(&row.r#type, env.as_ref().map(|e| &e.data)) {
            continue;
        }
        *report.events.entry(row.r#type.clone()).or_default() += 1;
        let Some(env) = env else {
            continue;
        };
        for (field, map) in [
            ("harness", &mut report.harness),
            ("model", &mut report.model),
        ] {
            if let Some(v) = env.data.get(field).and_then(Value::as_str) {
                if let Some(clean) = usage_events::identifier_value(v) {
                    *map.entry(clean.to_string()).or_default() += 1;
                }
            }
        }
    }
    report
}

/// The exact bytes a send posts and `show` prints.
pub fn body(key: &str, report: &Report) -> String {
    let mut props = match serde_json::to_value(report) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    props.insert("$process_person_profile".into(), json!(false));
    json!({
        "api_key": key,
        "batch": [{
            "event": EVENT,
            "uuid": usage_events::event_uuid(&format!("{}:{}", report.install_id, report.day)),
            "timestamp": format!("{}T23:59:59Z", report.day),
            "distinct_id": format!("install:{}", report.install_id),
            "properties": props,
        }],
    })
    .to_string()
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Answer {
    Yes,
    No,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Consent {
    pub answer: Answer,
    pub at: String,
    pub schema: u32,
}

/// The first gate that holds the send, in check order.
#[derive(Debug, PartialEq)]
pub enum Hold {
    ForcedOff(&'static str),
    Ci,
    ConfigOff,
    NoConsent,
    ConsentNo,
    OldSchema(u32),
    NoKey,
}

impl Hold {
    pub fn describe(&self) -> String {
        match self {
            Hold::ForcedOff(var) => format!("{var} is set"),
            Hold::Ci => "CI is set".into(),
            Hold::ConfigOff => "[telemetry.usage_report] enabled = false".into(),
            Hold::NoConsent => "no answer yet (run `fno doctor telemetry on`)".into(),
            Hold::ConsentNo => "you answered no".into(),
            Hold::OldSchema(s) => {
                format!("your yes was for report schema {s}; this build sends {SCHEMA}, so it asks again")
            }
            Hold::NoKey => "this build carries no key (only official releases send)".into(),
        }
    }
}

/// Everything the gate reads, gathered once so the gate itself is pure.
pub struct Inputs<'a> {
    pub forced_off: Option<&'static str>,
    pub ci: bool,
    pub config_enabled: bool,
    pub consent: Option<Consent>,
    pub key: Option<&'a str>,
}

pub fn gate<'a>(inputs: &Inputs<'a>) -> Result<&'a str, Hold> {
    if let Some(var) = inputs.forced_off {
        return Err(Hold::ForcedOff(var));
    }
    if inputs.ci {
        return Err(Hold::Ci);
    }
    if !inputs.config_enabled {
        return Err(Hold::ConfigOff);
    }
    match &inputs.consent {
        None => return Err(Hold::NoConsent),
        Some(c) if c.answer == Answer::No => return Err(Hold::ConsentNo),
        Some(c) if c.schema != SCHEMA => return Err(Hold::OldSchema(c.schema)),
        Some(_) => {}
    }
    inputs.key.ok_or(Hold::NoKey)
}

fn ci_set() -> bool {
    std::env::var("CI").is_ok_and(|v| !v.trim().is_empty())
}

fn config_enabled(cwd: &Path) -> bool {
    crate::agents_config::config_lookup(cwd, &["telemetry", "usage_report", "enabled"])
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

fn dir(home: &AgentsHome) -> PathBuf {
    home.otlp_export_dir().join("usage_report")
}

fn consent_path(home: &AgentsHome) -> PathBuf {
    dir(home).join("consent.json")
}

fn sent_path(home: &AgentsHome, day: &str) -> PathBuf {
    dir(home).join("sent").join(format!("{day}.json"))
}

pub fn read_consent(home: &AgentsHome) -> Option<Consent> {
    let text = std::fs::read_to_string(consent_path(home)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The one writer of the consent file. Callers: the terminal door and the
/// setup wizard, through the same door.
pub fn record_consent(home: &AgentsHome, answer: Answer) -> std::io::Result<()> {
    let consent = Consent {
        answer,
        at: Utc::now().to_rfc3339(),
        schema: SCHEMA,
    };
    let path = consent_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string(&consent)? + "\n")
}

pub fn resolve(cwd: &Path, home: &AgentsHome) -> Result<&'static str, Hold> {
    gate(&Inputs {
        forced_off: otlp_export::forced_off(),
        ci: ci_set(),
        config_enabled: config_enabled(cwd),
        consent: read_consent(home),
        key: build_key(),
    })
}

/// The install id the old session-start count wrote, kept so an id stays
/// stable across the move; minted in the same 16-random-bytes hex form when
/// it is missing.
fn install_id() -> String {
    let path = crate::scratch::fno_state_root()
        .join("install")
        .join("fno_install_seen.id");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let id = text.trim();
        if id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit()) {
            return id.to_string();
        }
    }
    let mut buf = [0u8; 16];
    if getrandom::fill(&mut buf).is_err() {
        return "unknown".into();
    }
    let id: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, format!("{id}\n"));
    id
}

fn facts() -> Facts {
    let channel = std::fs::read_to_string(
        crate::scratch::fno_state_root()
            .join("install")
            .join("channel"),
    )
    .unwrap_or_default();
    Facts {
        install_id: install_id(),
        fno_version: env!("CARGO_PKG_VERSION").to_string(),
        channel: if channel.trim().is_empty() {
            "other".into()
        } else {
            channel
        },
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
    }
}

fn day_rows(home: &AgentsHome, day: NaiveDate) -> Vec<crate::event_store::EventRow> {
    let start = day
        .and_hms_opt(0, 0, 0)
        .map(|t| t.and_utc().timestamp_millis())
        .unwrap_or_default();
    usage_events::rows_between(
        &usage_events::Journals::of(home),
        start,
        Some(start + 86_400_000 - 1),
    )
}

/// The day the next send covers: yesterday until it is sent, then today
/// (which goes out tomorrow, once the day is complete).
fn next_day(home: &AgentsHome, today: NaiveDate) -> NaiveDate {
    let yesterday = today - Duration::days(1);
    if sent_path(home, &yesterday.to_string()).exists() {
        today
    } else {
        yesterday
    }
}

fn report_for(home: &AgentsHome, day: NaiveDate) -> Report {
    build(&day.to_string(), &day_rows(home, day), &facts())
}

fn prune_sent(home: &AgentsHome) {
    let Ok(entries) = std::fs::read_dir(dir(home).join("sent")) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    files.sort();
    let excess = files.len().saturating_sub(KEEP_SENT);
    for path in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

/// One send for the last complete day, at most once. A 2xx or a permanent
/// 4xx stamps the day with the body that went; anything else retries on the
/// next tick.
pub(crate) fn send_if_due(
    home: &AgentsHome,
    key: &str,
    poster: &mut dyn HttpPost,
    today: NaiveDate,
    build_report: &dyn Fn(NaiveDate) -> Report,
) -> otlp_export::BeatReport {
    let day = today - Duration::days(1);
    let stamp = sent_path(home, &day.to_string());
    if stamp.exists() {
        return otlp_export::BeatReport {
            acted: 0,
            skip: Some("caught_up"),
            detail: vec![format!("usage report: {day} already sent")],
        };
    }
    let payload = body(key, &build_report(day));
    let headers = [("Content-Type".to_string(), "application/json".to_string())];
    let result = poster.post(URL, &headers, &payload);
    if !(result.delivered() || result.dropped()) {
        return otlp_export::BeatReport {
            acted: 0,
            skip: Some("held"),
            detail: vec![format!(
                "usage report: {day} held for retry ({})",
                result
                    .status
                    .map_or("no status".into(), |c| format!("status {c}"))
            )],
        };
    }
    if let Some(parent) = stamp.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&stamp, &payload);
    prune_sent(home);
    let outcome = if result.delivered() {
        "sent"
    } else {
        "rejected permanently"
    };
    otlp_export::BeatReport {
        acted: 1,
        skip: None,
        detail: vec![format!("usage report: {day} {outcome}")],
    }
}

/// `fno doctor telemetry <status|show|on|off>`, reached as
/// `fno-agents evals events --report <action>`.
pub fn command(args: &[String]) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(home) = AgentsHome::from_env_opt() else {
        eprintln!("fno doctor telemetry: no agents home");
        return 1;
    };
    let today = Utc::now().date_naive();
    match args.first().map(String::as_str) {
        Some("status") | None => {
            match resolve(&cwd, &home) {
                Ok(_) => println!(
                    "usage report: on; next send covers {}",
                    next_day(&home, today)
                ),
                Err(hold) => println!("usage report: off - {}", hold.describe()),
            }
            println!("see what it sends: fno doctor telemetry show; docs: {DOCS}");
            0
        }
        Some("show") => {
            let day = next_day(&home, today);
            println!(
                "{}",
                body(build_key().unwrap_or("<no key>"), &report_for(&home, day))
            );
            eprintln!(
                "usage report: the body the send for {day} posts; sent bodies stay under {}",
                dir(&home).join("sent").display()
            );
            eprintln!(
                "usage report: counts these kinds: {}",
                usage_events::KINDS
                    .iter()
                    .map(|k| k.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            0
        }
        Some("on") => turn_on(&home, today),
        Some("off") => match record_consent(&home, Answer::No) {
            Ok(()) => {
                println!("usage report: off; nothing is sent");
                0
            }
            Err(e) => {
                eprintln!("usage report: could not record the answer: {e}");
                1
            }
        },
        Some(other) => {
            eprintln!("usage: fno doctor telemetry <status|show|on|off> (got {other})");
            2
        }
    }
}

fn turn_on(home: &AgentsHome, today: NaiveDate) -> i32 {
    // A pane an agent drives is a real pty, so the terminal check alone
    // admits it; an agent harness's own shell carries one of these markers.
    let agent_shell = ["CLAUDECODE", "CODEX_THREAD_ID", "FNO_AGENT_SELF"]
        .iter()
        .any(|var| std::env::var(var).is_ok_and(|v| !v.trim().is_empty()));
    if ci_set()
        || agent_shell
        || !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal())
    {
        eprintln!("usage report: consent needs a person at a terminal; nothing recorded");
        return 2;
    }
    let report = report_for(home, next_day(home, today));
    println!("{}", body(build_key().unwrap_or("<no key>"), &report));
    eprintln!("This is the whole report. Field list: {DOCS}");
    if build_key().is_none() {
        eprintln!("This build carries no key, so it never sends, even with a yes.");
    }
    eprint!("Send this report once a day? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    // A closed stdin reads zero bytes: that is not a yes.
    if std::io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        eprintln!("\nusage report: no answer; nothing recorded");
        return 1;
    }
    if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        eprintln!("usage report: not turned on");
        return 0;
    }
    match record_consent(home, Answer::Yes) {
        Ok(()) => {
            eprintln!("usage report: on; turn it off with `fno doctor telemetry off`");
            0
        }
        Err(e) => {
            eprintln!("usage report: could not record the answer: {e}");
            1
        }
    }
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

fn emit_tick_row(home: &AgentsHome, acted: u64, skip: Option<&str>, detail: &str) {
    let journal = crate::loop_runtime::Journal::new_raw(
        home.events_jsonl(),
        crate::daemon::global_events_path(home),
    );
    crate::tick_ledger::emit_tick(
        &journal,
        "usage_report",
        crate::tick_ledger::SCHED_DAEMON,
        acted,
        skip,
        Some(detail),
        INTERVAL_S,
    );
}

pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
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
        match resolve(&cwd, &home) {
            Err(hold) => emit_tick_row(&home, 0, Some("disabled"), &hold.describe()),
            Ok(key) => {
                let mut poster = crate::attention_http::CurlPost;
                let report =
                    send_if_due(&home, key, &mut poster, Utc::now().date_naive(), &|day| {
                        report_for(&home, day)
                    });
                emit_tick_row(&home, report.acted, report.skip, &report.detail.join("; "));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attention_http::PostResult;

    fn row(kind: &str, data: Value) -> crate::event_store::EventRow {
        crate::event_store::EventRow {
            seq: 0,
            event_id: "e".into(),
            ts_ms: 0,
            r#type: kind.into(),
            source: "test".into(),
            scope: None,
            retention_class: "normal".into(),
            reject_reason: None,
            line: json!({"ts": "2026-11-10T00:00:00Z", "data": data}).to_string(),
            history_only: false,
            recovery_batch: None,
        }
    }

    fn facts() -> Facts {
        Facts {
            install_id: "0123456789abcdef0123456789abcdef".into(),
            fno_version: "0.4.9".into(),
            channel: "binary\n".into(),
            os: "macos".into(),
            arch: "aarch64".into(),
        }
    }

    struct Stub {
        status: Option<u16>,
        bodies: Vec<String>,
    }

    impl HttpPost for Stub {
        fn post(&mut self, _: &str, _: &[(String, String)], body: &str) -> PostResult {
            self.bodies.push(body.to_string());
            PostResult {
                status: self.status,
                body: String::new(),
                error: None,
            }
        }
    }

    fn home() -> (tempfile::TempDir, AgentsHome) {
        let tmp = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(tmp.path());
        (tmp, home)
    }

    #[test]
    fn each_gate_holds_the_send_in_order() {
        let yes = |schema| {
            Some(Consent {
                answer: Answer::Yes,
                at: "t".into(),
                schema,
            })
        };
        let open = || Inputs {
            forced_off: None,
            ci: false,
            config_enabled: true,
            consent: yes(SCHEMA),
            key: Some("phc_k"),
        };
        assert_eq!(gate(&open()), Ok("phc_k"));
        let cases: Vec<(Inputs, Hold)> = vec![
            (
                Inputs {
                    forced_off: Some("DO_NOT_TRACK"),
                    ..open()
                },
                Hold::ForcedOff("DO_NOT_TRACK"),
            ),
            (Inputs { ci: true, ..open() }, Hold::Ci),
            (
                Inputs {
                    config_enabled: false,
                    ..open()
                },
                Hold::ConfigOff,
            ),
            (
                Inputs {
                    consent: None,
                    ..open()
                },
                Hold::NoConsent,
            ),
            (
                Inputs {
                    consent: Some(Consent {
                        answer: Answer::No,
                        at: "t".into(),
                        schema: SCHEMA,
                    }),
                    ..open()
                },
                Hold::ConsentNo,
            ),
            (
                Inputs {
                    consent: yes(SCHEMA - 1),
                    ..open()
                },
                Hold::OldSchema(SCHEMA - 1),
            ),
            (
                Inputs {
                    key: None,
                    ..open()
                },
                Hold::NoKey,
            ),
        ];
        for (inputs, hold) in cases {
            assert_eq!(gate(&inputs), Err(hold));
        }
    }

    #[test]
    fn report_counts_allowlisted_identifiers_only() {
        let rows = vec![
            row(
                "agent_spawned",
                json!({"harness": "claude", "model": "claude-opus-5-5", "machine": "jasons-mbp", "title": "secret"}),
            ),
            row(
                "agent_spawned",
                json!({"harness": "/Users/me/bin/claude", "model": "m".repeat(129)}),
            ),
            row("consult_call", json!({"harness": "codex"})),
            row("not_on_the_list", json!({"harness": "claude"})),
        ];
        let report = build("2026-11-10", &rows, &facts());
        assert_eq!(
            report.events,
            BTreeMap::from([("agent_spawned".into(), 2), ("consult_call".into(), 1)])
        );
        assert_eq!(
            report.harness,
            BTreeMap::from([("claude".into(), 1), ("codex".into(), 1)])
        );
        assert_eq!(
            report.model,
            BTreeMap::from([("claude-opus-5-5".into(), 1)])
        );
        assert_eq!(report.channel, "binary");
        let payload = body("phc_k", &report);
        let parsed: Value = serde_json::from_str(&payload).unwrap();
        let props = parsed
            .pointer("/batch/0/properties")
            .and_then(Value::as_object)
            .expect("one event with properties");
        let mut keys: Vec<&String> = props.keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "$process_person_profile",
                "arch",
                "channel",
                "day",
                "events",
                "fno_version",
                "harness",
                "install_id",
                "model",
                "os",
                "schema"
            ]
        );
        for banned in ["jasons-mbp", "secret", "/Users"] {
            assert!(!payload.contains(banned), "{banned} leaked: {payload}");
        }
    }

    #[test]
    fn send_posts_the_shown_bytes_once_a_day() {
        let (_tmp, home) = home();
        let today = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let rows = vec![row("agent_spawned", json!({"harness": "claude"}))];
        let make = |day: NaiveDate| build(&day.to_string(), &rows, &facts());
        let shown = body("phc_k", &make(next_day(&home, today)));
        let mut stub = Stub {
            status: Some(200),
            bodies: vec![],
        };
        let first = send_if_due(&home, "phc_k", &mut stub, today, &make);
        assert_eq!(first.acted, 1);
        assert_eq!(stub.bodies, vec![shown.clone()]);
        assert_eq!(
            std::fs::read_to_string(sent_path(&home, "2026-11-10")).unwrap(),
            shown
        );
        let second = send_if_due(&home, "phc_k", &mut stub, today, &make);
        assert_eq!((second.acted, second.skip), (0, Some("caught_up")));
        assert_eq!(stub.bodies.len(), 1);
        assert_eq!(next_day(&home, today), today);
    }

    #[test]
    fn transient_failure_retries_and_stamps_nothing() {
        let (_tmp, home) = home();
        let today = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let make = |day: NaiveDate| build(&day.to_string(), &[], &facts());
        let mut stub = Stub {
            status: Some(503),
            bodies: vec![],
        };
        let held = send_if_due(&home, "phc_k", &mut stub, today, &make);
        assert_eq!((held.acted, held.skip), (0, Some("held")));
        assert!(!sent_path(&home, "2026-11-10").exists());
        let mut ok = Stub {
            status: Some(200),
            bodies: vec![],
        };
        assert_eq!(send_if_due(&home, "phc_k", &mut ok, today, &make).acted, 1);
    }
}
