//! `fno doctor migrate ids --apply` and `--rollback <dir>`: every retired
//! q-/d- hex key becomes its number, in place, across the stores that hold it.
//!
//! The event dbs are the record (their journals keep one rotation
//! generation), so nothing is rebuilt: each changed row is rewritten where it
//! sits. Each change lands in `undo.jsonl` in the dated backup dir before the
//! next one runs. Rollback replays the log newest first and reverts a change
//! only while its value still reads what apply wrote: a later write survives
//! and is named. A failed apply rolls itself back the same way.

use super::super::{current_store, db_path, mint_local, mint_remote, pull, DDL};
use super::{plan_map, store_keys};
use regex::Regex;
use rusqlite::types::{Value as Sql, ValueRef};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// A text that may hold a key: the SQL side of [`rewrite`]'s narrowing.
const KEY_GLOB: &str = "*[dq]-[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]*";

const MAP_DDL: &str =
    "CREATE TABLE IF NOT EXISTS fno_id_map (hex TEXT PRIMARY KEY, label TEXT NOT NULL)";

/// The free-text columns of the shared backlog tables that cite a decision
/// or question by id. Key columns move through their own steps.
const TEXT_CELLS: &[(&str, &[&str])] = &[
    (
        "nodes",
        &["title", "description", "completion_note", "deferred_reason"],
    ),
    ("comments", &["body", "details"]),
];

fn token() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"[dq]-[0-9a-f]{8}(?:@[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?)?")
            .expect("token regex")
    })
}

/// The text rewrite every store shares: a mapped key, with or without a
/// copy's `@host` tail, becomes its number. A token glued to a word on either
/// side (a sha, a longer hex run) is not a key and stays as typed, and so
/// does a key the map does not hold.
pub(crate) fn rewrite<'t>(map: &BTreeMap<String, String>, text: &'t str) -> Cow<'t, str> {
    let mut out = String::new();
    let mut last = 0;
    for found in token().find_iter(text) {
        let glued_before = text[..found.start()]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric());
        let glued_after = text[found.end()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric());
        let Some(label) = map
            .get(&found.as_str()[..10])
            .filter(|_| !glued_before && !glued_after)
        else {
            continue;
        };
        out.push_str(&text[last..found.start()]);
        out.push_str(label);
        last = found.end();
    }
    if last == 0 {
        return Cow::Borrowed(text);
    }
    out.push_str(&text[last..]);
    Cow::Owned(out)
}

fn sha(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

fn sha_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// One change apply made, with what rollback needs to take it back.
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Undo {
    /// A text cell rewritten in place.
    Cell {
        db: PathBuf,
        table: String,
        column: String,
        rowid: i64,
        before: String,
        after: String,
    },
    /// An event row's line rewritten; `row_hash` follows the line.
    Line {
        db: PathBuf,
        table: String,
        rowid: i64,
        before: String,
        after_hash: String,
    },
    /// A deleted row, every column kept: an event row whose rewrite repeats
    /// another row's line, or a numbers row whose key took another number.
    Gone {
        db: PathBuf,
        table: String,
        row: BTreeMap<String, Value>,
    },
    /// A numbers row whose key moved from the hex to its number.
    Rekey {
        db: PathBuf,
        store: String,
        kind: String,
        num: i64,
        before: String,
        after: String,
    },
    /// A decision whose key moved from `from` to `to`.
    Moved {
        graph: PathBuf,
        from: String,
        to: String,
        before: String,
        after: String,
    },
    /// A map row apply wrote.
    Mapped { db: PathBuf, hex: String },
    /// A file rewritten, and renamed when its name held a key. Its bytes
    /// before apply sit at `backup`.
    File {
        from: PathBuf,
        to: PathBuf,
        backup: PathBuf,
        after: String,
    },
}

/// The undo log. A change is staged while its transaction runs and lands
/// in `undo.jsonl` only once that transaction commits, so a rolled-back
/// transaction leaves no entry to undo.
struct Log {
    dir: PathBuf,
    file: std::fs::File,
    entries: Vec<Undo>,
    staged: Vec<Undo>,
    copies: usize,
}

impl Log {
    fn open(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir.join("files")).map_err(|e| e.to_string())?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("undo.jsonl"))
            .map_err(|e| format!("open {}: {e}", dir.join("undo.jsonl").display()))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            file,
            entries: Vec::new(),
            staged: Vec::new(),
            copies: 0,
        })
    }

    fn push(&mut self, undo: Undo) -> Result<(), String> {
        self.staged.push(undo);
        Ok(())
    }

    /// Write every staged entry: its change is now durable.
    fn commit(&mut self) -> Result<(), String> {
        for undo in self.staged.drain(..) {
            let line = serde_json::to_string(&undo).map_err(|e| e.to_string())?;
            writeln!(self.file, "{line}").map_err(|e| e.to_string())?;
            self.entries.push(undo);
        }
        Ok(())
    }

    fn backup_path(&mut self, name: &str) -> PathBuf {
        self.copies += 1;
        self.dir
            .join("files")
            .join(format!("{}-{name}", self.copies))
    }
}

fn cell_json(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => json!(f),
        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
        ValueRef::Blob(b) => {
            json!({ "blob": b.iter().map(|x| format!("{x:02x}")).collect::<String>() })
        }
    }
}

fn cell_sql(value: &Value) -> Sql {
    match value {
        Value::Null => Sql::Null,
        Value::Number(n) => n
            .as_i64()
            .map(Sql::Integer)
            .unwrap_or_else(|| Sql::Real(n.as_f64().unwrap_or_default())),
        Value::String(s) => Sql::Text(s.clone()),
        Value::Object(o) => {
            let hex = o.get("blob").and_then(Value::as_str).unwrap_or_default();
            let bytes = (0..hex.len() / 2)
                .filter_map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok())
                .collect();
            Sql::Blob(bytes)
        }
        other => Sql::Text(other.to_string()),
    }
}

/// Every column of one row, by rowid.
fn whole_row(
    conn: &Connection,
    table: &str,
    rowid: i64,
) -> Result<BTreeMap<String, Value>, String> {
    let mut stmt = conn
        .prepare(&format!("SELECT * FROM \"{table}\" WHERE rowid = ?1"))
        .map_err(|e| e.to_string())?;
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    stmt.query_row(params![rowid], |row| {
        let mut out = BTreeMap::new();
        for (i, name) in names.iter().enumerate() {
            out.insert(name.clone(), cell_json(row.get_ref(i)?));
        }
        Ok(out)
    })
    .map_err(|e| e.to_string())
}

fn insert_row(conn: &Connection, table: &str, row: &BTreeMap<String, Value>) -> Result<(), String> {
    let names: Vec<&String> = row.keys().collect();
    let sql = format!(
        "INSERT OR IGNORE INTO \"{table}\" ({}) VALUES ({})",
        names
            .iter()
            .map(|n| format!("\"{n}\""))
            .collect::<Vec<_>>()
            .join(", "),
        (1..=names.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    conn.execute(&sql, params_from_iter(row.values().map(cell_sql)))
        .map(drop)
        .map_err(|e| e.to_string())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    conn.prepare(&format!("SELECT \"{column}\" FROM \"{table}\" LIMIT 0"))
        .is_ok()
}

/// What one apply rewrote, per store, for the receipt.
#[derive(Default, Serialize)]
pub(crate) struct Tally {
    decisions: usize,
    numbers: usize,
    cells: usize,
    event_rows: usize,
    duplicates: usize,
    files: usize,
}

struct Apply<'m> {
    map: &'m BTreeMap<String, String>,
    log: Log,
    tally: Tally,
}

impl Apply<'_> {
    fn text<'t>(&self, text: &'t str) -> Cow<'t, str> {
        rewrite(self.map, text)
    }

    /// The local numbers table: each key takes its number as its key, a row
    /// that names another number for a mapped key goes, and the map lands
    /// beside it so a hex that comes back reads as its number.
    fn numbers(&mut self, db: &Path, current: &str) -> Result<(), String> {
        let map = self.map;
        let mut conn = Connection::open(db).map_err(|e| e.to_string())?;
        conn.execute_batch(&format!("{DDL};{MAP_DDL}"))
            .map_err(|e| e.to_string())?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let rows: Vec<(i64, String, String, i64, String)> = {
            let mut stmt = tx
                .prepare("SELECT rowid, store, kind, num, key FROM fno_numbers")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
        };
        for (rowid, store, kind, num, key) in rows {
            let Some(label) = map.get(&key) else {
                continue;
            };
            let own = format!("{}{num}", kind.to_uppercase());
            if store == current && own == *label {
                tx.execute(
                    "UPDATE fno_numbers SET key = ?1 WHERE rowid = ?2",
                    params![label, rowid],
                )
                .map_err(|e| e.to_string())?;
                self.log.push(Undo::Rekey {
                    db: db.to_path_buf(),
                    store,
                    kind,
                    num,
                    before: key,
                    after: label.clone(),
                })?;
            } else {
                let row = whole_row(&tx, "fno_numbers", rowid)?;
                tx.execute("DELETE FROM fno_numbers WHERE rowid = ?1", params![rowid])
                    .map_err(|e| e.to_string())?;
                self.log.push(Undo::Gone {
                    db: db.to_path_buf(),
                    table: "fno_numbers".into(),
                    row,
                })?;
            }
            self.tally.numbers += 1;
        }
        for (hex, label) in map {
            let wrote = tx
                .execute(
                    "INSERT OR IGNORE INTO fno_id_map (hex, label) VALUES (?1, ?2)",
                    params![hex, label],
                )
                .map_err(|e| e.to_string())?;
            if wrote > 0 {
                self.log.push(Undo::Mapped {
                    db: db.to_path_buf(),
                    hex: hex.clone(),
                })?;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        self.log.commit()
    }

    /// The shared backlog tables. A decision's key moves in two flushes, so
    /// the primary (foreign keys on, deletes sent first) never sees a join
    /// row point at a missing decision: first the copy under the number and
    /// the repointed joins, then the delete of the hex row.
    fn backlog(&mut self, graph: &Path) -> Result<(), String> {
        let map = self.map;
        let db = crate::backlog::database_path(graph);
        let mut conn = crate::backlog::open_holding_lock(graph).map_err(|e| e.to_string())?;
        let rows: Vec<(i64, String, String)> = {
            let mut stmt = conn
                .prepare("SELECT rowid, event_id, data FROM decisions ORDER BY seq")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
        };
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let mut moved = Vec::new();
        for (rowid, event_id, data) in rows {
            let after = self.text(&data).into_owned();
            match map.get(&event_id) {
                Some(label) => {
                    if copy_decision(&tx, &event_id, label, &after)? {
                        moved.push(event_id.clone());
                        self.log.push(Undo::Moved {
                            graph: graph.to_path_buf(),
                            from: event_id,
                            to: label.clone(),
                            before: data,
                            after,
                        })?;
                        self.tally.decisions += 1;
                    }
                }
                None if after != data => {
                    tx.execute(
                        "UPDATE decisions SET data = ?1 WHERE rowid = ?2",
                        params![after, rowid],
                    )
                    .map_err(|e| e.to_string())?;
                    self.log.push(Undo::Cell {
                        db: db.clone(),
                        table: "decisions".into(),
                        column: "data".into(),
                        rowid,
                        before: data,
                        after,
                    })?;
                    self.tally.cells += 1;
                }
                None => {}
            }
        }
        for (table, columns) in TEXT_CELLS {
            for column in *columns {
                if !has_column(&tx, table, column) {
                    continue;
                }
                let cells: Vec<(i64, String)> = {
                    let mut stmt = tx
                        .prepare(&format!(
                            "SELECT rowid, \"{column}\" FROM \"{table}\" \
                             WHERE \"{column}\" GLOB '{KEY_GLOB}'"
                        ))
                        .map_err(|e| e.to_string())?;
                    let rows = stmt
                        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                        .map_err(|e| e.to_string())?;
                    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
                };
                for (rowid, before) in cells {
                    let after = self.text(&before).into_owned();
                    if after == before {
                        continue;
                    }
                    tx.execute(
                        &format!("UPDATE \"{table}\" SET \"{column}\" = ?1 WHERE rowid = ?2"),
                        params![after, rowid],
                    )
                    .map_err(|e| e.to_string())?;
                    self.log.push(Undo::Cell {
                        db: db.clone(),
                        table: table.to_string(),
                        column: column.to_string(),
                        rowid,
                        before,
                        after,
                    })?;
                    self.tally.cells += 1;
                }
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        self.log.commit()?;
        crate::backlog_share::flush(graph)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        for from in &moved {
            drop_decision(&tx, from)?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        crate::backlog_share::flush(graph).map(drop)
    }

    /// One event db: every row whose line names a mapped key gets the new
    /// line and its hash. A row whose new line another row already holds is
    /// that row again under the store's own dedupe rule, so it goes.
    ///
    /// The scan reads outside any write lock: an events.db can run to a
    /// gigabyte, and a writer held that long loses its row to the dead-letter
    /// file. Each write then matches the line it read, so a row that moved
    /// since the scan is left for the next apply.
    fn events(&mut self, db: &Path) -> Result<(), String> {
        let mut conn = Connection::open(db).map_err(|e| e.to_string())?;
        conn.busy_timeout(std::time::Duration::from_secs(10))
            .map_err(|e| e.to_string())?;
        let mut changes = Vec::new();
        for table in ["events", "event_observation_pending"] {
            if !has_column(&conn, table, "line") || !has_column(&conn, table, "row_hash") {
                continue;
            }
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT rowid, line FROM {table} WHERE line GLOB '{KEY_GLOB}'"
                ))
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| e.to_string())?;
            for row in rows {
                let (rowid, before) = row.map_err(|e| e.to_string())?;
                let after = self.text(&before).into_owned();
                if after != before {
                    changes.push((table, rowid, before, after));
                }
            }
        }
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        for (table, rowid, before, after) in changes {
            let hash = sha(after.as_bytes());
            let taken: bool = tx
                .query_row(
                    &format!("SELECT EXISTS (SELECT 1 FROM {table} WHERE row_hash = ?1)"),
                    params![hash],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if taken {
                let row = whole_row(&tx, table, rowid)?;
                let gone = tx
                    .execute(
                        &format!("DELETE FROM {table} WHERE rowid = ?1 AND line = ?2"),
                        params![rowid, before],
                    )
                    .map_err(|e| e.to_string())?;
                if gone > 0 {
                    self.log.push(Undo::Gone {
                        db: db.to_path_buf(),
                        table: table.into(),
                        row,
                    })?;
                    self.tally.duplicates += 1;
                }
                continue;
            }
            let wrote = tx
                .execute(
                    &format!(
                        "UPDATE {table} SET line = ?1, row_hash = ?2 WHERE rowid = ?3 AND line = ?4"
                    ),
                    params![after, hash, rowid, before],
                )
                .map_err(|e| e.to_string())?;
            if wrote > 0 {
                self.log.push(Undo::Line {
                    db: db.to_path_buf(),
                    table: table.into(),
                    rowid,
                    before,
                    after_hash: sha_hex(after.as_bytes()),
                })?;
                self.tally.event_rows += 1;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        self.log.commit()
    }

    /// One file: the text rewritten, and the name too when `rename`. The
    /// bytes before land in the backup dir first. A new name another file
    /// already holds refuses.
    fn file(&mut self, from: &Path, rename: bool) -> Result<(), String> {
        let Ok(before) = std::fs::read_to_string(from) else {
            return Ok(());
        };
        let name = from
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let after = self.text(&before).into_owned();
        let new_name = if rename {
            self.text(&name).into_owned()
        } else {
            name.clone()
        };
        if after == before && new_name == name {
            return Ok(());
        }
        let to = from.with_file_name(&new_name);
        if to != from && to.exists() {
            return Err(format!(
                "{} would rename onto {}, which exists",
                from.display(),
                to.display()
            ));
        }
        let backup = self.log.backup_path(&name);
        std::fs::copy(from, &backup).map_err(|e| format!("back up {}: {e}", from.display()))?;
        write_atomic(&to, &after)?;
        if to != from {
            std::fs::remove_file(from).map_err(|e| e.to_string())?;
        }
        self.log.push(Undo::File {
            from: from.to_path_buf(),
            to,
            backup,
            after: sha_hex(after.as_bytes()),
        })?;
        self.log.commit()?;
        self.tally.files += 1;
        Ok(())
    }
}

/// The copy half of a decision's key move: a row under `to` with `data`,
/// and every join row repointed. A `to` row a stopped move already wrote
/// keeps its data and still takes the joins. False when no `to` row exists.
fn copy_decision(conn: &Connection, from: &str, to: &str, data: &str) -> Result<bool, String> {
    let columns: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info('decisions') WHERE name NOT IN ('seq', 'event_id', 'data')")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
    };
    let list = columns
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    conn.execute(
        &format!(
            "INSERT INTO decisions (event_id, data, {list}) \
             SELECT ?2, ?3, {list} FROM decisions WHERE event_id = ?1 \
             AND NOT EXISTS (SELECT 1 FROM decisions WHERE event_id = ?2)"
        ),
        params![from, to, data],
    )
    .map_err(|e| e.to_string())?;
    let held: bool = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM decisions WHERE event_id = ?1)",
            params![to],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !held {
        return Ok(false);
    }
    conn.execute(
        "UPDATE node_decisions SET event_id = ?2 WHERE event_id = ?1",
        params![from, to],
    )
    .map_err(|e| e.to_string())?;
    Ok(true)
}

/// The delete half: the old row goes once no join row names it.
fn drop_decision(conn: &Connection, from: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM decisions WHERE event_id = ?1 \
         AND NOT EXISTS (SELECT 1 FROM node_decisions WHERE event_id = ?1)",
        params![from],
    )
    .map(drop)
    .map_err(|e| e.to_string())
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_file_name(format!(
        ".{}.migrate-ids",
        path.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    ));
    std::fs::write(&tmp, text).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("replace {}: {e}", path.display()))
}

/// The event dbs under `dir`: every `*.db` with an `events` table.
fn event_dbs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "db"))
        .filter(|p| {
            Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .is_ok_and(|c| has_column(&c, "events", "line"))
        })
        .collect();
    out.sort();
    out
}

/// The journals an event db ingests, from its own cursor rows.
fn journals(db: &Path) -> Vec<PathBuf> {
    let Ok(conn) = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare("SELECT DISTINCT path FROM ingest_cursor") else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |r| r.get::<_, String>(0));
    rows.map(|rows| {
        rows.flatten()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .collect()
    })
    .unwrap_or_default()
}

/// Every markdown file under `root`, dot dirs skipped.
fn markdown(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for path in entries.flatten().map(|e| e.path()) {
        let hidden = path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.'));
        if hidden {
            continue;
        }
        if path.is_dir() && !path.is_symlink() {
            markdown(&path, out);
        } else if path.extension().is_some_and(|x| x == "md") {
            out.push(path);
        }
    }
}

/// A consistent copy of each sqlite store, the last resort beside the log.
fn back_up_dbs(dir: &Path, dbs: &[PathBuf]) -> Result<(), String> {
    for (n, db) in dbs.iter().enumerate() {
        let name = db
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let conn = Connection::open(db).map_err(|e| format!("open {}: {e}", db.display()))?;
        // Two dirs can each hold an `events.db`: the index keeps both copies.
        let into = dir
            .join(format!("{n}-{name}"))
            .to_string_lossy()
            .into_owned();
        conn.execute("VACUUM INTO ?1", params![into])
            .map_err(|e| format!("back up {}: {e}", db.display()))?;
    }
    Ok(())
}

/// The numbers for the later answers of one question: each restates a
/// ruling the first answer made, and is its own record with its own number.
fn mint_fresh(graph_db: &Path, keys: &[String]) -> Result<(), String> {
    if keys.is_empty() {
        return Ok(());
    }
    let conn = super::super::open_write_at(graph_db)?;
    let host = fno_mail::address::normalize_host(&crate::claims::hostname());
    match crate::store_remote::configured()? {
        Some(remote) => {
            mint_remote(&remote, keys, &host)?;
            pull(&remote, &conn).map(drop)
        }
        None => keys
            .iter()
            .try_for_each(|key| mint_local(&conn, key, super::super::LOCAL_STORE, &host)),
    }
}

/// `--apply [--json]`.
pub(crate) fn apply(json_out: bool) -> i32 {
    let graph = crate::graph_get::default_graph_path();
    let graph_db = db_path();
    let current = current_store();
    let plan = || -> Result<super::IdMap, String> {
        let conn = super::super::open_read_at(&graph_db)
            .ok_or_else(|| format!("cannot read {}", graph_db.display()))?;
        let keys = store_keys(Some(&conn)).map_err(|e| e.to_string())?;
        Ok(plan_map(&conn, &keys, &current))
    };
    let mut map = match plan() {
        Ok(map) => map,
        Err(error) => {
            eprintln!("migrate ids: {error}");
            return 1;
        }
    };
    if !map.fresh.is_empty() {
        if let Err(error) = mint_fresh(&graph_db, &map.fresh) {
            eprintln!("migrate ids: the later answers could not take their numbers: {error}");
            return 1;
        }
        map = match plan() {
            Ok(map) => map,
            Err(error) => {
                eprintln!("migrate ids: {error}");
                return 1;
            }
        };
    }
    if map.refuses() || !map.fresh.is_empty() {
        eprintln!("migrate ids: the map refuses; run --dry-run for the keys it cannot carry");
        return 1;
    }
    if map.mapped.is_empty() {
        println!("migrate ids: no hex key is left to retire");
        return 0;
    }
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let Some(db_dir) = graph_db.parent().map(Path::to_path_buf) else {
        eprintln!("migrate ids: {} has no parent dir", graph_db.display());
        return 1;
    };
    let dir = db_dir.join("backups").join(format!("migrate-ids-{stamp}"));
    let state_dir = crate::law_firings::default_path()
        .parent()
        .map(Path::to_path_buf);
    let mut stores = event_dbs(&db_dir);
    if let Some(state) = &state_dir {
        stores.extend(event_dbs(state));
    }
    let prepared = Log::open(&dir).and_then(|log| {
        let mut all = vec![graph_db.clone()];
        all.extend(stores.iter().cloned());
        back_up_dbs(&dir, &all)?;
        let tsv: String = map
            .mapped
            .iter()
            .map(|(hex, label)| format!("{hex}\t{label}\n"))
            .collect();
        std::fs::write(dir.join("id-map.tsv"), tsv).map_err(|e| e.to_string())?;
        Ok(log)
    });
    let log = match prepared {
        Ok(log) => log,
        Err(error) => {
            eprintln!("migrate ids: the backup failed, nothing changed: {error}");
            return 1;
        }
    };
    let mut run = Apply {
        map: &map.mapped,
        log,
        tally: Tally::default(),
    };
    if let Err(error) = steps(&mut run, &graph, &graph_db, &current, &stores) {
        eprintln!("migrate ids: apply stopped at {error}");
        let left = undo_all(std::mem::take(&mut run.log.entries));
        if left.is_empty() {
            eprintln!("migrate ids: every change apply made is rolled back");
        }
        for line in left {
            eprintln!("migrate ids: not rolled back: {line}");
        }
        return 1;
    }
    let t = &run.tally;
    if json_out {
        println!(
            "{}",
            json!({"applied": true, "backup": dir, "mapped": map.mapped.len(), "tally": t})
        );
    } else {
        println!(
            "migrate ids: {} keys took their numbers in store {current}\n  \
             decisions moved: {}, numbers rows: {}, text cells: {}, event rows: {} ({} duplicates gone), files: {}\n  \
             backup and undo log: {}\n  \
             rollback: fno doctor migrate ids --rollback {}",
            map.mapped.len(),
            t.decisions,
            t.numbers,
            t.cells,
            t.event_rows,
            t.duplicates,
            t.files,
            dir.display(),
            dir.display()
        );
    }
    0
}

/// Every rewrite, in order: numbers, the shared backlog, each event db with
/// its journals, the state files, then the vault pages.
fn steps(
    run: &mut Apply<'_>,
    graph: &Path,
    graph_db: &Path,
    current: &str,
    stores: &[PathBuf],
) -> Result<(), String> {
    run.numbers(graph_db, current)
        .map_err(|e| format!("numbers table: {e}"))?;
    run.backlog(graph)
        .map_err(|e| format!("backlog tables: {e}"))?;
    for db in stores {
        run.events(db)
            .map_err(|e| format!("{}: {e}", db.display()))?;
        for journal in journals(db) {
            run.file(&journal, false)
                .map_err(|e| format!("{}: {e}", journal.display()))?;
        }
    }
    let mut files = vec![crate::law_firings::default_path()];
    if let Ok(entries) = crate::attention_arm::attention_dir().and_then(std::fs::read_dir) {
        files.extend(
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json")),
        );
    }
    for file in files {
        run.file(&file, false)
            .map_err(|e| format!("{}: {e}", file.display()))?;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    if let Ok(vault) = crate::plans_path::vault_root(&cwd) {
        let mut pages = Vec::new();
        markdown(Path::new(&vault), &mut pages);
        for page in pages {
            run.file(&page, true)
                .map_err(|e| format!("{}: {e}", page.display()))?;
        }
    }
    Ok(())
}

/// `--rollback <dir>`.
pub(crate) fn rollback(dir: &Path) -> i32 {
    let log = dir.join("undo.jsonl");
    let file = match std::fs::File::open(&log) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("migrate ids: cannot read {}: {error}", log.display());
            return 1;
        }
    };
    let mut entries = Vec::new();
    for (n, line) in std::io::BufReader::new(file).lines().enumerate() {
        let parsed = line
            .map_err(|e| e.to_string())
            .and_then(|l| serde_json::from_str::<Undo>(&l).map_err(|e| e.to_string()));
        match parsed {
            Ok(entry) => entries.push(entry),
            Err(error) => {
                eprintln!("migrate ids: {} line {}: {error}", log.display(), n + 1);
                return 1;
            }
        }
    }
    let total = entries.len();
    let left = undo_all(entries);
    for line in &left {
        println!("not rolled back: {line}");
    }
    println!(
        "migrate ids: {} of {total} changes rolled back from {}",
        total - left.len(),
        dir.display()
    );
    i32::from(!left.is_empty())
}

/// Take back every entry, newest first. Each is a compare-and-swap: a value
/// a later write changed stays, and its line comes back in the result.
fn undo_all(entries: Vec<Undo>) -> Vec<String> {
    let mut left = Vec::new();
    let mut moved: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for entry in entries.into_iter().rev() {
        if let Undo::Moved { graph, to, .. } = &entry {
            moved.entry(graph.clone()).or_default().push(to.clone());
        }
        if let Err(reason) = undo_one(&entry) {
            left.push(format!("{entry:?}: {reason}"));
        }
    }
    // The second half of each move back: the number's row goes once the
    // primary holds the hex row and its joins again.
    for (graph, labels) in moved {
        let done = (|| -> Result<(), String> {
            crate::backlog_share::flush(&graph)?;
            let conn = crate::backlog::open_holding_lock(&graph).map_err(|e| e.to_string())?;
            for label in &labels {
                drop_decision(&conn, label)?;
            }
            crate::backlog_share::flush(&graph).map(drop)
        })();
        if let Err(reason) = done {
            left.push(format!("decisions in {}: {reason}", graph.display()));
        }
    }
    left
}

fn changed(n: usize) -> Result<(), String> {
    if n == 0 {
        Err("changed since apply; left as it is".into())
    } else {
        Ok(())
    }
}

fn undo_one(entry: &Undo) -> Result<(), String> {
    let open = |db: &Path| Connection::open(db).map_err(|e| e.to_string());
    match entry {
        Undo::Cell {
            db,
            table,
            column,
            rowid,
            before,
            after,
        } => {
            // The shared tables take their writes through the share path.
            let shared =
                table == "decisions" || TEXT_CELLS.iter().any(|(t, _)| *t == table.as_str());
            let conn = if shared {
                crate::backlog::open_holding_lock(&crate::graph_get::default_graph_path())
                    .map_err(|e| e.to_string())?
            } else {
                open(db)?
            };
            changed(
                conn.execute(
                    &format!(
                        "UPDATE \"{table}\" SET \"{column}\" = ?1 WHERE rowid = ?2 AND \"{column}\" = ?3"
                    ),
                    params![before, rowid, after],
                )
                .map_err(|e| e.to_string())?,
            )
        }
        Undo::Line {
            db,
            table,
            rowid,
            before,
            after_hash,
        } => {
            let conn = open(db)?;
            let current: Option<Vec<u8>> = conn
                .query_row(
                    &format!("SELECT row_hash FROM {table} WHERE rowid = ?1"),
                    params![rowid],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            let held = current.map(|h| h.iter().map(|b| format!("{b:02x}")).collect::<String>());
            if held.as_deref() != Some(after_hash.as_str()) {
                return changed(0);
            }
            conn.execute(
                &format!("UPDATE {table} SET line = ?1, row_hash = ?2 WHERE rowid = ?3"),
                params![before, sha(before.as_bytes()), rowid],
            )
            .map(drop)
            .map_err(|e| e.to_string())
        }
        Undo::Gone { db, table, row } => insert_row(&open(db)?, table, row),
        Undo::Rekey {
            db,
            store,
            kind,
            num,
            before,
            after,
        } => changed(
            open(db)?
                .execute(
                    "UPDATE fno_numbers SET key = ?1 \
                     WHERE store = ?2 AND kind = ?3 AND num = ?4 AND key = ?5",
                    params![before, store, kind, num, after],
                )
                .map_err(|e| e.to_string())?,
        ),
        Undo::Mapped { db, hex } => open(db)?
            .execute("DELETE FROM fno_id_map WHERE hex = ?1", params![hex])
            .map(drop)
            .map_err(|e| e.to_string()),
        Undo::Moved {
            graph,
            from,
            to,
            before,
            after,
        } => {
            let conn = crate::backlog::open_holding_lock(graph).map_err(|e| e.to_string())?;
            let held: Option<String> = conn
                .query_row(
                    "SELECT data FROM decisions WHERE event_id = ?1",
                    params![to],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if held.as_deref() != Some(after.as_str()) {
                return changed(0);
            }
            copy_decision(&conn, to, from, before).map(drop)
        }
        Undo::File {
            from,
            to,
            backup,
            after,
        } => {
            let now = std::fs::read(to).map_err(|e| e.to_string())?;
            if sha_hex(&now) != *after {
                return changed(0);
            }
            let text = std::fs::read_to_string(backup).map_err(|e| e.to_string())?;
            write_atomic(from, &text)?;
            if to != from {
                std::fs::remove_file(to).map_err(|e| e.to_string())?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_rewrite_takes_mapped_keys_and_copies_and_leaves_glued_and_unmapped_text() {
        let m = map(&[("d-aff1a9b5", "D3478"), ("q-0000beef", "Q87")]);
        assert_eq!(
            rewrite(
                &m,
                "law-d-aff1a9b5 answers q-0000beef@mac2, see (d-aff1a9b5)."
            ),
            "law-D3478 answers Q87, see (D3478)."
        );
        // A sha tail, a longer hex run and an unmapped key are not retired keys.
        for text in ["abd-aff1a9b5", "d-aff1a9b5c", "d-00000001 stays"] {
            assert_eq!(rewrite(&m, text), text);
        }
        assert!(matches!(rewrite(&m, "no key here"), Cow::Borrowed(_)));
    }

    #[test]
    fn an_event_row_takes_its_new_line_and_hash_a_repeat_goes_and_rollback_restores_both() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("questions.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE events (seq INTEGER PRIMARY KEY, row_hash BLOB UNIQUE NOT NULL, line TEXT NOT NULL)",
        )
        .unwrap();
        let lines = [
            r#"{"question_id":"q-0000beef"}"#,
            r#"{"question_id":"q-0000beef@mac2"}"#,
            r#"{"other":"x"}"#,
        ];
        for line in lines {
            conn.execute(
                "INSERT INTO events (row_hash, line) VALUES (?1, ?2)",
                params![sha(line.as_bytes()), line],
            )
            .unwrap();
        }
        let m = map(&[("q-0000beef", "D12")]);
        let mut run = Apply {
            map: &m,
            log: Log::open(&dir.path().join("backup")).unwrap(),
            tally: Tally::default(),
        };
        run.events(&db).unwrap();
        let read = |c: &Connection| -> Vec<String> {
            let mut s = c.prepare("SELECT line FROM events ORDER BY seq").unwrap();
            let rows = s.query_map([], |r| r.get(0)).unwrap();
            rows.flatten().collect()
        };
        assert_eq!(
            read(&conn),
            [r#"{"question_id":"D12"}"#, r#"{"other":"x"}"#]
        );
        let hash: Vec<u8> = conn
            .query_row("SELECT row_hash FROM events WHERE seq = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(hash, sha(br#"{"question_id":"D12"}"#));
        assert_eq!((run.tally.event_rows, run.tally.duplicates), (1, 1));
        assert!(undo_all(run.log.entries).is_empty());
        assert_eq!(read(&conn), lines);
    }

    #[test]
    fn numbers_rekey_to_their_labels_drop_a_second_number_and_roll_back() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("graph.db");
        let conn = super::super::super::open_write_at(&db).unwrap();
        mint_local(&conn, "q-0000b001", "r2d2", "h").unwrap(); // D1
        mint_local(&conn, "d-0000c001", "r2d2", "h").unwrap(); // D2, an answer of D1
        mint_local(&conn, "q-0000b001", "100.75.150.88", "h").unwrap();
        let m = map(&[("q-0000b001", "D1"), ("d-0000c001", "D1")]);
        let mut run = Apply {
            map: &m,
            log: Log::open(&dir.path().join("backup")).unwrap(),
            tally: Tally::default(),
        };
        run.numbers(&db, "r2d2").unwrap();
        let keys = |c: &Connection| -> Vec<String> {
            let mut s = c
                .prepare("SELECT store || ':' || key FROM fno_numbers ORDER BY store, num")
                .unwrap();
            let rows = s.query_map([], |r| r.get(0)).unwrap();
            rows.flatten().collect()
        };
        assert_eq!(keys(&conn), ["r2d2:D1"]);
        // A retired hex that comes back reads as its number and mints nothing.
        assert_eq!(
            super::super::super::resolve_in(Some(&conn), 'd', "d-0000c001", "r2d2").as_deref(),
            Some("D1")
        );
        assert!(
            super::super::super::unnumbered(&conn, vec!["q-0000b001".into()], "r2d2").is_empty()
        );
        assert!(undo_all(run.log.entries).is_empty());
        assert_eq!(
            keys(&conn),
            [
                "100.75.150.88:q-0000b001",
                "r2d2:q-0000b001",
                "r2d2:d-0000c001"
            ]
        );
    }

    #[test]
    fn a_file_rewrite_renames_a_keyed_page_and_rollback_puts_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let page = dir.path().join("q-0000beef.md");
        std::fs::write(&page, "question_id: q-0000beef\n").unwrap();
        let m = map(&[("q-0000beef", "D12")]);
        let mut run = Apply {
            map: &m,
            log: Log::open(&dir.path().join("backup")).unwrap(),
            tally: Tally::default(),
        };
        run.file(&page, true).unwrap();
        let moved = dir.path().join("D12.md");
        assert_eq!(
            std::fs::read_to_string(&moved).unwrap(),
            "question_id: D12\n"
        );
        assert!(!page.exists());
        assert!(undo_all(run.log.entries).is_empty());
        assert_eq!(
            std::fs::read_to_string(&page).unwrap(),
            "question_id: q-0000beef\n"
        );
        assert!(!moved.exists());
    }
}
