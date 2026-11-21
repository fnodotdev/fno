//! The api_requests forwarder at its real boundary: the resolved posture,
//! the read-only window over the otel store, and the beat's delivery rule
//! against an injected poster. These drive eval modules from outside the
//! G3 scope, which docs/test-audit/scopes.tsv counts in their own scope
//! (G3 keeps cap 0; this file's path matches no G3 row and lands in G2).

use fno_agent::attention_http::{HttpPost, PostResult};
use fno_agent::eval::otel_ingest::requests_in_window;
use fno_agent::eval::otlp_export::{resolve, run_requests_beat, OtlpConfig, Resolved};
use fno_agent::paths::AgentsHome;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::Value;

/// One OTLP attribute, typed at the boundary: key and value, so the
/// assertions read fields instead of literal key gets.
#[derive(Deserialize)]
struct Attr {
    key: String,
    value: Value,
}

fn attr(rec: &Value, k: &str) -> Value {
    let list: Vec<Attr> =
        serde_json::from_value(rec.pointer("/attributes").cloned().unwrap_or(Value::Null))
            .unwrap_or_default();
    list.into_iter()
        .find(|a| a.key == k)
        .map(|a| a.value)
        .unwrap_or(Value::Null)
}

fn s(v: &Value) -> String {
    v.pointer("/stringValue")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn i(v: &Value) -> Option<i64> {
    v.pointer("/intValue").and_then(Value::as_i64)
}

fn d(v: &Value) -> Option<f64> {
    v.pointer("/doubleValue").and_then(Value::as_f64)
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

fn home(tag: &str) -> (tempfile::TempDir, AgentsHome) {
    let tmp = tempfile::tempdir().unwrap();
    let home = AgentsHome::at(tmp.path().join(tag));
    (tmp, home)
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

fn insert_row(conn: &Connection, dedupe: &str, ts: Option<&str>) {
    conn.execute(
        "INSERT OR IGNORE INTO api_requests VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            format!("rid:{dedupe}"),
            "sess-fwd",
            ts,
            "claude-sonnet-5",
            3_050_000i64,
            100i64,
            50i64,
            rusqlite::types::Null,
            rusqlite::types::Null,
            "fno:target",
            rusqlite::types::Null,
            rusqlite::types::Null,
        ],
    )
    .unwrap();
}

fn seeded_db(home: &AgentsHome) -> (std::path::PathBuf, Connection) {
    let db = home.otel_dir().join("otel.db");
    std::fs::create_dir_all(home.otel_dir()).unwrap();
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(fno_agent::otel_read::schema_sql())
        .unwrap();
    (db, conn)
}

fn state_json(home: &AgentsHome) -> Value {
    let text = std::fs::read_to_string(home.otlp_export_dir().join("state.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn cutoff_ms() -> i64 {
    chrono::DateTime::parse_from_rfc3339("2026-11-20T10:00:00Z")
        .unwrap()
        .timestamp_millis()
}

/// The forward key rides the same gate chain and defaults off, so a
/// config that never names it forwards nothing. Failure: the chain reads
/// the key as on by default, and the beat reads otel.db with the key
/// unset.
#[test]
fn forward_requests_resolves_off_by_default() {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("FNO_NO_CANONICAL_CONFIG", "1");
    std::env::set_var("FNO_CONFIG", dir.path().join("config.toml"));
    std::env::remove_var("OTEL_SDK_DISABLED");
    std::env::remove_var("DO_NOT_TRACK");
    std::env::set_var("FNO_OTLP_HEADERS", "a=b");
    std::fs::write(
        dir.path().join("config.toml"),
        "[telemetry.otlp]\nenabled = true\nendpoint = \"https://collector.example/v1/logs\"\n",
    )
    .unwrap();
    match resolve(dir.path()) {
        Resolved::On(c) => assert!(!c.forward_requests, "the key unset reads off"),
        _ => panic!("enabled with headers resolves on"),
    }
    std::fs::write(
        dir.path().join("config.toml"),
        "[telemetry.otlp]\nenabled = true\nendpoint = \"https://collector.example/v1/logs\"\nforward_requests = true\n",
    )
    .unwrap();
    match resolve(dir.path()) {
        Resolved::On(c) => assert!(c.forward_requests),
        _ => panic!("the key set reads on"),
    }
    std::env::remove_var("FNO_CONFIG");
    std::env::remove_var("FNO_OTLP_HEADERS");
}

/// The window reads the stored rows read-only: a row outside the window
/// and an undated row answer nothing, and one millisecond under the
/// cutoff admits the boundary row the millisecond-formatted string floor
/// exists for. Failure: the SQL floor or the parse filter drops an
/// in-window row or admits an out-of-window one.
#[test]
fn requests_in_window_filters_by_ts() {
    let (_tmp, home) = home("fwd-window");
    let (db, conn) = seeded_db(&home);
    insert_row(&conn, "fresh", Some("2026-11-20T11:00:00Z"));
    insert_row(&conn, "boundary", Some("2026-11-20T09:59:59.999Z"));
    insert_row(&conn, "old", Some("2026-11-19T00:00:00Z"));
    insert_row(&conn, "undated", None);
    let cutoff = cutoff_ms();
    let got = requests_in_window(&db, cutoff).unwrap();
    let keys: Vec<&str> = got.iter().map(|r| r.dedupe_key.as_str()).collect();
    assert_eq!(
        keys,
        vec!["rid:fresh"],
        "in-window rows oldest first; out-of-window and undated rows stay home"
    );
    assert_eq!(requests_in_window(&db, cutoff - 1).unwrap().len(), 2);
}

/// The delivered phase: one in-window dated row ships, the payload
/// carries the identity and numbers under its own scope, absent fields
/// stay absent, and the state file records the delivery.
#[test]
fn requests_beat_delivers_and_records_the_batch() {
    let (_tmp, home) = home("fwd-deliver");
    let (db, conn) = seeded_db(&home);
    insert_row(&conn, "fresh", Some("2026-11-20T11:00:00Z"));
    insert_row(&conn, "undated", None);
    let cutoff = cutoff_ms();
    let now = cutoff + 3600 * 1000;
    let rows = requests_in_window(&db, cutoff).unwrap();
    let mut poster = Stub {
        status: Some(200),
        bodies: Vec::new(),
    };
    let report = run_requests_beat(&cfg(), &home, rows, &mut poster, now);
    assert_eq!(report.acted, 1, "only the in-window, dated row ships");
    let body = poster.bodies.first().unwrap();
    let payload: Value = serde_json::from_str(body).unwrap();
    let scope = payload.pointer("/resourceLogs/0/scopeLogs/0").unwrap();
    assert_eq!(
        scope.pointer("/scope/name").and_then(Value::as_str),
        Some("fno.api_requests")
    );
    let rec = scope.pointer("/logRecords/0").unwrap();
    assert_eq!(
        rec.pointer("/eventName").and_then(Value::as_str),
        Some("fno.api_request")
    );
    assert_eq!(s(&attr(rec, "session.id")), "sess-fwd");
    assert_eq!(d(&attr(rec, "fno.cost_usd")), Some(3.05));
    assert_eq!(i(&attr(rec, "fno.input_tokens")), Some(100));
    assert_eq!(s(&attr(rec, "skill.name")), "fno:target");
    assert!(
        attr(rec, "plugin.name").is_null(),
        "absent fields stay absent"
    );
    let state = state_json(&home);
    assert_eq!(
        state
            .pointer("/delivered")
            .and_then(Value::as_object)
            .map(|m| m.len()),
        Some(1)
    );
}

/// The redelivery reads caught_up: the shared delivered map dedupes.
#[test]
fn requests_beat_redelivery_reads_caught_up() {
    let (_tmp, home) = home("fwd-caughtup");
    let (db, conn) = seeded_db(&home);
    insert_row(&conn, "fresh", Some("2026-11-20T11:00:00Z"));
    let cutoff = cutoff_ms();
    let now = cutoff + 3600 * 1000;
    let mut poster = Stub {
        status: Some(200),
        bodies: Vec::new(),
    };
    let rows = requests_in_window(&db, cutoff).unwrap();
    run_requests_beat(&cfg(), &home, rows, &mut poster, now);
    let mut poster = Stub {
        status: Some(200),
        bodies: Vec::new(),
    };
    let rows = requests_in_window(&db, cutoff).unwrap();
    let report = run_requests_beat(&cfg(), &home, rows, &mut poster, now);
    assert_eq!(report.skip, Some("caught_up"));
}

/// A permanent 4xx drops the row for good; the dropped id never retries.
#[test]
fn a_permanent_4xx_drops_the_batch() {
    let (_tmp, home) = home("fwd-drop");
    let (db, conn) = seeded_db(&home);
    insert_row(&conn, "fresh", Some("2026-11-20T11:00:00Z"));
    let cutoff = cutoff_ms();
    let now = cutoff + 3600 * 1000;
    let rows = requests_in_window(&db, cutoff).unwrap();
    let mut poster = Stub {
        status: Some(400),
        bodies: Vec::new(),
    };
    let report = run_requests_beat(&cfg(), &home, rows, &mut poster, now);
    assert_eq!(report.acted, 1);
    let state = state_json(&home);
    assert_eq!(
        state
            .pointer("/dropped")
            .and_then(Value::as_object)
            .map(|m| m.len()),
        Some(1),
        "a permanent 4xx drops, never retries"
    );
}

/// A hold ships nothing, keeps the row pending, and names its leg in the
/// shared held_error so status_line attributes it.
#[test]
fn a_hold_keeps_rows_pending_and_names_the_leg() {
    let (_tmp, home) = home("fwd-hold");
    let (db, conn) = seeded_db(&home);
    insert_row(&conn, "fresh", Some("2026-11-20T11:00:00Z"));
    let cutoff = cutoff_ms();
    let now = cutoff + 3600 * 1000;
    let rows = requests_in_window(&db, cutoff).unwrap();
    let mut poster = Stub {
        status: None,
        bodies: Vec::new(),
    };
    let report = run_requests_beat(&cfg(), &home, rows, &mut poster, now);
    assert_eq!(report.acted, 0);
    let state = state_json(&home);
    let held = state
        .pointer("/held_error")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        held.starts_with("api_requests:"),
        "the hold names its leg: {held}"
    );
}

/// The receipt names its scope and record count, so an operator reading
/// receipts.jsonl can tell the two legs apart.
#[test]
fn receipts_carry_the_scope_field() {
    let (_tmp, home) = home("fwd-receipt");
    let (db, conn) = seeded_db(&home);
    insert_row(&conn, "fresh", Some("2026-11-20T11:00:00Z"));
    let cutoff = cutoff_ms();
    let now = cutoff + 3600 * 1000;
    let rows = requests_in_window(&db, cutoff).unwrap();
    let mut poster = Stub {
        status: Some(200),
        bodies: Vec::new(),
    };
    run_requests_beat(&cfg(), &home, rows, &mut poster, now);
    let receipts = std::fs::read_to_string(home.otlp_export_dir().join("receipts.jsonl")).unwrap();
    let line: Value = serde_json::from_str(receipts.lines().last().unwrap()).unwrap();
    assert_eq!(
        line.pointer("/scope").and_then(Value::as_str),
        Some("api_requests")
    );
    assert_eq!(line.pointer("/records").and_then(Value::as_i64), Some(1));
}
