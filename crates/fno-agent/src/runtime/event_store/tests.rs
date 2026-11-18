//! Store tests: ingest generations, v1-to-v2 migration, retention classes,
//! identity extraction, rejected-row preservation.

use super::*;
use serde_json::json;
use std::io::Write;

fn append(path: &Path, rows: &[serde_json::Value]) {
    let mut fh = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    for row in rows {
        writeln!(fh, "{row}").unwrap();
    }
}

fn checkin(ts: &str, scope: &str, change: &str) -> serde_json::Value {
    json!({"ts": ts, "type": "lead_checkin", "source": "loop",
           "data": {"scope": scope, "change": change}})
}

fn count_events(store: &Path) -> i64 {
    open_read(store)
        .unwrap()
        .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
        .unwrap()
}

fn count_type(store: &Path, event_type: &str) -> i64 {
    open_read(store)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM events WHERE type = ?1",
            params![event_type],
            |r| r.get(0),
        )
        .unwrap()
}

/// An ephemeral row that must survive import sits one hour old: import
/// prunes on a fresh store, so any fixed ts crosses the retention floor
/// and the row vanishes.
fn fresh_ts() -> String {
    (chrono::Utc::now() - chrono::Duration::hours(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[test]
fn rotation_overwrite_keeps_ingested_history() {
    // Once `.1` is replaced by the next generation, the rows only the store
    // holds are still there.
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    let rotated = dir.path().join("events.jsonl.1");
    append(
        &rotated,
        &[checkin("2026-09-09T08:00:00Z", "x-aaaa", "gen 1")],
    );
    append(&live, &[checkin("2026-09-10T08:00:00Z", "x-aaaa", "gen 2")]);
    sync(&live).unwrap();
    // The rename: gen 2 becomes the new .1, gen 3 lands live.
    std::fs::rename(&live, &rotated).unwrap();
    append(&live, &[checkin("2026-09-11T08:00:00Z", "x-aaaa", "gen 3")]);
    let receipt = sync(&live).unwrap();
    assert_eq!(receipt.ingested, 1, "only gen 3 is new");
    assert_eq!(count_events(&receipt.store), 3);
    assert_eq!(count_type(&receipt.store, "lead_checkin"), 3);
    // The alias table rides the same read: a pre-rename row surfaces to a
    // new-spelling query.
    append(
        &live,
        &[json!({"ts": "2026-09-12T08:00:00Z", "type": "lead_checkin",
                 "source": "loop", "data": {"scope": "x-aaaa", "change": "old spelling"}})],
    );
    sync(&live).unwrap();
    let hits = query_events(
        &live,
        &EventQuery {
            types: vec!["lead_checkin".into()],
            ..Default::default()
        },
    )
    .unwrap();
    let changes: Vec<String> = hits
        .iter()
        .filter_map(|r| serde_json::from_str::<serde_json::Value>(&r.line).ok())
        .filter_map(|v| v["data"]["change"].as_str().map(|c| c.to_string()))
        .collect();
    assert_eq!(
        changes,
        ["gen 1", "gen 2", "gen 3", "old spelling"],
        "the alias table must surface the pre-rename row beside the new ones"
    );
}

#[test]
fn prune_keeps_durable_and_gate_deletes_only_expired_ephemeral() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    append(
        &live,
        &[
            checkin("2026-05-01T08:00:00Z", "x-aaaa", "ancient durable"),
            checkin("2026-09-10T08:00:00Z", "x-aaaa", "fresh durable"),
            json!({"ts": "2026-05-01T08:00:00Z", "type": "review_attestation",
                   "source": "reviewer", "data": {"reviewer": "code-review",
                   "head_sha": "abc", "verdict": "pass"}}),
            json!({"ts": "2026-05-01T08:00:00Z", "type": "mux_pane_counters",
                   "source": "mux", "data": {"panes": []}}),
            json!({"ts": fresh_ts(), "type": "mux_pane_counters",
                   "source": "mux", "data": {"panes": []}}),
        ],
    );
    // A fresh store's first prune fires immediately (last_prune_ms starts at
    // 0), so the EXPIRED ephemeral row never survives even the first sync.
    let receipt = sync(&live).unwrap();
    let store = receipt.store;
    assert_eq!(count_events(&store), 4, "only the expired ephemeral left");
    assert_eq!(
        count_type(&store, "review_attestation"),
        1,
        "gate rows stay"
    );
    assert_eq!(count_type(&store, "lead_checkin"), 2, "durable rows stay");
    assert_eq!(
        count_type(&store, "mux_pane_counters"),
        1,
        "the fresh ephemeral row stays; the 2026-05-01 one is gone"
    );
    // Force the daily prune again; the fresh ephemeral row survives.
    let writable = Connection::open(&store).unwrap();
    writable
        .execute("DELETE FROM events_meta WHERE key = 'last_prune_ms'", [])
        .unwrap();
    drop(writable);
    sync(&live).unwrap();
    assert_eq!(
        count_events(&store),
        4,
        "a second prune deletes nothing new"
    );
    assert_eq!(count_type(&store, "mux_pane_counters"), 1);
}

#[test]
fn v1_store_migrates_in_place_oldest_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("events.db");
    let conn = Connection::open(&store).unwrap();
    conn.execute_batch(
        "CREATE TABLE events (
             row_hash BLOB PRIMARY KEY NOT NULL,
             ts_ms INTEGER NOT NULL,
             type TEXT NOT NULL,
             source TEXT NOT NULL,
             scope TEXT,
             reject_reason TEXT,
             line TEXT NOT NULL
         ) WITHOUT ROWID;
         CREATE INDEX events_scope_type_ts ON events(scope, type, ts_ms);
         CREATE TABLE ingest_cursor (
             dev INTEGER NOT NULL, ino INTEGER NOT NULL, head_hash BLOB NOT NULL,
             \"offset\" INTEGER NOT NULL, path TEXT NOT NULL, updated_ms INTEGER NOT NULL,
             PRIMARY KEY (dev, ino)
         );
         CREATE TABLE events_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )
    .unwrap();
    let lines = [
        checkin("2026-09-09T08:00:00Z", "x-aaaa", "older").to_string(),
        checkin("2026-09-10T12:00:00Z", "x-aaaa", "newest").to_string(),
        json!({"ts": "2026-05-01T08:00:00Z", "type": "review_attestation",
               "source": "reviewer", "data": {"reviewer": "code-review",
               "head_sha": "abc", "verdict": "pass"}})
        .to_string(),
    ];
    for line in &lines {
        let hash = Sha256::digest(line.as_bytes()).to_vec();
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        let ts = parse_rfc3339_ms(value["ts"].as_str().unwrap()).unwrap();
        let ty = if line.contains("review_attestation") {
            "review_attestation"
        } else {
            "lead_checkin"
        };
        conn.execute(
            "INSERT INTO events (row_hash, ts_ms, type, source, scope, reject_reason, line)
             VALUES (?1, ?2, ?3, 'test', NULL, NULL, ?4)",
            params![hash, ts, ty, line],
        )
        .unwrap();
    }
    drop(conn);

    // Opening through the store migrates in place.
    let live = dir.path().join("events.jsonl");
    let receipt = sync(&live).unwrap();
    assert_eq!(receipt.store, store);
    assert_eq!(count_events(&store), 3, "migration loses no row");
    let conn = open_read(&store).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let mut stmt = conn
        .prepare("SELECT seq, ts_ms, event_id, retention_class, line FROM events ORDER BY seq")
        .unwrap();
    let rows: Vec<(i64, i64, String, String, String)> = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), 3);
    // Oldest-first seq, deterministic even though v1 had no order.
    let tss: Vec<i64> = rows.iter().map(|r| r.1).collect();
    let mut sorted = tss.clone();
    sorted.sort();
    assert_eq!(tss, sorted, "seq follows ts order");
    for (seq, row) in rows.iter().enumerate() {
        assert_eq!(row.0 as usize, seq + 1, "seq is 1-based and dense");
        let expected_hash = Sha256::digest(row.4.as_bytes()).to_vec();
        assert_eq!(row.2, format!("legacy:{}", hex(&expected_hash)));
    }
    assert!(
        rows.iter().any(|r| r.3 == "gate"),
        "the attestation row carries its gate class"
    );
    // A second open is a no-op, never a rebuild.
    drop(stmt);
    drop(conn);
    sync(&live).unwrap();
    assert_eq!(count_events(&store), 3);
}

#[test]
fn future_schema_is_refused_by_writers_and_readers_without_downgrade() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    let store = store_path(&live);
    let conn = Connection::open(&store).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE events {EVENTS_V2_COLUMNS}; PRAGMA user_version = {};",
        SCHEMA_VERSION + 1
    ))
    .unwrap();
    drop(conn);

    let line = checkin("2026-09-10T12:00:00Z", "x-aaaa", "future").to_string();
    let write_error = append_envelope(&live, &line, None).unwrap_err().to_string();
    assert!(write_error.contains("newer than this build understands"));

    let read_error = query_events(&live, &EventQuery::default())
        .unwrap_err()
        .to_string();
    assert!(read_error.contains("newer than this build understands"));

    // AC2-EDGE: the typed variant a caller can match, same refusal.
    assert!(matches!(
        query_events(&live, &EventQuery::default()),
        Err(EventStoreError::NewerSchema {
            found,
            supported,
            ..
        }) if found == SCHEMA_VERSION + 1 && supported == SCHEMA_VERSION
    ));

    let conn = Connection::open(&store).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION + 1);
    let rows: i64 = conn
        .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn append_refuses_newline_and_bad_scope_and_bad_ts() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    assert!(append_envelope(&live, "{\"a\":1}\n{\"b\":2}", None).is_err());
    let bad_scope = json!({"ts": "2026-09-17T12:00:00Z", "type": "lead_checkin",
        "source": "loop", "data": {"scope": "x-1 ready, two words", "change": "one"}})
    .to_string();
    let err = append_envelope(&live, &bad_scope, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("canonical team scope"), "err: {err}");
    let bad_ts = json!({"ts": "not-a-time", "type": "lead_checkin",
        "source": "loop", "data": {"scope": "x-1", "change": "one"}})
    .to_string();
    let err = append_envelope(&live, &bad_ts, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("RFC3339"), "err: {err}");
    assert!(
        !store_path(&live).exists(),
        "refused writes never create a store"
    );
}

#[test]
fn journal_text_checked_errs_on_a_broken_store_and_journal_text_falls_back() {
    // AC1-ERR
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    append(&live, &[checkin("2026-09-10T12:00:00Z", "x-aaaa", "live")]);
    std::fs::write(dir.path().join("events.db"), b"not a database").unwrap();
    let err = journal_text_checked(&live, &EventQuery::of_types(&["lead_checkin"]))
        .unwrap_err()
        .to_string();
    assert!(err.contains("events.db"), "err names the store path: {err}");
    assert_eq!(
        journal_text(&live, &["lead_checkin"]),
        std::fs::read_to_string(&live).unwrap()
    );
}

#[test]
fn a_lost_row_is_dead_lettered_by_type_and_busy_locks_are_matched() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("events.jsonl");
    let store = store_path(&journal);
    // Junk in the store file fails the open's schema probe, so the commit
    // errors and the row is lost for real.
    std::fs::write(&store, b"junk".repeat(50)).unwrap();
    let envelope = json!({
        "ts": "2026-10-07T00:00:00Z",
        "type": "claim_released",
        "source": "fno-loop",
        "data": {
            "session_id": "s-1",
            "key": "node:x-1:claude:s-1",
            "holder": "s-1",
            "pid": 4242,
            "host": "test-host",
            "acquired_at": "2026-10-06T00:00:00Z",
            "duration_held_ms": 60000,
        },
    })
    .to_string();
    let error = append_envelope(&journal, &envelope, None)
        .unwrap_err()
        .to_string();
    // The open's schema probe fails NOTADB, so the diagnostic is the bare
    // named() string, not the corrupt-image form.
    assert!(error.contains("file is not a database"), "{error}");

    let sidecar = PathBuf::from(format!("{}.lost.jsonl", store.display()));
    let line = std::fs::read_to_string(&sidecar).unwrap();
    assert!(line.contains("\"type\":\"claim_released\""), "{line}");

    assert!(lock_busy(".../events.db: database is locked"));
    assert!(!lock_busy(".../events.db: file is not a database"));
    assert!(!lock_busy(""));
}

#[test]
fn query_runs_folds_two_carriers_to_the_newest_and_drops_a_covered_import() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("events.jsonl");
    append(
        &journal,
        &[
            json!({"ts":"2026-01-01T00:00:01Z","type":"run_recorded","source":"target",
                   "data":{"run":{"fno_id":"s1","cost_usd":1.0}}}),
            json!({"ts":"2026-01-01T00:00:02Z","type":"run_recorded","source":"target",
                   "data":{"run":{"fno_id":"s1","cost_usd":2.0,"verifier_verdict":"pass"}}}),
            json!({"ts":"2026-01-01T00:00:03Z","type":"run_imported","source":"migration",
                   "data":{"run":{"fno_id":"s1","cost_usd":9.0}}}),
            json!({"ts":"2026-01-01T00:00:04Z","type":"run_imported","source":"migration",
                   "data":{"run":{"fno_id":"s2","cost_usd":5.0}}}),
        ],
    );
    import_all(&journal).unwrap();
    let runs = runs::query_runs(&journal, &runs::RunFilter::default()).unwrap();
    assert_eq!(runs.len(), 2, "newest carrier wins; covered import drops");
    assert_eq!(runs[0]["fno_id"], "s1");
    assert_eq!(runs[0]["cost_usd"], 2.0);
    assert_eq!(runs[0]["verifier_verdict"], "pass");
    assert_eq!(runs[1]["fno_id"], "s2", "uncovered import still answers");
}

mod coverage;
mod observation;
