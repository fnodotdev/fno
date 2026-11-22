//! `fno doctor migrate ids` (rides `census --ids`): the map from every
//! `q-`/`d-` hex key to the number that replaces it. `--check` counts what
//! is left to retire; `--dry-run` (the default) builds the map and refuses
//! on any key the map cannot carry. `--apply` and `--rollback` live in
//! [`apply`].

use super::{answer_links, beat, canonical, current_store, db_path, open_read_at};
use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

/// The code paths that still mint a hex key for a question or decision.
/// Each entry leaves this list in the change that deletes its mint, and the
/// cutover is done when `--check` reads zero here and zero hex keys.
const HEX_MINT_SITES: &[&str] = &[];

fn is_hex_key(key: &str) -> bool {
    let tail = key.strip_prefix("d-").or_else(|| key.strip_prefix("q-"));
    tail.is_some_and(|hex| hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The one field a decision row contributes to the key census.
#[derive(Deserialize)]
struct DecisionIdRow {
    #[serde(default)]
    decision_id: String,
}

/// Why the hex keys could not be read.
#[derive(Debug, thiserror::Error)]
enum KeysError {
    #[error("the decision store is unreadable: {0}")]
    Decisions(String),
    #[error("the numbers table is unreadable: {0}")]
    Numbers(#[from] rusqlite::Error),
}

/// The hex keys this machine holds: decision rows, asked questions, and
/// every numbered key, copies folded to their bare id.
fn store_keys(conn: Option<&Connection>) -> Result<BTreeSet<String>, KeysError> {
    let decisions = crate::decision_index::default_store_live().map_err(KeysError::Decisions)?;
    let home = crate::paths::AgentsHome::from_env();
    let (asked, _) =
        crate::question_intake::question_rows(&crate::provider_cap::questions_path(&home));
    let mut keys: BTreeSet<String> = decisions
        .rows
        .iter()
        .filter_map(|row| DecisionIdRow::deserialize(row).ok())
        .map(|row| row.decision_id)
        .chain(asked.keys().cloned())
        .map(|id| canonical(&id).to_string())
        .collect();
    if let Some(conn) = conn {
        let mut stmt = conn.prepare("SELECT DISTINCT key FROM fno_numbers")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        keys.extend(rows.flatten());
    }
    keys.retain(|key| is_hex_key(key));
    Ok(keys)
}

/// The migration map over one store, and every key it refuses.
#[derive(Default, Debug)]
pub(crate) struct IdMap {
    pub mapped: BTreeMap<String, String>,
    /// Answer decisions that take their question's number.
    pub answers: usize,
    pub unmapped: Vec<String>,
    /// A number two unlinked keys both claim.
    pub collisions: Vec<(String, Vec<String>)>,
    /// A key numbered only in another store, with that store.
    pub foreign_only: Vec<(String, String)>,
    /// A later answer to a question an earlier answer took the number of.
    /// It restates the ruling as its own record, so apply mints it a number.
    pub fresh: Vec<String>,
}

impl IdMap {
    pub fn refuses(&self) -> bool {
        !(self.unmapped.is_empty() && self.collisions.is_empty() && self.foreign_only.is_empty())
    }
}

fn own_label(conn: &Connection, key: &str, current: &str) -> Option<String> {
    conn.query_row(
        "SELECT kind, num FROM fno_numbers WHERE key = ?1 AND store = ?2",
        params![key, current],
        |row| {
            Ok(format!(
                "{}{}",
                row.get::<_, String>(0)?.to_uppercase(),
                row.get::<_, i64>(1)?
            ))
        },
    )
    .ok()
}

/// A decision row's place in commit order; a key with no row sorts last.
fn seq_of(conn: &Connection, key: &str) -> i64 {
    conn.query_row(
        "SELECT seq FROM decisions WHERE event_id = ?1",
        params![key],
        |row| row.get(0),
    )
    .unwrap_or(i64::MAX)
}

/// Whether a decision's key is this number already.
fn decision_holds_key(conn: &Connection, label: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM decisions WHERE event_id = ?1",
        params![label],
        |_| Ok(()),
    )
    .is_ok()
}

fn foreign_store(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT store FROM fno_numbers WHERE key = ?1 ORDER BY created_at DESC LIMIT 1",
        params![key],
        |row| row.get(0),
    )
    .ok()
}

/// Map each key to its number in `current`. Only the current store's number
/// counts: another store's number for the same record is dropped, never
/// kept as an alias. An answer takes its question's number before its own
/// row: an older beat numbered some answers, and one record keeps one number.
pub(crate) fn plan_map(conn: &Connection, keys: &BTreeSet<String>, current: &str) -> IdMap {
    let mut out = IdMap::default();
    let mut by_label: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let links = answer_links(conn);
    // The earliest answer of a question takes its number. A later one is
    // its own record: it keeps a number of its own, or apply mints one.
    let mut first: BTreeMap<&str, (i64, &str)> = BTreeMap::new();
    for key in keys {
        if let Some(question) = links.get(key) {
            let seq = seq_of(conn, key);
            let slot = first.entry(canonical(question)).or_insert((seq, key));
            if seq < slot.0 {
                *slot = (seq, key);
            }
        }
    }
    for key in keys {
        let question = links.get(key).map(|question| canonical(question));
        // An answer written since the mint holds the number as its key.
        let taken = question
            .and_then(|question| own_label(conn, question, current))
            .is_some_and(|label| decision_holds_key(conn, &label));
        let later = taken
            || question
                .and_then(|question| first.get(question))
                .is_some_and(|(_, earliest)| *earliest != key.as_str());
        if later && own_label(conn, key, current).is_none() {
            out.fresh.push(key.clone());
            continue;
        }
        let via_question = links
            .get(key)
            .filter(|_| !later)
            .and_then(|question| own_label(conn, canonical(question), current));
        if let Some(label) = via_question {
            out.answers += 1;
            out.mapped.insert(key.clone(), label);
            continue;
        }
        match own_label(conn, key, current) {
            Some(label) => {
                by_label.entry(label.clone()).or_default().push(key.clone());
                out.mapped.insert(key.clone(), label);
            }
            None => match foreign_store(conn, key) {
                Some(store) => out.foreign_only.push((key.clone(), store)),
                None => out.unmapped.push(key.clone()),
            },
        }
    }
    out.collisions = by_label
        .into_iter()
        .filter(|(_, keys)| keys.len() > 1)
        .collect();
    out
}

/// `census --ids [--check|--dry-run|--apply|--rollback <dir>] [--json]`.
pub fn run(args: &[String]) -> i32 {
    let json_out = args.iter().any(|a| a == "--json" || a == "-J");
    if let Some(at) = args.iter().position(|a| a == "--rollback") {
        let Some(dir) = args.get(at + 1) else {
            eprintln!("migrate ids: --rollback names the backup dir an apply printed");
            return 2;
        };
        return apply::rollback(std::path::Path::new(dir));
    }
    let check = args.iter().any(|a| a == "--check");
    if !check {
        // One beat first, so a key filed since the last beat has its number.
        if let Ok(cwd) = std::env::current_dir() {
            if let Err(error) = beat(&cwd) {
                eprintln!("migrate ids: the numbering beat failed ({error}); mapping what is held");
            }
        }
    }
    let conn = open_read_at(&db_path());
    let keys = match store_keys(conn.as_ref()) {
        Ok(keys) => keys,
        Err(error) => {
            eprintln!("migrate ids: {error}");
            return 1;
        }
    };
    if check {
        let report = json!({"hex_keys": keys.len(), "hex_mint_sites": HEX_MINT_SITES.len()});
        if json_out {
            println!("{report}");
        } else {
            println!(
                "hex keys: {}\nhex mint sites: {}",
                keys.len(),
                HEX_MINT_SITES.len()
            );
        }
        return 0;
    }
    if args.iter().any(|a| a == "--apply") {
        drop(conn);
        return apply::apply(json_out);
    }
    let Some(conn) = conn else {
        eprintln!("migrate ids: cannot read {}", db_path().display());
        return 1;
    };
    let current = current_store();
    let map = plan_map(&conn, &keys, &current);
    if json_out {
        println!(
            "{}",
            json!({
                "store": current,
                "mapped": map.mapped.len(),
                "answers": map.answers,
                "unmapped": map.unmapped,
                "collisions": map.collisions.iter().map(|(l, k)| json!({"label": l, "keys": k})).collect::<Vec<_>>(),
                "foreign_only": map.foreign_only.iter().map(|(k, s)| json!({"key": k, "store": s})).collect::<Vec<_>>(),
                "fresh": map.fresh,
            })
        );
    } else {
        println!(
            "store {current}: {} keys map to numbers ({} answers take their question's number)",
            map.mapped.len(),
            map.answers
        );
        for key in &map.fresh {
            println!("fresh: {key} is a later answer; apply mints it a number of its own");
        }
        for key in &map.unmapped {
            println!("refused: {key} has no number in any store");
        }
        for (label, keys) in &map.collisions {
            println!("refused: {label} is claimed by {}", keys.join(", "));
        }
        for (key, store) in &map.foreign_only {
            println!(
                "refused: {key} is numbered only in store {store}; join the primary and let the beat re-number it"
            );
        }
    }
    i32::from(map.refuses())
}

#[path = "numbers_migrate_apply.rs"]
mod apply;

#[cfg(test)]
mod tests {
    use super::super::{mint_local, open_write_at};
    use super::*;

    fn store() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_write_at(&dir.path().join("graph.db")).unwrap();
        conn.execute_batch("CREATE TABLE decisions (data TEXT)")
            .unwrap();
        (dir, conn)
    }

    fn keys(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn an_answer_maps_to_its_questions_number_and_nothing_is_unmapped() {
        let (_dir, conn) = store();
        // The answer also holds an older row of its own (D5): the link wins.
        for key in [
            "d-00000a01",
            "d-00000a02",
            "d-00000a03",
            "q-00000b01",
            "d-00000c01",
        ] {
            mint_local(&conn, key, "r2d2", "h").unwrap();
        }
        conn.execute(
            "INSERT INTO decisions (data) VALUES (?1)",
            [r#"{"decision_id":"d-00000c01","trace":{"parent_span_id":"q-00000b01"}}"#],
        )
        .unwrap();
        let all = keys(&[
            "d-00000a01",
            "d-00000a02",
            "d-00000a03",
            "q-00000b01",
            "d-00000c01",
        ]);
        let map = plan_map(&conn, &all, "r2d2");
        assert!(!map.refuses(), "{map:?}");
        assert_eq!(map.mapped.get("d-00000c01").map(String::as_str), Some("D4"));
        assert_eq!(map.mapped.get("q-00000b01").map(String::as_str), Some("D4"));
        assert_eq!(map.answers, 1);
    }

    #[test]
    fn a_key_numbered_only_in_another_store_is_refused_with_that_store() {
        let (_dir, conn) = store();
        mint_local(&conn, "d-00000a01", "r2d2", "h").unwrap();
        mint_local(&conn, "d-00000f01", "100.75.150.88", "h").unwrap();
        let map = plan_map(
            &conn,
            &keys(&["d-00000a01", "d-00000f01", "q-0000dead"]),
            "r2d2",
        );
        assert!(map.refuses());
        assert_eq!(
            map.foreign_only,
            [("d-00000f01".to_string(), "100.75.150.88".to_string())]
        );
        assert_eq!(map.unmapped, ["q-0000dead"]);
    }
}
