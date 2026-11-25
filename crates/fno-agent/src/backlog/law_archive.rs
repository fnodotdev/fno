//! `fno inbox decisions archive [--apply]`: move every non-live decision row
//! to the cold archive file, `decisions-archive.jsonl` at the state root
//! (node x-85f4). The move is a projection, not a delete: rows are copied
//! row-faithfully (ts, type, data) into the archive, the default reads skip
//! them through the archive-key filter in `read_store_rows`, and the
//! citation gate joins the archive back in so every d-id stays resolvable.
//! Nothing is deleted from the db or the journal.

use serde_json::{json, Value};

use crate::backlog::decisions::SCOPE_AMEND_EVENT;
use crate::decision_index::{
    default_archive_path, default_state_path, derive_rows, drop_expired, read_archive_rows,
    read_store_rows, read_store_rows_raw,
};
use crate::graph_get::default_graph_path;

const ARCHIVE_USAGE: &str = "usage: fno inbox decisions archive [--apply]  (no flag: the dry run)";

fn row_key(row: &Value) -> (String, String) {
    let etype = row
        .get("_event_type")
        .and_then(Value::as_str)
        .unwrap_or("operator_decision")
        .to_string();
    let id = ["decision_id", "retraction_id", "target_decision_id"]
        .iter()
        .find_map(|k| row.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()))
        .unwrap_or("")
        .to_string();
    (etype, id)
}

/// The envelope a flattened row reconstructs to: its own fields under
/// `data`, the event type back on the envelope, ts on top. The db's
/// `source` column is not carried, so the line holds ts/type/data only.
fn envelope_of(row: &Value) -> Value {
    let etype = row
        .get("_event_type")
        .and_then(Value::as_str)
        .unwrap_or("operator_decision");
    let ts = row.get("ts").cloned().unwrap_or(Value::Null);
    let mut data = row.as_object().cloned().unwrap_or_default();
    data.remove("_event_type");
    data.remove("ts");
    json!({"ts": ts, "type": etype, "data": data})
}

/// The partition: rows that stay (a live decision, or an amendment whose
/// target stays) versus rows that move to the archive. Pure over the
/// inputs, so every rule is testable without a store.
fn partition(
    raw: &[Value],
    live_ids: &std::collections::BTreeSet<String>,
    existing: &std::collections::BTreeSet<(String, String)>,
) -> (Vec<Value>, usize) {
    let mut to_move: Vec<Value> = Vec::new();
    let mut kept = 0usize;
    for row in raw {
        let etype = row.get("_event_type").and_then(Value::as_str).unwrap_or("");
        let key = row_key(row);
        if existing.contains(&key) {
            kept += 1;
            continue;
        }
        let stays = match etype {
            "operator_decision" => {
                let id = row.get("decision_id").and_then(Value::as_str).unwrap_or("");
                !id.is_empty() && live_ids.contains(&id.to_lowercase())
            }
            SCOPE_AMEND_EVENT => {
                let t = row
                    .get("target_decision_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                !t.is_empty() && live_ids.contains(&t.to_lowercase())
            }
            _ => false,
        };
        if stays {
            kept += 1;
        } else {
            to_move.push(envelope_of(row));
        }
    }
    (to_move, kept)
}

pub fn run_archive(argv: &[String]) -> i32 {
    if argv.iter().any(|a| a.contains("--help")) {
        println!("{ARCHIVE_USAGE}");
        return 0;
    }
    for bad in argv.iter().filter(|a| a.as_str() != "--apply") {
        eprintln!("fno inbox decisions archive: no such option: {bad}\n{ARCHIVE_USAGE}");
        return 2;
    }
    let apply = argv.iter().any(|a| a.as_str() == "--apply");
    let graph = default_graph_path();
    let jsonl = default_state_path("decisions.jsonl");
    let (raw, _damaged) = match read_store_rows_raw(&graph, &jsonl) {
        Ok(pair) => pair,
        Err(reason) => {
            eprintln!(
                "fno inbox decisions archive: the decision store could not be read ({reason})"
            );
            return 1;
        }
    };
    // The live view: the public read (amendments applied, archived keys
    // skipped, unproven withheld) through the lifecycle derivation and the
    // expiry join. These are the ids that stay.
    let (pub_rows, _) = match read_store_rows(&graph, &jsonl) {
        Ok(pair) => pair,
        Err(reason) => {
            eprintln!(
                "fno inbox decisions archive: the decision store could not be read ({reason})"
            );
            return 1;
        }
    };
    let mut live_index = derive_rows(pub_rows, 0);
    drop_expired(&mut live_index, &graph);
    let live_ids: std::collections::BTreeSet<String> = live_index
        .rows
        .iter()
        .filter(|r| r.get("_event_type").and_then(Value::as_str) == Some("operator_decision"))
        .filter_map(|r| r.get("decision_id").and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .map(|id| id.to_lowercase())
        .collect();
    let existing: std::collections::BTreeSet<(String, String)> =
        read_archive_rows(&default_archive_path())
            .0
            .iter()
            .map(row_key)
            .collect();
    let (to_move, kept) = partition(&raw, &live_ids, &existing);
    if !apply {
        println!(
            "would move {} row(s) to {} ({} row(s) stay); pass --apply to move them",
            to_move.len(),
            default_archive_path().display(),
            kept
        );
        return 0;
    }
    if to_move.is_empty() {
        println!(
            "nothing to move; the archive is current at {}",
            default_archive_path().display()
        );
        return 0;
    }
    use std::io::Write as IoWrite;
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(default_archive_path())
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("fno inbox decisions archive: cannot open the archive ({e})");
            return 1;
        }
    };
    let mut moved = 0usize;
    let mut write_failed = false;
    for env in &to_move {
        let line = serde_json::to_string(env).unwrap_or_default();
        if line.is_empty() || writeln!(file, "{line}").is_err() {
            write_failed = true;
            break;
        }
        moved += 1;
    }
    let _ = file.flush();
    if write_failed {
        eprintln!("fno inbox decisions archive: the archive write failed after {moved} row(s)");
        return 1;
    }
    println!(
        "moved {moved} row(s) to {}",
        default_archive_path().display()
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(id: &str, supersedes: Option<&str>) -> Value {
        let mut row = json!({
            "_event_type": "operator_decision",
            "decision_id": id,
            "subject": "s",
            "decision": "text",
            "ts": "2026-09-01T00:00:00Z",
        });
        if let Some(s) = supersedes {
            row["supersedes"] = json!(s);
        }
        row
    }

    #[test]
    fn the_envelope_round_trips_through_flatten() {
        let row = json!({
            "_event_type": "operator_decision",
            "decision_id": "d-aaaa0001",
            "decision": "text",
            "ts": "2026-09-01T00:00:00Z",
        });
        let env = envelope_of(&row);
        assert_eq!(env["type"], json!("operator_decision"));
        assert_eq!(env["data"]["decision_id"], json!("d-aaaa0001"));
        assert!(!env["data"].as_object().unwrap().contains_key("_event_type"));
    }

    #[test]
    fn the_partition_keeps_live_rows_and_moves_the_rest() {
        let live: std::collections::BTreeSet<String> =
            ["d-aaaa0001"].into_iter().map(str::to_string).collect();
        let existing: std::collections::BTreeSet<(String, String)> = Default::default();
        let amend = |target: &str| {
            json!({
                "_event_type": SCOPE_AMEND_EVENT,
                "amendment_id": "d-cccc0003",
                "target_decision_id": target,
                "reach": "global",
                "ts": "2026-09-03T00:00:00Z",
            })
        };
        let raw = vec![
            dec("d-aaaa0001", None), // live: stays
            dec("d-bbbb0002", None), // retired: moves
            json!({
                "_event_type": "decision_retracted",
                "target_decision_id": "d-dddd0004",
                "retraction_id": "d-eeee0005",
                "ts": "2026-09-02T00:00:00Z",
            }), // retraction: moves
            amend("d-aaaa0001"),     // live target: stays
            amend("d-bbbb0002"),     // dead target: moves
            json!({
                "_event_type": "law_trigger",
                "decision_id": "d-ffff0006",
                "ts": "2026-09-04T00:00:00Z",
            }), // unknown type: moves
        ];
        let (to_move, kept) = partition(&raw, &live, &existing);
        assert_eq!(kept, 2, "the live law and the live-target amendment stay");
        assert_eq!(
            to_move.len(),
            4,
            "dead law, retraction, dead-target amend, law_trigger"
        );
        let ids: Vec<String> = to_move
            .iter()
            .map(|e| {
                e["data"]
                    .as_object()
                    .unwrap()
                    .values()
                    .find_map(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
                    .to_lowercase()
            })
            .collect();
        assert!(ids.contains(&"d-bbbb0002".to_string()));
        assert!(ids.contains(&"d-eeee0005".to_string()));
        assert!(
            ids.contains(&"d-cccc0003".to_string()),
            "dead-target amend moves"
        );
        assert!(ids.contains(&"d-ffff0006".to_string()));
    }

    #[test]
    fn the_partition_skips_rows_already_in_the_archive() {
        let live: std::collections::BTreeSet<String> =
            ["d-aaaa0001"].into_iter().map(str::to_string).collect();
        let existing: std::collections::BTreeSet<(String, String)> =
            [("operator_decision".to_string(), "d-bbbb0002".to_string())]
                .into_iter()
                .collect();
        let raw = vec![dec("d-aaaa0001", None), dec("d-bbbb0002", None)];
        let (to_move, kept) = partition(&raw, &live, &existing);
        assert_eq!(
            (to_move.len(), kept),
            (0, 2),
            "the archived key counts as kept and moves nothing"
        );
    }
}
