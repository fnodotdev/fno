//! The stop hook's lead-turn gate: two checks on the turn a lead ends.
//!
//! 1. A waiting-on-you line goes back to the lead when this turn read no
//!    live state behind it. An ask is said once and re-raised only on a
//!    state change, and only a fresh read tells the two apart (a lead once
//!    re-sent "the rc waits for your approval" hours after the user approved
//!    it and the run published).
//! 2. A node the lead filed from an operator request goes back when it
//!    leaves the turn as a bare idea: neither dispatched (a claim on it, or a
//!    status past the idea rungs) nor parked with a named reason (a deferral,
//!    which always carries one, or a blocker).
//!
//! A turn is every row after the previous end-of-turn reply, so a beat woken
//! by a loop, mail or hook row is its own turn whatever shape the wake takes.
//! Claude transcripts only: a codex rollout carries no claude assistant row,
//! so the gate passes it.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;

use serde_json::Value;

use crate::repeated_asks::waiting_asks_in;

/// The transcript tail the gate reads. A lead transcript grows for days; the
/// current turn sits at the end.
const TAIL_BYTES: u64 = 8 * 1024 * 1024;
/// How far back the filing check reaches past the turn start (an SQLite
/// date modifier), so a node filed before a blocked stop is still read on
/// the next one.
const FILING_LOOKBACK: &str = "-2 hours";
/// Shell commands that read live state an ask can rest on. The check-in is
/// not one: a lead runs it every beat, so it would clear every stale ask.
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
    /// The timestamp of the first row after the previous end-of-turn reply.
    started_at: Option<String>,
    /// Every shell command the lead ran in this turn.
    commands: Vec<String>,
}

/// Some block reason when the ending lead turn fails either check.
pub(super) fn gate(transcript: &Path, session_ids: &[&str], message: &str) -> Option<String> {
    let turn = parse_turn(&read_tail(transcript)?, message)?;
    if let Some(reason) = unread_ask(message, &turn) {
        return Some(reason);
    }
    let since = turn.started_at.as_deref()?;
    let graph = crate::backlog::database_path(&crate::graph_get::default_graph_path());
    let nodes = undispatched_operator_nodes(&graph, session_ids, since, FILING_LOOKBACK);
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

/// The turn that `message` ends. When the harness already wrote `message`
/// as the newest end-of-turn row, the boundary is the end-of-turn row
/// before it. None for a transcript with no claude assistant row, such as a
/// codex rollout, whose commands this parser cannot see.
fn parse_turn(raw: &str, message: &str) -> Option<Turn> {
    let rows: Vec<Value> = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|row| row.get("isSidechain").and_then(Value::as_bool) != Some(true))
        .collect();
    if !rows.iter().any(is_assistant) {
        return None;
    }
    let ends: Vec<usize> = (0..rows.len())
        .filter(|&i| is_end_turn_reply(&rows[i]))
        .collect();
    let written = ends.last().is_some_and(|&last| {
        crate::loopcheck::extract_assistant_text(&rows[last]).trim() == message.trim()
            && !rows[last + 1..].iter().any(is_assistant)
    });
    let boundary = if written {
        ends.len().checked_sub(2).map(|i| ends[i])
    } else {
        ends.last().copied()
    };
    let current = &rows[boundary.map_or(0, |i| i + 1)..];
    let mut turn = Turn {
        started_at: current
            .iter()
            .find_map(|row| row.get("timestamp").and_then(Value::as_str))
            .map(str::to_string),
        ..Turn::default()
    };
    for row in current.iter().filter(|row| is_assistant(row)) {
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
    Some(turn)
}

fn is_assistant(row: &Value) -> bool {
    row.get("type").and_then(Value::as_str) == Some("assistant")
}

fn is_end_turn_reply(row: &Value) -> bool {
    is_assistant(row)
        && row.pointer("/message/stop_reason").and_then(Value::as_str) == Some("end_turn")
}

fn unread_ask(message: &str, turn: &Turn) -> Option<String> {
    if turn
        .commands
        .iter()
        .any(|command| LIVE_READS.iter().any(|read| command.contains(read)))
    {
        return None;
    }
    let ask = waiting_asks_in(message).into_iter().next()?;
    Some(format!(
        "This report says work waits on the user (\"{ask}\"), and this turn read no live state \
behind it. Re-read that state now (gh run view, gh pr view, fno do pr status, fno inbox \
outstanding). If it has not changed since you last said it, drop the line: an ask is said once \
and re-raised only when its state changes. If it changed, say what changed."
    ))
}

/// The operator-request nodes these sessions filed since the earlier of
/// `since` and now shifted by `lookback`, that still sit undispatched and
/// unparked. An unreadable graph reads as none: the gate never holds a stop
/// on its own read failure.
fn undispatched_operator_nodes(
    graph: &Path,
    session_ids: &[&str],
    since: &str,
    lookback: &str,
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
           AND julianday(n.created_at) >= min(julianday(?2), julianday('now', ?3)) \
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
        let Ok(rows) = statement.query_map([*session, since, lookback], |row| {
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
        "This lead filed {list} from an operator request and left it a bare idea. A node filed \
from an operator request leaves the turn dispatched or parked with a named reason. \
Dispatch it: fno agent spawn \"/fno:target {first}\". Or park it: fno backlog defer {first} \
-K later -R \"<why it waits>\", or name its blocker: fno backlog update {first} blocked_by+=<id>."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prompt(ts: &str, text: &str) -> String {
        json!({"type": "user", "timestamp": ts, "isMeta": true,
            "message": {"role": "user", "content": text}})
        .to_string()
    }

    fn reply(text: &str) -> String {
        json!({"type": "assistant", "message": {"role": "assistant", "stop_reason": "end_turn",
            "content": [{"type": "text", "text": text}]}})
        .to_string()
    }

    fn bash(command: &str) -> String {
        json!({"type": "assistant", "message": {"role": "assistant", "stop_reason": "tool_use",
            "content": [{"type": "tool_use", "name": "Bash", "input": {"command": command}}]}})
        .to_string()
    }

    const ASK: &str = "The v0.0.17 rc still waits for your approval.";

    #[test]
    fn a_waiting_ask_needs_a_live_read_in_its_own_turn() {
        // Beat 150 read the run; beat 189 (a meta-flagged wake) read nothing.
        let old_read = [
            prompt("2026-11-19T18:00:00Z", "beat"),
            bash("gh run view 123 --json status"),
            reply("Run green."),
        ]
        .join("\n");
        let blind = [
            old_read.clone(),
            prompt("2026-11-19T19:45:00Z", "beat"),
            bash("ls"),
        ];
        let turn = parse_turn(&blind.join("\n"), ASK).expect("claude turn");
        assert_eq!(turn.started_at.as_deref(), Some("2026-11-19T19:45:00Z"));
        assert_eq!(turn.commands, vec!["ls".to_string()]);
        let reason = unread_ask(ASK, &turn).expect("an unread ask blocks");
        assert!(reason.contains("waits for your approval"), "{reason}");
        assert!(
            reason.contains("re-raised only when its state changes"),
            "{reason}"
        );

        // The harness already wrote the message: the boundary is the reply before it.
        let written = [
            old_read,
            prompt("2026-11-19T19:45:00Z", "beat"),
            bash("gh pr view 9"),
            reply(ASK),
        ];
        let turn = parse_turn(&written.join("\n"), ASK).expect("claude turn");
        assert_eq!(turn.started_at.as_deref(), Some("2026-11-19T19:45:00Z"));
        assert!(
            unread_ask(ASK, &turn).is_none(),
            "a same-turn live read clears it"
        );
    }

    #[test]
    fn a_report_with_no_waiting_ask_or_a_codex_rollout_never_blocks() {
        let codex = json!({"type": "response_item",
            "payload": {"type": "message", "role": "assistant", "content": ASK}});
        assert!(parse_turn(&codex.to_string(), ASK).is_none());
        let turn = parse_turn(
            &[prompt("2026-11-19T19:45:00Z", "b"), bash("ls")].join("\n"),
            "x",
        )
        .expect("claude turn");
        assert!(unread_ask("Merged PR 371; CI green on 372.", &turn).is_none());
        assert!(unread_ask("You already ruled on the rc: your approval landed.", &turn).is_none());
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
        // A zero lookback reads the turn alone; x-older sits before it.
        let since = "2026-11-19T19:45:00.000Z";
        let nodes = undispatched_operator_nodes(&graph, &["lead-1"], since, "+0 hours");
        assert_eq!(nodes, vec![("x-bare".to_string(), "bare idea".to_string())]);
        let reason = undispatched_reason(&nodes);
        assert!(
            reason.contains("fno agent spawn \"/fno:target x-bare\""),
            "{reason}"
        );
        assert!(reason.contains("fno backlog defer x-bare"), "{reason}");
        assert!(undispatched_operator_nodes(&graph, &["other"], since, "+0 hours").is_empty());
    }
}
