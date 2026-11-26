//! The numbers of decisions and questions: `D142` and the legacy `Q87`.
//!
//! Decisions and questions are one record: a question is a decision not yet
//! made, and answering it keeps its number. New rows mint in the one D
//! sequence whatever their key shape; an answer decision carries no number
//! of its own and reads as the number its question holds. Old Q rows keep
//! their numbers and stay resolvable beside the D rows.
//!
//! A new record's number is its key ([`mint`]). A record from before that
//! keeps its hash key, read here beside its number, until `fno doctor
//! migrate ids --apply` retires it. The store mints: the shared primary when
//! `store.remote_url` is set, else this machine's graph.db. With a primary set, a machine never mints locally; its
//! local `fno_numbers` table is a cache of the primary's rows. Two machines
//! therefore read one number for one question.
//!
//! `decisions.seq` is a per-machine rowid and two machines can write the same
//! value, so it is never a number and this module never reads it.

use crate::store_remote::{Remote, SqlValue};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DDL: &str = "CREATE TABLE IF NOT EXISTS fno_numbers (\
kind TEXT NOT NULL, \
num INTEGER NOT NULL, \
key TEXT NOT NULL, \
store TEXT NOT NULL, \
host TEXT NOT NULL, \
created_at INTEGER NOT NULL, \
PRIMARY KEY (store, kind, num), \
UNIQUE (store, key))";

const INSERT: &str = "INSERT OR IGNORE INTO fno_numbers \
(kind, num, key, store, host, created_at) \
SELECT ?1, COALESCE(MAX(num), 0) + 1, ?2, ?3, ?4, strftime('%s','now') \
FROM fno_numbers WHERE kind = ?1 AND store = ?3";

/// A node number skips every value that already names a node: an all-digit
/// hash is a real id, so the node due that number takes the next free one. One typed token then reads one node. `substr(?1, 3)` strips the `n:` off the kind.
const NODE_INSERT: &str = "INSERT OR IGNORE INTO fno_numbers \
(kind, num, key, store, host, created_at) \
SELECT ?1, (WITH RECURSIVE c(n) AS (\
SELECT COALESCE(MAX(num), 0) + 1 FROM fno_numbers WHERE kind = ?1 AND store = ?3 \
UNION ALL SELECT n + 1 FROM c \
WHERE EXISTS (SELECT 1 FROM nodes WHERE id = substr(?1, 3) || '-' || n)) \
SELECT n FROM c ORDER BY n DESC LIMIT 1), ?2, ?3, ?4, strftime('%s','now')";

const LOCAL_STORE: &str = "local";
pub(crate) const NODE_KIND: &str = "n:";
/// The kind of a delivered-mail number: `fmail-87` names the row keyed by the
/// message's hash id. Decision labels never list it ([`labels_in`]).
pub(crate) const MAIL_KIND: &str = "m";
const TICK: Duration = Duration::from_secs(60);
const MINT_BATCH: usize = 200;

/// The bare id of a question copy: `q-1234abcd@mac2` is `q-1234abcd`. A
/// copy and its original hold one number and one board row.
///
/// A fixed id such as `provider-cap:<lane>@<host>` keeps its host: two machines
/// both use that id for two different questions.
pub fn canonical(id: &str) -> &str {
    match id.split_once('@') {
        Some((bare, _)) if is_question_hash(bare) => bare,
        _ => id,
    }
}

pub(crate) fn is_question_hash(id: &str) -> bool {
    id.strip_prefix("q-")
        .is_some_and(|hex| hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A key that is its own number: `D142`, or a retired question's `Q87`.
pub(crate) fn is_label_key(key: &str) -> bool {
    key.strip_prefix(['D', 'Q']).is_some_and(|digits| {
        (1..=7).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
    })
}

/// Whether `text` names `id` as a whole word, in any case: `D1` is not in
/// `D12`.
pub(crate) fn names_id(text: &str, id: &str) -> bool {
    let (text, id) = (text.to_lowercase(), id.to_lowercase());
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    !id.is_empty()
        && text.match_indices(&id).any(|(at, _)| {
            !word(text[..at].chars().next_back()) && !word(text[at + id.len()..].chars().next())
        })
}

fn kind_of(key: &str) -> Option<&'static str> {
    // A retraction prints as `retracts D142`, and a label key is a number
    // already: neither takes one.
    if key.starts_with("r-") || is_label_key(key) {
        None
    }
    // One record, one number: a question is a decision not yet made, so
    // every new row mints in the single D sequence whatever its key shape.
    // A question that already holds a Q row keeps it - `unnumbered` filters
    // by key, so the old row is never re-minted, and Q numbers stay
    // resolvable beside the D numbers.
    else {
        Some("d")
    }
}

fn is_number_text(text: &str) -> Option<(char, &str, Option<&str>)> {
    let (head, store) = match text.split_once('@') {
        Some((head, store)) if !store.is_empty() => (head, Some(store)),
        _ => (text, None),
    };
    let mut chars = head.chars();
    let kind = chars.next()?.to_ascii_lowercase();
    let rest = chars.as_str();
    let digits = rest.strip_prefix('-').unwrap_or(rest);
    let is_digits = (1..=7).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit());
    (matches!(kind, 'd' | 'q') && is_digits).then_some((kind, digits, store))
}

/// The store name this machine counts in.
pub fn current_store() -> String {
    match crate::store_remote::configured() {
        Ok(Some(remote)) => remote.host().to_string(),
        _ => LOCAL_STORE.to_string(),
    }
}

/// A read-only connection on the local table. A missing file or table reads
/// as no numbers, so every printer falls back to the hash.
fn open_read() -> Option<Connection> {
    open_read_at(&db_path())
}

/// Run `read` over this machine's table on the process's reused read
/// handle; `None` when the store cannot be read. The decision and attention
/// passes resolve thousands of tokens a minute, and a fresh open parses the
/// whole graph schema each time.
fn read_store<T>(read: impl FnOnce(Option<&Connection>) -> T) -> T {
    let mut read = Some(read);
    let shared = crate::store_conn::with_read(&db_path(), |conn| {
        (read.take().expect("with_read runs the read once"))(Some(conn))
    });
    match shared {
        Ok(value) => value,
        Err(_) => (read.take().expect("a refused with_read never ran the read"))(None),
    }
}

/// The graph store's SQLite file. `default_graph_path` names the `graph.json`
/// anchor, which is not the database.
fn db_path() -> std::path::PathBuf {
    crate::backlog::database_path(&crate::graph_get::default_graph_path())
}

fn open_read_at(graph: &Path) -> Option<Connection> {
    Connection::open_with_flags(graph, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
}

fn open_write_at(graph: &Path) -> Result<Connection, String> {
    // A fresh state tree has no db dir yet; the beat and the receipts are
    // the first writers there, so the open must bring its own directory.
    if let Some(parent) = graph.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(graph).map_err(|e| format!("open {}: {e}", graph.display()))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    conn.execute_batch(DDL).map_err(|e| e.to_string())?;
    Ok(conn)
}

/// The counter a node id counts in: `n:x` for `x-aaaa`. Each prefix counts
/// alone, so `x-15` and `fb-15` are two nodes.
fn node_kind(id: &str) -> Option<String> {
    let (prefix, tail) = id.split_once('-')?;
    let word = !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_lowercase());
    (word && !tail.is_empty()).then(|| format!("{NODE_KIND}{prefix}"))
}

/// The kind and key of every id to number: decisions and questions by hash
/// shape, nodes by the `n:` marker the caller put on them.
fn kind_and_key(key: &str) -> Option<(String, &str)> {
    if fno_mail::address::is_mail_hash(key) {
        return Some((MAIL_KIND.to_string(), key));
    }
    match key.strip_prefix(NODE_KIND) {
        Some(id) => Some((node_kind(id)?, id)),
        None => Some((kind_of(key)?.to_string(), key)),
    }
}

fn insert_for(kind: &str) -> &'static str {
    if kind.starts_with(NODE_KIND) {
        NODE_INSERT
    } else {
        INSERT
    }
}

/// Mint one key in a local store. A second mint of a key is a no-op. A node
/// key reads `n:<node id>`.
pub fn mint_local(conn: &Connection, key: &str, store: &str, host: &str) -> Result<(), String> {
    let Some((kind, key)) = kind_and_key(key) else {
        return Ok(());
    };
    conn.execute(insert_for(&kind), params![kind, key, store, host])
        .map(drop)
        .map_err(|e| e.to_string())
}

fn mint_remote(remote: &Remote, keys: &[String], host: &str) -> Result<(), String> {
    let store = remote.host().to_string();
    let steps: Vec<(String, Vec<SqlValue>)> = keys
        .iter()
        .filter_map(|key| {
            let (kind, key) = kind_and_key(key)?;
            Some((
                insert_for(&kind).to_string(),
                vec![
                    SqlValue::Text(kind),
                    SqlValue::Text(key.to_string()),
                    SqlValue::Text(store.clone()),
                    SqlValue::Text(host.to_string()),
                ],
            ))
        })
        .collect();
    remote
        .transaction(&steps)
        .map(drop)
        .map_err(|(_, error)| error)
}

/// Copy the primary's rows this machine does not hold into the local cache.
/// Numbers only grow and the primary serializes the mint, so the highest
/// cached number per kind is the cursor.
fn pull(remote: &Remote, conn: &Connection) -> Result<usize, String> {
    let store = remote.host().to_string();
    let mut pulled = 0;
    let kinds = remote.execute(
        "SELECT DISTINCT kind FROM fno_numbers WHERE store = ?",
        &[SqlValue::Text(store.clone())],
    )?;
    let kinds: Vec<String> = kinds
        .rows
        .iter()
        .filter_map(|row| row.first().and_then(SqlValue::text).map(str::to_string))
        .collect();
    for kind in kinds.iter().map(String::as_str) {
        let cursor: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(num), 0) FROM fno_numbers WHERE store = ?1 AND kind = ?2",
                params![store, kind],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        let reply = remote.execute(
            "SELECT num, key, host, created_at FROM fno_numbers \
             WHERE store = ? AND kind = ? AND num > ? ORDER BY num",
            &[
                SqlValue::Text(store.clone()),
                SqlValue::Text(kind.to_string()),
                SqlValue::Integer(cursor),
            ],
        )?;
        for row in &reply.rows {
            let (Some(num), Some(key), Some(host), Some(created)) = (
                row.first().and_then(SqlValue::integer),
                row.get(1).and_then(SqlValue::text),
                row.get(2).and_then(SqlValue::text),
                row.get(3).and_then(SqlValue::integer),
            ) else {
                continue;
            };
            pulled += conn
                .execute(
                    "INSERT OR IGNORE INTO fno_numbers \
                     (kind, num, key, store, host, created_at) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![kind, num, key, store, host, created],
                )
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(pulled)
}

fn print_row(kind: &str, num: i64, store: &str, current: &str) -> String {
    let letter = match kind.strip_prefix(NODE_KIND) {
        Some(prefix) => format!("{prefix}-"),
        None => kind.to_ascii_uppercase(),
    };
    if store == current {
        format!("{letter}{num}")
    } else {
        format!("{letter}{num}@{store}")
    }
}

/// The number of one id: the own row first, then the question an answer
/// decision answers, then the newest row from another store, printed
/// qualified.
pub fn label_in(conn: &Connection, id: &str, current: &str) -> Option<String> {
    label_row(conn, id, current)
        .or_else(|| retired(conn, canonical(id)))
        .or_else(|| {
            // One record, one number: an answer decision carries no number of
            // its own. It reads as the number its question already holds, so
            // answering never splits one record into two labels.
            let question = answer_question_of(conn, id)?;
            label_row(conn, &question, current)
        })
}

/// The number `migrate ids --apply` gave a hex key it retired. `None` on a
/// store the migration never ran on, or for a key its map never held.
fn retired(conn: &Connection, hex: &str) -> Option<String> {
    conn.query_row(
        "SELECT label FROM fno_id_map WHERE hex = ?1",
        params![hex],
        |row| row.get(0),
    )
    .ok()
}

/// The label read over the own row: this store first, then the newest row
/// another store holds.
fn label_row(conn: &Connection, id: &str, current: &str) -> Option<String> {
    conn.query_row(
        "SELECT kind, num, store FROM fno_numbers WHERE key = ?1 \
         ORDER BY store = ?2 DESC, created_at DESC LIMIT 1",
        params![canonical(id), current],
        |row| {
            Ok(print_row(
                &row.get::<_, String>(0)?,
                row.get(1)?,
                &row.get::<_, String>(2)?,
                current,
            ))
        },
    )
    .ok()
}

/// The decisions-table fields the answer link reads, typed at the read
/// boundary (R1): the LIKE narrows candidates, this struct picks the row's
/// own fields.
#[derive(serde::Deserialize)]
struct AnswerRow {
    #[serde(default)]
    decision_id: String,
    #[serde(default)]
    trace: Option<AnswerTraceRow>,
}

#[derive(serde::Deserialize)]
struct AnswerTraceRow {
    parent_span_id: Option<String>,
}

/// The question an answer decision answers: the decision row's trace names
/// it as `parent_span_id`. `None` when the id is not a d- id or the store
/// holds no such decision.
fn answer_question_of(conn: &Connection, id: &str) -> Option<String> {
    if !id.starts_with("d-") {
        return None;
    }
    let Ok(mut stmt) = conn.prepare("SELECT data FROM decisions WHERE data LIKE ?") else {
        return None;
    };
    let Ok(rows) = stmt.query_map(params![format!("%\"decision_id\":\"{id}\"%")], |row| {
        row.get::<_, String>(0)
    }) else {
        return None;
    };
    for data in rows.flatten() {
        let Ok(row) = serde_json::from_str::<AnswerRow>(&data) else {
            continue;
        };
        // The LIKE can hit a row that only quotes the id in its own data;
        // the decision_id field decides which row answers.
        if row.decision_id != id {
            continue;
        }
        return row
            .trace
            .and_then(|t| t.parent_span_id)
            .filter(|q| !q.is_empty());
    }
    None
}

/// Every answer link this machine holds: decision id to the question it
/// answered. One read per listing; the per-id read is [`answer_question_of`].
fn answer_links(conn: &Connection) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(mut stmt) =
        conn.prepare("SELECT data FROM decisions WHERE data LIKE '%parent_span_id%'")
    else {
        return out;
    };
    let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) else {
        return out;
    };
    for data in rows.flatten() {
        let Ok(row) = serde_json::from_str::<AnswerRow>(&data) else {
            continue;
        };
        let question = match row.trace.and_then(|t| t.parent_span_id) {
            Some(q) if !q.is_empty() => q,
            _ => continue,
        };
        out.insert(row.decision_id, question);
    }
    out
}

/// The display text for one id over an open store: the number alone, the
/// hash only while no store holds a number for it.
pub(crate) fn display_in(conn: &Connection, id: &str, current: &str) -> String {
    label_in(conn, id, current).unwrap_or_else(|| id.to_string())
}

/// Every number this machine holds, keyed by hash, for a listing. The current
/// store's row wins over another store's row for one key.
pub fn labels_in(conn: &Connection, current: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT key, kind, num, store FROM fno_numbers WHERE kind != 'm' \
             ORDER BY store = ?1, created_at",
    ) else {
        return out;
    };
    let rows = stmt.query_map(params![current], |row| {
        Ok((
            row.get::<_, String>(0)?,
            print_row(
                &row.get::<_, String>(1)?,
                row.get(2)?,
                &row.get::<_, String>(3)?,
                current,
            ),
        ))
    });
    if let Ok(rows) = rows {
        out.extend(rows.flatten());
    }
    // An answer decision prints as its question's number: one record, one
    // label. The question is the numbered side of the link.
    let linked: Vec<(String, String)> = answer_links(conn)
        .into_iter()
        .filter_map(|(decision, question)| {
            out.get(&question).map(|label| (decision, label.clone()))
        })
        .collect();
    for (decision, label) in linked {
        out.entry(decision).or_insert(label);
    }
    out
}

/// The hash a typed token names. `D142`, `d142`, `d-142` resolve through the
/// table. `d-` plus 8 hex is a hash and passes through lowercased, even when
/// all 8 are digits. `None` means the token reads as a number no row holds.
///
/// The typed letter selects the sequence and every door accepts both: one
/// record means a Q number names the same row on the decide door that the
/// question door serves. An answer takes its question's number as its key,
/// so the decision door (`kind` is `d`) reads a number as that decision
/// first, while the question can still sit under a hash.
///
/// A bare number prefers this store's row and falls back to the newest row
/// another store holds: the same number names one id on every machine. A
/// number typed with a `@store` tail resolves strictly in that store.
pub fn resolve_in(
    conn: Option<&Connection>,
    kind: char,
    text: &str,
    current: &str,
) -> Option<String> {
    let text = text.trim();
    let Some((letter, digits, store)) = is_number_text(text) else {
        // A hash in any case reads as its lowercase form; any other word (a
        // subject) passes through as typed.
        let lowered = text.to_ascii_lowercase();
        let is_hash = lowered
            .get(2..)
            .is_some_and(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            && (lowered.starts_with("d-") || lowered.starts_with("q-"));
        if !is_hash {
            return Some(text.to_string());
        }
        return Some(conn.and_then(|c| retired(c, &lowered)).unwrap_or(lowered));
    };
    let num: i64 = digits.parse().ok()?;
    let store = store.unwrap_or(current);
    let conn = conn?;
    let key = format!("{}{num}", letter.to_ascii_uppercase());
    let decided = || {
        conn.query_row(
            "SELECT event_id FROM decisions WHERE event_id = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .ok()
    };
    if kind == 'd' && store == current {
        if let Some(id) = decided() {
            return Some(id);
        }
    }
    let sql = if store == current {
        // A bare number prefers this store's row, then the newest foreign
        // row: the same number names one id on every machine.
        "SELECT key FROM fno_numbers WHERE kind = ?1 AND num = ?2 \
         ORDER BY store = ?3 DESC, created_at DESC LIMIT 1"
    } else {
        // A number typed with a @store tail names that store strictly.
        "SELECT key FROM fno_numbers WHERE kind = ?1 AND num = ?2 AND store = ?3 LIMIT 1"
    };
    conn.query_row(sql, params![letter.to_string(), num, store], |row| {
        row.get(0)
    })
    .ok()
    // A peer's decision can arrive before its number does.
    .or_else(|| (store == current).then(decided).flatten())
}

/// A number token nothing holds reads as this refusal, never as a hash.
pub fn unresolved(text: &str) -> String {
    format!(
        "no {} numbered {} on this machine yet; the next beat pulls new numbers",
        if text.trim().to_ascii_lowercase().starts_with('d') {
            "decision"
        } else {
            "question"
        },
        text.trim().to_ascii_uppercase()
    )
}

/// Whether the token reads as a q-/d- display number (`D3478`, `d-142`,
/// `q87@r2d2`): the form a lookup door accepts beside the hash while both
/// keys exist. Pure shape, no store read.
pub fn reads_as_number(text: &str) -> bool {
    is_number_text(text.trim()).is_some()
}

/// `resolve_in` over this machine's table.
pub fn resolve(kind: char, text: &str) -> Option<String> {
    read_store(|conn| resolve_in(conn, kind, text, &current_store()))
}

/// [`resolve`] with one primary pull behind a miss: a number the local cache
/// has not caught up to still names its id on the machine that asks. A hash
/// or subject passes through untouched; only a number token can trigger the
/// pull, and a qualified `Q5@host` never does.
pub fn resolve_with_pull(kind: char, text: &str) -> Option<String> {
    if let Some(id) = resolve(kind, text) {
        return Some(id);
    }
    let (_, _, store) = is_number_text(text.trim())?;
    if store.is_some() {
        return None;
    }
    let remote = crate::store_remote::configured().ok().flatten()?;
    let conn = open_write_at(&db_path()).ok()?;
    let _ = pull(&remote, &conn);
    resolve(kind, text)
}

/// The `<prefix>-<digits>` split of a typed node number: `x-15` is
/// (`x`, 15, no store). A `@store` tail names another machine's count.
fn node_number_text(text: &str) -> Option<(&str, i64, Option<&str>)> {
    let (head, store) = match text.split_once('@') {
        Some((head, store)) if !store.is_empty() => (head, Some(store)),
        _ => (text, None),
    };
    let (prefix, digits) = head.split_once('-')?;
    let word = !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_lowercase());
    let digits_ok = (1..=7).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit());
    (word && digits_ok).then(|| (prefix, digits.parse().unwrap_or(0), store))
}

/// The node id a typed number names: `x-15` is the 15th `x` node. `None`
/// when the token is not a number or nothing holds it. The caller asks the
/// exact-id tier first, so a hash that reads like a number still wins.
pub fn resolve_node_in(conn: Option<&Connection>, text: &str, current: &str) -> Option<String> {
    let (prefix, num, store) = node_number_text(text.trim())?;
    conn?
        .prepare_cached("SELECT key FROM fno_numbers WHERE kind = ?1 AND num = ?2 AND store = ?3")
        .ok()?
        .query_row(
            params![
                format!("{NODE_KIND}{prefix}"),
                num,
                store.unwrap_or(current)
            ],
            |row| row.get(0),
        )
        .ok()
}

/// `resolve_node_in` over this machine's table.
pub fn resolve_node(text: &str) -> Option<String> {
    node_number_text(text.trim())?;
    read_store(|conn| resolve_node_in(conn, text, &current_store()))
}

/// Whether the token reads as a node display number (`x-15`): the shape a
/// person copies off a board row, distinct from the id form the graph keys
/// on. The write-door token gate and the miss refusals read this shape.
pub fn reads_as_node_number(text: &str) -> bool {
    node_number_text(text.trim()).is_some()
}

/// The node id a free-text token names, the tiers a closure-line reader
/// resolves with: an exact node row wins first (a legacy all-digit hash is a
/// real id and stays itself even when it reads as another node's number),
/// then the number tier. `None` leaves the caller its raw token. Fail-open:
/// an unread store resolves nothing, so every reader falls back to the hash.
pub(crate) fn resolve_node_ref_in(
    conn: Option<&Connection>,
    text: &str,
    current: &str,
) -> Option<String> {
    let conn = conn?;
    let trimmed = text.trim();
    if conn
        .query_row(
            "SELECT 1 FROM nodes WHERE id = ?1",
            params![trimmed],
            |_| Ok(()),
        )
        .is_ok()
    {
        return Some(trimmed.to_string());
    }
    resolve_node_in(Some(conn), trimmed, current)
}

/// `resolve_node_ref_in` over this machine's table.
pub(crate) fn resolve_node_ref(text: &str) -> Option<String> {
    read_store(|conn| resolve_node_ref_in(conn, text, &current_store()))
}

/// Every number a node holds, as text: `x-15`. A new hash must avoid these,
/// or one typed token would name two nodes.
pub fn node_number_texts() -> std::collections::BTreeSet<String> {
    let Some(conn) = open_read() else {
        return Default::default();
    };
    let Ok(mut stmt) =
        conn.prepare("SELECT substr(kind, 3) || '-' || num FROM fno_numbers WHERE kind LIKE 'n:%'")
    else {
        return Default::default();
    };
    let rows = stmt.query_map([], |row| row.get::<_, String>(0));
    rows.map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// `Q87`, or `None` while the arm has not reached this id.
pub fn label(id: &str) -> Option<String> {
    read_store(|conn| label_in(conn?, id, &current_store()))
}

/// `Q87`, or the id alone while no store holds a number for it. The hash
/// never reaches a reader beside its number: one short token names one row.
pub fn display(id: &str) -> String {
    read_store(|conn| match conn {
        Some(conn) => display_in(conn, id, &current_store()),
        None => id.to_string(),
    })
}

/// The number a mail hash id holds in an open store: this store's row first,
/// then the newest row another store holds.
pub(crate) fn mail_number_in(conn: &Connection, hash: &str, current: &str) -> Option<i64> {
    conn.query_row(
        "SELECT num FROM fno_numbers WHERE kind = 'm' AND key = ?1 \
         ORDER BY store = ?2 DESC, created_at DESC LIMIT 1",
        params![hash, current],
        |row| row.get(0),
    )
    .ok()
}

/// The mail hash id a number names, over an open store: this store's row
/// first, then the newest row another store holds.
pub(crate) fn mail_hash_in(conn: &Connection, number: i64, current: &str) -> Option<String> {
    conn.query_row(
        "SELECT key FROM fno_numbers WHERE kind = 'm' AND num = ?1 \
         ORDER BY store = ?2 DESC, created_at DESC LIMIT 1",
        params![number, current],
        |row| row.get(0),
    )
    .ok()
}

/// The header token of a delivered mail: `fmail-87` once the store numbers
/// the hash id (minted on first read), the hash itself while no store can.
/// Any id that is not a mail hash passes through.
pub fn mail_token(id: &str) -> String {
    if !fno_mail::address::is_mail_hash(id) {
        return id.to_string();
    }
    let held = || read_store(|conn| mail_number_in(conn?, id, &current_store()));
    let number = held().or_else(|| {
        mint_now(&db_path(), id)?;
        held()
    });
    match number {
        Some(n) => format!("fmail-{n}"),
        None => id.to_string(),
    }
}

/// The number token a mail hash id already holds, `fmail-87`: a read that
/// never mints, for listings with many rows.
pub fn mail_token_held(id: &str) -> Option<String> {
    if !fno_mail::address::is_mail_hash(id) {
        return None;
    }
    read_store(|conn| mail_number_in(conn?, id, &current_store())).map(|n| format!("fmail-{n}"))
}

/// The header token for a read path: the held number, else the hash. Never
/// mints, so a thread read makes no primary call per message. Delivery mints
/// ([`mail_token`]).
pub fn mail_token_read(id: &str) -> String {
    mail_token_held(id).unwrap_or_else(|| id.to_string())
}

/// The hash id a typed or delivered mail token names. `fmail-87` reads the
/// store; a hash, a legacy `msg-` id or an unnumbered number passes through
/// as typed, so a lookup that never held the number fails where it always did.
pub fn mail_hash(token: &str) -> String {
    let token = token.trim();
    if !fno_mail::address::is_mail_number(token) {
        return token.to_string();
    }
    let number = token
        .strip_prefix("fmail-")
        .and_then(|n| n.parse::<i64>().ok());
    let held = |n: i64| read_store(|conn| mail_hash_in(conn?, n, &current_store()));
    number
        .and_then(|n| held(n).or_else(|| pull_then(|| held(n))))
        .unwrap_or_else(|| token.to_string())
}

/// One primary pull, then the read again: a number a peer minted before this
/// machine's cache caught up still names its id. `None` with no primary set.
fn pull_then<T>(read: impl FnOnce() -> Option<T>) -> Option<T> {
    let remote = crate::store_remote::configured().ok().flatten()?;
    let conn = open_write_at(&db_path()).ok()?;
    let _ = pull(&remote, &conn);
    read()
}

/// Every number this machine holds, for a printer with many rows.
pub fn labels() -> HashMap<String, String> {
    read_store(|conn| {
        conn.map(|conn| labels_in(conn, &current_store()))
            .unwrap_or_default()
    })
}

/// The numbers map for one store's db file: the served store, not the
/// machine default. A missing file or table reads as no numbers, so a
/// keeper serving a store with no `fno_numbers` still answers.
pub fn labels_at(graph: &Path) -> HashMap<String, String> {
    let db = crate::backlog::database_path(graph);
    match open_read_at(&db) {
        Some(conn) => labels_in(&conn, &current_store()),
        None => Default::default(),
    }
}

/// Stamp `_number` onto every numbered row, before the field projection.
/// Unnumbered rows stay without the key, so a reader's null falls back to
/// the hash.
pub fn attach_numbers(rows: &mut [Value], held: &HashMap<String, String>) {
    if held.is_empty() {
        return;
    }
    for row in rows.iter_mut() {
        let id = crate::announce::row_str(row, "id").map(str::to_string);
        let Some(obj) = row.as_object_mut() else {
            continue;
        };
        if let Some(number) = id.as_deref().and_then(|id| held.get(id)) {
            obj.shift_insert(0, "_number".to_string(), Value::String(number.clone()));
        }
    }
}

/// Put the number beside every hash a text cites: `q-aae5187b` reads
/// `Q87 (q-aae5187b)`. A hash already in that form, or one with no number,
/// stays as it is. Machine text keeps the hash, so a reader can still copy it.
pub fn annotate(text: &str) -> String {
    let held = labels();
    annotate_with(text, &held)
}

fn is_question_or_decision(hash: &str) -> bool {
    hash.len() == 10 && (hash.starts_with("d-") || hash.starts_with("q-"))
}

/// A node id sits inside paths (`worktrees/x-aaaa`), branch names
/// (`feature/x-aaaa-slug`), backticked commands and URLs. Only a hash that
/// stands alone between spaces and sentence punctuation gets a number beside it.
pub(crate) fn standalone(text: &str, span: std::ops::Range<usize>) -> bool {
    let before = text[..span.start].chars().next_back();
    let after = text[span.end..].chars().next();
    let after_next = text[span.end..].chars().nth(1);
    let open = before.is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '[' | '"' | '\''));
    let close = match after {
        None => true,
        Some(c) if c.is_whitespace() => true,
        Some(',' | ';' | ':' | ')' | ']' | '"' | '\'') => true,
        Some('.') => after_next.is_none_or(char::is_whitespace),
        _ => false,
    };
    open && close
}

fn annotate_with(text: &str, held: &HashMap<String, String>) -> String {
    static HASH: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let hash = HASH.get_or_init(|| {
        regex::Regex::new(r"\b(?:[dq]-[0-9a-f]{8}|[a-z]{1,8}-[0-9a-f]{4})\b").expect("hash regex")
    });
    if held.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for found in hash.find_iter(text) {
        out.push_str(&text[last..found.start()]);
        match held.get(found.as_str()) {
            // A question or decision the store numbered prints as the number
            // alone: the hex is an internal key, never reader text. A hash a
            // "(" already frames is the old composite form; it stays as it is.
            Some(number)
                if is_question_or_decision(found.as_str())
                    && !text[..found.start()].ends_with('(') =>
            {
                out.push_str(number);
            }
            // A node ref inside paths and branches keeps its hash; a
            // standalone one gains the number beside it.
            Some(number)
                if !text[..found.start()].ends_with('(') && standalone(text, found.range()) =>
            {
                out.push_str(&format!("{number} ({})", found.as_str()));
            }
            _ => out.push_str(found.as_str()),
        }
        last = found.end();
    }
    out.push_str(&text[last..]);
    out
}

/// The keys to number, oldest first: every decision, then every question,
/// merged on the timestamp, copies folded to their bare id.
fn wanted(
    decisions: &[Value],
    asked: &serde_json::Map<String, Value>,
    extra: &[String],
) -> Vec<String> {
    let mut stamped: Vec<(String, String)> = Vec::new();
    for row in decisions {
        let id = row.get("decision_id").and_then(Value::as_str).unwrap_or("");
        if id.starts_with("d-") {
            let ts = row.get("ts").and_then(Value::as_str).unwrap_or("");
            stamped.push((ts.to_string(), id.to_string()));
        }
    }
    for (id, row) in asked {
        let ts = row.get("ts").and_then(Value::as_str).unwrap_or("");
        stamped.push((ts.to_string(), canonical(id).to_string()));
    }
    for id in extra {
        stamped.push((String::from("~"), canonical(id).to_string()));
    }
    stamped.sort();
    let mut seen = std::collections::HashSet::new();
    stamped
        .into_iter()
        .filter(|(_, id)| seen.insert(id.clone()))
        .map(|(_, id)| id)
        .collect()
}

fn unnumbered(conn: &Connection, keys: Vec<String>, store: &str) -> Vec<String> {
    let bare = |key: &str| key.strip_prefix(NODE_KIND).unwrap_or(key).to_string();
    // A hex key the migration retired is held too: a copy or a peer's row
    // that brings it back must not mint the record a second number.
    let held: std::collections::HashSet<String> = conn
        .prepare("SELECT key FROM fno_numbers WHERE store = ?1 UNION SELECT hex FROM fno_id_map")
        .or_else(|_| conn.prepare("SELECT key FROM fno_numbers WHERE store = ?1"))
        .and_then(|mut stmt| {
            let rows = stmt.query_map(params![store], |row| row.get::<_, String>(0))?;
            Ok(rows.flatten().collect())
        })
        .unwrap_or_default();
    keys.into_iter()
        .filter(|key| !held.contains(&bare(key)))
        .collect()
}

/// One sweep: mint every unnumbered key, then cache the primary's rows. An
/// unreachable primary ends the sweep quietly; the next one finds the same
/// keys, so no outbox holds them.
pub fn sweep(
    graph: &Path,
    remote: Option<&Remote>,
    keys: Vec<String>,
    host: &str,
) -> Result<usize, String> {
    let conn = open_write_at(graph)?;
    let Some(remote) = remote else {
        let missing = unnumbered(&conn, keys, LOCAL_STORE);
        for key in &missing {
            mint_local(&conn, key, LOCAL_STORE, host)?;
        }
        return Ok(missing.len());
    };
    let outcome = (|| {
        // Only a success is remembered: a primary that was down on the first
        // beat must get the create again when it returns.
        static SCHEMA: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        if SCHEMA.get().is_none() {
            remote.script(DDL)?;
            let _ = SCHEMA.set(());
        }
        let missing = unnumbered(&conn, keys, remote.host());
        let (nodes, rest): (Vec<String>, Vec<String>) = missing
            .into_iter()
            .partition(|key| key.starts_with(NODE_KIND));
        // A bound per beat keeps the backfill under the reply wait.
        if !rest.is_empty() {
            mint_remote(remote, &rest[..rest.len().min(MINT_BATCH)], host)?;
        }
        // Nodes mint in their own transaction: their insert reads the
        // primary's `nodes` table, and a primary without it must not stop
        // the decision and question counts.
        let nodes_result = if nodes.is_empty() {
            Ok(())
        } else {
            mint_remote(remote, &nodes[..nodes.len().min(MINT_BATCH)], host)
        };
        let pulled = pull(remote, &conn);
        nodes_result?;
        pulled
    })();
    match outcome {
        Err(error) if crate::store_remote::is_unreachable(&error) => Ok(0),
        other => other,
    }
}

/// Mint one key right now and read its label back: the create receipt's
/// synchronous front half of [`sweep`], keyed to the row just written. The
/// daemon beat stays the authority. Any error, including an unreachable
/// primary, reads `None`, and the next beat mints the same key as a no-op.
pub fn mint_now(graph: &Path, key: &str) -> Option<String> {
    let remote = crate::store_remote::configured().ok().flatten();
    let host = fno_mail::address::normalize_host(&crate::claims::hostname());
    mint_now_at(graph, key, remote.as_ref(), &host)
}

/// [`mint_now`] over an explicit store and host, so tests stay local.
pub(crate) fn mint_now_at(
    graph: &Path,
    key: &str,
    remote: Option<&Remote>,
    host: &str,
) -> Option<String> {
    let conn = open_write_at(graph).ok()?;
    let outcome: Result<(), String> = (|| {
        // An answer shares its question's number: minting the answer here
        // would split one record into two. The label read below follows the
        // link, and the beat numbers the question.
        if answer_question_of(&conn, key).is_some() {
            return Ok(());
        }
        let Some(remote) = remote else {
            for missing_key in unnumbered(&conn, vec![key.to_string()], LOCAL_STORE) {
                mint_local(&conn, &missing_key, LOCAL_STORE, host)?;
            }
            return Ok(());
        };
        static SCHEMA: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        if SCHEMA.get().is_none() {
            remote.script(DDL)?;
            let _ = SCHEMA.set(());
        }
        let missing = unnumbered(&conn, vec![key.to_string()], remote.host());
        if missing.is_empty() {
            return Ok(());
        }
        mint_remote(remote, &missing, host)?;
        pull(remote, &conn)?;
        Ok(())
    })();
    if outcome.is_err() {
        return None;
    }
    // The store this machine counts in: the caller's remote, else local.
    let current = match remote {
        Some(remote) => remote.host().to_string(),
        None => LOCAL_STORE.to_string(),
    };
    label_in(&conn, key.strip_prefix(NODE_KIND).unwrap_or(key), &current)
}

/// The sequential-mode reservation: the next `n:<prefix>` number, stored
/// with the id it becomes as its key, so a sequential node's number and id
/// are one value. Skips a number a node already holds as its id, the same
/// skip as `NODE_INSERT`.
const SEQ_RESERVE: &str = "INSERT INTO fno_numbers \
(kind, num, key, store, host, created_at) \
SELECT ?1, n, substr(?1, 3) || '-' || n, ?2, ?3, strftime('%s','now') FROM (\
WITH RECURSIVE c(n) AS (\
SELECT COALESCE(MAX(num), 0) + 1 FROM fno_numbers WHERE kind = ?1 AND store = ?2 \
UNION ALL SELECT n + 1 FROM c \
WHERE EXISTS (SELECT 1 FROM nodes WHERE id = substr(?1, 3) || '-' || n)) \
SELECT n FROM c ORDER BY n DESC LIMIT 1) RETURNING key";

/// Why a sequential id could not be reserved.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ReserveError {
    #[error("sequential id reserve: the store config is unreadable: {0}")]
    Config(String),
    #[error("sequential id reserve failed in the local store: {0}")]
    Local(String),
    #[error("sequential id reserve failed at the primary {host}: {detail}")]
    Primary { host: String, detail: String },
}

/// Reserve the next sequential node id for `prefix` (`t` or `t-`) at the
/// store's authority: the primary when one is set, else this graph.db. An
/// unreachable primary is an error, never a local mint: a number minted
/// offline would give one node two numbers.
pub(crate) fn reserve_node_id(graph: &Path, prefix: &str) -> Result<String, ReserveError> {
    let remote = crate::store_remote::configured().map_err(ReserveError::Config)?;
    let host = fno_mail::address::normalize_host(&crate::claims::hostname());
    reserve_node_id_at(graph, prefix, remote.as_ref(), &host)
}

pub(crate) fn reserve_node_id_at(
    graph: &Path,
    prefix: &str,
    remote: Option<&Remote>,
    host: &str,
) -> Result<String, ReserveError> {
    let kind = format!("{NODE_KIND}{}", prefix.trim_end_matches('-'));
    let conn = open_write_at(graph).map_err(ReserveError::Local)?;
    let Some(remote) = remote else {
        return conn
            .query_row(SEQ_RESERVE, params![kind, LOCAL_STORE, host], |row| {
                row.get(0)
            })
            .map_err(|e| ReserveError::Local(e.to_string()));
    };
    let at_primary = |detail: String| ReserveError::Primary {
        host: remote.host().to_string(),
        detail,
    };
    remote.script(DDL).map_err(at_primary)?;
    let reply = remote
        .execute(
            SEQ_RESERVE,
            &[
                SqlValue::Text(kind),
                SqlValue::Text(remote.host().to_string()),
                SqlValue::Text(host.to_string()),
            ],
        )
        .map_err(at_primary)?;
    let key = reply
        .rows
        .first()
        .and_then(|row| row.first())
        .and_then(SqlValue::text)
        .map(str::to_string)
        .ok_or_else(|| at_primary("it returned no reserved id".to_string()))?;
    // Cache the row so this machine reads the number before the next beat.
    // A failed pull costs only that early read: the beat pulls it later.
    if let Err(error) = pull(remote, &conn) {
        eprintln!("sequential id {key}: number cache pull failed ({error}); the beat retries");
    }
    Ok(key)
}

/// Every node id, oldest first, so history reads x-1..x-N in filing order.
fn node_ids_oldest_first(graph: &Path) -> Vec<String> {
    let Some(conn) = open_read_at(graph) else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare("SELECT id FROM nodes ORDER BY created_at, ordinal, id") else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |row| row.get::<_, String>(0));
    rows.map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// The keys the beat numbers after the answer fold: an answer decision adds
/// no second number. A linked answer whose question is asked drops its own
/// key; the question is the numbered side of the record. An answer whose
/// question is not asked (a store that lost the question row) still numbers
/// itself.
fn fold_answers(
    mut keys: Vec<String>,
    links: &HashMap<String, String>,
    asked: &std::collections::HashSet<String>,
) -> Vec<String> {
    keys.retain(|key| {
        !(key.starts_with("d-") && links.get(key).is_some_and(|q| asked.contains(q)))
    });
    keys
}

/// The canonical asked ids as a set, the fold's membership test.
fn asked_ids(asked: &serde_json::Map<String, Value>) -> std::collections::HashSet<String> {
    asked.keys().map(|id| canonical(id).to_string()).collect()
}

/// Land what waited for the primary. A caller runs it before [`beat`], so
/// the sweep reads what landed.
fn land_queued() {
    if let Err(error) = mint::drain() {
        eprintln!("numbers-arm: the outbox waits ({error})");
    }
}

/// Number what is recorded. It records nothing, so a dry run can use it.
fn beat(cwd: &Path) -> Result<usize, String> {
    let remote = crate::store_remote::configured()?;
    let decisions = crate::decision_index::default_store_live()?;
    let home = crate::paths::AgentsHome::from_env();
    let (asked, _) =
        crate::question_intake::question_rows(&crate::provider_cap::questions_path(&home));
    // Notes and pins ride the board without an ask row; they get a number too.
    let (items, _, _) = crate::attention_arm::read_items(cwd);
    let extra: Vec<String> = items.into_iter().map(|item| item.id).collect();
    let mut keys = wanted(&decisions.rows, &asked, &extra);
    if let Some(conn) = open_read_at(&db_path()) {
        let links = answer_links(&conn);
        keys = fold_answers(keys, &links, &asked_ids(&asked));
    }
    keys.extend(
        node_ids_oldest_first(&db_path())
            .into_iter()
            .map(|id| format!("{NODE_KIND}{id}")),
    );
    sweep(
        &db_path(),
        remote.as_ref(),
        keys,
        &fno_mail::address::normalize_host(&crate::claims::hostname()),
    )
}

#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

/// The daemon's numbering beat: every 60 seconds, one in flight, off the
/// loop. The first beat is the backfill, so history reads D1..DN in time
/// order.
pub fn maybe_tick(arm: &Arm, cwd: &Path) {
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|tick| tick.elapsed() < TICK)
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    let cwd = cwd.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        land_queued();
        if let Err(error) = beat(&cwd) {
            eprintln!("numbers-arm: {error}");
        }
    });
}

#[path = "numbers_mint.rs"]
pub(crate) mod mint;

#[path = "numbers_migrate.rs"]
pub(crate) mod migrate;

#[cfg(test)]
#[path = "numbers_display_guard_tests.rs"]
mod display_guard;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn conn() -> (tempfile::TempDir, std::path::PathBuf, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let graph = dir.path().join("graph.db");
        let conn = open_write_at(&graph).unwrap();
        (dir, graph, conn)
    }

    #[test]
    fn a_second_mint_keeps_the_number_and_a_copy_reads_the_same_row() {
        let (_dir, _graph, conn) = conn();
        mint_local(&conn, "d-aaaaaaaa", "local", "h").unwrap();
        mint_local(&conn, "d-bbbbbbbb", "local", "h").unwrap();
        mint_local(&conn, "d-aaaaaaaa", "local", "h").unwrap();
        mint_local(&conn, "q-0000abcd", "local", "h").unwrap();
        mint_local(&conn, "r-0000abcd", "local", "h").unwrap();
        assert_eq!(label_in(&conn, "d-aaaaaaaa", "local").unwrap(), "D1");
        assert_eq!(label_in(&conn, "d-bbbbbbbb", "local").unwrap(), "D2");
        assert_eq!(label_in(&conn, "q-0000abcd@imac", "local").unwrap(), "D3");
        assert_eq!(label_in(&conn, "r-0000abcd", "local"), None);
        assert_eq!(canonical("provider-cap:zai@imac"), "provider-cap:zai@imac");
        // The display form is the number alone: the hex never reaches a
        // reader while a number exists.
        assert_eq!(display_in(&conn, "q-0000abcd", "local"), "D3");
        assert_eq!(display_in(&conn, "q-ffffffff", "local"), "q-ffffffff");
    }

    #[test]
    fn a_node_ref_resolves_the_exact_hash_first_then_the_number() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::backlog::open(&dir.path().join("graph.json")).unwrap();
        conn.execute(DDL, []).unwrap();
        let node = crate::backlog::model::Node::from_json(&json!({
            "id": "x-5555", "slug": "n-5555", "title": "N", "type": "feature",
            "status": "ready", "priority": "p2", "domain": "code",
            "created_at": "2026-09-11T00:00:00+00:00",
        }))
        .unwrap();
        crate::backlog::nodes::save(&conn, &node).unwrap();
        conn.execute(
            "INSERT INTO fno_numbers (kind, num, key, store, host, created_at) VALUES ('n:x', 5555, 'x-9999', 'local', 'h', 0)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO fno_numbers (kind, num, key, store, host, created_at) VALUES ('n:x', 6666, 'x-aaaa', 'local', 'h', 0)",
            [],
        ).unwrap();
        // The exact tier wins: a legacy all-digit hash stays itself even
        // when it reads as another node's number.
        assert_eq!(
            resolve_node_ref_in(Some(&conn), "x-5555", "local").as_deref(),
            Some("x-5555")
        );
        // The number tier names the node the row carries.
        assert_eq!(
            resolve_node_ref_in(Some(&conn), "x-6666", "local").as_deref(),
            Some("x-aaaa")
        );
        // Nothing holds x-7777: the caller keeps the raw token.
        assert_eq!(resolve_node_ref_in(Some(&conn), "x-7777", "local"), None);
    }

    #[test]
    fn resolve_reads_every_number_form_and_never_a_hash_as_a_number() {
        let (_dir, _graph, conn) = conn();
        mint_local(&conn, "d-cafe0001", "local", "h").unwrap();
        mint_local(&conn, "q-0000abcd", "local", "h").unwrap();
        for typed in ["D1", "d1", "d-1", " D-1 "] {
            assert_eq!(
                resolve_in(Some(&conn), 'd', typed, "local").unwrap(),
                "d-cafe0001"
            );
        }
        // One record: the question mints in the D sequence, and every door
        // accepts both letters. The Q letter still reads the old q rows.
        assert_eq!(
            resolve_in(Some(&conn), 'q', "D2", "local").unwrap(),
            "q-0000abcd"
        );
        // Eight digits is a hash, by length.
        assert_eq!(
            resolve_in(Some(&conn), 'd', "d-12345678", "local").unwrap(),
            "d-12345678"
        );
        assert_eq!(
            resolve_in(Some(&conn), 'q', "Q-0000ABCD", "local").unwrap(),
            "q-0000abcd"
        );
        assert_eq!(resolve_in(Some(&conn), 'd', "D142", "local"), None);
        assert_eq!(resolve_in(None, 'd', "D1", "local"), None);

        // Another store's number prints qualified and still names the id:
        // a typed number resolves on any machine that holds the row.
        assert_eq!(label_in(&conn, "d-cafe0001", "linux1").unwrap(), "D1@local");
        assert_eq!(
            resolve_in(Some(&conn), 'd', "D1", "linux1").unwrap(),
            "d-cafe0001"
        );
        assert_eq!(
            resolve_in(Some(&conn), 'd', "D1@local", "linux1").unwrap(),
            "d-cafe0001"
        );
        mint_local(&conn, "d-cafe0001", "linux1", "h").unwrap();
        assert_eq!(label_in(&conn, "d-cafe0001", "linux1").unwrap(), "D1");
        mint_local(&conn, "d-beef0002", "linux1", "h").unwrap();
        assert_eq!(
            resolve_in(Some(&conn), 'd', "D2", "linux1").unwrap(),
            "d-beef0002"
        );
        // A question numbered on another machine reads the same way: a
        // machine with no row of its own falls back to the newest foreign
        // row, while a store that minted its own keeps it.
        conn.execute(
            "INSERT INTO fno_numbers (kind, num, key, store, host, created_at) \
             VALUES ('q', 1, 'q-beef0003', 'other', 'h', 9999999999)",
            [],
        )
        .unwrap();
        assert_eq!(
            resolve_in(Some(&conn), 'q', "Q1", "other").unwrap(),
            "q-beef0003"
        );
        assert_eq!(
            resolve_in(Some(&conn), 'q', "Q1", "linux1").unwrap(),
            "q-beef0003"
        );
        // A qualified number resolves strictly in the store it names, even
        // when another store holds the same number with a different key.
        conn.execute(
            "INSERT INTO fno_numbers (kind, num, key, store, host, created_at) \
             VALUES ('d', 1, 'd-beef0004', 'other2', 'h', 9999999998)",
            [],
        )
        .unwrap();
        assert_eq!(
            resolve_in(Some(&conn), 'd', "D1@other2", "local").unwrap(),
            "d-beef0004"
        );
    }

    #[test]
    fn a_sweep_numbers_oldest_first_and_folds_a_copy_onto_its_original() {
        let (_dir, graph, conn) = conn();
        let decisions = vec![
            json!({"decision_id": "d-00000002", "ts": "2026-02-01"}),
            json!({"decision_id": "d-00000001", "ts": "2026-01-01"}),
            json!({"retraction_id": "r-00000001", "ts": "2026-03-01"}),
        ];
        let mut asked = serde_json::Map::new();
        asked.insert("q-00000001".into(), json!({"ts": "2026-01-02"}));
        asked.insert("q-00000001@imac".into(), json!({"ts": "2026-01-03"}));
        let keys = wanted(&decisions, &asked, &["note-a".to_string()]);
        assert_eq!(sweep(&graph, None, keys.clone(), "h").unwrap(), 4 - 1 + 1);
        assert_eq!(sweep(&graph, None, keys, "h").unwrap(), 0);
        assert_eq!(label_in(&conn, "d-00000001", "local").unwrap(), "D1");
        assert_eq!(label_in(&conn, "q-00000001", "local").unwrap(), "D2");
        assert_eq!(label_in(&conn, "d-00000002", "local").unwrap(), "D3");
        assert_eq!(label_in(&conn, "note-a", "local").unwrap(), "D4");
    }

    #[test]
    fn an_answer_shares_its_question_number_and_never_mints_its_own() {
        let (_dir, graph, conn) = conn();
        crate::backlog::decisions::ensure_table(&conn).unwrap();
        let mut asked = serde_json::Map::new();
        asked.insert("q-0000abcd".into(), json!({"ts": "2026-01-02"}));
        let decisions = vec![json!({"decision_id": "d-99990001", "ts": "2026-01-01"})];
        let keys = wanted(&decisions, &asked, &[]);
        assert_eq!(sweep(&graph, None, keys.clone(), "h").unwrap(), 2);
        assert_eq!(label_in(&conn, "d-99990001", "local").unwrap(), "D1");
        assert_eq!(label_in(&conn, "q-0000abcd", "local").unwrap(), "D2");
        // The answer decision (trace names the question) adds no second
        // number: the beat folds it onto the question, and the label read
        // follows the link.
        let answer = json!({
            "ts": "2026-01-03T00:00:00.000Z",
            "type": crate::backlog::decisions::DECISION_EVENT,
            "data": {
                "decision_id": "d-88880001",
                "trace": {"parent_span_id": "q-0000abcd"},
            },
        });
        crate::backlog::decisions::insert_event(&conn, &answer).unwrap();
        let links = answer_links(&conn);
        let answer_key = "d-88880001";
        assert_eq!(
            links.get(answer_key).map(String::as_str),
            Some("q-0000abcd")
        );
        let decisions = vec![
            json!({"decision_id": "d-99990001", "ts": "2026-01-01"}),
            json!({"decision_id": "d-88880001", "ts": "2026-01-03"}),
        ];
        let keys = fold_answers(wanted(&decisions, &asked, &[]), &links, &asked_ids(&asked));
        assert_eq!(sweep(&graph, None, keys, "h").unwrap(), 0);
        // The label read follows the link: the answer prints D2, and a D
        // token resolves to the question id.
        assert_eq!(label_in(&conn, "d-88880001", "local").unwrap(), "D2");
        assert_eq!(
            resolve_in(Some(&conn), 'd', "D2", "local").unwrap(),
            "q-0000abcd"
        );
    }

    #[test]
    fn a_go_node_resolves_its_display_number() {
        let (_dir, graph, conn) = conn();
        // The node mint's CTE reads the nodes table for taken numbers.
        conn.execute_batch("CREATE TABLE nodes AS SELECT 'go-9zzz' AS id")
            .unwrap();
        let keys: Vec<String> = ["go-1a2b", "go-3c4d"]
            .iter()
            .map(|id| format!("n:{id}"))
            .collect();
        assert_eq!(sweep(&graph, None, keys, "h").unwrap(), 2);
        assert_eq!(label_in(&conn, "go-1a2b", "local").unwrap(), "go-1");
        assert_eq!(
            resolve_node_in(Some(&conn), "go-1", "local").as_deref(),
            Some("go-1a2b")
        );
    }

    #[test]
    fn nodes_count_per_prefix_and_skip_numbers_that_are_hashes() {
        let (_dir, graph, conn) = conn();
        // `x-2` is a real hash, so the second `x` node takes 3.
        conn.execute_batch("CREATE TABLE nodes AS SELECT 'x-2' AS id")
            .unwrap();
        let keys: Vec<String> = ["x-aaaa", "x-bbbb", "fb-cccc"]
            .iter()
            .map(|id| format!("n:{id}"))
            .collect();
        assert_eq!(sweep(&graph, None, keys.clone(), "h").unwrap(), 3);
        assert_eq!(sweep(&graph, None, keys, "h").unwrap(), 0);
        assert_eq!(label_in(&conn, "x-aaaa", "local").unwrap(), "x-1");
        assert_eq!(label_in(&conn, "x-bbbb", "local").unwrap(), "x-3");
        assert_eq!(label_in(&conn, "fb-cccc", "local").unwrap(), "fb-1");
        let found = |text| resolve_node_in(Some(&conn), text, "local");
        assert_eq!(found("x-3").as_deref(), Some("x-bbbb"));
        assert_eq!(found("fb-1").as_deref(), Some("fb-cccc"));
        assert_eq!(found("x-2"), None);
        assert_eq!(found("x-aaaa"), None);
    }

    #[test]
    fn mint_now_labels_a_new_node_and_a_second_call_is_a_noop() {
        let (_dir, graph, conn) = conn();
        conn.execute_batch("CREATE TABLE nodes AS SELECT 'x-2' AS id")
            .unwrap();
        // `x-2` is a real hash, so the first minted node skips it, exactly
        // like the beat's NODE_INSERT.
        assert_eq!(
            mint_now_at(&graph, "n:x-aaaa", None, "h").as_deref(),
            Some("x-1")
        );
        // The second call mints nothing and reads the same label back.
        assert_eq!(
            mint_now_at(&graph, "n:x-aaaa", None, "h").as_deref(),
            Some("x-1")
        );
        assert_eq!(label_in(&conn, "x-aaaa", "local").unwrap(), "x-1");
    }

    #[test]
    fn a_mail_hash_takes_a_number_and_the_number_names_the_hash_back() {
        let (_dir, graph, conn) = conn();
        let (first, second) = ("fmail-0badc0de1234", "fmail-0123456789ab");
        mint_now_at(&graph, first, None, "h");
        mint_now_at(&graph, second, None, "h");
        mint_now_at(&graph, first, None, "h");
        assert_eq!(mail_number_in(&conn, first, "local"), Some(1));
        assert_eq!(mail_number_in(&conn, second, "local"), Some(2));
        assert_eq!(mail_hash_in(&conn, 2, "local").as_deref(), Some(second));
        assert_eq!(mail_hash_in(&conn, 3, "local"), None);
        // A mail row never reads as a decision label.
        assert!(labels_in(&conn, "local").is_empty());
    }

    #[test]
    fn mint_now_reads_none_when_the_store_cannot_open() {
        let dir = tempfile::tempdir().unwrap();
        // The open creates a missing dir, so only a file in the way refuses it.
        let blocker = dir.path().join("a-file");
        std::fs::write(&blocker, "").unwrap();
        let graph = blocker.join("graph.db");
        assert!(open_write_at(&graph).is_err());
        assert_eq!(mint_now_at(&graph, "n:x-aaaa", None, "h"), None);
    }

    #[test]
    fn annotate_replaces_the_hash_with_the_number() {
        let held = HashMap::from([("q-0000abcd".to_string(), "Q3".to_string())]);
        assert_eq!(
            annotate_with("see q-0000abcd and q-ffffffff", &held),
            "see Q3 and q-ffffffff"
        );
        // Old text that already carries the composite form stays as it is.
        assert_eq!(annotate_with("Q3 (q-0000abcd)", &held), "Q3 (q-0000abcd)");
    }

    #[test]
    fn a_node_number_beside_a_path_or_branch_is_left_alone() {
        let held = HashMap::from([("x-aaaa".to_string(), "x-12".to_string())]);
        assert_eq!(
            annotate_with("take x-aaaa, then x-aaaa.", &held),
            "take x-12 (x-aaaa), then x-12 (x-aaaa)."
        );
        for kept in [
            "worktrees/footnote/x-aaaa/crates",
            "feature/x-aaaa-numbers",
            "run `fno backlog get x-aaaa`",
        ] {
            assert_eq!(annotate_with(kept, &held), kept);
        }
    }

    #[test]
    fn a_missing_table_reads_as_no_number() {
        let dir = tempfile::tempdir().unwrap();
        let graph = dir.path().join("graph.db");
        Connection::open(&graph)
            .unwrap()
            .execute_batch("CREATE TABLE t (a)")
            .unwrap();
        let conn = open_read_at(&graph).unwrap();
        assert_eq!(label_in(&conn, "q-1", "local"), None);
        assert!(labels_in(&conn, "local").is_empty());
    }

    #[test]
    fn the_projection_stamps_the_served_store_and_leaves_the_unnumbered_alone() {
        let (_dir, graph, conn) = conn();
        // The mint consults the nodes table to skip numbers that name a real
        // node, the same fixture shape the count test uses.
        conn.execute_batch("CREATE TABLE nodes AS SELECT 'x-2' AS id")
            .unwrap();
        let store = current_store();
        mint_local(&conn, "n:x-aaaa", &store, "h").unwrap();
        drop(conn);
        let held = labels_at(&graph);
        assert_eq!(held.get("x-aaaa").map(String::as_str), Some("x-1"));
        let mut rows = vec![json!({"id": "x-aaaa"}), json!({"id": "x-bbbb"})];
        attach_numbers(&mut rows, &held);
        assert_eq!(rows.first().unwrap()["_number"], json!("x-1"));
        assert!(rows.get(1).unwrap().get("_number").is_none());
        // A store file with no numbers table reads as no numbers.
        let bare = tempfile::tempdir().unwrap();
        let bare_graph = bare.path().join("graph.json");
        assert!(labels_at(&bare_graph).is_empty());
    }

    #[test]
    fn a_sequential_reserve_counts_up_skips_a_held_id_and_reads_as_its_own_number() {
        let (_dir, graph, conn) = conn();
        // A hex-era node already numbered t-1, and a node whose id is t-3.
        conn.execute_batch("CREATE TABLE nodes AS SELECT 't-3' AS id")
            .unwrap();
        mint_local(&conn, "n:t-aaaa", LOCAL_STORE, "h").unwrap();
        drop(conn);
        assert_eq!(reserve_node_id_at(&graph, "t-", None, "h").unwrap(), "t-2");
        assert_eq!(reserve_node_id_at(&graph, "t", None, "h").unwrap(), "t-4");
        let conn = open_read_at(&graph).unwrap();
        // Number and id are one value: the board and every door read t-4.
        assert_eq!(label_in(&conn, "t-4", LOCAL_STORE).as_deref(), Some("t-4"));
        assert_eq!(
            resolve_node_in(Some(&conn), "t-2", LOCAL_STORE).as_deref(),
            Some("t-2")
        );
    }
}
