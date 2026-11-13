//! One send per key. A send whose key matches an earlier row that has not
//! reached its recipient writes nothing: the earlier row already waits on
//! the bus for that turn. A row that a turn record holds does not block a
//! new one, so a deliberate re-send after a landed message still goes out.

use serde::Deserialize;
use serde_json::Value;

use crate::state::newest_state;

/// What makes two sends the same send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendKey {
    from: String,
    to: String,
    in_reply_to: String,
    body: String,
}

impl SendKey {
    /// The key of a send. `scheduled` marks a machine sender (`--origin
    /// scheduler`): its text repeats with a new count each tick ("no check-in
    /// for 58m", then "63m"), so its numbers do not count. An agent's numbers
    /// do count: "PR 136" and "PR 137" are two messages.
    pub fn derive(
        from: &str,
        to: &str,
        in_reply_to: Option<&str>,
        body: &str,
        scheduled: bool,
    ) -> SendKey {
        let body = strip_header(body).trim();
        SendKey {
            from: from.to_string(),
            to: to.to_string(),
            in_reply_to: in_reply_to.unwrap_or("").to_string(),
            body: if scheduled {
                without_numbers(body)
            } else {
                body.to_string()
            },
        }
    }

    fn of_row(row: &MessageRow, scheduled: bool) -> SendKey {
        SendKey::derive(
            &row.from,
            &row.to,
            row.in_reply_to.as_deref(),
            &row.body,
            scheduled,
        )
    }
}

/// The fields of a bus row the key reads, parsed once. A row whose fields
/// are not strings parses empty and keys nothing.
#[derive(Debug, Default, Deserialize)]
struct MessageRow {
    #[serde(default)]
    id: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
    #[serde(default)]
    in_reply_to: Option<String>,
    #[serde(default)]
    body: String,
}

impl MessageRow {
    fn of(row: &Value) -> MessageRow {
        MessageRow::deserialize(row).unwrap_or_default()
    }
}

/// The id of the newest row in `unread` whose key equals `key` and whose
/// delivery state, folded from `rows`, has not reached the recipient. A
/// row a turn record accepted never counts.
pub fn duplicate_of(
    unread: &[&Value],
    rows: &[Value],
    key: &SendKey,
    scheduled: bool,
) -> Option<String> {
    unread
        .iter()
        .rev()
        .map(|row| MessageRow::of(row))
        .filter(|row| !row.id.is_empty() && SendKey::of_row(row, scheduled) == *key)
        .find(|row| !newest_state(rows, &row.id).is_accepted())
        .map(|row| row.id)
}

/// The body without the sender header a send stamps on its first line
/// (`` `@sender · fmail-...` ``): the raw body a caller hands in and the
/// stored row then compare equal.
fn strip_header(body: &str) -> &str {
    match body.split_once('\n') {
        Some((first, rest))
            if first.starts_with("`@") && first.ends_with('`') && first.contains("fmail-") =>
        {
            rest
        }
        _ => body,
    }
}

fn without_numbers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_digits = false;
    for c in text.chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                out.push('#');
            }
            in_digits = true;
        } else {
            out.push(c);
            in_digits = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TO: &str = "a1b2c3d4-1111-2222-3333-444455556666";

    fn row(id: &str, body: &str) -> Value {
        json!({"v": 1, "id": id, "from": "fno-lead-wake", "to": TO, "kind": "send",
               "body": format!("`@fno-lead-wake · {id}`\n{body}")})
    }

    fn accepted(of: &str) -> Value {
        json!({"kind": "state", "to": TO,
               "meta": {"of": of, "state": "accepted-new-turn",
                        "evidence": {"record": "t.jsonl", "turn_id": null, "at": "x"}}})
    }

    /// A second send whose key has an unread row that has not reached reads
    /// duplicate; a landed earlier copy, or another in_reply_to, does not
    /// (AC7-HP, AC11-EDGE, AC12-EDGE).
    #[test]
    fn mail_addr_second_send_same_key_reads_duplicate() {
        let first = row("fmail-000000000001", "no check-in beat journaled for 58m");
        let rows = vec![first.clone()];
        let unread = vec![&first];
        let key = SendKey::derive(
            "fno-lead-wake",
            TO,
            None,
            "no check-in beat journaled for 63m",
            true,
        );
        assert_eq!(
            duplicate_of(&unread, &rows, &key, true).as_deref(),
            Some("fmail-000000000001"),
            "a scheduled repeat with a new count is the same send"
        );
        let agent = SendKey::derive(
            "fno-lead-wake",
            TO,
            None,
            "no check-in beat journaled for 63m",
            false,
        );
        assert_eq!(
            duplicate_of(&unread, &rows, &agent, false),
            None,
            "an agent's numbers count"
        );
        let reply = SendKey::derive(
            "fno-lead-wake",
            TO,
            Some("fmail-0000000000aa"),
            "no check-in beat journaled for 58m",
            true,
        );
        assert_eq!(
            duplicate_of(&unread, &rows, &reply, true),
            None,
            "another in_reply_to is a new send"
        );
        let landed = vec![first.clone(), accepted("fmail-000000000001")];
        assert_eq!(
            duplicate_of(&unread, &landed, &key, true),
            None,
            "a landed copy never blocks a re-send"
        );
    }
}
