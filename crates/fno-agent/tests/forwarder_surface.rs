//! The forwarder legs at their real boundary: the resolved posture, the
//! read-only window over the otel store, and each beat's delivery rule
//! against an injected poster. These drive eval modules from outside the
//! G3 scope, which docs/test-audit/scopes.tsv counts in their own scope
//! (G3 keeps cap 0; this file's path matches no G3 row and lands in G2).
//! The project env hook boundary lives here too: folder data may not run
//! code or steer fno.

use fno_agent::attention_http::{HttpPost, PostResult};
use fno_agent::eval::otel_ingest::requests_in_window;
use fno_agent::eval::otlp_export::{resolve, run_beat, run_requests_beat, OtlpConfig, Resolved};
use fno_agent::feed::FeedRow;
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

/// A held feed beat names its leg in the shared held_error the same way,
/// so status_line attributes the hold when both legs share one state file.
#[test]
fn a_held_feed_beat_names_its_leg() {
    let (_tmp, home) = home("fwd-feed-hold");
    let now = cutoff_ms() + 3600 * 1000;
    let rows = vec![FeedRow {
        ts: "2026-11-20T11:00:00Z".to_string(),
        ..Default::default()
    }];
    let mut poster = Stub {
        status: None,
        bodies: Vec::new(),
    };
    let report = run_beat(&cfg(), &home, rows, &mut poster, now);
    assert_eq!(report.acted, 0);
    let state = state_json(&home);
    let held = state
        .pointer("/held_error")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(held.starts_with("feed:"), "the hold names its leg: {held}");
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

fn bash_path() -> std::path::PathBuf {
    std::env::var_os("FNO_TEST_BASH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/bin/bash"))
}

fn zsh_path() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("FNO_TEST_ZSH") {
        return Some(std::path::PathBuf::from(p));
    }
    ["/bin/zsh", "/usr/bin/zsh"]
        .iter()
        .map(std::path::PathBuf::from)
        .find(|p| p.exists())
}

/// A project env file is untrusted folder data at the hook boundary:
/// keys must be plain shell names, reserved controls drop, values
/// round-trip exactly through the emission form, a forged name list
/// never survives, and FNO_PROJECT_ENV_KEYS lands last. Integer shell
/// variables (HISTCMD, OPTIND, RANDOM, SRANDOM) evaluate on assignment,
/// so the emission must unset the name first; tilde and a leading
/// equals stay literal; no emission deletes a function. Failure: the
/// hook writes raw keys or an evaluating form, so sourcing the emitted
/// file runs folder data, steers a reserved control, or loses a value.
/// Each arm runs under bash and, where present, under zsh. The emitted
/// file must also parse: the pre-fix replacement turned a quoted value
/// into a syntax error under bash 3.2.
#[test]
fn project_env_file_cannot_steer_fno() {
    let hook =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../hooks/worktree-env-reload.sh");
    let ws = tempfile::tempdir().unwrap();
    let env_out = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();

    std::fs::write(
        ws.path().join(".env"),
        concat!(
            "# a comment\n",
            "APP_TOKEN=plain-value\n",
            "QUOTED=\"has spaces and 'a quote'\"\n",
            "EVIL_VAL=a'; touch fno-hook-marker; echo 'b\n",
            "DASH-KEY=1\n",
            "X$(touch fno-hook-marker)=plain\n",
            "HISTCMD=x$(touch fno-hook-marker)y\n",
            "OPTIND=x$(touch fno-hook-marker)y\n",
            "RANDOM=x$(touch fno-hook-marker)y\n",
            "SRANDOM=x$(touch fno-hook-marker)y\n",
            "TILDE_VAL=~/steered\n",
            "EQ_VAL==touch fno-hook-marker\n",
            "ZZ_LAST=ok\n",
            "FNO_CONFIG=/steered/config\n",
            "CLAUDE_ENV_FILE=steered\n",
            "PATH=/steered/bin\n",
            "DO_NOT_TRACK=steered\n",
            "PROMPT_COMMAND=steered\n",
            "FPATH=/steered/fpath\n",
            "NODE_OPTIONS=--steered\n",
            "PYTHONSTARTUP=/steered.py\n",
            "ANTHROPIC_BASE_URL=https://steered\n",
            "GH_HOST=steered\n",
            "HTTPS_PROXY=steered\n",
            "FNO_PROJECT_ENV_KEYS=FORGED\n",
        ),
    )
    .unwrap();

    let env_file = env_out.path().join("env");
    let hook_child = std::process::Command::new(bash_path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("CLAUDE_ENV_FILE", &env_file)
        .arg(&hook)
        .current_dir(ws.path())
        .spawn()
        .expect("spawn the hook");
    let run = hook_child.wait_with_output().expect("reap the hook");
    assert!(
        run.status.success(),
        "the hook runs clean: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let emitted = std::fs::read_to_string(&env_file).unwrap();
    let last = emitted.lines().last().unwrap();
    assert_eq!(
        last,
        "export FNO_PROJECT_ENV_KEYS=\"APP_TOKEN EQ_VAL EVIL_VAL HISTCMD OPTIND QUOTED RANDOM SRANDOM TILDE_VAL ZZ_LAST\"",
        "the sorted allowed names land last: {last}"
    );

    let mut shells = vec![bash_path()];
    if let Some(zsh) = zsh_path() {
        shells.push(zsh);
    }
    for sh in &shells {
        let label = sh.display().to_string();
        let parse_child = std::process::Command::new(sh)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .arg("-n")
            .arg(&env_file)
            .spawn()
            .expect("spawn the parse check");
        let parsed = parse_child
            .wait_with_output()
            .expect("reap the parse check");
        assert!(
            parsed.status.success(),
            "the emitted file parses under {label}: {}",
            String::from_utf8_lossy(&parsed.stderr)
        );

        let source_child = std::process::Command::new(sh)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .arg("-c")
            .arg(
                "EVIL_VAL() { echo kept; }; . \"$1\"; \
                 printf '%s\\n' \"V=$EVIL_VAL\"; \
                 printf '%s\\n' \"F=$(EVIL_VAL 2>/dev/null || echo gone)\"; env",
            )
            .arg("_")
            .arg(&env_file)
            .current_dir(ws.path())
            .env("HOME", home.path())
            .spawn()
            .expect("spawn the sourcing child");
        let sourced = source_child
            .wait_with_output()
            .expect("reap the sourcing child");
        assert!(
            sourced.status.success(),
            "sourcing exits 0 under {label}: {}",
            String::from_utf8_lossy(&sourced.stderr)
        );
        let out = String::from_utf8_lossy(&sourced.stdout).to_string();

        let markers: Vec<std::path::PathBuf> = std::fs::read_dir(ws.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("fno-hook-marker"))
            })
            .collect();
        assert!(
            markers.is_empty(),
            "no hostile key or value ran under {label}: {markers:?}"
        );
        for denied in [
            "FNO_CONFIG=/steered/config",
            "CLAUDE_ENV_FILE=steered",
            "PATH=/steered/bin",
            "DO_NOT_TRACK=steered",
            "PROMPT_COMMAND=steered",
            "FPATH=/steered/fpath",
            "NODE_OPTIONS=--steered",
            "PYTHONSTARTUP=/steered.py",
            "ANTHROPIC_BASE_URL=https://steered",
            "GH_HOST=steered",
            "HTTPS_PROXY=steered",
            "FNO_PROJECT_ENV_KEYS=FORGED",
        ] {
            assert!(
                !out.contains(denied),
                "a reserved control survived under {label}: {denied}"
            );
        }

        assert!(
            out.starts_with("V=a'; touch fno-hook-marker; echo 'b\nF=kept\n"),
            "the hostile value stays literal and the function survives under {label}"
        );

        let value = |name: &str| -> String {
            let prefix = format!("{name}=");
            let line = out
                .lines()
                .find(|l| l.starts_with(&prefix))
                .unwrap_or_else(|| panic!("{name} missing from the sourced env under {label}"));
            line[prefix.len()..].to_string()
        };
        assert_eq!(value("APP_TOKEN"), "plain-value");
        assert_eq!(value("QUOTED"), "has spaces and 'a quote'");
        assert_eq!(
            value("ZZ_LAST"),
            "ok",
            "the last fixture row loads in every shell under {label}"
        );
        let is_bash = sh
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("bash"))
            .unwrap_or(false);
        let present = |name: &str| -> Option<String> {
            let prefix = format!("{name}=");
            out.lines()
                .find(|l| l.starts_with(&prefix))
                .map(|l| l[prefix.len()..].to_string())
        };
        for name in ["HISTCMD", "OPTIND", "RANDOM", "SRANDOM"] {
            match (is_bash, present(name)) {
                (true, got) => assert_eq!(
                    got.as_deref(),
                    Some("x$(touch fno-hook-marker)y"),
                    "an integer-name value stays literal under {label}"
                ),
                // zsh refuses some specials at the trial, so the name may
                // stay unset; if it loads, it must be the literal.
                (false, Some(got)) => assert_eq!(
                    got, "x$(touch fno-hook-marker)y",
                    "an integer-name value stays literal under {label}"
                ),
                (false, None) => {}
            }
        }
        assert_eq!(
            value("TILDE_VAL"),
            "~/steered",
            "tilde stays literal under {label}"
        );
        assert_eq!(
            value("EQ_VAL"),
            "=touch fno-hook-marker",
            "a leading equals stays literal under {label}"
        );
    }
}
