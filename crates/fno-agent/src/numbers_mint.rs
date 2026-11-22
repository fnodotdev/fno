//! The number is the key: a question or decision takes its `D<n>` when it is
//! created, at the store's authority. An unreachable primary mints nothing
//! here. The record waits in the [`outbox`] with no id until a beat can
//! number it, so one number never names two records.

use super::{
    canonical, is_label_key, is_question_hash, mint_local, mint_remote, open_write_at, pull,
    retired, DDL, LOCAL_STORE,
};
use crate::store_remote::{Remote, SqlValue};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

#[path = "numbers_outbox.rs"]
mod outbox;
pub(crate) use outbox::*;

const RESERVE: &str = "INSERT INTO fno_numbers \
(kind, num, key, store, host, created_at) \
SELECT 'd', COALESCE(MAX(num), 0) + 1, 'D' || (COALESCE(MAX(num), 0) + 1), \
?1, ?2, strftime('%s','now') \
FROM fno_numbers WHERE kind = 'd' AND store = ?1 RETURNING key";

/// Why a record got no number. An unreachable primary is not here: that
/// reads `Ok(None)` and the caller queues the record.
#[derive(Debug, thiserror::Error)]
pub(crate) enum MintError {
    #[error("no number minted: the store config is unreadable: {0}")]
    Config(String),
    #[error("no number minted: the local store failed: {0}")]
    Store(String),
    #[error("no number minted: the primary {host} refused: {detail}")]
    Primary { host: String, detail: String },
}

impl From<rusqlite::Error> for MintError {
    fn from(error: rusqlite::Error) -> Self {
        MintError::Store(error.to_string())
    }
}

/// An outage is `Ok`, so the caller queues. Anything else is the error.
fn outage(remote: &Remote, detail: String) -> Result<(), MintError> {
    if crate::store_remote::is_unreachable(&detail) {
        return Ok(());
    }
    Err(MintError::Primary {
        host: remote.host().to_string(),
        detail,
    })
}

fn authority() -> Result<(Option<Remote>, String), MintError> {
    let remote = crate::store_remote::configured().map_err(MintError::Config)?;
    let host = fno_mail::address::normalize_host(&crate::claims::hostname());
    Ok((remote, host))
}

/// The key of a new question or decision. `Ok(None)` means queue it.
pub(crate) fn mint_key() -> Result<Option<String>, MintError> {
    let (remote, host) = authority()?;
    mint_key_at(&super::db_path(), remote.as_ref(), &host)
}

pub(crate) fn mint_key_at(
    db: &Path,
    remote: Option<&Remote>,
    host: &str,
) -> Result<Option<String>, MintError> {
    let conn = open_write_at(db).map_err(MintError::Store)?;
    reserve(&conn, remote, host)
}

fn reserve(
    conn: &Connection,
    remote: Option<&Remote>,
    host: &str,
) -> Result<Option<String>, MintError> {
    let Some(remote) = remote else {
        let key = conn.query_row(RESERVE, params![LOCAL_STORE, host], |row| row.get(0))?;
        return Ok(Some(key));
    };
    let reserved = remote.script(DDL).and_then(|()| {
        remote.execute(
            RESERVE,
            &[
                SqlValue::Text(remote.host().to_string()),
                SqlValue::Text(host.to_string()),
            ],
        )
    });
    let reply = match reserved {
        Ok(reply) => reply,
        Err(detail) => return outage(remote, detail).map(|()| None),
    };
    let key = reply
        .rows
        .first()
        .and_then(|row| row.first())
        .and_then(SqlValue::text)
        .ok_or_else(|| MintError::Primary {
            host: remote.host().to_string(),
            detail: "it returned no key".to_string(),
        })?
        .to_string();
    // The number is spent at the primary. A failed cache pull costs only the
    // early read, so it never loses the key.
    if let Err(error) = pull(remote, conn) {
        eprintln!("{key}: number cache pull failed ({error}); the beat retries");
    }
    Ok(Some(key))
}

/// The key of the decision that answers `question`: that question's own
/// number, until a decision holds it. `Ok(None)` means queue it.
pub(crate) fn answer_key(graph: &Path, question: &str) -> Result<Option<String>, MintError> {
    let (remote, host) = authority()?;
    let db = crate::backlog::database_path(graph);
    answer_key_at(&db, question, remote.as_ref(), &host)
}

pub(crate) fn answer_key_at(
    db: &Path,
    question: &str,
    remote: Option<&Remote>,
    host: &str,
) -> Result<Option<String>, MintError> {
    let conn = open_write_at(db).map_err(MintError::Store)?;
    let store = remote.map_or(LOCAL_STORE, Remote::host);
    let question = canonical(question);
    let mut label = own_number(&conn, question, store)?.or_else(|| retired(&conn, question));
    if label.is_none() && (is_label_key(question) || is_question_hash(question)) {
        // An older question the beat has not reached gets its number now. A
        // number is never made up from typed text: a label needs a row.
        match remote {
            None if is_question_hash(question) => {
                mint_local(&conn, question, store, host).map_err(MintError::Store)?;
            }
            None => {}
            Some(remote) => {
                let numbered = remote.script(DDL).and_then(|()| {
                    if is_question_hash(question) {
                        mint_remote(remote, &[question.to_string()], host)?;
                    }
                    pull(remote, &conn)
                });
                if let Err(detail) = numbered {
                    return outage(remote, detail).map(|()| None);
                }
            }
        }
        label = own_number(&conn, question, store)?;
    }
    if let Some(label) = label {
        if !decision_holds(&conn, &label, question)? {
            return Ok(Some(label));
        }
    }
    reserve(&conn, remote, host)
}

/// This store's number for `key`. Another store's number is never a key here.
fn own_number(conn: &Connection, key: &str, store: &str) -> Result<Option<String>, MintError> {
    Ok(conn
        .query_row(
            "SELECT upper(kind) || num FROM fno_numbers WHERE key = ?1 AND store = ?2",
            params![key, store],
            |row| row.get(0),
        )
        .optional()?)
}

/// Whether a decision already holds the number or answers the question.
/// `decisions.event_id` drops a conflicting insert in silence, so only a
/// missing table reads as free.
fn decision_holds(conn: &Connection, label: &str, question: &str) -> Result<bool, MintError> {
    let table: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'decisions')",
        [],
        |row| row.get(0),
    )?;
    if !table {
        return Ok(false);
    }
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM decisions WHERE event_id IN (?1, ?2) \
         OR (json_valid(data) AND json_extract(data, '$.question_id') IN (?1, ?2)))",
        params![label, question],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
#[path = "numbers_mint_tests.rs"]
mod tests;
