//! The wake meter: how much of the lead's own transcript is machine wake
//! against a typed turn, plus the subagent token spend those notifications
//! carried. Same single-read posture as [`crate::refusal_rate`]: the file is
//! read once, and an unreadable transcript is an error, never a silent zero.

use crate::harness::{AsHarness, Harness};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Machine wakes beyond 3 to 1 over typed turns is the attention line the
/// check-in prints OVER at (lead-5317-succeed-g3 ran 6.5 to 1). A named
/// constant, the same stance as `DEFAULT_BLUEPRINT_CEILING`: no config key.
const WAKE_RATIO_CEILING: u64 = 3;

pub(crate) fn wake_class(provenance: crate::provenance::Provenance) -> Option<bool> {
    use crate::provenance::{HarnessKind, Provenance};
    match provenance {
        Provenance::Relay(_)
        | Provenance::Harness(HarnessKind::LoopWakeup)
        | Provenance::Harness(HarnessKind::StopHook)
        | Provenance::Keepalive => Some(true),
        Provenance::Operator | Provenance::Unknown => Some(false),
        Provenance::Harness(_) => None,
    }
}

pub(crate) fn wake_meter_text(
    harness: &str,
    raw: &str,
    since_epoch: Option<f64>,
) -> Result<Value, String> {
    let turns = if harness.as_harness() == Harness::Codex {
        crate::provenance::codex_shaped_turns(raw)
    } else {
        crate::provenance::claude_shaped_turns(raw)
    };
    let mut machine: u64 = 0;
    let mut user: u64 = 0;
    // Per task id, in notify order: latest value overall, latest before the
    // cutoff, latest at or after it. The counts are cumulative per id, so the
    // spend since the last beat is the after-minus-before delta, never a sum.
    // A codex rollout carries no task notifications, so the token fields read
    // their session floor (0) there.
    let mut tokens: HashMap<String, (u64, u64, Option<u64>)> = HashMap::new();
    for turn in turns {
        if turn.text.is_empty() {
            continue;
        }
        let prov =
            crate::provenance::classify_turn(&turn.obj, &crate::provenance::BusIndex::empty(), "");
        // A held-park heartbeat turn is neither machine nor user: the lead
        // re-parked, so the turn measures nothing about the wake balance,
        // and a permanently parked lead must not read OVER on heartbeats
        // alone. Only machine-provenance turns skip, so a typed turn
        // quoting the marker still counts as user. The marker is
        // lead_wake::park_beat_text's own token seam.
        match wake_class(prov) {
            Some(true) if turn.text.contains("daemon (held-park heartbeat") => {}
            Some(true) => machine += 1,
            Some(false) => user += 1,
            None => {}
        }
        if turn.text.contains("<task-notification>") {
            for (id, n) in parse_task_tokens(&turn.text) {
                let before = match (turn.ts_epoch, since_epoch) {
                    (_, None) => true,
                    // a stampless notification cannot be placed in the window
                    (None, Some(_)) => true,
                    (Some(ts), Some(cut)) => ts < cut,
                };
                let slot = tokens.entry(id).or_insert((0, 0, None));
                slot.0 = n;
                if before {
                    slot.1 = n;
                } else {
                    slot.2 = Some(n);
                }
            }
        }
    }
    let tokens_session: u64 = tokens.values().map(|t| t.0).sum();
    let tokens_since = match since_epoch {
        None => tokens_session,
        Some(_) => tokens
            .values()
            .filter_map(|t| t.2.map(|after| after.saturating_sub(t.1)))
            .sum(),
    };
    let ratio = if user == 0 {
        Value::Null
    } else {
        json!(machine as f64 / user as f64)
    };
    let over = machine > WAKE_RATIO_CEILING * user;
    Ok(json!({
        "machine": machine,
        "user": user,
        "ratio": ratio,
        "over": over,
        "tokens_since": tokens_since,
        "tokens_session": tokens_session,
    }))
}

/// Every `<task-id>` / `<subagent_tokens>` pair in the body: a claude wake
/// can batch several `<task-notification>` blocks into one user row.
fn parse_task_tokens(text: &str) -> Vec<(String, u64)> {
    text.split("<task-notification>")
        .skip(1)
        .filter_map(|block| {
            let id = between(block, "<task-id>", "</task-id>")?;
            let n = between(block, "<subagent_tokens>", "</subagent_tokens>")?;
            Some((id.to_string(), n.trim().parse::<u64>().ok()?))
        })
        .collect()
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str, ts: &str, is_meta: bool) -> String {
        let mut obj = json!({
            "type": "user",
            "timestamp": ts,
            "message": {"content": [{"type": "text", "text": text}]},
        });
        if is_meta {
            obj["isMeta"] = json!(true);
        }
        obj.to_string()
    }

    fn notification(id: &str, tokens: u64, ts: &str) -> String {
        row(
            &format!(
                "<task-notification><task-id>{id}</task-id><subagent_tokens>{tokens}</subagent_tokens></task-notification>"
            ),
            ts,
            false,
        )
    }

    #[test]
    fn counts_machine_wakes_typed_turns_and_flags_over() {
        let mut lines = Vec::new();
        for i in 0..7 {
            lines.push(notification(
                &format!("t{i}"),
                1000 + i,
                "2026-09-24T12:00:00Z",
            ));
        }
        for _ in 0..2 {
            lines.push(row(
                "<fno_mail from=\"peer\">hi</fno_mail>",
                "2026-09-24T12:01:00Z",
                false,
            ));
        }
        lines.push(row("lead check-in: beat 4", "2026-09-24T12:02:00Z", true));
        for _ in 0..3 {
            lines.push(row(
                "what moved since the last beat",
                "2026-09-24T12:03:00Z",
                false,
            ));
        }
        let result = wake_meter_text("claude", &lines.join("\n"), None).unwrap();
        assert_eq!(result["machine"], 10);
        assert_eq!(result["user"], 3);
        assert!((result["ratio"].as_f64().unwrap() - 3.333_333).abs() < 1e-5);
        assert_eq!(result["over"], true);
    }

    /// A held-park heartbeat turn counts neither machine nor user: a lead
    /// whose only turns are heartbeats reads machine 0 and no OVER flag.
    #[test]
    fn a_held_park_heartbeat_turn_leaves_the_machine_count_out() {
        #[derive(serde::Deserialize, Default)]
        struct MeterView {
            #[serde(default)]
            machine: Option<u64>,
            #[serde(default)]
            user: Option<u64>,
            #[serde(default)]
            over: Option<bool>,
        }
        let lines = vec![
            row(
                "Automatic lead-wake from the fno daemon (held-park heartbeat, 1442m since the last beat): every row in scope still waits on a question, so the cadence wake sleeps. Run your check-in to re-stamp the park or start work: fno agent hq checkin.",
                "2026-09-24T12:00:00Z",
                true,
            ),
            row(
                "Automatic lead-wake from the fno daemon: no check-in beat journaled for 51m and the beat is due at 55m. Run your check-in now: fno agent hq checkin.",
                "2026-09-24T12:05:00Z",
                true,
            ),
        ];
        let view =
            |result: Value| -> MeterView { serde_json::from_value(result).unwrap_or_default() };
        let result = view(wake_meter_text("claude", &lines.join("\n"), None).unwrap());
        assert_eq!(
            result.machine,
            Some(1),
            "only the non-heartbeat wake counts"
        );
        assert_eq!(result.user, Some(0));
        assert_eq!(
            result.over,
            Some(true),
            "a real wake with no typed turn still flags OVER"
        );
        // A typed turn that QUOTES the wake's marker phrase still counts as
        // user: the marker is the wake text's own token seam, not a bare
        // phrase any reply can carry.
        let mut lines = lines;
        lines.push(row(
            "The daemon said \"daemon (held-park heartbeat, 1442m since the last beat)\" but a question just landed; taking the beat now.",
            "2026-09-16T12:09:00Z",
            false,
        ));
        let result = view(wake_meter_text("claude", &lines.join("\n"), None).unwrap());
        assert_eq!(
            result.machine,
            Some(1),
            "the quoted phrase stays out of machine"
        );
        assert_eq!(
            result.user,
            Some(1),
            "the quoted phrase still counts as a typed turn"
        );
        assert_eq!(result.over, Some(false));
    }
}
