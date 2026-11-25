//! `fno inbox decisions rescope <subject-or-id> --global|--project <slug>`:
//! amend a live law's reach in place. One small `decision_scope_amended`
//! event; the law keeps its id and text; readers apply the newest amendment
//! through `row_scope`'s reach preference. No superseded twin row. Like
//! renew, the operator's own yes is the door's proof: never widen or narrow
//! without it.

use serde_json::{json, Value};

use crate::backlog::decisions::SCOPE_AMEND_EVENT;
use crate::decision_index::{self, de};
use crate::law_match::{
    all_decision_rows, append_decision, autonomy_ask_gate, chat_may_retire, door_refuse,
    is_decision_id, now_iso, project_events_journal, queue_ruling, resolve_caller, row_id,
    row_subject, scope_filtered_rows, Caller,
};

const RESCOPE_USAGE: &str = "usage: fno inbox decisions rescope <subject-or-id> (--global | --project <slug>) [--quote <the user's words>] | fno inbox decisions rescope --proposal";

/// The reach words the proposal heuristic reads the law's text for.
const FLEET_WORDS: &[&str] = &[
    "every session",
    "all sessions",
    "fleet",
    "each machine",
    "operator",
];

pub fn run_rescope(argv: &[String]) -> i32 {
    if argv.iter().any(|a| a.contains("--help")) {
        println!("{RESCOPE_USAGE}");
        return 0;
    }
    // Proposal mode: the page, not a write. Every live law, its effective
    // reach, the proposed reach and the one-line reason.
    if argv.is_empty() || argv.iter().any(|a| a.as_str() == "--proposal") {
        return run_proposal();
    }
    let mut token = None;
    let mut reach = None;
    let mut quote = None;
    let mut it = argv.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--global" => reach = Some("global".to_string()),
            "--project" => reach = it.next().map(|v| format!("project:{}", v.trim())),
            "--quote" => quote = it.next().cloned(),
            a if a.starts_with('-') => {
                eprintln!("fno inbox decisions rescope: no such option: {a}\n{RESCOPE_USAGE}");
                return 2;
            }
            a if token.is_none() => token = Some(a.trim().to_string()),
            _ => {
                eprintln!("fno inbox decisions rescope: too many arguments\n{RESCOPE_USAGE}");
                return 2;
            }
        }
    }
    let (Some(token), Some(reach)) = (token, reach) else {
        eprintln!(
            "fno inbox decisions rescope: a decision id and --global or --project <slug> are required\n{RESCOPE_USAGE}"
        );
        return 2;
    };
    // Dual read: the display number names its hash through the table; a
    // hash lowercases; a subject passes through as typed. A number nothing
    // holds refuses before any read.
    let resolved = crate::numbers::resolve('d', token.trim());
    let token = match resolved {
        Some(id) => id,
        None => {
            eprintln!(
                "fno inbox decisions rescope: {}\n{RESCOPE_USAGE}",
                crate::numbers::unresolved(token.trim())
            );
            return 2;
        }
    };
    let caller = match resolve_caller() {
        Ok(c) => c,
        Err(message) => return door_refuse(&message),
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    if let Err(message) = autonomy_ask_gate(&caller, quote.as_deref(), &cwd) {
        return door_refuse(&message);
    }
    let (rows, _damaged) = match all_decision_rows() {
        Ok(pair) => pair,
        Err(reason) => {
            return door_refuse(&format!(
                "the decision index could not be read ({reason}); nothing rescoped"
            ))
        }
    };
    let rows = scope_filtered_rows(rows);
    let data = match rescope_data(&token, &reach, &rows, &caller) {
        Ok(d) => d,
        Err(refusal) => return door_refuse(&refusal.to_string()),
    };
    let amendment_id = match crate::numbers::mint::mint_key() {
        Ok(key) => key,
        Err(error) => return door_refuse(&error.to_string()),
    };
    let mut data = data;
    if let (Some(map), Some(id)) = (data.as_object_mut(), &amendment_id) {
        map.insert("amendment_id".into(), json!(id));
    }
    let target = data
        .get(de::TARGET)
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let prior = data
        .get(de::PRIOR_REACH)
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let envelope =
        json!({"ts": now_iso(), "type": SCOPE_AMEND_EVENT, "source": "target", "data": data});
    let Some(amendment_id) = &amendment_id else {
        return queue_ruling(
            envelope,
            project_events_journal(),
            false,
            crate::numbers::mint::After::Nothing,
        );
    };
    if let Err(code) = append_decision(&envelope, amendment_id) {
        return code;
    }
    println!("{amendment_id}");
    eprintln!("fno inbox decisions rescope: {target} now reads {reach} (was {prior})");
    0
}

/// The ways a rescope refuses (docs/rust-style.md R6): the caller matches
/// the variant, the door prints the message.
#[derive(Debug, thiserror::Error)]
enum RescopeRefusal {
    #[error("reach must be global or project:<slug>, got {0:?}")]
    BadReach(String),
    #[error("no live decision matches {0:?}; nothing to rescope")]
    NoMatch(String),
    #[error("{0} live decisions match {1:?} ({2}); rescope one by id")]
    Ambiguous(usize, String, String),
    #[error("rescoping {0} needs superuser authority (got {1}; it was recorded as {2}); only the user rescopes it, so ask them with `fno inbox outstanding ask`")]
    NeedsUser(String, String, String),
}

/// The amendment's data, pure over the rows so every refusal is testable.
/// The reach value is validated here, not just parsed: `global` or
/// `project:<nonempty slug>` and nothing else.
fn rescope_data(
    token: &str,
    reach: &str,
    rows: &[Value],
    caller: &Caller,
) -> Result<Value, RescopeRefusal> {
    if reach != "global" && !(reach.starts_with("project:") && reach.len() > "project:".len()) {
        return Err(RescopeRefusal::BadReach(reach.to_string()));
    }
    let retired = decision_index::retirement_map(rows);
    let live: Vec<&Value> = rows
        .iter()
        .filter(|r| {
            !crate::law_match::is_retraction_row(r)
                && decision_index::is_law(r)
                && !retired.contains_key(&row_id(r).to_lowercase())
        })
        .collect();
    let hits: Vec<&Value> = if is_decision_id(token) {
        live.into_iter()
            .filter(|r| row_id(r).eq_ignore_ascii_case(token))
            .collect()
    } else {
        live.into_iter()
            .filter(|r| row_subject(r).trim().eq_ignore_ascii_case(token))
            .collect()
    };
    let old = match hits.as_slice() {
        [one] => *one,
        [] => return Err(RescopeRefusal::NoMatch(token.to_string())),
        many => {
            let ids: Vec<&str> = many.iter().map(|r| row_id(r)).collect();
            return Err(RescopeRefusal::Ambiguous(
                many.len(),
                token.to_string(),
                ids.join(", "),
            ));
        }
    };
    if caller.authority != "operator"
        && (caller.authority != "chat_attested" || !chat_may_retire(old))
    {
        return Err(RescopeRefusal::NeedsUser(
            row_id(old).to_string(),
            caller.authority.to_string(),
            old.get(de::AUTHORITY)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        ));
    }
    let mut data = json!({
        "authority_source": caller.authority,
        "decided_by": caller.decided_by,
        "target_decision_id": row_id(old),
        "reach": reach,
        "prior_reach": decision_index::row_scope(old),
    });
    if let Some(map) = data.as_object_mut() {
        if let Some(a) = &caller.attested_by {
            map.insert("attested_by".into(), json!(a));
        }
        if let Some(r) = &caller.relayed_by {
            map.insert("relayed_by".into(), json!(r));
        }
        crate::identity::stamp_person(map);
    }
    Ok(data)
}

/// The proposal page: every live law, its effective reach, the proposed
/// reach and a one-line reason. Printed, so the user approves on one pass;
/// the approved rows go back through the same door one rescope at a time.
fn run_proposal() -> i32 {
    let (rows, _damaged) = match all_decision_rows() {
        Ok(pair) => pair,
        Err(reason) => {
            eprintln!(
                "fno inbox decisions rescope: the decision index could not be read ({reason})"
            );
            return 1;
        }
    };
    let rows = scope_filtered_rows(rows);
    for line in proposal_lines(&rows) {
        println!(
            "| {} | {} | {} | {} | {} |",
            line.id, line.subject, line.current, line.proposed, line.reason
        );
    }
    0
}

/// One page row.
pub(crate) struct ProposalLine {
    id: String,
    subject: String,
    current: String,
    proposed: String,
    reason: String,
}

/// The pure proposal: the reach each live law carries and the reach it is
/// proposed to carry, with the evidence the proposal cites. Explicit rows
/// propose their own value; unscoped rows propose from their text.
pub(crate) fn proposal_lines(rows: &[Value]) -> Vec<ProposalLine> {
    let retired = decision_index::retirement_map(rows);
    let mut lines = Vec::new();
    for row in rows.iter().filter(|r| {
        !crate::law_match::is_retraction_row(r)
            && decision_index::is_law(r)
            && !retired.contains_key(&row_id(r).to_lowercase())
    }) {
        let id = row_id(row).to_string();
        let subject = row_subject(row).trim().to_string();
        let current = decision_index::row_scope(row).to_string();
        let (proposed, reason) = proposed_reach(row, &current);
        lines.push(ProposalLine {
            id,
            subject,
            current,
            proposed,
            reason,
        });
    }
    lines.sort_by(|a, b| b.current.cmp(&a.current).then_with(|| a.id.cmp(&b.id)));
    lines
}

/// The heuristic: explicit stamps keep their value; unscoped rows propose
/// from text evidence, defaulting to `project:fno`, which is the reach the
/// reader already applies to a pre-scope row.
fn proposed_reach(row: &Value, current: &str) -> (String, String) {
    let explicit = row.get(de::REACH).is_some()
        || row
            .get(de::SCOPE)
            .and_then(Value::as_str)
            .map(|v| decision_index::is_reach_value(v))
            .unwrap_or(false);
    if explicit {
        return (current.to_string(), "already explicit".to_string());
    }
    let subject = row_subject(row).to_lowercase();
    let text = format!(
        "{} {}",
        subject,
        row.get(de::DECISION)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase()
    );
    if text.contains("fnh") {
        return ("project:fnh".to_string(), "text names fnh".to_string());
    }
    if text.contains("fno-books") || text.split_whitespace().any(|w| w == "books") {
        return (
            "project:fno-books".to_string(),
            "text names fno-books".to_string(),
        );
    }
    if decision_index::is_fleet_subject(&subject) {
        return (
            "global".to_string(),
            "fleet subject reads global today; stamp it".to_string(),
        );
    }
    if let Some(word) = FLEET_WORDS.iter().find(|w| text.contains(*w)) {
        return ("global".to_string(), format!("text says {word}"));
    }
    (
        "project:fno".to_string(),
        "pre-scope row recorded in fno; reach made explicit".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn law(id: &str, scope: Option<&str>, subject: &str, text: &str) -> Value {
        let mut row = json!({
            "_event_type": "operator_decision", "decision_id": id, "subject": subject,
            "decision": text, "authority_source": "operator",
            "strength": "warn", "ts": "2026-09-01T00:00:00Z"
        });
        if let Some(s) = scope {
            if let Some(map) = row.as_object_mut() {
                map.insert("scope".into(), json!(s));
            }
        }
        row
    }

    #[test]
    fn rescope_data_amends_reach_and_refuses_the_rest() {
        let op = Caller::as_authority("operator");
        let rows = vec![law(
            "d-aaaa0001",
            Some("project:demo"),
            "quiet-hours",
            "No pushes at night",
        )];
        let data = rescope_data("d-aaaa0001", "global", &rows, &op).expect("rescopes");
        assert_eq!(data.get(de::TARGET), Some(&json!("d-aaaa0001")));
        assert_eq!(data.get(de::REACH), Some(&json!("global")));
        assert_eq!(data.get(de::PRIOR_REACH), Some(&json!("project:demo")));

        // A bad reach value refuses before any lookup.
        let refusal = rescope_data("d-aaaa0001", "internal", &rows, &op).expect_err("bad reach");
        assert!(refusal.to_string().contains("reach must be"), "{refusal}");
        let refusal = rescope_data("d-aaaa0001", "project:", &rows, &op).expect_err("empty slug");
        assert!(refusal.to_string().contains("reach must be"), "{refusal}");

        let refusal = rescope_data("nope", "global", &rows, &op).expect_err("none");
        assert!(
            refusal.to_string().contains("no live decision matches"),
            "{refusal}"
        );

        // Two live laws under one subject refuse until one is named by id.
        let rows = vec![
            law("d-aaaa0001", None, "quiet-hours", "No pushes at night"),
            law("d-bbbb0002", None, "quiet-hours", "No deploys either"),
        ];
        let refusal = rescope_data("quiet-hours", "global", &rows, &op).expect_err("ambiguous");
        assert!(
            refusal.to_string().contains("rescope one by id"),
            "{refusal}"
        );
    }

    #[test]
    fn rescoping_needs_the_operators_yes() {
        let chat = Caller::as_authority("chat_attested");
        let agent = Caller::as_authority("agent");
        let rows = vec![law("d-aaaa0001", None, "quiet-hours", "No pushes at night")];
        for caller in [&chat, &agent] {
            let refusal = rescope_data("d-aaaa0001", "global", &rows, caller)
                .expect_err("not the user's yes");
            assert!(
                refusal.to_string().contains("needs superuser authority"),
                "{refusal}"
            );
        }
    }

    #[test]
    fn proposal_proposes_from_text_evidence_and_keeps_explicit_rows() {
        let rows = vec![
            law(
                "d-aaaa0001",
                Some("project:fno"),
                "review-gate",
                "Two review rounds",
            ),
            law("d-bbbb0002", None, "dispatch-rules", "How workers dispatch"),
            law(
                "d-cccc0003",
                None,
                "fnh-installer",
                "The fnh installer layout",
            ),
            law(
                "d-dddd0004",
                None,
                "reading-list",
                "Every session reads the board",
            ),
            law("d-eeee0005", None, "quiet-hours", "No pushes at night"),
        ];
        let lines = proposal_lines(&rows);
        let get = |id: &str| lines.iter().find(|l| l.id == id).expect("line");
        assert_eq!(get("d-aaaa0001").proposed, "project:fno");
        assert_eq!(get("d-aaaa0001").reason, "already explicit");
        assert_eq!(get("d-bbbb0002").proposed, "global");
        assert_eq!(get("d-cccc0003").proposed, "project:fnh");
        assert_eq!(get("d-dddd0004").proposed, "global");
        assert_eq!(get("d-eeee0005").proposed, "project:fno");
    }
}
