//! `fno-agents hook gate` - every native claude PreToolUse guard in one
//! process.
//!
//! hooks.json once registered one command per guard, so a Bash call started
//! six bash shells and six fno-agents processes (measured 2026-11-13 under
//! fleet load: about 1.1 s of hook time per call, the rule set rebuilt from
//! the store on each). This entry runs the same guard entries in the former
//! registration order, each with the payload injected and its answer
//! captured, and prints one merged answer.
//!
//! Merge: a deny from any guard refuses and carries every refusal reason; an
//! `ask` outranks an allow. Notices join into one `additionalContext` and one
//! `systemMessage`, and a rewritten tool input passes through. Only a guard's
//! refusal refuses: advice never blocks. A guard that answers nonzero or
//! panics is skipped with a stderr line, as the wrappers skipped a binary
//! that could not answer.
//!
//! The session-state report runs on a side thread so its RPC bound never
//! adds to the verdict, and the control-mail drain runs last. Both are
//! claude-only and mux-profile: the agy and pi guard host, and a pack
//! install, run this entry for its block guards alone.

use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::time::Duration;

type Entry = fn(&[String]) -> i32;

/// The payload fields this entry reads. A field of the wrong type reads as
/// an empty payload, which runs no guard: the old unparsable-payload allow.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Payload {
    tool_name: Option<String>,
    cwd: Option<String>,
    session_id: Option<String>,
}

impl Payload {
    fn read(raw: &str) -> Self {
        serde_json::from_str(raw.trim()).unwrap_or_default()
    }

    fn cwd(&self) -> Option<PathBuf> {
        self.cwd
            .as_deref()
            .filter(|c| !c.is_empty())
            .map(PathBuf::from)
    }
}

/// One guard's answer, in the PreToolUse shape the emit helpers print.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Answer {
    decision: Option<String>,
    reason: Option<String>,
    system_message: Option<String>,
    hook_specific_output: Option<Specific>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Specific {
    permission_decision: Option<String>,
    permission_decision_reason: Option<String>,
    additional_context: Option<String>,
    updated_input: Option<Value>,
}

/// The guards one tool call meets, in the former hooks.json order.
fn chain(tool: &str, mux: bool) -> Vec<(&'static str, Entry)> {
    let is = |names: &[&str]| names.contains(&tool);
    let mut out: Vec<(&'static str, Entry)> = Vec::new();
    if is(&["Edit", "Write", "Bash"]) {
        out.push(("write-gate", super::write_gate::run));
    }
    if is(&["Edit", "Write"]) {
        out.push(("join-partition-guard", join_partition));
    }
    if is(&["Edit", "Write", "NotebookEdit", "Bash"]) {
        out.push(("lead-guard", super::lead_guard::run));
    }
    if mux && tool == "SendMessage" {
        out.push(("send-message-guard", super::send_message_guard::run));
    }
    if is(&["Edit", "Write", "Bash", "Skill"]) {
        out.push(("rules", rules_pre_tool));
    }
    if tool == "Bash" {
        out.push(("pretooluse-bash", super::pretooluse_bash::run));
    }
    if tool.starts_with("mcp__") {
        out.push(("effect-guard", crate::effect_gate::run_hook));
    }
    if tool == "EnterWorktree" {
        out.push((
            "subagent-worktree-guard",
            super::subagent_worktree_guard::run,
        ));
    }
    out
}

/// The write gate's posture when it cannot answer, the same one its shim
/// holds: a payload naming a protected state file is refused, any other
/// payload proceeds.
fn write_gate_fail_closed(raw: &str) -> Option<String> {
    let protected = [
        "graph.json",
        "target-state.md",
        "graph.db",
        ".fno/artifacts/",
    ];
    if !protected.iter().any(|p| raw.contains(p)) {
        return None;
    }
    let reason = "graph-write-protect: the write gate could not answer for a payload \
                  referencing a protected state file; blocking fail-closed.";
    Some(
        json!({"decision": "block", "reason": reason, "hookSpecificOutput": {
            "hookEventName": "PreToolUse", "permissionDecision": "deny",
            "permissionDecisionReason": reason}})
        .to_string(),
    )
}

fn rules_pre_tool(_args: &[String]) -> i32 {
    super::rules::run("pre-tool")
}

/// The join partition guard behind the stat its shell hook ran first: only a
/// joined worktree (a real `.fno/join-partition/`) pays for the judge.
fn join_partition(_args: &[String]) -> i32 {
    let raw = super::read_stdin();
    let payload = Payload::read(&raw);
    let Some(cwd) = payload
        .cwd()
        .filter(|c| c.join(".fno/join-partition").is_dir())
    else {
        return super::emit_allow();
    };
    let tool = payload.tool_name.as_deref().unwrap_or("");
    let verdict = super::join_partition_guard::judge(&raw);
    super::emit_guard_decision(&cwd, "join-partition-write-guard", tool, verdict.is_some());
    match verdict {
        Some(reason) => super::refusal::emit_one(super::refusal::Refusal::new(
            super::refusal::Guard::JoinPartition,
            reason,
        )),
        None => super::emit_allow(),
    }
}

/// How long the verdict waits for the session-state report. The report
/// bounds its own RPC at 2 s; past this the process exits and drops it.
const SIDE_LEG_WAIT: Duration = Duration::from_millis(2500);

pub fn run(_args: &[String]) -> i32 {
    let raw = super::read_stdin();
    let payload = Payload::read(&raw);
    let tool = payload.tool_name.clone().unwrap_or_default();
    let hosted = std::env::var("FNO_HOOK_HARNESS").is_ok_and(|h| !h.is_empty() && h != "claude");
    // A pack install drops every mux script, session-state.sh among them,
    // and with it the mux legs this entry folds in.
    let mux = std::env::var_os("FNO_REPO_ROOT")
        .map(PathBuf::from)
        .is_none_or(|root| root.join("hooks/session-state.sh").is_file());
    let side_legs = mux && !hosted;

    let whole: Option<Value> = serde_json::from_str(raw.trim()).ok();
    let report = whole.filter(|_| side_legs).map(|whole| {
        let (done, wait) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            super::session_state::process("claude", "PreToolUse", &whole);
            let _ = done.send(());
        });
        wait
    });

    let mut answers = Vec::new();
    if !tool.is_empty() {
        for (name, entry) in chain(&tool, mux) {
            let failure = match std::panic::catch_unwind(|| super::captured(&raw, || entry(&[]))) {
                Ok((0, out)) => {
                    answers.push(out);
                    continue;
                }
                Ok((code, _)) => format!("answered exit {code}"),
                Err(_) => "panicked".to_string(),
            };
            eprintln!("hook gate: {name} {failure}; skipped");
            if name == "write-gate" {
                answers.extend(write_gate_fail_closed(&raw));
            }
        }
    }

    if side_legs {
        let session = payload.session_id.as_deref().unwrap_or("");
        let cwd = payload
            .cwd()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        answers.extend(crate::mail_control_drain::tool_boundary_hook(session, &cwd));
    }

    println!("{}", merge(&answers));
    if let Some(wait) = report {
        let _ = wait.recv_timeout(SIDE_LEG_WAIT);
    }
    0
}

/// Fold every guard's answer into the one PreToolUse answer.
pub(crate) fn merge(answers: &[String]) -> Value {
    let mut denies: Vec<String> = Vec::new();
    let mut asks: Vec<String> = Vec::new();
    let mut allows: Vec<String> = Vec::new();
    let mut allowed = false;
    let mut context: Vec<String> = Vec::new();
    let mut system: Vec<String> = Vec::new();
    let mut updated: Option<Value> = None;
    let lines = answers.iter().flat_map(|a| a.lines());
    for answer in lines.filter_map(|l| serde_json::from_str::<Answer>(l.trim()).ok()) {
        let hso = answer.hook_specific_output.unwrap_or_default();
        let reason = hso.permission_decision_reason.or(answer.reason);
        let decision = hso.permission_decision.as_deref();
        if decision == Some("deny") || answer.decision.as_deref() == Some("block") {
            denies.push(reason.unwrap_or_else(|| "refused".to_string()));
            continue;
        }
        if decision == Some("ask") {
            asks.extend(reason);
            continue;
        }
        if decision == Some("allow") {
            allowed = true;
            allows.extend(reason);
        }
        context.extend(hso.additional_context);
        system.extend(answer.system_message);
        if updated.is_none() {
            updated = hso.updated_input;
        }
    }

    let mut hso = Map::new();
    hso.insert("hookEventName".into(), json!("PreToolUse"));
    let mut out = Map::new();
    if !denies.is_empty() {
        denies.extend(asks);
        let reason = denies.join("\n\n");
        out.insert("decision".into(), json!("block"));
        out.insert("reason".into(), json!(reason));
        hso.insert("permissionDecision".into(), json!("deny"));
        hso.insert("permissionDecisionReason".into(), json!(reason));
    } else if !asks.is_empty() {
        hso.insert("permissionDecision".into(), json!("ask"));
        hso.insert("permissionDecisionReason".into(), json!(asks.join("\n\n")));
    } else {
        if allowed {
            hso.insert("permissionDecision".into(), json!("allow"));
            hso.insert(
                "permissionDecisionReason".into(),
                json!(allows.join("\n\n")),
            );
        }
        if let Some(input) = updated {
            hso.insert("updatedInput".into(), input);
        }
    }
    if !context.is_empty() {
        hso.insert("additionalContext".into(), json!(context.join("\n\n")));
    }
    if !system.is_empty() {
        out.insert("systemMessage".into(), json!(system.join("\n")));
    }
    if hso.len() > 1 {
        out.insert("hookSpecificOutput".into(), Value::Object(hso));
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::merge;
    use serde_json::json;

    /// One refusal refuses the call and carries every refusal reason, while
    /// a notice from another guard rides along; with no refusal the notices
    /// join and the rewrite passes through; nothing at all is `{}`.
    #[test]
    fn merge_refuses_on_any_deny_and_otherwise_joins_advice() {
        let deny = json!({"decision": "block", "reason": "no",
            "hookSpecificOutput": {"hookEventName": "PreToolUse",
                "permissionDecision": "deny", "permissionDecisionReason": "no"}});
        let warn = json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
            "permissionDecision": "allow", "permissionDecisionReason": "heads up",
            "additionalContext": "heads up"}, "systemMessage": "heads up"});
        let rewrite = json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
            "permissionDecision": "allow", "permissionDecisionReason": "named",
            "updatedInput": {"command": "x --name y"}, "additionalContext": "named"}});

        let refused = merge(&["{}".into(), deny.to_string(), warn.to_string()]);
        assert_eq!(refused["decision"], "block");
        assert_eq!(refused["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(refused["reason"], "no");
        assert_eq!(
            refused["hookSpecificOutput"]["additionalContext"],
            "heads up"
        );
        assert!(refused["hookSpecificOutput"]["updatedInput"].is_null());

        let advised = merge(&[warn.to_string(), rewrite.to_string()]);
        assert!(advised["decision"].is_null());
        let hso = &advised["hookSpecificOutput"];
        assert_eq!(hso["permissionDecision"], "allow");
        assert_eq!(hso["additionalContext"], "heads up\n\nnamed");
        assert_eq!(hso["updatedInput"]["command"], "x --name y");
        assert_eq!(advised["systemMessage"], "heads up");

        assert_eq!(merge(&["{}".into(), String::new()]), json!({}));
    }

    /// A write gate that cannot answer still refuses a protected state file,
    /// and lets every other payload through.
    #[test]
    fn a_failed_write_gate_refuses_only_protected_state_paths() {
        let graph = r#"{"tool_name":"Edit","tool_input":{"file_path":"/h/.fno/db/graph.db"}}"#;
        let refused = super::write_gate_fail_closed(graph).expect("a refusal");
        assert_eq!(merge(&[refused])["decision"], "block");
        let notes = r#"{"tool_name":"Edit","tool_input":{"file_path":"/h/notes.md"}}"#;
        assert!(super::write_gate_fail_closed(notes).is_none());
    }
}
