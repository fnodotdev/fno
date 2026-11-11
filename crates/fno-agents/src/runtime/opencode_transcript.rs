//! The opencode transcript source: the sqlite store opened read-only and
//! rendered as claude-shaped JSONL, so the shared classifier reads it with
//! no new rule. Sessions with a `parent_id` (subagent children) never list.
//!
//! The store is only ever read: `SQLITE_OPEN_READ_ONLY`, no migration, no
//! write of any kind. A store that cannot be opened or queried is a
//! `probe` error naming the store and the sqlite text, never a panic and
//! never a partial guess.

use crate::provenance::{
    claude_shaped_tool_uses, claude_shaped_turns, in_roots, within_window, SessionFile,
    TranscriptSource, Turn,
};
use rusqlite::{OpenFlags, OptionalExtension};
use serde_json::Value;
use std::path::{Path, PathBuf};

const SESSION_SQL: &str = "SELECT s.id, s.directory, s.time_updated, \
     (SELECT coalesce(sum(length(p.data)), 0) FROM part p WHERE p.session_id = s.id) \
     FROM session s WHERE s.parent_id IS NULL";

/// Parts of one session, message-ordered (m.id breaks time ties and groups
/// the render).
const PART_SQL: &str = "SELECT m.id, m.data, p.data, m.time_created FROM part p \
     JOIN message m ON m.id = p.message_id WHERE p.session_id = ?1 \
     ORDER BY m.time_created, m.id, p.id";

/// The 2.x session list: the row carries its own token rollups, so the size
/// sums the message data lengths.
const V2_SESSION_SQL: &str = "SELECT s.id, s.directory, s.time_updated, \
     (SELECT coalesce(sum(length(m.data)), 0) FROM session_message m WHERE m.session_id = s.id) \
     FROM session_v2 s WHERE s.parent_id IS NULL";

/// The 2.x stream: message rows (`k` 0) then their part rows (`k` 1),
/// durable-seq ordered; a part's `ordinal` is its slot in the message.
const V2_READ_SQL: &str =
    "SELECT 0, m.seq, m.id, m.type, m.data, NULL, NULL, NULL, m.time_created \
     FROM session_message m WHERE m.session_id = ?1 \
     UNION ALL \
     SELECT 1, msg.seq, msg.id, msg.type, msg.data, p.ordinal, p.type, p.data, p.time_created \
     FROM session_part p JOIN session_message msg ON msg.id = p.message_id \
     WHERE p.session_id = ?1 \
     ORDER BY 2, 1, 5";

/// `${XDG_DATA_HOME:-$HOME/.local/share}/opencode`, the directory opencode
/// keeps its per-channel stores in (`opencode.db`, `opencode-<channel>.db`).
fn stores_dir() -> PathBuf {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(".local").join("share"),
            None => PathBuf::from("."),
        });
    data_home.join("opencode")
}

/// Every opencode store on this machine: `$OPENCODE_DB` when set and
/// non-empty, else every file matching `opencode*.db` in the data dir,
/// sorted. The glob covers the per-channel names without spawning opencode.
pub(crate) fn opencode_stores() -> Vec<PathBuf> {
    if let Some(db) = std::env::var_os("OPENCODE_DB").filter(|v| !v.is_empty()) {
        return vec![PathBuf::from(db)];
    }
    let dir = stores_dir();
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("opencode") && n.ends_with(".db"))
        })
        .collect();
    out.sort();
    out
}

pub(crate) fn open_read_only(path: &Path) -> Result<rusqlite::Connection, String> {
    // The plain open first so its error stays the reported failure; the
    // immutable retry covers the clean-exit mint (header stays WAL, the
    // -wal/-shm sidecars gone) this opencode.db family is left in.
    let conn = match rusqlite::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(conn) => conn,
        Err(original) => crate::graph_store::open_read_only_wal_safe(path)
            .ok_or_else(|| format!("{}: {original}", path.display()))?,
    };
    conn.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(conn)
}

/// Which store family a connection holds: `Some(true)` is 2.x
/// (`session_v2` present), `Some(false)` is 1.x (`session` present), `None`
/// is neither.
pub(crate) fn store_family(conn: &rusqlite::Connection) -> Option<bool> {
    let v2 = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'session_v2'",
            [],
            |_| Ok(()),
        )
        .optional()
        .ok()
        .flatten();
    if v2.is_some() {
        return Some(true);
    }
    let v1 = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'session'",
            [],
            |_| Ok(()),
        )
        .optional()
        .ok()
        .flatten();
    if v1.is_some() {
        return Some(false);
    }
    None
}

/// The opencode transcript store: one or more sqlite databases read-only.
/// `dbs` is [`opencode_stores`]' result (tests inject fixture stores);
/// `roots` scopes the fold to the requested projects, `None` reads every
/// project.
pub(crate) struct OpencodeSource {
    pub(crate) dbs: Vec<PathBuf>,
    pub(crate) roots: Option<Vec<PathBuf>>,
}

impl OpencodeSource {
    /// Every declared store must open read-only and answer both queries;
    /// the first failure names the store and the sqlite text. An empty
    /// store list is a miss naming the searched dir.
    pub(crate) fn probe(&self) -> Result<(), String> {
        if self.dbs.is_empty() {
            return Err(format!(
                "no opencode store under {}",
                stores_dir().display()
            ));
        }
        for db in &self.dbs {
            let conn = open_read_only(db)?;
            match store_family(&conn) {
                Some(true) => {
                    conn.prepare(V2_SESSION_SQL)
                        .map_err(|e| format!("{}: {e}", db.display()))?;
                    conn.prepare(V2_READ_SQL)
                        .map_err(|e| format!("{}: {e}", db.display()))?;
                }
                Some(false) => {
                    conn.prepare(SESSION_SQL)
                        .map_err(|e| format!("{}: {e}", db.display()))?;
                    conn.prepare(PART_SQL)
                        .map_err(|e| format!("{}: {e}", db.display()))?;
                }
                None => return Err(format!("{}: no session or session_v2 table", db.display())),
            }
        }
        Ok(())
    }
}

impl TranscriptSource for OpencodeSource {
    fn harness(&self) -> &'static str {
        "opencode"
    }

    fn sessions(&self, days: u64) -> Vec<SessionFile> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut out = Vec::new();
        for db in &self.dbs {
            let Ok(conn) = open_read_only(db) else {
                continue;
            };
            let Some(is_v2) = store_family(&conn) else {
                continue;
            };
            let Ok(mut stmt) = conn.prepare(if is_v2 { V2_SESSION_SQL } else { SESSION_SQL })
            else {
                continue;
            };
            let Ok(rows) = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })
                .map_err(|e| e.to_string())
            else {
                continue;
            };
            for row in rows.flatten() {
                let (id, directory, time_updated, size) = row;
                // Both stamps are milliseconds; the window reads seconds.
                let mtime = time_updated.max(0) as u64 / 1000;
                if !within_window(mtime, days, now) {
                    continue;
                }
                if !in_roots(directory.as_deref().map(Path::new), self.roots.as_deref()) {
                    continue;
                }
                out.push(SessionFile {
                    path: PathBuf::from(format!("{}#{id}", db.display())),
                    session_id: id,
                    mtime,
                    size: size.max(0) as u64,
                });
            }
        }
        out.sort_by(|a, b| b.mtime.cmp(&a.mtime));
        out
    }

    /// One session's transcript as claude-shaped JSONL, re-queried from the
    /// store the `path` names (`<db>#<session id>`).
    fn read(&self, file: &SessionFile) -> String {
        let display = file.path.to_string_lossy().into_owned();
        let Some((db, sid)) = display.rsplit_once('#') else {
            return String::new();
        };
        let Ok(conn) = open_read_only(Path::new(db)) else {
            return String::new();
        };
        if store_family(&conn) == Some(true) {
            return read_v2(&conn, sid);
        }
        let Ok(mut stmt) = conn.prepare(PART_SQL) else {
            return String::new();
        };
        let Ok(rows) = stmt
            .query_map([sid], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(|e| e.to_string())
        else {
            return String::new();
        };
        // Parts of one message are contiguous in the ordering above; the
        // message id groups them.
        let mut current: Option<(String, String, Vec<(String, i64)>)> = None;
        let mut out = String::new();
        for row in rows.flatten() {
            let (m_id, m_data, p_data, time_created) = row;
            match &mut current {
                Some((cur_id, _, parts)) if *cur_id == m_id => parts.push((p_data, time_created)),
                _ => {
                    if let Some((_, m_data, parts)) = current.take() {
                        render_message(&mut out, &m_data, &parts);
                    }
                    current = Some((m_id, m_data, vec![(p_data, time_created)]));
                }
            }
        }
        if let Some((_, m_data, parts)) = current.take() {
            render_message(&mut out, &m_data, &parts);
        }
        out
    }

    fn turns(&self, raw: &str) -> Vec<Turn> {
        claude_shaped_turns(raw)
    }

    fn tool_uses(&self, raw: &str) -> usize {
        claude_shaped_tool_uses(raw)
    }
}

/// One message's parts rendered as claude-shaped JSONL lines. A user
/// message's typed text parts join into one user row, its `synthetic: true`
/// text parts become the same row with `isMeta`; each `type: tool` part
/// becomes an assistant row carrying one `tool_use` block. Other shapes
/// render nothing.
fn render_message(out: &mut String, m_data: &str, parts: &[(String, i64)]) {
    let Ok(msg) = serde_json::from_str::<Value>(m_data) else {
        return;
    };
    let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
    let ts = parts.first().map(|p| p.1).unwrap_or(0).max(0);
    if role == "user" {
        let mut typed: Vec<String> = Vec::new();
        let mut synthetic: Vec<String> = Vec::new();
        for (p_data, _) in parts {
            let Ok(part) = serde_json::from_str::<Value>(p_data) else {
                continue;
            };
            if part.get("type").and_then(|v| v.as_str()) != Some("text") {
                continue;
            }
            let Some(text) = part.get("text").and_then(|v| v.as_str()) else {
                continue;
            };
            if part
                .get("synthetic")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                synthetic.push(text.to_string());
            } else {
                typed.push(text.to_string());
            }
        }
        if !typed.is_empty() {
            push_user_row(out, ts, &typed.join("\n"));
        }
        for text in synthetic {
            push_row(
                out,
                &serde_json::json!({
                    "type": "user", "isMeta": true, "timestamp": rfc3339_ms(ts),
                    "message": {"role": "user", "content": text},
                }),
            );
        }
    }
    for (p_data, _) in parts {
        let Ok(part) = serde_json::from_str::<Value>(p_data) else {
            continue;
        };
        if part.get("type").and_then(|v| v.as_str()) == Some("tool") {
            push_row(
                out,
                &serde_json::json!({
                    "type": "assistant",
                    "message": {"content": [{"type": "tool_use"}]},
                }),
            );
        }
    }
}

/// The 2.x read: group the stream rows by message and render each group.
fn read_v2(conn: &rusqlite::Connection, sid: &str) -> String {
    let Ok(mut stmt) = conn.prepare(V2_READ_SQL) else {
        return String::new();
    };
    let Ok(rows) = stmt
        .query_map([sid], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, Option<i64>>(6)?,
            ))
        })
        .map_err(|e| e.to_string())
    else {
        return String::new();
    };
    // The ordering keeps a message's rows adjacent: the message row first
    // (ordinal NULL), then its parts by ordinal.
    let mut current: Option<(String, String, String, Vec<(String, String, i64)>)> = None;
    let mut out = String::new();
    for row in rows.flatten() {
        let (_k, mid, mtype, mdata, ordinal, pdata, created, _ptype) = row;
        match &mut current {
            Some((cur_id, _, _, parts)) if *cur_id == mid => {
                if let (Some(ordinal), Some(pdata)) = (ordinal, pdata) {
                    parts.push((ordinal.to_string(), pdata, created));
                }
            }
            _ => {
                if let Some((_, mtype, mdata, parts)) = current.take() {
                    render_v2_message(&mut out, &mtype, &mdata, parts);
                }
                current = Some((mid, mtype, mdata, vec![]));
            }
        }
    }
    if let Some((_, mtype, mdata, parts)) = current.take() {
        render_v2_message(&mut out, &mtype, &mdata, parts);
    }
    out
}

/// One 2.x message group rendered as claude-shaped JSONL: a user message's
/// text comes from the message row itself, a synthetic message renders the
/// isMeta row, and an assistant message's tool parts render tool_use rows
/// (the same shapes the 1.x renderer emits).
fn render_v2_message(
    out: &mut String,
    mtype: &str,
    mdata: &str,
    parts: Vec<(String, String, i64)>,
) {
    let msg: Value = serde_json::from_str(mdata).unwrap_or(Value::Null);
    let ts = msg
        .get("time")
        .and_then(|t| t.get("created"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if mtype == "user" {
        if let Some(text) = msg.get("text").and_then(|v| v.as_str()) {
            push_user_row(out, ts, text);
        }
    }
    if mtype == "synthetic" {
        if let Some(text) = msg.get("text").and_then(|v| v.as_str()) {
            let row = serde_json::json!({
                "type": "user", "isMeta": true, "timestamp": rfc3339_ms(ts),
                "message": {"role": "user", "content": text},
            });
            push_row(out, &row);
        }
    }
    if mtype == "assistant" {
        for (_ordinal, ptype, _pdata) in &parts {
            if ptype == "tool" {
                let row = serde_json::json!({
                    "type": "assistant",
                    "message": {"content": [{"type": "tool_use"}]},
                });
                push_row(out, &row);
            }
        }
    }
}

pub(crate) fn push_row(out: &mut String, row: &Value) {
    out.push_str(&row.to_string());
    out.push('\n');
}

/// The claude-shaped user row every renderer emits: the shared wire shape
/// the classifier reads. `isMeta` rows (synthetic text) build their own via
/// [`push_row`].
pub(crate) fn push_user_row(out: &mut String, ts_ms: i64, content: &str) {
    push_row(
        out,
        &serde_json::json!({
            "type": "user", "timestamp": rfc3339_ms(ts_ms),
            "message": {"role": "user", "content": content},
        }),
    );
}

pub(crate) fn rfc3339_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp(ms.div_euclid(1000), 0)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::BusIndex;
    use rusqlite::Connection;

    const NOW_MS: i64 = 1_785_781_960_124;

    fn user_msg() -> &'static str {
        r#"{"role":"user","time":{"created":1785781960124}}"#
    }

    /// The plan fixture: one top-level session in /fixture/project whose user
    /// message has one typed text part, one synthetic text part, and one tool
    /// part; one child session with a parent_id; one top-level session in
    /// /elsewhere.
    fn fixture_store(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fno-opencode-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("opencode.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT, time_updated INTEGER);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, data TEXT);",
        )
        .unwrap();
        for (id, parent, directory) in [
            ("s1", None, "/fixture/project"),
            ("s2", Some("s1"), "/fixture/project"),
            ("s3", None, "/elsewhere"),
        ] {
            conn.execute(
                "INSERT INTO session VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, parent, directory, NOW_MS],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO message VALUES ('m1', 's1', ?1, ?2)",
            rusqlite::params![NOW_MS, user_msg()],
        )
        .unwrap();
        for (id, data) in [
            (
                r#"p1"#,
                r#"{"type":"text","text":"please widen the review gate"}"#,
            ),
            (
                "p2",
                r#"{"type":"text","text":"<environment_context>ignored</environment_context>","synthetic":true}"#,
            ),
            ("p3", r#"{"type":"tool","tool":"bash","callID":"c1"}"#),
        ] {
            conn.execute(
                "INSERT INTO part VALUES (?1, 'm1', 's1', ?2)",
                rusqlite::params![id, data],
            )
            .unwrap();
        }
        db
    }

    #[test]
    fn roots_narrow_to_the_project_and_drop_children() {
        let db = fixture_store("roots");
        let src = OpencodeSource {
            dbs: vec![db.clone()],
            roots: Some(vec![PathBuf::from("/fixture/project")]),
        };
        let sessions = src.sessions(0);
        assert_eq!(
            sessions.len(),
            1,
            "top-level s1 only, no child, no elsewhere"
        );
        assert_eq!(sessions[0].session_id, "s1");
        assert_eq!(
            sessions[0].path,
            PathBuf::from(format!("{}#s1", db.display()))
        );
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn the_render_classifies_operator_synthetic_and_tool() {
        let db = fixture_store("render");
        let src = OpencodeSource {
            dbs: vec![db.clone()],
            roots: None,
        };
        let file = &src.sessions(0)[0];
        let raw = src.read(file);
        let turns = src.turns(&raw);
        let mut counters: std::collections::BTreeMap<&str, u64> = std::collections::BTreeMap::new();
        for turn in &turns {
            let p = crate::provenance::classify_turn(&turn.obj, &BusIndex::empty(), "s1");
            *counters.entry(p.label()).or_insert(0) += 1;
        }
        assert_eq!(counters.get("unknown"), Some(&1), "typed text part");
        assert_eq!(
            counters.get("harness_synthetic"),
            Some(&1),
            "synthetic text part"
        );
        assert_eq!(src.tool_uses(&raw), 1, "tool part");
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    /// The positive control on opencode 1.14.50's REAL `session` schema: the
    /// reader must answer a store whose every NOT NULL column is present,
    /// and its freshness window must exclude a 3-day-old session. The
    /// columns mirror `sqlite3 .schema session` on 1.14.50.
    fn real_schema_store(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fno-opencode-real-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("opencode.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE `session` (
                `id` text PRIMARY KEY,
                `project_id` text NOT NULL,
                `parent_id` text,
                `slug` text NOT NULL,
                `directory` text NOT NULL,
                `title` text NOT NULL,
                `version` text NOT NULL,
                `share_url` text,
                `summary_additions` integer,
                `summary_deletions` integer,
                `summary_files` integer,
                `summary_diffs` text,
                `revert` text,
                `permission` text,
                `time_created` integer NOT NULL,
                `time_updated` integer NOT NULL,
                `time_compacting` integer,
                `time_archived` integer,
                `workspace_id` text,
                `path` text,
                `agent` text,
                `model` text,
                `cost` real DEFAULT 0 NOT NULL,
                `tokens_input` integer DEFAULT 0 NOT NULL,
                `tokens_output` integer DEFAULT 0 NOT NULL,
                `tokens_reasoning` integer DEFAULT 0 NOT NULL,
                `tokens_cache_read` integer DEFAULT 0 NOT NULL,
                `tokens_cache_write` integer DEFAULT 0 NOT NULL,
                `metadata` text
            );
            CREATE TABLE `message` (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
            CREATE TABLE `part` (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, data TEXT);",
        )
        .unwrap();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let old_ms = now_ms - 3 * 24 * 3600 * 1000;
        for (id, slug, updated) in [
            ("ses_fresh", "fresh-session", now_ms),
            ("ses_stale", "stale-session", old_ms),
        ] {
            conn.execute(
                "INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated) \
                 VALUES (?1, 'proj', ?2, '/fixture/real', 't', '1.14.50', ?3, ?3)",
                rusqlite::params![id, slug, updated],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO message VALUES ('m1', 'ses_fresh', ?1, ?2)",
            rusqlite::params![now_ms, user_msg()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part VALUES ('p1', 'm1', 'ses_fresh', ?1)",
            rusqlite::params![r#"{"type":"text","text":"real-schema control"}"#],
        )
        .unwrap();
        db
    }

    #[test]
    fn real_schema_session_window_lists_fresh_and_drops_stale() {
        let db = real_schema_store("window");
        let src = OpencodeSource {
            dbs: vec![db.clone()],
            roots: None,
        };
        let ids: Vec<String> = src
            .sessions(1)
            .iter()
            .map(|s| s.session_id.clone())
            .collect();
        assert_eq!(ids, vec!["ses_fresh".to_string()], "stale is out of window");
        // The read path answers the fresh session's parts on the same store.
        let file = &src.sessions(1)[0];
        assert!(src.read(file).contains("real-schema control"));
        // With the fresh session gone, sessions(1) lists nothing.
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute("DELETE FROM session WHERE id = 'ses_fresh'", [])
                .unwrap();
        }
        assert!(src.sessions(1).is_empty());
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    /// The 2.x positive control: same window and read contract as the 1.x
    /// real-schema control, against a store whose reader-touched columns
    /// mirror upstream's `session_v2` family DDL.
    fn v2_store(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fno-opencode-v2-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("opencode.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE session_v2 (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT NOT NULL, time_updated INTEGER NOT NULL);
             CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, type TEXT NOT NULL, seq INTEGER NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);
             CREATE TABLE session_part (message_id TEXT NOT NULL, session_id TEXT NOT NULL, ordinal INTEGER NOT NULL, type TEXT NOT NULL, data TEXT NOT NULL, PRIMARY KEY (message_id, ordinal));",
        )
        .unwrap();
        for (id, updated) in [
            ("ses_fresh", NOW_MS),
            ("ses_stale", NOW_MS - 3 * 24 * 3600 * 1000),
        ] {
            conn.execute(
                "INSERT INTO session_v2 (id, parent_id, directory, time_updated) VALUES (?1, NULL, '/fixture/v2', ?2)",
                rusqlite::params![id, updated],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES \
             ('vm1', 'ses_fresh', 'user', 1, ?1, ?1, ?2)",
            rusqlite::params![NOW_MS, r#"{"text":"v2 real-schema control"}"#],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES \
             ('vm2', 'ses_fresh', 'assistant', 2, ?1, ?1, ?2)",
            rusqlite::params![NOW_MS, r#"{"time":{"created":2}}"#],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_part (message_id, session_id, ordinal, type, data) VALUES \
             ('vm2', 'ses_fresh', 0, 'tool', ?1)",
            rusqlite::params![
                r#"{"type":"tool","name":"bash","state":{"status":"completed","input":{}}}"#
            ],
        )
        .unwrap();
        db
    }

    #[test]
    fn v2_real_schema_window_and_read() {
        let db = v2_store("v2win");
        let src = OpencodeSource {
            dbs: vec![db.clone()],
            roots: None,
        };
        let ids: Vec<String> = src
            .sessions(1)
            .iter()
            .map(|s| s.session_id.clone())
            .collect();
        assert_eq!(ids, vec!["ses_fresh".to_string()]);
        let raw = src.read(&src.sessions(1)[0]);
        assert!(
            raw.contains("v2 real-schema control"),
            "user text renders: {raw}"
        );
        assert_eq!(src.tool_uses(&raw), 1, "tool part renders");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute("DELETE FROM session_v2 WHERE id = 'ses_fresh'", [])
                .unwrap();
        }
        assert!(src.sessions(1).is_empty());
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }
}
