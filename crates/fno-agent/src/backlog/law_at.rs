//! A law's action triggers: `--at-command <glob>` and `--at-door
//! <door>[:<subject-glob>]` on the record door. The row stores
//! `at_command: [..]` and `at_door: {door, subjects}`; the rule source turns
//! them into tier-0 rules, `law-<id>-at` and `law-<id>-door`, beside the
//! law's prompt rule. A law about an action then fires at the action, not
//! only when prompt text happens to rank it. The send door's `by-name:`
//! subject and the check-in's read of law fires at actions live here too.

use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

/// The doors a law can name: the door events with a wired decide() site.
pub const DOORS: &[&str] = &["send", "spawn", "merge"];

#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct AtDoor {
    pub door: String,
    pub subjects: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct LawAt {
    #[serde(rename = "at_command")]
    pub commands: Vec<String>,
    #[serde(rename = "at_door")]
    pub door: Option<AtDoor>,
}

impl LawAt {
    /// Parse one `--at-door` value. A second door word on one law refuses:
    /// one law, one door. The same door again adds a subject.
    pub fn set_door(&mut self, spec: &str) -> Result<(), String> {
        let (door, subject) = match spec.split_once(':') {
            Some((d, s)) => (d.trim(), Some(s.trim())),
            None => (spec.trim(), None),
        };
        if !DOORS.contains(&door) {
            return Err(format!(
                "--at-door takes {}[:<subject-glob>], not {spec:?}",
                DOORS.join("|")
            ));
        }
        let at = self.door.get_or_insert_with(|| AtDoor {
            door: door.to_string(),
            subjects: Vec::new(),
        });
        if at.door != door {
            return Err(format!(
                "--at-door names one door per law; this law already names {}",
                at.door
            ));
        }
        if let Some(s) = subject.filter(|s| !s.is_empty()) {
            at.subjects.push(s.to_string());
        }
        Ok(())
    }

    /// Store the triggers on the law row; an empty selector writes nothing.
    pub fn write(&self, row: &mut Map<String, Value>) {
        let commands: Vec<&str> = self
            .commands
            .iter()
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
            .collect();
        if !commands.is_empty() {
            row.insert("at_command".into(), json!(commands));
        }
        if let Some(d) = &self.door {
            row.insert(
                "at_door".into(),
                json!({"door": d.door, "subjects": d.subjects}),
            );
        }
    }

    pub fn of(row: &Value) -> Self {
        serde::Deserialize::deserialize(row).unwrap_or_default()
    }

    /// One `(rule-id suffix, [rule.when] lines)` pair per trigger the row
    /// names: `-at` for the command, `-door` for the door. A row with both
    /// fires at either.
    pub fn when_lines(&self, quote: impl Fn(&str) -> String) -> Vec<(&'static str, String)> {
        let list = |items: &[String]| {
            items
                .iter()
                .map(|i| format!("\"{}\"", quote(i)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut out = Vec::new();
        if !self.commands.is_empty() {
            out.push((
                "-at",
                format!(
                    "events = [\"pre-tool\"]\ntools = [\"Bash\"]\ncommands = [{}]",
                    list(&self.commands)
                ),
            ));
        }
        if let Some(d) = &self.door {
            let mut when = format!("events = [\"door:{}\"]", quote(&d.door));
            if !d.subjects.is_empty() {
                when.push_str(&format!("\nsubjects = [{}]", list(&d.subjects)));
            }
            out.push(("-door", when));
        }
        out
    }
}

/// The `door:send` subjects for one recipient: the recipient as typed, plus
/// `by-name:<recipient>` when it is not session-id-shaped (a uuid or an
/// 8-hex fno id), so a law about addressing by name can match. A system
/// arm address (`fno/quiet-recovery`) names no session, so it stays bare.
pub fn send_subjects(recipient: &str) -> Vec<String> {
    let mut out = vec![recipient.to_string()];
    if !recipient.contains('/') && !crate::session_truth::is_session_id_prefix(recipient) {
        out.push(format!("by-name:{recipient}"));
    }
    out
}

const LOOKBACK_MS: i64 = 24 * 3600 * 1000;
const MAX_LAWS: usize = 5;
const MAX_SESSIONS: usize = 3;

#[derive(Deserialize)]
struct SpanLine {
    #[serde(default)]
    data: SpanData,
}

#[derive(Deserialize, Default)]
struct SpanData {
    #[serde(default)]
    span_kind: String,
    #[serde(default)]
    event: String,
    law: Option<String>,
    #[serde(default)]
    trace: SpanTrace,
}

#[derive(Deserialize, Default)]
struct SpanTrace {
    actor_session: Option<String>,
}

/// The lead check-in's `law_fires` reading: the laws that fired at an
/// action (a pre-tool call or a door) since the previous beat's `ts` (24h
/// with no previous beat), or null when none did, so a quiet beat prints
/// no line.
pub fn fires_at_actions(events_paths: &[PathBuf], since: Option<&str>) -> Value {
    let since_ms = since
        .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
        .map(|t| t.timestamp_millis())
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis() - LOOKBACK_MS);
    fires_with(events_paths, since_ms, &crate::numbers::labels())
}

/// The summary over the window, laws by fire count, each named by its
/// short number when it has one.
fn fires_with(events_paths: &[PathBuf], since_ms: i64, labels: &HashMap<String, String>) -> Value {
    let query = crate::event_store::EventQuery {
        types: vec!["decision_span".to_string()],
        since_ms: Some(since_ms),
        ..Default::default()
    };
    let mut seen = HashSet::new();
    let mut by_law: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
    for path in events_paths {
        for row in crate::event_store::query_events(path, &query).unwrap_or_default() {
            if !seen.insert(row.event_id.clone()) {
                continue;
            }
            let Ok(span) = serde_json::from_str::<SpanLine>(&row.line) else {
                continue;
            };
            let d = span.data;
            let at_action = d.event == "pre-tool" || d.event.starts_with("door:");
            let (true, true, Some(law)) = (d.span_kind == "rule", at_action, d.law) else {
                continue;
            };
            let entry = by_law.entry(law).or_default();
            entry.0 += 1;
            if let Some(sid) = d.trace.actor_session {
                let short: String = sid.chars().take(8).collect();
                if !entry.1.contains(&short) {
                    entry.1.push(short);
                }
            }
        }
    }
    if by_law.is_empty() {
        return Value::Null;
    }
    let mut laws: Vec<_> = by_law.into_iter().collect();
    laws.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    let parts: Vec<String> = laws
        .into_iter()
        .take(MAX_LAWS)
        .map(|(law, (n, mut sessions))| {
            let label = labels.get(&law).cloned().unwrap_or(law);
            sessions.truncate(MAX_SESSIONS);
            if sessions.is_empty() {
                format!("{label} x{n}")
            } else {
                format!("{label} x{n} (sessions {})", sessions.join(", "))
            }
        })
        .collect();
    json!(parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn door_spec_parses_stores_and_refuses_an_unknown_door() {
        let mut at = LawAt::default();
        at.commands.push("fno agent mail send *".into());
        at.set_door("send:by-name:*").unwrap();
        at.set_door("send:@*").unwrap();
        let err = at.set_door("spawn").unwrap_err();
        assert!(err.contains("already names send"), "{err}");
        let err = LawAt::default().set_door("teleport").unwrap_err();
        assert!(err.contains("send|spawn|merge"), "{err}");

        let mut row = Map::new();
        at.write(&mut row);
        let back = LawAt::of(&Value::Object(row));
        assert_eq!(back, at);
        assert_eq!(
            back.door.as_ref().unwrap().subjects,
            vec!["by-name:*", "@*"]
        );
        assert_eq!(LawAt::of(&json!({"decision": "x"})), LawAt::default());
    }

    #[test]
    fn a_name_recipient_carries_a_by_name_subject_and_an_id_does_not() {
        assert_eq!(send_subjects("nova"), ["nova", "by-name:nova"]);
        let uuid = "0e0b4dc7-e6b9-454a-b0dc-ebc0a2dd33a6";
        assert_eq!(send_subjects(uuid), [uuid]);
        assert_eq!(send_subjects("a8a39a7a"), ["a8a39a7a"]);
        assert_eq!(send_subjects("fno/quiet-recovery"), ["fno/quiet-recovery"]);
    }

    #[test]
    fn law_fires_at_actions_count_per_law_and_a_quiet_window_is_null() {
        let td = tempfile::TempDir::new().unwrap();
        let journal = td.path().join("events.jsonl");
        let hour_ago = chrono::Utc::now().timestamp_millis() - 3_600_000;
        assert_eq!(
            fires_with(&[journal.clone()], hour_ago, &HashMap::new()),
            Value::Null
        );
        let span = |law: Option<&str>, event: &str, sid: &str| {
            json!({
                "ts": chrono::Utc::now().to_rfc3339(), "type": "decision_span", "source": "target",
                "data": {"span_kind": "rule", "rule": "r", "law": law, "event": event,
                         "verdict": "warn", "trace": {"actor_session": sid}},
            })
            .to_string()
        };
        for line in [
            span(Some("d-e952ed19"), "door:send", "a1b2c3d4-0000"),
            span(Some("d-e952ed19"), "door:send", "a1b2c3d4-0000"),
            span(Some("d-e952ed19"), "prompt", "ffff0000-0000"),
            span(None, "pre-tool", "ffff0000-0000"),
        ] {
            crate::event_store::append_envelope(&journal, &line, None).unwrap();
        }
        let labels = HashMap::from([("d-e952ed19".to_string(), "D1051".to_string())]);
        assert_eq!(
            fires_with(&[journal.clone()], hour_ago, &labels),
            json!("D1051 x2 (sessions a1b2c3d4)"),
            "a prompt fire and a law-less rule are not law fires at an action"
        );
        let from_now = chrono::Utc::now().timestamp_millis() + 60_000;
        assert_eq!(
            fires_with(&[journal], from_now, &labels),
            Value::Null,
            "a fire before the previous beat is not reported again"
        );
    }
}
