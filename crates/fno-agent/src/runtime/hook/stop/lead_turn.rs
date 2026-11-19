//! The stop hook's lead-turn gate: two checks on the turn a lead ends.
//!
//! 1. A waiting-on-you ask that an earlier reply already made goes back to
//!    the lead when this turn read no live state. The default is to say an
//!    ask once and re-raise it only on a state change, so a repeat must rest
//!    on a fresh read (a lead once re-sent "the rc waits for your approval"
//!    hours after the user approved it and the run published).
//! 2. A node the lead filed this turn from an operator request goes back
//!    when it leaves the turn as a bare idea: neither dispatched (a claim on
//!    it, or a status past the idea rungs) nor parked with a named reason (a
//!    deferral, which always carries one, or a blocker).
//!
//! Claude transcripts only: a codex rollout carries no end-of-turn marker
//! here, so the gate passes it.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;

use serde_json::Value;

use crate::repeated_asks::{same_ask, waiting_asks_in};

/// The transcript tail the gate reads. A lead transcript grows for days; the
/// current turn and the replies before it sit at the end.
const TAIL_BYTES: u64 = 8 * 1024 * 1024;
const PRIOR_REPLIES: usize = 20;
/// Shell commands that read live state an ask can rest on. The check-in is
/// not one: a lead runs it every beat, so it would clear every repeat.
const LIVE_READS: &[&str] = &[
    "gh run ",
    "gh pr ",
    "gh api ",
    "gh release ",
    "fno inbox ",
    "fno do pr ",
    "fno backlog get ",
];
/// The statuses a bare filing holds: nothing has picked the node up.
const UNDISPATCHED: &[&str] = &["idea", "intake", "triage", "design"];

#[derive(Debug, Default)]
pub(super) struct Turn {
    /// The timestamp of the user prompt that opened this turn.
    started_at: Option<String>,
    /// Every shell command the lead ran in this turn.
    commands: Vec<String>,
    /// The end-of-turn replies before this turn, oldest first.
    prior_replies: Vec<String>,
}

/// Some block reason when the ending lead turn fails either check.
pub(super) fn gate(transcript: &Path, session_ids: &[&str], message: &str) -> Option<String> {
    let turn = parse_turn(&read_tail(transcript)?);
    if let Some(reason) = stale_ask(message, &turn) {
        return Some(reason);
    }
    let since = turn.started_at.as_deref()?;
    let graph = crate::backlog::database_path(&crate::graph_get::default_graph_path());
    let nodes = undispatched_operator_nodes(&graph, session_ids, since);
    (!nodes.is_empty()).then(|| undispatched_reason(&nodes))
}

fn read_tail(transcript: &Path) -> Option<String> {
    let mut file = std::fs::File::open(transcript).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    // A cut first line is half a row; drop it.
    Some(if start > 0 {
        text.split_once('\n')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_default()
    } else {
        text
    })
}

fn parse_turn(raw: &str) -> Turn {
    let rows: Vec<Value> = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|row| row.get("isSidechain").and_then(Value::as_bool) != Some(true))
        .collect();
    let start = rows.iter().rposition(is_prompt_row);
    let mut turn = Turn {
        started_at: start.and_then(|i| {
            rows[i]
                .get("timestamp")
                .and_then(Value::as_str)
                .map(str::to_string)
        }),
        ..Turn::default()
    };
    let (before, current) = rows.split_at(start.map_or(0, |i| i + 1));
    for row in before.iter().filter(|row| is_end_turn_reply(row)) {
        let text = crate::loopcheck::extract_assistant_text(row);
        if !text.trim().is_empty() {
            turn.prior_replies.push(text);
        }
    }
    let excess = turn.prior_replies.len().saturating_sub(PRIOR_REPLIES);
    turn.prior_replies.drain(..excess);
    for row in current {
        if row.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let content = row.pointer("/message/content").and_then(Value::as_array);
        for item in content.into_iter().flatten() {
            if item.get("type").and_then(Value::as_str) == Some("tool_use")
                && item.get("name").and_then(Value::as_str) == Some("Bash")
            {
                if let Some(command) = item.pointer("/input/command").and_then(Value::as_str) {
                    turn.commands.push(command.to_string());
                }
            }
        }
    }
    turn
}

/// A typed prompt: a user row that is not hook meta and carries no tool result.
fn is_prompt_row(row: &Value) -> bool {
    if row.get("type").and_then(Value::as_str) != Some("user")
        || crate::provenance::json_truthy(row.get("isMeta"))
    {
        return false;
    }
    match row.pointer("/message/content") {
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(Value::Array(items)) => !items
            .iter()
            .any(|item| item.get("type").and_then(Value::as_str) == Some("tool_result")),
        _ => false,
    }
}

fn is_end_turn_reply(row: &Value) -> bool {
    row.get("type").and_then(Value::as_str) == Some("assistant")
        && row.pointer("/message/stop_reason").and_then(Value::as_str) == Some("end_turn")
}

fn stale_ask(message: &str, turn: &Turn) -> Option<String> {
    if turn
        .commands
        .iter()
        .any(|command| LIVE_READS.iter().any(|read| command.contains(read)))
    {
        return None;
    }
    let earlier: Vec<String> = turn
        .prior_replies
        .iter()
        .flat_map(|reply| waiting_asks_in(reply))
        .collect();
    let repeated = waiting_asks_in(message)
        .into_iter()
        .find(|ask| earlier.iter().any(|old| same_ask(ask, old)))?;
    Some(format!(
        "This report repeats a waiting-on-you ask an earlier reply already made (\"{repeated}\"), \
and this turn read no live state behind it. Re-read that state now (gh run view, gh pr view, \
fno do pr status, fno inbox outstanding). If it has not changed, drop the line: an ask is \
said once and re-raised only when its state changes. If it changed, say what changed."
    ))
}

/// The operator-request nodes these sessions filed at or after `since` that
/// still sit undispatched and unparked. An unreadable graph reads as none:
/// the gate never holds a stop on its own read failure.
fn undispatched_operator_nodes(
    graph: &Path,
    session_ids: &[&str],
    since: &str,
) -> Vec<(String, String)> {
    let Ok(connection) = crate::store_conn::open_read(graph) else {
        return Vec::new();
    };
    let statuses = UNDISPATCHED
        .iter()
        .map(|status| format!("'{status}'"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT n.id, n.title FROM node_provenance p JOIN nodes n ON n.id = p.node_id \
         WHERE p.source_session_id = ?1 \
           AND (p.source_kind = 'operator_request' OR p.request_origin = 'operator_request') \
           AND julianday(n.created_at) >= julianday(?2) \
           AND n.archived_at IS NULL \
           AND n.status IN ({statuses}) \
           AND NOT EXISTS (SELECT 1 FROM claims c WHERE c.key = 'node:' || n.id) \
           AND NOT EXISTS (SELECT 1 FROM relations r \
                           WHERE r.type = 'blocks' AND r.related_node_id = n.id) \
         ORDER BY n.created_at"
    );
    let Ok(mut statement) = connection.prepare(&sql) else {
        return Vec::new();
    };
    let mut nodes: Vec<(String, String)> = Vec::new();
    for session in session_ids.iter().filter(|id| !id.is_empty()) {
        let Ok(rows) = statement.query_map([*session, since], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) else {
            continue;
        };
        for row in rows.flatten() {
            if !nodes.iter().any(|(id, _)| *id == row.0) {
                nodes.push(row);
            }
        }
    }
    nodes
}

fn undispatched_reason(nodes: &[(String, String)]) -> String {
    let list = nodes
        .iter()
        .map(|(id, title)| format!("{id} ({title})"))
        .collect::<Vec<_>>()
        .join(", ");
    let first = &nodes[0].0;
    format!(
        "This turn filed {list} from an operator request and left it a bare idea. A node filed \
from an operator request leaves the turn dispatched or parked with a named reason. \
Dispatch it: fno agent spawn \"/fno:target {first}\". Or park it: fno backlog defer {first} \
-K later -R \"<why it waits>\", or name its blocker: fno backlog update {first} blocked_by+=<id>."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(value: Value) -> String {
        value.to_string()
    }

    fn prompt(ts: &str, text: &str) -> String {
        row(json!({"type": "user", "timestamp": ts, "message": {"role": "user", "content": text}}))
    }

    fn reply(text: &str) -> String {
        row(
            json!({"type": "assistant", "message": {"role": "assistant", "stop_reason": "end_turn",
            "content": [{"type": "text", "text": text}]}}),
        )
    }

    fn bash(command: &str) -> String {
        row(
            json!({"type": "assistant", "message": {"role": "assistant", "stop_reason": "tool_use",
            "content": [{"type": "tool_use", "name": "Bash", "input": {"command": command}}]}}),
        )
    }

    fn tool_result() -> String {
        row(json!({"type": "user", "message": {"role": "user",
            "content": [{"type": "tool_result", "content": "ok"}]}}))
    }

    const ASK: &str = "The v0.0.17 rc still waits for your approval.";

    #[test]
    fn a_repeated_waiting_ask_with_no_live_read_blocks_and_a_read_clears_it() {
        let earlier = [prompt("2026-11-19T18:00:00Z", "beat"), reply(ASK)].join("\n");
        let blind = [
            earlier.clone(),
            prompt("2026-11-19T19:45:00Z", "beat"),
            bash("ls"),
        ]
        .join("\n");
        let reason = stale_ask(ASK, &parse_turn(&blind)).expect("a blind repeat blocks");
        assert!(reason.contains("waits for your approval"), "{reason}");
        assert!(
            reason.contains("re-raised only when its state changes"),
            "{reason}"
        );

        let read = [
            earlier.clone(),
            prompt("2026-11-19T19:45:00Z", "beat"),
            bash("gh run view 123 --json status"),
            tool_result(),
        ]
        .join("\n");
        let turn = parse_turn(&read);
        assert_eq!(turn.started_at.as_deref(), Some("2026-11-19T19:45:00Z"));
        assert!(
            stale_ask(ASK, &turn).is_none(),
            "a same-turn live read clears it"
        );
    }

    #[test]
    fn a_first_ask_or_a_plain_report_never_blocks() {
        let fresh = [prompt("2026-11-19T19:45:00Z", "beat"), bash("ls")].join("\n");
        assert!(
            stale_ask(ASK, &parse_turn(&fresh)).is_none(),
            "said once is fine"
        );
        let earlier = [prompt("2026-11-19T18:00:00Z", "beat"), reply(ASK)].join("\n");
        let turn = parse_turn(&[earlier, prompt("2026-11-19T19:45:00Z", "b")].join("\n"));
        assert!(stale_ask("Merged PR 371; CI green on 372.", &turn).is_none());
        assert!(stale_ask("You already ruled on the rc: your approval landed.", &turn).is_none());
    }

    #[test]
    fn the_turn_starts_at_the_last_typed_prompt_not_a_tool_result() {
        let raw = [
            prompt("2026-11-19T19:00:00Z", "first"),
            reply("done"),
            prompt("2026-11-19T19:45:00Z", "second"),
            bash("fno backlog idea x"),
            tool_result(),
        ]
        .join("\n");
        let turn = parse_turn(&raw);
        assert_eq!(turn.started_at.as_deref(), Some("2026-11-19T19:45:00Z"));
        assert_eq!(turn.prior_replies, vec!["done".to_string()]);
        assert_eq!(turn.commands, vec!["fno backlog idea x".to_string()]);
    }

    #[test]
    fn an_operator_node_filed_this_turn_must_leave_dispatched_or_parked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let graph = dir.path().join("graph.db");
        let db = rusqlite::Connection::open(&graph).expect("open");
        db.execute_batch(
            "CREATE TABLE nodes (id TEXT PRIMARY KEY, title TEXT, status TEXT, created_at TEXT, archived_at TEXT);
             CREATE TABLE node_provenance (node_id TEXT PRIMARY KEY, source_kind TEXT, request_origin TEXT, source_session_id TEXT);
             CREATE TABLE claims (key TEXT PRIMARY KEY);
             CREATE TABLE relations (node_id TEXT, related_node_id TEXT, type TEXT);
             INSERT INTO nodes VALUES
               ('x-bare', 'bare idea', 'idea', '2026-11-19T19:47:57.167132+00:00', NULL),
               ('x-spawned', 'spawned', 'idea', '2026-11-19T19:48:00Z', NULL),
               ('x-parked', 'parked', 'deferred', '2026-11-19T19:48:01Z', NULL),
               ('x-blocked', 'blocked', 'idea', '2026-11-19T19:48:02Z', NULL),
               ('x-older', 'older turn', 'idea', '2026-11-19T18:00:00Z', NULL),
               ('x-organic', 'organic', 'idea', '2026-11-19T19:48:03Z', NULL);
             INSERT INTO node_provenance VALUES
               ('x-bare', 'operator_request', 'operator_request', 'lead-1'),
               ('x-spawned', 'operator_request', 'operator_request', 'lead-1'),
               ('x-parked', 'operator_request', 'operator_request', 'lead-1'),
               ('x-blocked', 'operator_request', 'operator_request', 'lead-1'),
               ('x-older', 'operator_request', 'operator_request', 'lead-1'),
               ('x-organic', 'organic', 'organic', 'lead-1');
             INSERT INTO claims VALUES ('node:x-spawned');
             INSERT INTO relations VALUES ('x-other', 'x-blocked', 'blocks');",
        )
        .expect("seed");
        drop(db);
        let nodes = undispatched_operator_nodes(&graph, &["lead-1"], "2026-11-19T19:45:00.000Z");
        assert_eq!(nodes, vec![("x-bare".to_string(), "bare idea".to_string())]);
        let reason = undispatched_reason(&nodes);
        assert!(
            reason.contains("fno agent spawn \"/fno:target x-bare\""),
            "{reason}"
        );
        assert!(reason.contains("fno backlog defer x-bare"), "{reason}");
        assert!(undispatched_operator_nodes(&graph, &["other"], "2026-11-19T19:45:00Z").is_empty());
    }
}
