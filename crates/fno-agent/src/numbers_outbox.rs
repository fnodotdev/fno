//! The outbox: an ask or a ruling made while the primary was away. It holds
//! no id. A beat stamps the number, writes the record through the doors'
//! own append paths, and only then drops the row.

use super::{answer_key_at, authority, reserve, MintError};
use crate::numbers::{open_read_at, open_write_at, retired};
use crate::store_remote::Remote;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// What a door prints in place of a number.
pub(crate) const QUEUED: &str = "queued, number pending";

const DDL: &str = "CREATE TABLE IF NOT EXISTS fno_number_outbox (\
seq INTEGER PRIMARY KEY AUTOINCREMENT, \
same TEXT NOT NULL UNIQUE, \
record TEXT NOT NULL, \
label TEXT, \
created_at INTEGER NOT NULL)";

/// The work a door does once its ruling is recorded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum After {
    Nothing,
    Law { sweep: Vec<String> },
    Decide { question: Option<String> },
}

/// One record with every field but its id.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub(crate) enum Queued {
    Question {
        event: Value,
        journal: PathBuf,
        index: PathBuf,
    },
    Decision {
        event: Value,
        journal: PathBuf,
        index: PathBuf,
        graph: PathBuf,
        /// A law row signs over its id, so it signs when the number lands.
        sign: bool,
        after: After,
    },
}

#[derive(Default, Deserialize)]
struct Envelope {
    #[serde(default)]
    ts: String,
    #[serde(default)]
    data: Fields,
}

#[derive(Default, Deserialize)]
struct Fields {
    question: Option<String>,
    question_id: Option<String>,
    subject: Option<String>,
    node: Option<String>,
    session_id: Option<String>,
    decision: Option<String>,
    supersedes: Option<String>,
    decided_by: Option<String>,
    authority_source: Option<String>,
}

impl Queued {
    fn event(&self) -> &Value {
        match self {
            Queued::Question { event, .. } | Queued::Decision { event, .. } => event,
        }
    }

    fn envelope(&self) -> Result<Envelope, MintError> {
        Envelope::deserialize(self.event()).map_err(store_error)
    }

    /// Two writes of one record share this key, so a retry amends its row.
    fn same(&self) -> Result<String, MintError> {
        let f = self.envelope()?.data;
        let key = match self {
            Queued::Question { .. } => {
                serde_json::json!(["question", f.subject.or(f.question), f.node, f.session_id])
            }
            Queued::Decision { .. } => serde_json::json!([
                "decision",
                f.subject,
                f.decision,
                f.question_id,
                f.supersedes
            ]),
        };
        Ok(key.to_string())
    }
}

fn store_error(error: impl std::fmt::Display) -> MintError {
    MintError::Store(error.to_string())
}

/// Hold `record` until a beat can number it. The newest copy wins until
/// then; a numbered row is already being written and stays as it is.
pub(crate) fn enqueue(db: &Path, record: &Queued) -> Result<(), MintError> {
    let conn = open_write_at(db).map_err(MintError::Store)?;
    conn.execute_batch(DDL)?;
    conn.execute(
        "INSERT INTO fno_number_outbox (same, record, created_at) \
         VALUES (?1, ?2, strftime('%s','now')) \
         ON CONFLICT(same) DO UPDATE SET record = excluded.record WHERE label IS NULL",
        params![
            record.same()?,
            serde_json::to_string(record).map_err(store_error)?
        ],
    )?;
    Ok(())
}

/// [`enqueue`] in this machine's store.
pub(crate) fn queue(record: &Queued) -> Result<(), MintError> {
    enqueue(&crate::numbers::db_path(), record)
}

/// Why a machine-raised question was not filed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FileError {
    #[error(transparent)]
    Mint(#[from] MintError),
    #[error("{0}")]
    Write(String),
}

/// File a question the machine raised: numbered now, or queued (`Ok(None)`).
pub(crate) fn file_question(
    mut event: Value,
    journal: PathBuf,
    index: PathBuf,
) -> Result<Option<String>, FileError> {
    let Some(id) = super::mint_key()? else {
        queue(&Queued::Question {
            event,
            journal,
            index,
        })?;
        return Ok(None);
    };
    stamp(&mut event, "question_id", &id);
    crate::question_intake::append_question_event(&event, &journal, &index)
        .map_err(|(_, line)| FileError::Write(line))?;
    Ok(Some(id))
}

/// Write the id into a record, and into its span: the row is the span.
fn stamp(event: &mut Value, field: &str, id: &str) {
    if let Some(data) = event.pointer_mut("/data").and_then(Value::as_object_mut) {
        data.insert(field.to_string(), id.into());
        if let Some(trace) = data.get_mut("trace").and_then(Value::as_object_mut) {
            trace.insert("span_id".to_string(), id.into());
        }
    }
}

/// A record queued before the migration can cite a key it retired since.
fn retire_refs(conn: &Connection, event: &mut Value) {
    for at in [
        "/data/question_id",
        "/data/supersedes",
        "/data/overturns",
        "/data/trace/parent_span_id",
    ] {
        if let Some(slot) = event.pointer_mut(at) {
            if let Some(label) = slot.as_str().and_then(|hex| retired(conn, hex)) {
                *slot = label.into();
            }
        }
    }
}

/// A question still waiting for its number.
pub(crate) struct Waiting {
    pub seq: i64,
    pub title: String,
    pub ts: String,
    pub node: Option<String>,
    pub subject: Option<String>,
}

/// The queued questions, oldest first, for the listings that show them.
pub(crate) fn queued_questions(db: &Path) -> Vec<Waiting> {
    let Some(conn) = open_read_at(db) else {
        return Vec::new();
    };
    let rows = match rows_of(&conn) {
        Ok(rows) => rows,
        Err(error) => {
            eprintln!("numbers outbox: cannot list the queued questions ({error})");
            return Vec::new();
        }
    };
    rows.into_iter()
        .filter_map(|(seq, record)| {
            let record: Queued = serde_json::from_str(&record).ok()?;
            let Queued::Question { .. } = record else {
                return None;
            };
            let Envelope { ts, data } = record.envelope().ok()?;
            let text = data.question.unwrap_or_default();
            Some(Waiting {
                seq,
                title: text.lines().next().unwrap_or_default().to_string(),
                ts,
                node: data.node,
                subject: data.subject,
            })
        })
        .collect()
}

fn rows_of(conn: &Connection) -> Result<Vec<(i64, String)>, MintError> {
    let table: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master \
         WHERE type = 'table' AND name = 'fno_number_outbox')",
        [],
        |row| row.get(0),
    )?;
    if !table {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare("SELECT seq, record FROM fno_number_outbox ORDER BY seq")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Number and write what waited. The daemon beat's first step.
pub(crate) fn drain() -> Result<usize, MintError> {
    let (remote, host) = authority()?;
    drain_at(&crate::numbers::db_path(), remote.as_ref(), &host)
}

/// How many records landed. An outage ends the pass; the next beat retries.
pub(crate) fn drain_at(db: &Path, remote: Option<&Remote>, host: &str) -> Result<usize, MintError> {
    let conn = open_write_at(db).map_err(MintError::Store)?;
    let mut landed = 0;
    for (seq, record) in rows_of(&conn)? {
        let Ok(mut queued) = serde_json::from_str::<Queued>(&record) else {
            eprintln!("numbers outbox: row {seq} does not parse; it stays queued");
            continue;
        };
        let Some(saved) = conn
            .query_row(
                "SELECT label FROM fno_number_outbox WHERE seq = ?1",
                params![seq],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
        else {
            continue;
        };
        if saved.is_none() {
            let Some(label) = number_for(&conn, db, &queued, remote, host)? else {
                return Ok(landed);
            };
            conn.execute(
                "UPDATE fno_number_outbox SET label = ?2 WHERE seq = ?1 AND label IS NULL",
                params![seq, label],
            )?;
        }
        // The label freezes the row, so this read is the record that lands.
        let Some((label, record)) = conn
            .query_row(
                "SELECT label, record FROM fno_number_outbox WHERE seq = ?1",
                params![seq],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        else {
            continue;
        };
        queued = serde_json::from_str(&record).map_err(store_error)?;
        if let Err(error) = write(&conn, &mut queued, &label) {
            eprintln!("numbers outbox: {label} is not recorded yet ({error}); the beat retries");
            continue;
        }
        let dropped = conn.execute("DELETE FROM fno_number_outbox WHERE seq = ?1", params![seq])?;
        // Only the pass that dropped the row mails and closes.
        if dropped == 1 {
            landed += 1;
            follow_up(&queued, &label);
        }
    }
    Ok(landed)
}

fn number_for(
    conn: &Connection,
    db: &Path,
    queued: &Queued,
    remote: Option<&Remote>,
    host: &str,
) -> Result<Option<String>, MintError> {
    let question = match queued {
        Queued::Question { .. } => None,
        Queued::Decision { .. } => queued.envelope()?.data.question_id,
    };
    let Some(question) = question else {
        return reserve(conn, remote, host);
    };
    let Some(label) = answer_key_at(db, &question, remote, host)? else {
        return Ok(None);
    };
    // An earlier answer still in the outbox holds the number already.
    let held: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM fno_number_outbox WHERE label = ?1)",
        params![label],
        |row| row.get(0),
    )?;
    if held {
        return reserve(conn, remote, host);
    }
    Ok(Some(label))
}

fn write(conn: &Connection, queued: &mut Queued, label: &str) -> Result<(), FileError> {
    match queued {
        Queued::Question {
            event,
            journal,
            index,
        } => {
            stamp(event, "question_id", label);
            crate::question_intake::append_question_event(event, journal, index)
                .map_err(|(_, line)| FileError::Write(line))
        }
        Queued::Decision {
            event,
            journal,
            index,
            graph,
            sign,
            ..
        } => {
            retire_refs(conn, event);
            stamp(event, "decision_id", label);
            if *sign {
                let ts = Envelope::deserialize(&*event).map_err(store_error)?.ts;
                if let Some(data) = event.pointer_mut("/data").and_then(Value::as_object_mut) {
                    crate::identity::sign_law_row(data, "operator_decision", &ts);
                }
            }
            let line = event.to_string();
            for path in [&*journal, &*index] {
                crate::event_store::append_envelope(path, &line, None)
                    .map_err(|e| FileError::Write(e.to_string()))?;
            }
            crate::backlog::api::decision_record(
                &crate::backlog::api::Store::new(graph.as_path()),
                event.clone(),
            )
            .map(drop)
            .map_err(|e| FileError::Write(e.0))
        }
    }
}

fn follow_up(queued: &Queued, label: &str) {
    let Queued::Decision { journal, after, .. } = queued else {
        return;
    };
    let Ok(Envelope { data, .. }) = queued.envelope() else {
        return;
    };
    let subject = data.subject.as_deref().unwrap_or_default();
    let decision = data.decision.as_deref().unwrap_or_default();
    match after {
        After::Nothing => {}
        After::Law { sweep } => crate::law_match::after_law(label, subject, decision, sweep),
        After::Decide { question } => crate::decide_door::after_decide(
            journal,
            label,
            subject,
            decision,
            data.decided_by.as_deref(),
            data.authority_source.as_deref(),
            question.as_deref(),
        ),
    }
}
