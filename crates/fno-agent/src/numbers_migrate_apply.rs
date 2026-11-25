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
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// Why a migrate step failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum MigrateError {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Graph(#[from] crate::backlog::StoreDbError),
    /// A seam that reports its failure as text: the share flush, the
    /// numbers primary, the graph opener, the key census.
    #[error("{0}")]
    Store(String),
    /// A failure with the path or step it happened at.
    #[error("{what}: {source}")]
    At {
        what: String,
        #[source]
        source: Box<MigrateError>,
    },
    #[error("{} would rename onto {}, which exists", from.display(), to.display())]
    RenameTaken { from: PathBuf, to: PathBuf },
    #[error("changed since apply; left as it is")]
    Changed,
    #[error("fno_numbers row {0} vanished mid-transaction")]
    Vanished(i64),
}

impl From<String> for MigrateError {
    fn from(text: String) -> Self {
        Self::Store(text)
    }
}

/// Name the path or step a failure happened at.
fn at<E: Into<MigrateError>>(what: impl std::fmt::Display) -> impl FnOnce(E) -> MigrateError {
    let what = what.to_string();
    move |e| MigrateError::At {
        what,
        source: Box::new(e.into()),
    }
}

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
    fn open(dir: &Path) -> Result<Self, MigrateError> {
        std::fs::create_dir_all(dir.join("files"))?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("undo.jsonl"))
            .map_err(at(format!("open {}", dir.join("undo.jsonl").display())))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            file,
            entries: Vec::new(),
            staged: Vec::new(),
            copies: 0,
        })
    }

    fn push(&mut self, undo: Undo) -> Result<(), MigrateError> {
        self.staged.push(undo);
        Ok(())
    }

    /// Write every staged entry: its change is now durable.
    fn commit(&mut self) -> Result<(), MigrateError> {
        for undo in self.staged.drain(..) {
            let line = serde_json::to_string(&undo)?;
            writeln!(self.file, "{line}")?;
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
            json!(b)
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
        // A blob travels as its byte array.
        Value::Array(bytes) => Sql::Blob(
            bytes
                .iter()
                .filter_map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()))
                .collect(),
        ),
        other => Sql::Text(other.to_string()),
    }
}

/// Every column of one row, by rowid. `None` once the row is gone.
fn whole_row(
    conn: &Connection,
    table: &str,
    rowid: i64,
) -> Result<Option<BTreeMap<String, Value>>, MigrateError> {
    let mut stmt = conn.prepare(&format!("SELECT * FROM \"{table}\" WHERE rowid = ?1"))?;
    let names: Vec<String> = stmt.column_names().map(str::to_string).collect();
    stmt.query_row(params![rowid], |row| {
        let mut out = BTreeMap::new();
        for (i, name) in names.iter().enumerate() {
            out.insert(name.clone(), cell_json(row.get_ref(i)?));
        }
        Ok(out)
    })
    .optional()
    .map_err(MigrateError::from)
}

fn insert_row(
    conn: &Connection,
    table: &str,
    row: &BTreeMap<String, Value>,
) -> Result<(), MigrateError> {
    let names: Vec<&String> = row.keys().collect();
    // A row that cannot go back is named, never counted as restored.
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
    changed(conn.execute(&sql, params_from_iter(row.values().map(cell_sql)))?)
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
    fn numbers(&mut self, db: &Path, current: &str) -> Result<(), MigrateError> {
        let map = self.map;
        let mut conn = Connection::open(db)?;
        conn.execute_batch(&format!("{DDL};{MAP_DDL}"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows: Vec<(i64, String, String, i64, String)> = {
            let mut stmt = tx.prepare("SELECT rowid, store, kind, num, key FROM fno_numbers")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?;
            rows.collect::<Result<_, _>>()?
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
                )?;
                self.log.push(Undo::Rekey {
                    db: db.to_path_buf(),
                    store,
                    kind,
                    num,
                    before: key,
                    after: label.clone(),
                })?;
            } else {
                let row =
                    whole_row(&tx, "fno_numbers", rowid)?.ok_or(MigrateError::Vanished(rowid))?;
                tx.execute("DELETE FROM fno_numbers WHERE rowid = ?1", params![rowid])?;
                self.log.push(Undo::Gone {
                    db: db.to_path_buf(),
                    table: "fno_numbers".into(),
                    row,
                })?;
            }
            self.tally.numbers += 1;
        }
        for (hex, label) in map {
            let wrote = tx.execute(
                "INSERT OR IGNORE INTO fno_id_map (hex, label) VALUES (?1, ?2)",
                params![hex, label],
            )?;
            if wrote > 0 {
                self.log.push(Undo::Mapped {
                    db: db.to_path_buf(),
                    hex: hex.clone(),
                })?;
            }
        }
        tx.commit()?;
        self.log.commit()
    }

    /// The shared backlog tables. A decision's key moves in two flushes, so
    /// the primary (foreign keys on, deletes sent first) never sees a join
    /// row point at a missing decision: first the copy under the number and
    /// the repointed joins, then the delete of the hex row. A peer that wrote
    /// a row first gets its row back through the share take-back, so each
    /// flush the primary refuses as stale runs the pass again over it.
    fn backlog(&mut self, graph: &Path) -> Result<(), MigrateError> {
        let mut moved = BTreeSet::new();
        self.backlog_pass(graph, &mut moved)?;
        settle(graph, || self.backlog_pass(graph, &mut moved))?;
        let drop_moved = || -> Result<(), MigrateError> {
            let mut conn = crate::backlog::open_holding_lock(graph)?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            for from in &moved {
                drop_decision(&tx, from)?;
            }
            Ok(tx.commit()?)
        };
        drop_moved()?;
        settle(graph, drop_moved)
    }

    /// One rewrite of the shared tables in one local transaction. A decision
    /// an earlier pass moved takes its joins again without a second entry.
    fn backlog_pass(
        &mut self,
        graph: &Path,
        moved: &mut BTreeSet<String>,
    ) -> Result<(), MigrateError> {
        let map = self.map;
        let db = crate::backlog::database_path(graph);
        let mut conn = crate::backlog::open_holding_lock(graph)?;
        let rows: Vec<(i64, String, String)> = {
            let mut stmt =
                conn.prepare("SELECT rowid, event_id, data FROM decisions ORDER BY seq")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (rowid, event_id, data) in rows {
            let after = self.text(&data).into_owned();
            match map.get(&event_id) {
                Some(label) => {
                    if copy_decision(&tx, &event_id, label, &after)?
                        && moved.insert(event_id.clone())
                    {
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
                    crate::backlog::decisions::set_data(&tx, rowid, &after)?;
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
                    let mut stmt = tx.prepare(&format!(
                        "SELECT rowid, \"{column}\" FROM \"{table}\" \
                             WHERE \"{column}\" GLOB '{KEY_GLOB}'"
                    ))?;
                    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                    rows.collect::<Result<_, _>>()?
                };
                for (rowid, before) in cells {
                    let after = self.text(&before).into_owned();
                    if after == before {
                        continue;
                    }
                    tx.execute(
                        &format!("UPDATE \"{table}\" SET \"{column}\" = ?1 WHERE rowid = ?2"),
                        params![after, rowid],
                    )?;
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
        tx.commit()?;
        self.log.commit()
    }

    /// One event db: every row whose line names a mapped key gets the new
    /// line and its hash. A row whose new line another row already holds is
    /// that row again under the store's own dedupe rule, so it goes.
    ///
    /// The scan reads outside any write lock: an events.db can run to a
    /// gigabyte, and a writer held that long loses its row to the dead-letter
    /// file. Each write then matches the line it read, so a row that moved
    /// since the scan is left for the next apply.
    fn events(&mut self, db: &Path) -> Result<(), MigrateError> {
        let mut conn = Connection::open(db)?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        let mut changes = Vec::new();
        for table in ["events", "event_observation_pending"] {
            if !has_column(&conn, table, "line") || !has_column(&conn, table, "row_hash") {
                continue;
            }
            let mut stmt = conn.prepare(&format!(
                "SELECT rowid, line FROM {table} WHERE line GLOB '{KEY_GLOB}'"
            ))?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (rowid, before) = row?;
                let after = self.text(&before).into_owned();
                if after != before {
                    changes.push((table, rowid, before, after));
                }
            }
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (table, rowid, before, after) in changes {
            let hash = sha(after.as_bytes());
            let taken: bool = tx.query_row(
                &format!("SELECT EXISTS (SELECT 1 FROM {table} WHERE row_hash = ?1)"),
                params![hash],
                |r| r.get(0),
            )?;
            if taken {
                // An ephemeral row can expire between the scan and this write.
                let Some(row) = whole_row(&tx, table, rowid)? else {
                    continue;
                };
                let gone = tx.execute(
                    &format!("DELETE FROM {table} WHERE rowid = ?1 AND line = ?2"),
                    params![rowid, before],
                )?;
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
            let wrote = tx.execute(
                &format!(
                    "UPDATE {table} SET line = ?1, row_hash = ?2 WHERE rowid = ?3 AND line = ?4"
                ),
                params![after, hash, rowid, before],
            )?;
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
        tx.commit()?;
        self.log.commit()
    }

    /// One file: the text rewritten, and the name too when `rename`. The
    /// bytes before land in the backup dir first. A new name another file
    /// already holds refuses.
    fn file(&mut self, from: &Path, rename: bool) -> Result<(), MigrateError> {
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
            return Err(MigrateError::RenameTaken {
                from: from.to_path_buf(),
                to,
            });
        }
        let backup = self.log.backup_path(&name);
        std::fs::copy(from, &backup).map_err(at(format!("back up {}", from.display())))?;
        write_atomic(&to, &after)?;
        if to != from {
            std::fs::remove_file(from)?;
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

/// The copy half of a decision's key move; the SQL lives with its table.
fn copy_decision(
    conn: &Connection,
    from: &str,
    to: &str,
    data: &str,
) -> Result<bool, MigrateError> {
    Ok(crate::backlog::decisions::copy_under_key(
        conn, from, to, data,
    )?)
}

/// The delete half of a decision's key move.
fn drop_decision(conn: &Connection, from: &str) -> Result<(), MigrateError> {
    Ok(crate::backlog::decisions::drop_moved_key(conn, from)?)
}

/// Flushes one store's shared writes may take: each stale-row refusal costs
/// one more, and a refusal past the last reaches the caller.
const SHARE_ATTEMPTS: usize = 5;

/// Flush the shared backlog behind `graph`. A stale-row refusal took the
/// primary's rows back for the node a peer wrote. The sync brings the rest
/// of the peer's writes, such as a new join on a hex key the drop must keep,
/// then `again` runs over the fresh rows and the flush goes again.
fn settle(
    graph: &Path,
    mut again: impl FnMut() -> Result<(), MigrateError>,
) -> Result<(), MigrateError> {
    let mut attempt = 1;
    loop {
        match crate::backlog_share::flush(graph) {
            Ok(_) => return Ok(()),
            Err(refusal)
                if crate::backlog_share::is_stale_row_refusal(&refusal)
                    && attempt < SHARE_ATTEMPTS =>
            {
                attempt += 1;
                crate::backlog_share::sync(graph)?;
                again()?;
            }
            Err(refusal) => return Err(refusal.into()),
        }
    }
}

fn write_atomic(path: &Path, text: &str) -> Result<(), MigrateError> {
    let tmp = path.with_file_name(format!(
        ".{}.migrate-ids",
        path.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    ));
    std::fs::write(&tmp, text).map_err(at(format!("write {}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(at(format!("replace {}", path.display())))
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
fn back_up_dbs(dir: &Path, dbs: &[PathBuf]) -> Result<(), MigrateError> {
    for (n, db) in dbs.iter().enumerate() {
        let name = db
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let conn = Connection::open(db).map_err(at(format!("open {}", db.display())))?;
        // Two dirs can each hold an `events.db`: the index keeps both copies.
        let into = dir
            .join(format!("{n}-{name}"))
            .to_string_lossy()
            .into_owned();
        conn.execute("VACUUM INTO ?1", params![into])
            .map_err(at(format!("back up {}", db.display())))?;
    }
    Ok(())
}

/// The numbers for the later answers of one question: each restates a
/// ruling the first answer made, and is its own record with its own number.
fn mint_fresh(graph_db: &Path, keys: &[String]) -> Result<(), MigrateError> {
    if keys.is_empty() {
        return Ok(());
    }
    let conn = super::super::open_write_at(graph_db)?;
    let host = fno_mail::address::normalize_host(&crate::claims::hostname());
    match crate::store_remote::configured()? {
        Some(remote) => {
            mint_remote(&remote, keys, &host)?;
            pull(&remote, &conn)?;
            Ok(())
        }
        None => {
            for key in keys {
                mint_local(&conn, key, super::super::LOCAL_STORE, &host)?;
            }
            Ok(())
        }
    }
}

/// The hex-to-number pairs a previous apply wrote into `fno_id_map`. A run
/// whose own map comes up empty still needs them: the first apply retires
/// every hex this machine holds, so a second project's apply would otherwise
/// return before its vault step and leave that project's pages on their
/// hashes for good.
/// The hex-to-number pairs `fno_id_map` holds.
type StoredPairs = BTreeMap<String, String>;

fn stored_map(db: &Path) -> Result<StoredPairs, MigrateError> {
    let conn = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let present: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'fno_id_map')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        return Ok(BTreeMap::new());
    }
    let mut stmt = conn.prepare("SELECT hex, label FROM fno_id_map")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    rows.collect::<Result<StoredPairs, _>>()
        .map_err(MigrateError::from)
}

/// `--apply [--json]`.
pub(crate) fn apply(json_out: bool) -> i32 {
    let graph = crate::graph_get::default_graph_path();
    let graph_db = db_path();
    let current = current_store();
    let plan = || -> Result<super::IdMap, MigrateError> {
        let conn = super::super::open_read_at(&graph_db)
            .ok_or_else(|| format!("cannot read {}", graph_db.display()))?;
        let keys = store_keys(Some(&conn)).map_err(|e| MigrateError::Store(e.to_string()))?;
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
    // The pairs a previous apply stored join the map, so this run also
    // rewrites what still cites a retired hex: a second project's apply
    // carries nothing fresh, and the stores the map already covered read
    // back unchanged.
    match stored_map(&graph_db) {
        Ok(stored) => {
            for (hex, label) in stored {
                map.mapped.entry(hex).or_insert(label);
            }
        }
        Err(error) => {
            eprintln!("migrate ids: {error}");
            return 1;
        }
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
        std::fs::write(dir.join("id-map.tsv"), tsv)?;
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
    print_receipt(&run.tally, &dir, map.mapped.len(), &current, json_out);
    0
}

/// The apply receipt: what moved, and the rollback command for it.
fn print_receipt(t: &Tally, dir: &Path, mapped: usize, current: &str, json_out: bool) {
    if json_out {
        println!(
            "{}",
            json!({"applied": true, "backup": dir, "mapped": mapped, "tally": t})
        );
    } else {
        println!(
            "migrate ids: {} keys took their numbers in store {current}\n  \
             decisions moved: {}, numbers rows: {}, text cells: {}, event rows: {} ({} duplicates gone), files: {}\n  \
             backup and undo log: {}\n  \
             rollback: fno doctor migrate ids --rollback {}",
            mapped,
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
}

/// Every rewrite, in order: numbers, the shared backlog, each event db with
/// its journals, the state files, then the vault pages.
fn steps(
    run: &mut Apply<'_>,
    graph: &Path,
    graph_db: &Path,
    current: &str,
    stores: &[PathBuf],
) -> Result<(), MigrateError> {
    run.numbers(graph_db, current)
        .map_err(at("numbers table"))?;
    run.backlog(graph).map_err(at("backlog tables"))?;
    for db in stores {
        run.events(db).map_err(at(db.display()))?;
        for journal in journals(db) {
            run.file(&journal, false).map_err(at(journal.display()))?;
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
        run.file(&file, false).map_err(at(file.display()))?;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    if let Ok(vault) = crate::plans_path::vault_root(&cwd) {
        let mut pages = Vec::new();
        markdown(Path::new(&vault), &mut pages);
        for page in pages {
            run.file(&page, true).map_err(at(page.display()))?;
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
            .map_err(MigrateError::from)
            .and_then(|l| serde_json::from_str::<Undo>(&l).map_err(MigrateError::from));
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
    let mut cells = BTreeSet::new();
    let mut shared: BTreeMap<PathBuf, Vec<&Undo>> = BTreeMap::new();
    for entry in entries.iter().rev() {
        // A share take-back makes apply write one cell twice. The newest
        // entry holds what the cell read before the last write.
        if let Undo::Cell {
            db,
            table,
            column,
            rowid,
            ..
        } = entry
        {
            if !cells.insert((db, table, column, *rowid)) {
                continue;
            }
        }
        match entry.shared_store() {
            Some(db) => shared.entry(db).or_default().push(entry),
            None => {
                if let Err(reason) = undo_one(entry) {
                    left.push(format!("{entry:?}: {reason}"));
                }
            }
        }
    }
    for (db, group) in shared {
        let mut failed = Vec::new();
        let mut back = Vec::new();
        let mut pass = || -> Result<(), MigrateError> {
            failed.clear();
            back.clear();
            for entry in &group {
                match undo_one(entry) {
                    Err(reason) => failed.push(format!("{entry:?}: {reason}")),
                    // Only a move that went back loses its numbered row: one
                    // a later write changed is the sole copy of that decision.
                    Ok(()) => {
                        if let Undo::Moved { to, .. } = entry {
                            back.push(to.clone());
                        }
                    }
                }
            }
            Ok(())
        };
        let settled = pass().and_then(|()| settle(&db, &mut pass));
        left.extend(failed);
        // The second half of each move back: the number's row goes once the
        // primary holds the hex row and its joins again.
        let drop_back = || -> Result<(), MigrateError> {
            let conn = crate::backlog::open_holding_lock(&db)?;
            for label in &back {
                drop_decision(&conn, label)?;
            }
            Ok(())
        };
        let done = settled.and_then(|()| drop_back().and_then(|()| settle(&db, drop_back)));
        if let Err(reason) = done {
            left.push(format!("shared backlog {}: {reason}", db.display()));
        }
    }
    left
}

/// The two shared backlog tables a cell entry can name, beside `decisions`.
fn shared_table(table: &str) -> bool {
    table == "decisions" || TEXT_CELLS.iter().any(|(t, _)| *t == table)
}

impl Undo {
    /// The shared backlog store an entry writes through, by its db path.
    fn shared_store(&self) -> Option<PathBuf> {
        match self {
            Self::Moved { graph, .. } => Some(crate::backlog::database_path(graph)),
            Self::Cell { db, table, .. } if shared_table(table) => Some(db.clone()),
            _ => None,
        }
    }
}

fn changed(n: usize) -> Result<(), MigrateError> {
    if n == 0 {
        Err(MigrateError::Changed)
    } else {
        Ok(())
    }
}

/// Take one event line back while its hash still reads what apply wrote.
fn undo_line(
    conn: &Connection,
    table: &str,
    rowid: i64,
    before: &str,
    after_hash: &str,
) -> Result<(), MigrateError> {
    let current: Option<Vec<u8>> = conn
        .query_row(
            &format!("SELECT row_hash FROM {table} WHERE rowid = ?1"),
            params![rowid],
            |r| r.get(0),
        )
        .optional()?;
    let held = current.map(|h| h.iter().map(|b| format!("{b:02x}")).collect::<String>());
    if held.as_deref() != Some(after_hash) {
        return changed(0);
    }
    conn.execute(
        &format!("UPDATE {table} SET line = ?1, row_hash = ?2 WHERE rowid = ?3"),
        params![before, sha(before.as_bytes()), rowid],
    )
    .map(drop)
    .map_err(MigrateError::from)
}

/// Take one text cell back while it reads what apply wrote. A cell a share
/// take-back already put back reads its before value, and that is done too.
fn undo_cell(
    db: &Path,
    table: &str,
    column: &str,
    rowid: i64,
    before: &str,
    after: &str,
) -> Result<(), MigrateError> {
    // The shared tables take their writes through the share path.
    let conn = if shared_table(table) {
        crate::backlog::open_holding_lock(db)?
    } else {
        Connection::open(db)?
    };
    let wrote = conn.execute(
        &format!("UPDATE \"{table}\" SET \"{column}\" = ?1 WHERE rowid = ?2 AND \"{column}\" = ?3"),
        params![before, rowid, after],
    )?;
    let held: Option<String> = conn
        .query_row(
            &format!("SELECT \"{column}\" FROM \"{table}\" WHERE rowid = ?1"),
            params![rowid],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    changed(wrote.max(usize::from(held.as_deref() == Some(before))))
}

fn undo_one(entry: &Undo) -> Result<(), MigrateError> {
    let open = |db: &Path| Connection::open(db);
    match entry {
        Undo::Cell {
            db,
            table,
            column,
            rowid,
            before,
            after,
        } => undo_cell(db, table, column, *rowid, before, after),
        Undo::Line {
            db,
            table,
            rowid,
            before,
            after_hash,
        } => undo_line(&open(db)?, table, *rowid, before, after_hash),
        Undo::Gone { db, table, row } => insert_row(&open(db)?, table, row),
        Undo::Rekey {
            db,
            store,
            kind,
            num,
            before,
            after,
        } => changed(open(db)?.execute(
            "UPDATE fno_numbers SET key = ?1 \
                     WHERE store = ?2 AND kind = ?3 AND num = ?4 AND key = ?5",
            params![before, store, kind, num, after],
        )?),
        Undo::Mapped { db, hex } => open(db)?
            .execute("DELETE FROM fno_id_map WHERE hex = ?1", params![hex])
            .map(drop)
            .map_err(MigrateError::from),
        Undo::Moved {
            graph,
            from,
            to,
            before,
            after,
        } => {
            let conn = crate::backlog::open_holding_lock(graph)?;
            let held: Option<String> = conn
                .query_row(
                    "SELECT data FROM decisions WHERE event_id = ?1",
                    params![to],
                    |r| r.get(0),
                )
                .optional()?;
            if held.is_none() {
                // An earlier rollback finished this move: the hex row holds.
                let back: bool = conn.query_row(
                    "SELECT EXISTS (SELECT 1 FROM decisions WHERE event_id = ?1)",
                    params![from],
                    |r| r.get(0),
                )?;
                return changed(usize::from(back));
            }
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
            let now = std::fs::read(to)?;
            if sha_hex(&now) != *after {
                return changed(0);
            }
            // A page written at the old name since apply is a later write.
            if to != from && from.exists() {
                return Err(MigrateError::RenameTaken {
                    from: to.clone(),
                    to: from.clone(),
                });
            }
            let text = std::fs::read_to_string(backup)?;
            write_atomic(from, &text)?;
            if to != from {
                std::fs::remove_file(to)?;
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

        // A moved decision that changed after apply stays under its number.
        let graph = dir.path().join("board").join("graph.json");
        std::fs::create_dir_all(dir.path().join("board")).unwrap();
        let board = crate::backlog::open(&graph).unwrap();
        let event = json!({"ts": "2026-11-21T00:00:00Z", "type": "operator_decision",
            "source": "test", "data": {"decision_id": "D7", "decision": "edited after apply"}});
        crate::backlog::decisions::insert_event(&board, &event).unwrap();
        let left = undo_all(vec![Undo::Moved {
            graph,
            from: "d-0000c007".into(),
            to: "D7".into(),
            before: "{}".into(),
            after: "what apply wrote".into(),
        }]);
        assert_eq!(left.len(), 1, "{left:?}");
        let kept: i64 = board
            .query_row(
                "SELECT count(*) FROM decisions WHERE event_id = 'D7'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, 1, "rollback deleted a decision it could not restore");
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

        // A page written at the old name after apply is never overwritten.
        std::fs::write(&moved, "apply wrote this").unwrap();
        std::fs::write(&page, "written after apply").unwrap();
        let backup = dir.path().join("before.md");
        std::fs::write(&backup, "before apply").unwrap();
        let left = undo_all(vec![Undo::File {
            from: page.clone(),
            to: moved.clone(),
            backup,
            after: sha_hex(b"apply wrote this"),
        }]);
        assert_eq!(left.len(), 1, "{left:?}");
        assert_eq!(
            std::fs::read_to_string(&page).unwrap(),
            "written after apply"
        );
        assert!(moved.exists());
    }

    /// The child of the two-project test: the real command twice, each run
    /// against the seeded store its env names, never a live one. The parent
    /// hands over project-a's env and cwd; the second run moves to the
    /// sibling project the way an operator would.
    #[test]
    #[ignore = "spawned with a seeded FNO_HOME by the two-project vault test"]
    fn migrate_ids_two_project_apply_child() {
        let code = super::super::run(&["--apply".to_string()]);
        eprintln!("child: apply from project-a exited {code}");
        assert_eq!(code, 0);
        // The backup dir stamps at second resolution; keep the two applies
        // in distinct dirs.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let sibling = std::env::current_dir()
            .unwrap()
            .parent()
            .unwrap()
            .join("project-b");
        std::env::set_var("FNO_CONFIG", sibling.join(".fno/config.toml"));
        std::env::set_current_dir(&sibling).unwrap();
        let code = super::super::run(&["--apply".to_string()]);
        eprintln!("child: apply from project-b exited {code}");
        assert_eq!(code, 0);
    }

    #[test]
    fn a_second_apply_from_another_project_rewrites_that_projects_vault() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("fno-home");
        let (proj_a, proj_b) = (root.path().join("project-a"), root.path().join("project-b"));
        let (vault_a, vault_b) = (root.path().join("vault-a"), root.path().join("vault-b"));
        for (proj, vault) in [(&proj_a, &vault_a), (&proj_b, &vault_b)] {
            std::fs::create_dir_all(proj.join(".fno")).unwrap();
            std::fs::create_dir_all(vault).unwrap();
            std::fs::write(
                proj.join(".fno/config.toml"),
                format!(
                    "[obsidian]\nenabled = true\nvault = \"{}\"\n",
                    vault.display()
                ),
            )
            .unwrap();
        }
        let conn = super::super::super::open_write_at(&home.join("db").join("graph.db")).unwrap();
        for key in ["d-0000feed", "q-0000beef"] {
            mint_local(&conn, key, "local", "h").unwrap();
        }
        drop(conn);
        // The backlog schema comes up with the store, so the decision index
        // the command reads finds its tables.
        drop(crate::backlog::open(&home.join("graph.json")).unwrap());
        let keyed_page = "ruled by q-0000beef@mac2, see d-0000feed\n";
        std::fs::write(vault_a.join("q-0000beef.md"), keyed_page).unwrap();
        std::fs::write(vault_a.join("ruling.md"), "d-0000feed holds it\n").unwrap();
        std::fs::write(vault_b.join("q-0000beef.md"), keyed_page).unwrap();
        std::fs::write(vault_b.join("notes.md"), "see d-0000feed for the ruling\n").unwrap();

        // The child carries its own FNO_HOME, HOME, FNO_CONFIG, spaces dir,
        // events journal and agents home, so no ambient or shared-sandbox
        // store is ever a candidate. FNO_AGENT_HOME is the load-bearing pin:
        // the attention projection the beat sweeps reads the question
        // journals beside the agents home's parent dir, and the CI job
        // exports FNO_AGENT_HOME into a per-runner sandbox whose journals
        // concurrent fixture tests append to. HOME pins the fallback the
        // same way. The filter names the full test path: --exact matches
        // nothing on a bare function name, and a child that ran nothing
        // still exits 0.
        let exe = std::env::current_exe().unwrap();
        let mut cmd = std::process::Command::new(&exe);
        cmd.args([
            "--exact",
            "numbers::migrate::apply::tests::migrate_ids_two_project_apply_child",
            "--ignored",
        ])
        .env("FNO_HOME", &home)
        .env("HOME", root.path())
        .env("FNO_AGENT_HOME", root.path().join("agent-home"))
        .env("FNO_CONFIG", proj_a.join(".fno/config.toml"))
        .env("FNO_SPACES_DIR", root.path().join("spaces"))
        .env("FNO_EVENTS_PATH", root.path().join("events.jsonl"))
        .env_remove("FNO_STATE_DIR")
        .current_dir(&proj_a);
        let out = crate::bounded_cmd::output_with_timeout_result(cmd, 600).unwrap();
        assert!(
            out.status.success(),
            "the two-project child failed:\n-- child stdout --\n{}\n-- child stderr --\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "the child matched no test:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );

        let db = home.join("db").join("graph.db");
        let stored = stored_map(&db).unwrap();
        assert_eq!(stored.len(), 2, "{stored:?}");
        let d_label = stored.get("d-0000feed").unwrap();
        let q_label = stored.get("q-0000beef").unwrap();
        for (vault, plain) in [(&vault_a, "ruling.md"), (&vault_b, "notes.md")] {
            let moved = vault.join(format!("{q_label}.md"));
            let page = std::fs::read_to_string(&moved).unwrap();
            assert!(page.contains(d_label) && page.contains(q_label), "{page:?}");
            assert!(!page.contains("d-0000feed") && !page.contains("q-0000beef"));
            assert!(!vault.join("q-0000beef.md").exists());
            let other = std::fs::read_to_string(vault.join(plain)).unwrap();
            assert!(other.contains(d_label), "{other:?}");
            assert!(!other.contains("d-0000feed"));
        }

        // The second run's undo log: one file entry per changed page.
        let backups = home.join("db").join("backups");
        let mut dirs: Vec<std::path::PathBuf> = std::fs::read_dir(&backups)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        dirs.sort();
        assert_eq!(dirs.len(), 2, "one backup dir per apply: {dirs:?}");
        let undo = std::fs::read_to_string(dirs.last().unwrap().join("undo.jsonl")).unwrap();
        let entries: Vec<Value> = undo
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(entries.len(), 2, "two pages changed: {undo:?}");
        for entry in &entries {
            assert_eq!(entry["op"].as_str(), Some("file"), "{entry:?}");
            let to = PathBuf::from(entry["to"].as_str().unwrap());
            assert!(to.starts_with(&vault_b), "{to:?}");
            assert!(to.is_file(), "{to:?}");
        }
    }

    fn node_with_decision(graph: &Path, title: &str) -> Connection {
        let board = crate::backlog::open(graph).unwrap();
        board
            .execute(
                "INSERT INTO nodes (id, ordinal, slug, title, kind, status, priority) \
                 VALUES ('x-1', 1, 'x-1', ?1, 'task', 'idea', 'p2')",
                [title],
            )
            .unwrap();
        let event = json!({"ts": "2026-11-24T00:00:00Z", "type": "operator_decision",
            "source": "test", "data": {"decision_id": "d-0000c007", "decision": "kept", "subject": "x-1"}});
        crate::backlog::decisions::record(&board, &event).unwrap();
        board
    }

    /// The 2026-11-24 stop: a peer retitles a node between this machine's
    /// sync and its apply. The primary refuses the stale batch and takes the
    /// node's rows back; the next pass rewrites the peer's title and lands.
    /// The rollback then puts the primary back on the hex key, nothing left.
    #[test]
    fn an_apply_a_peer_wrote_under_lands_on_the_primary_and_rolls_back_clean() {
        use crate::backlog_share::{flush, route_to_primary, seed, sync};
        let primary = crate::store_remote::test_primary::start();
        let on = |graph: &Path| {
            route_to_primary(Some((
                primary.remote.clone(),
                crate::backlog::database_path(graph),
            )))
        };
        let (a_dir, b_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (a, b) = (
            a_dir.path().join("graph.json"),
            b_dir.path().join("graph.json"),
        );
        drop(crate::backlog::open(&a).unwrap());
        drop(crate::backlog::open(&b).unwrap());
        on(&a);
        seed(&a).unwrap();
        drop(node_with_decision(&a, "cites d-0000c007"));
        flush(&a).unwrap();
        on(&b);
        sync(&b).unwrap();
        crate::backlog::open(&b)
            .unwrap()
            .execute(
                "UPDATE nodes SET title = 'b cites d-0000c007 too' WHERE id = 'x-1'",
                [],
            )
            .unwrap();
        flush(&b).unwrap();

        on(&a);
        let m = map(&[("d-0000c007", "D7")]);
        let dir = tempfile::tempdir().unwrap();
        let mut run = Apply {
            map: &m,
            log: Log::open(dir.path()).unwrap(),
            tally: Tally::default(),
        };
        run.backlog(&a).unwrap();
        let read = || -> (String, String, String) {
            primary
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT (SELECT title FROM nodes WHERE id = 'x-1'), \
                     (SELECT group_concat(event_id) FROM node_decisions WHERE node_id = 'x-1'), \
                     (SELECT group_concat(event_id) FROM decisions)",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap()
        };
        let landed = ("b cites D7 too".to_string(), "D7".into(), "D7".into());
        assert_eq!(read(), landed);

        let left = undo_all(run.log.entries);
        assert!(left.is_empty(), "{left:?}");
        let hex = "d-0000c007".to_string();
        assert_eq!(read(), ("b cites d-0000c007 too".into(), hex.clone(), hex));
        route_to_primary(None);
    }

    /// The leftover shape the stopped apply left on 25 decisions: the node
    /// joined under both keys, and a cell the take-back already put back.
    /// Rollback ends on the hex key alone and names nothing.
    #[test]
    fn a_rollback_over_a_node_joined_under_both_keys_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let graph = dir.path().join("graph.json");
        let board = node_with_decision(&graph, "cites d-0000c007");
        let before: String = board
            .query_row(
                "SELECT data FROM decisions WHERE event_id = 'd-0000c007'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let after = before.replace("kept", "kept, numbered");
        crate::backlog::decisions::copy_under_key(&board, "d-0000c007", "D7", &after).unwrap();
        board
            .execute(
                "INSERT INTO node_decisions (node_id, event_id, seq) VALUES ('x-1', 'd-0000c007', 9)",
                [],
            )
            .unwrap();
        let rowid: i64 = board
            .query_row("SELECT rowid FROM nodes WHERE id = 'x-1'", [], |r| r.get(0))
            .unwrap();
        let left = undo_all(vec![
            Undo::Moved {
                graph: graph.clone(),
                from: "d-0000c007".into(),
                to: "D7".into(),
                before,
                after,
            },
            Undo::Cell {
                db: crate::backlog::database_path(&graph),
                table: "nodes".into(),
                column: "title".into(),
                rowid,
                before: "cites d-0000c007".into(),
                after: "cites D7".into(),
            },
        ]);
        assert!(left.is_empty(), "{left:?}");
        let (joins, decisions): (String, String) = board
            .query_row(
                "SELECT (SELECT group_concat(event_id) FROM node_decisions), \
                 (SELECT group_concat(event_id) FROM decisions)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (joins.as_str(), decisions.as_str()),
            ("d-0000c007", "d-0000c007")
        );
    }
}
