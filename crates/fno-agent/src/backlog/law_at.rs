//! A law's action triggers: `--at-command <glob>` and `--at-door
//! <door>[:<subject-glob>]` on the record door. The row stores
//! `at_command: [..]` and `at_door: {door, subjects}`; the rule source turns
//! them into one tier-0 rule, `law-<id>-at`, beside the law's prompt rule. A
//! law about an action then fires at the action, not only when prompt text
//! happens to rank it.

use serde_json::{json, Map, Value};

/// The doors a law can name: the door events with a wired decide() site.
pub const DOORS: &[&str] = &["send", "spawn", "merge"];

#[derive(Debug, Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct AtDoor {
    pub door: String,
    pub subjects: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, serde::Deserialize)]
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
}
