//! The wake-context door: a bare resume word ("go", "continue", ...) on a
//! claude `UserPromptSubmit` for a session with an OPEN target run injects
//! the run state and the resume protocol, so a woken worker runs to its
//! next real gate instead of answering a one-word prompt vaguely and
//! stopping. This is the wake half of node x-3bb2: the stop gate already
//! governs the turn end; nothing governed the wake.
//!
//! Applicable means ALL of: the prompt is exactly a wake word, the registry
//! row binds this harness session to a node, a target manifest exists for
//! the session's cwd, the manifest's `harness_session_id` IS this session
//! (the run belongs to this worker, never a sibling), and no legacy
//! terminal status closed the run. Anything else is silent exit 0: nothing
//! optional sits on the foreground path (law d-bad5a42f). An injection
//! appends one `wake` row to the journals - the journal truth the stop
//! gate's fire history already reads, which the wake grace keys on.

use serde_json::Value;
use std::io::Read;
use std::path::Path;

/// The bare wake words. Exactly these, trimmed and lowercased: the machinery
/// itself sends "go" (resume_wake arm C) and "continue" (codex route), so a
/// human "go" and a machine wake share one contract. A prompt CONTAINING a
/// wake word ("go fix the flake") is a real instruction, never a wake.
const WAKE_WORDS: [&str; 5] = ["go", "continue", "resume", "go on", "keep going"];

/// Legacy terminal statuses: a run stamped with one is closed and must not
/// resurrect on a wake.
const CLOSED_STATUS: [&str; 3] = ["COMPLETE", "BLOCKED", "ABORTED"];

pub fn run(_rest: &[String]) -> i32 {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return 0;
    }
    if let Some(text) = wake_context(&input) {
        println!("{text}");
    }
    0
}

/// The manifest fields the wake decision and the protocol need. A local
/// minimal parser, same shape as loopcheck's `parse_manifest`.
struct ManifestFacts {
    session_id: String,
    harness_session_id: Option<String>,
    graph_node_id: Option<String>,
    input: String,
    plan_path: Option<String>,
    closed: bool,
}

fn manifest_facts(content: &str) -> Option<ManifestFacts> {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return None;
    }
    let after_first = &content[3..];
    let end = after_first.find("\n---")?;
    let mut f = ManifestFacts {
        session_id: String::new(),
        harness_session_id: None,
        graph_node_id: None,
        input: String::new(),
        plan_path: None,
        closed: false,
    };
    for line in after_first[..end].lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim().trim_matches(|c: char| c == '"' || c == '\'');
        match k.trim() {
            "fno_id" => f.session_id = v.to_string(),
            "session_id" => {
                if f.session_id.is_empty() {
                    f.session_id = v.to_string();
                }
            }
            "harness_session_id" => {
                if !v.is_empty() && v != "null" {
                    f.harness_session_id = Some(v.to_string());
                }
            }
            "graph_node_id" => {
                if !v.is_empty() {
                    f.graph_node_id = Some(v.to_string());
                }
            }
            "input" => f.input = v.to_string(),
            "plan_path" => {
                if !v.is_empty() {
                    f.plan_path = Some(v.to_string());
                }
            }
            "status" => {
                if CLOSED_STATUS.contains(&v.to_uppercase().as_str()) {
                    f.closed = true;
                }
            }
            _ => {}
        }
    }
    if f.session_id.is_empty() {
        return None;
    }
    Some(f)
}

/// The door's one decision, testable without stdin. `None` = silent.
pub(crate) fn wake_context(payload: &str) -> Option<String> {
    let payload: Value = serde_json::from_str(payload).ok()?;
    let sid = payload
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let prompt = payload
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !is_wake_word(prompt) {
        return None;
    }
    let harness = payload
        .get("harness")
        .and_then(Value::as_str)
        .unwrap_or("claude");

    let home = crate::paths::AgentsHome::from_env();
    let registry = crate::state::load_registry(&home.registry_json()).ok()?;
    let row = registry.find_by_session(harness, sid)?;
    let node = row.node.clone()?;
    let cwd = {
        let from_payload = payload.get("cwd").and_then(Value::as_str);
        match from_payload {
            c if c.map(str::trim).unwrap_or_default().is_empty() => row.cwd.clone(),
            Some(c) => c.to_string(),
            None => row.cwd.clone(),
        }
    };

    let manifest_path = crate::paths::worktree_space_dir(Path::new(&cwd)).join("target-state.md");
    let content = std::fs::read_to_string(&manifest_path).ok()?;
    let facts = manifest_facts(&content)?;
    // The run must belong to THIS session: a manifest naming a sibling
    // session is another worker's run, never ours to wake.
    if facts.harness_session_id.as_deref().map(str::trim) != Some(sid) {
        return None;
    }
    if facts.closed {
        return None;
    }
    let text = render_protocol(
        facts.graph_node_id.as_deref().unwrap_or(node.as_str()),
        &facts.input,
        facts.plan_path.as_deref(),
    );
    journal_wake(&cwd, &facts.session_id, sid, node.as_str());
    Some(text)
}

/// Bare means the trimmed, lowercased prompt EQUALS a wake word.
fn is_wake_word(prompt: &str) -> bool {
    WAKE_WORDS.contains(&prompt.trim().to_lowercase().as_str())
}

/// The injection: what a woken worker needs to run to its next real gate.
/// Short on purpose - every wake pays this into the model's context.
fn render_protocol(node: &str, input: &str, plan_path: Option<&str>) -> String {
    let plan = plan_path.unwrap_or("no plan bound");
    format!(
        "wake: target run {node} is still open in this session (input: {input}).\n\
         The user's wake word means: continue the mission now.\n\
         - Do not re-init and do not run target start; the claim and manifest are already yours.\n\
         - Work the next concrete step (plan: {plan}).\n\
         - End the turn only by the run's rules: emit the MISSION COMPLETE promise only when the PR is up, CI green and reviews drained; while waiting on an async check, arm ONE harness-tracked watcher and end with a <watching> tag; otherwise keep working.\n\
         - If the run is actually finished or genuinely blocked beyond you, say so plainly instead of idling."
    )
}

/// One `wake` row, project + global journals, shaped like the gate's own
/// rows: `data.session_id` is the manifest's fno session id so the stop
/// gate's per-session reads answer it; the harness id rides beside it.
fn journal_wake(cwd: &str, session_id: &str, harness_session_id: &str, node: &str) {
    let project = crate::paths::events_path(Path::new(cwd));
    let global = crate::gate::loopcheck::global_events_path();
    crate::gate::loopcheck::emit_to_both(
        &project,
        &global,
        "wake",
        serde_json::json!({
            "session_id": session_id,
            "harness_session_id": harness_session_id,
            "node": node,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_words_wake_containing_does_not() {
        for word in WAKE_WORDS {
            assert!(is_wake_word(word), "{word} must wake");
            assert!(is_wake_word(&format!("  {word}\n")), "trimmed {word}");
            assert!(is_wake_word(&word.to_uppercase()), "case-folded {word}");
        }
        assert!(!is_wake_word("go fix the flake in ci"));
        assert!(!is_wake_word("good morning"));
        assert!(!is_wake_word("going to need a review"));
        assert!(!is_wake_word(""));
    }

    #[test]
    fn manifest_facts_parse_the_real_shape() {
        let m = manifest_facts(
            "---\nfno_id: f-id-1\nsession_id: f-id-1\ninput: \"x-1\"\n\
             harness_session_id: 3afe9590-0496-4ee7-89be-cdc23dced8a2\n\
             graph_node_id: x-3bb2\nplan_path: /plans/p.md\n---\ntail",
        )
        .expect("parses");
        assert_eq!(m.session_id, "f-id-1");
        assert_eq!(
            m.harness_session_id.as_deref(),
            Some("3afe9590-0496-4ee7-89be-cdc23dced8a2")
        );
        assert_eq!(m.graph_node_id.as_deref(), Some("x-3bb2"));
        assert!(!m.closed);

        let legacy = manifest_facts("---\nfno_id: f-id-2\nstatus: COMPLETE\n---\n").unwrap();
        assert!(legacy.closed, "a terminal status means the run is closed");
        assert!(
            manifest_facts("not a manifest").is_none(),
            "no frontmatter, no facts"
        );
        assert!(
            manifest_facts("---\ninput: x\n---\n").is_none(),
            "no session id, no facts"
        );
        // init writes the literal string null for an unresolvable harness id.
        let null_sid = manifest_facts("---\nfno_id: f\nharness_session_id: null\n---\n").unwrap();
        assert_eq!(null_sid.harness_session_id, None);
    }

    #[test]
    fn protocol_names_node_plan_and_the_turn_rules() {
        let t = render_protocol("x-3bb2", "x-3bb2", Some("/plans/p.md"));
        assert!(t.contains("x-3bb2"));
        assert!(t.contains("/plans/p.md"));
        assert!(t.contains("MISSION COMPLETE"));
        assert!(t.contains("<watching>"));
        assert!(t.contains("Do not re-init"));
        let t = render_protocol("x-1", "x-1", None);
        assert!(t.contains("no plan bound"));
    }
}
