//! Who answers a permission ask a thread worker raises: one ladder, many
//! transports. The user's own harness rules decide first (an act they allow
//! never asks). Every ask that is left goes to fnod with one choice question,
//! and fnod's [`Escalate`] picks the answer:
//!
//! - nobody (fnod is sure the act is routine): allow it once;
//! - the model (fnod is unsure, untrained, slow or off): refuse the call
//!   with a note the model reads as the tool's refusal, so it changes course;
//! - the user (a sure irreversible act, or one the user's own `ask` rule
//!   raised): file an outstanding ask and hold the call until they answer.
//!
//! fnod down never allows. The transport (opencode here) only lists, parses
//! and replies; the decision lives once.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::fnod::{self, Decision, Escalate, Question};

pub(crate) const QUESTION_ID: &str = "thread_permission";
const DEFAULT_TIMEOUT_MS: u64 = 500;

/// One pending ask, in harness-neutral words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThreadAsk {
    pub harness: String,
    pub session_id: String,
    pub request_id: String,
    /// The permission the call needs: `bash`, `edit`, `external_directory`.
    pub action: String,
    /// What the call touches: a command, a path pattern.
    pub resources: Vec<String>,
    pub cwd: PathBuf,
    pub worker: Option<String>,
    /// The user's own rules evaluate this call to `ask`: they asked to be asked.
    pub user_rule: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Answer {
    Allow,
    /// Refuse the call; the text reaches the model as the refusal.
    ModelNote(String),
    /// Leave the call pending; the user answers question `question_id`.
    HoldForUser {
        question_id: String,
    },
}

impl Answer {
    fn as_str(&self) -> &'static str {
        match self {
            Answer::Allow => "allow",
            Answer::ModelNote(_) => "model_note",
            Answer::HoldForUser { .. } => "hold_for_user",
        }
    }
}

/// The question fnod reads. No corpus head is trained on it yet, so it
/// reads untrained and fnod's answer always goes to the model: the ruling
/// is that an unsure call never reaches the user straight.
pub(crate) fn question(irreversible: bool) -> Question {
    Question {
        id: QUESTION_ID.to_string(),
        wire: json!({"type": "choice",
        "instructions": "An unattended coding agent asks to run this tool call. How safe is it to allow without a person?",
        "criteria": {
            "routine": "Ordinary work inside the task: allowing it is safe.",
            "unsure": "It may be fine, but the agent should choose another way.",
            "irreversible": "It destroys, publishes or spends something a person must approve."
        }}),
        trained: false,
        escalate_below: 0.0,
        irreversible,
    }
}

/// The production ladder: fnod from fno config, the outstanding-ask door
/// for a hold, the journal under `events`.
pub(crate) fn decide(ask: &ThreadAsk, events: &Path) -> Answer {
    let timeout = crate::agents_config::config_value_deep(&ask.cwd, &["fnod", "timeout_ms"])
        .and_then(|v| v.as_integer())
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    decide_with(
        ask,
        &question(ask.user_rule),
        &fnod::configured_url(&ask.cwd),
        Duration::from_millis(timeout),
        &file_hold,
        events,
    )
}

/// Filing a held ask failed: the outstanding-ask door exited non-zero.
#[derive(Debug, thiserror::Error)]
#[error("outstanding ask exited {code}")]
pub(crate) struct HoldError {
    pub code: i32,
}

/// The ladder with its fnod question, server and hold door injected.
pub(crate) fn decide_with(
    ask: &ThreadAsk,
    q: &Question,
    url: &str,
    timeout: Duration,
    hold: &dyn Fn(&ThreadAsk) -> Result<String, HoldError>,
    events: &Path,
) -> Answer {
    let state = json!({
        "harness": ask.harness,
        "action": ask.action,
        "resources": ask.resources,
        "cwd": ask.cwd,
        "user_rule": ask.user_rule,
    });
    let d = fnod::ask(url, timeout, state, q);
    let label = d.label.as_deref();
    let mut hold_error = None;
    let answer = match (d.escalate, label) {
        (Escalate::None, Some("routine")) => Answer::Allow,
        (Escalate::User, _) | (Escalate::None, Some("irreversible")) => match hold(ask) {
            Ok(question_id) => Answer::HoldForUser { question_id },
            Err(e) => {
                hold_error = Some(e);
                Answer::ModelNote(note(ask, "it needs the user, and fno could not reach them"))
            }
        },
        _ => Answer::ModelNote(note(ask, &model_reason(&d))),
    };
    journal(events, ask, &d, &answer, hold_error.as_ref());
    answer
}

fn model_reason(d: &Decision) -> String {
    match &d.error {
        Some(_) => "fno could not judge it safe to run unattended".to_string(),
        None => "fno is not sure it is safe to run unattended".to_string(),
    }
}

/// The refusal text the model reads. It names fno, the act and the way on,
/// and names no command: a model handed a command runs it (measured on
/// opencode 1.18.33).
fn note(ask: &ThreadAsk, why: &str) -> String {
    format!(
        "fno held this {} call ({}): {why}. Do not retry it. Do the task another way, or say in your reply that this step needs the user.",
        ask.action,
        ask.resources.join(", ")
    )
}

fn journal(
    events: &Path,
    ask: &ThreadAsk,
    d: &Decision,
    answer: &Answer,
    hold_error: Option<&HoldError>,
) {
    let mut fields: Vec<(&str, Value)> = vec![
        ("harness", json!(ask.harness)),
        ("session_id", json!(ask.session_id)),
        ("request_id", json!(ask.request_id)),
        ("permission_action", json!(ask.action)),
        ("resources", json!(ask.resources)),
        ("user_rule", json!(ask.user_rule)),
        ("answer", json!(answer.as_str())),
        ("fnod_label", json!(d.label)),
        ("fnod_confidence", json!(d.confidence)),
        ("fnod_escalate", json!(d.escalate.as_str())),
        ("fnod_ms", json!(d.ms)),
    ];
    if let Some(worker) = &ask.worker {
        fields.push(("name", json!(worker)));
    }
    if let Some(e) = &d.error {
        fields.push(("fnod_error", json!(e)));
    }
    if let Answer::HoldForUser { question_id } = answer {
        fields.push(("question_id", json!(question_id)));
    }
    if let Some(e) = hold_error {
        fields.push(("hold_error", json!(e.to_string())));
    }
    crate::claude_ask::emit_event(events, "thread_permission_answered", &fields);
}

/// File the hold as an outstanding ask from the worker's cwd.
fn file_hold(ask: &ThreadAsk) -> Result<String, HoldError> {
    let who = ask.worker.as_deref().unwrap_or(&ask.session_id);
    let text = format!(
        "Thread worker {who} ({}) asks to run {} on {}. Allow it once or deny it?",
        ask.harness,
        ask.action,
        ask.resources.join(", ")
    );
    let filing = crate::outstanding_cmds::AskFiling {
        text,
        template: false,
        ask: Some(format!("fno agents ask {who} --answer allow_once|deny")),
        options: vec!["allow_once".to_string(), "deny".to_string()],
        blocks: Vec::new(),
        node: None,
        subject: Some(format!("{who} permission ask")),
        images: Vec::new(),
    };
    match crate::outstanding_cmds::file_ask(filing, &ask.cwd) {
        (0, Some(qid)) => Ok(qid),
        (code, _) => Err(HoldError { code }),
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)] // asserts index the journal row's data object
mod tests {
    use super::*;
    use crate::fnod::tests::serve_once;

    type Hold = Result<String, HoldError>;

    #[derive(serde::Deserialize)]
    struct Row {
        data: Value,
    }

    fn ask(user_rule: bool) -> ThreadAsk {
        ThreadAsk {
            harness: "opencode".into(),
            session_id: "ses_a".into(),
            request_id: "per_a".into(),
            action: "bash".into(),
            resources: vec!["touch /tmp/x".into()],
            cwd: PathBuf::from("/w"),
            worker: Some("w1".into()),
            user_rule,
        }
    }

    fn trained(irreversible: bool) -> Question {
        let mut q = question(irreversible);
        q.trained = true;
        q.escalate_below = 0.6;
        q
    }

    /// The `data` object of each journaled answer row.
    fn rows(events: &Path) -> Vec<Value> {
        crate::event_store::journal_text(events, &["thread_permission_answered"])
            .lines()
            .filter_map(|l| serde_json::from_str::<Row>(l).ok())
            .map(|r| r.data)
            .collect()
    }

    const ROUTINE: &str =
        r#"{"answers":{"thread_permission":{"probabilities":{"routine":0.9,"unsure":0.1}}}}"#;

    #[test]
    fn a_sure_routine_answer_allows_and_journals_its_label() {
        let dir = tempfile::tempdir().unwrap();
        let events = dir.path().join("events.jsonl");
        let (url, server) = serve_once(ROUTINE, Duration::ZERO);
        let no_hold = |_: &ThreadAsk| -> Hold { panic!("no hold") };
        let answer = decide_with(
            &ask(false),
            &trained(false),
            &url,
            Duration::from_secs(2),
            &no_hold,
            &events,
        );
        server.join().unwrap();
        assert_eq!(answer, Answer::Allow);
        let rows = rows(&events);
        assert_eq!(rows.len(), 1);
        let data = &rows[0];
        assert_eq!(data["fnod_label"], "routine");
        assert_eq!(data["answer"], "allow");
        assert_eq!(data["request_id"], "per_a");
    }

    #[test]
    fn fnod_off_or_slow_or_untrained_never_allows() {
        let dir = tempfile::tempdir().unwrap();
        let events = dir.path().join("events.jsonl");
        let no_hold = |_: &ThreadAsk| -> Hold { panic!("no hold") };
        let off = decide_with(
            &ask(true),
            &trained(true),
            "",
            Duration::from_millis(100),
            &no_hold,
            &events,
        );
        assert!(
            matches!(off, Answer::ModelNote(ref m) if m.contains("Do not retry")),
            "{off:?}"
        );

        let (url, server) = serve_once(ROUTINE, Duration::from_millis(600));
        let slow = decide_with(
            &ask(false),
            &trained(false),
            &url,
            Duration::from_millis(100),
            &no_hold,
            &events,
        );
        server.join().ok();
        assert!(matches!(slow, Answer::ModelNote(_)), "{slow:?}");

        // The shipped question is untrained: a sure routine answer still goes
        // to the model.
        let (url, server) = serve_once(ROUTINE, Duration::ZERO);
        let fresh = decide_with(
            &ask(false),
            &question(false),
            &url,
            Duration::from_secs(2),
            &no_hold,
            &events,
        );
        server.join().unwrap();
        assert!(matches!(fresh, Answer::ModelNote(_)), "{fresh:?}");
    }

    #[test]
    fn a_user_rule_holds_for_the_user_and_a_failed_filing_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let events = dir.path().join("events.jsonl");
        let filed = |_: &ThreadAsk| -> Hold { Ok("q-1".into()) };
        let (url, server) = serve_once(ROUTINE, Duration::ZERO);
        let held = decide_with(
            &ask(true),
            &trained(true),
            &url,
            Duration::from_secs(2),
            &filed,
            &events,
        );
        server.join().unwrap();
        assert_eq!(
            held,
            Answer::HoldForUser {
                question_id: "q-1".into()
            }
        );

        let broken = |_: &ThreadAsk| -> Hold { Err(HoldError { code: 3 }) };
        let (url, server) = serve_once(ROUTINE, Duration::ZERO);
        let refused = decide_with(
            &ask(true),
            &trained(true),
            &url,
            Duration::from_secs(2),
            &broken,
            &events,
        );
        server.join().unwrap();
        assert!(matches!(refused, Answer::ModelNote(_)), "{refused:?}");
        let data = rows(&events).pop().unwrap();
        assert_eq!(data["hold_error"], "outstanding ask exited 3");
    }
}
