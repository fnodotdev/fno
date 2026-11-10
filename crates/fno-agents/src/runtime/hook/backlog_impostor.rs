//! `fno-agents hook backlog-impostor` - refuse bare `backlog` in command
//! position: it resolves to the third-party Backlog.md CLI, not fno.
//!
//! This is the pipe guard's one four-harms reading, kept when the rest of
//! that guard was deleted (the 2026-11-07 guard-purge ruling): the
//! incident it refuses is write damage. On 2026-09-26 bare `backlog init`
//! ran the third-party CLI, whose guidelines block rewrote AGENTS.md and
//! CLAUDE.md in canonical (627 lines telling agents to run `backlog task
//! create`), and every session in canonical loaded it. The other pipe
//! readings (truncation, timeout-first, `$?` after a pipeline) policed
//! how a command's output reads; this one refuses what the command IS.
//!
//! Parse-only like its siblings: no subprocess, and it fails OPEN on
//! anything unexpected (a null payload, a non-Bash tool, a blank command,
//! an unbalanced quote). Why a refusal and not prose: prose does not fire
//! at the moment of a tool call; a refusal does.

use serde_json::Value;
use std::path::PathBuf;

use super::lead_guard::lex;
use super::test_run_guard::{head_of, stages};

/// Bare `backlog` in command position resolves to the third-party
/// Backlog.md CLI, not fno; the remedy is the fno prefix.
const BACKLOG_REASON: &str = "[fno backlog-impostor guard] `{cmd}` runs bare `backlog`, which resolves to the third-party Backlog.md CLI at ~/.bun/bin/backlog, not fno. On 2026-09-26 that CLI's guidelines block rewrote AGENTS.md and CLAUDE.md in canonical (627 lines telling agents to run `backlog task create`), and every session in canonical loaded it. Prefix the command: `fno backlog find|get|idea|update` is the node graph.";

/// Entry: read the payload once, judge, print, always exit 0.
pub fn run(_args: &[String]) -> i32 {
    let payload: Value = serde_json::from_str(super::read_stdin().trim()).unwrap_or(Value::Null);
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    // The guardrail preset owns whether this guard runs at all.
    if !crate::agents_config::guard_enabled(&cwd, "backlog-impostor") {
        return super::emit_allow();
    }
    let refusal = judge(&payload);
    super::emit_guard_decision(&cwd, "backlog-impostor-guard", "Bash", refusal.is_some());
    match refusal {
        Some(reason) => super::emit_block(&reason),
        None => super::emit_allow(),
    }
}

/// The verdict for one payload: a refusal, or None to allow. A null
/// payload, a non-Bash tool and a blank command allow.
pub(super) fn judge(payload: &Value) -> Option<String> {
    if payload.is_null() {
        return None;
    }
    if payload.get("tool_name").and_then(Value::as_str) != Some("Bash") {
        return None;
    }
    let cmd = payload
        .get("tool_input")
        .and_then(|ti| ti.get("command"))
        .and_then(Value::as_str)
        .filter(|c| !c.trim().is_empty())?;
    decide(cmd)
}

/// The whole verdict for one command string: a refusal, or None to allow.
/// Separated from `run` so tests exercise the same predicate the hook does.
fn decide(command: &str) -> Option<String> {
    let Some(toks) = lex(command) else {
        return None; // unbalanced quotes: cannot tell command position, allow
    };
    let mut seg: Vec<String> = Vec::new();
    for tok in toks {
        if matches!(tok.as_str(), ";" | ";;" | "&" | "&&" | "||" | "\n") {
            if let Some(r) = judge_segment(&seg) {
                return Some(r);
            }
            seg.clear();
            continue;
        }
        seg.push(tok);
    }
    judge_segment(&seg)
}

/// The impostor reading for one finished segment: bare `backlog` in command
/// position, in any pipeline stage. A reserved keyword leading the segment
/// (`if backlog ...`) is dropped so the command behind it is judged;
/// `fno backlog` and `echo backlog` do not refuse.
fn judge_segment(seg: &[String]) -> Option<String> {
    if seg.is_empty() {
        return None;
    }
    let cmd: Vec<String> = seg
        .iter()
        .skip_while(|t| matches!(t.as_str(), "if" | "while" | "until" | "then" | "do"))
        .cloned()
        .collect();
    if stages(&cmd)
        .iter()
        .any(|stage| head_of(stage, false).is_some_and(|(head, _, _)| head == "backlog"))
    {
        return Some(BACKLOG_REASON.replace("{cmd}", &seg.join(" ")));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn denied(cmd: &str) -> String {
        decide(cmd).expect("expected a refusal")
    }

    fn allowed(cmd: &str) {
        assert!(decide(cmd).is_none(), "expected allow: {cmd}");
    }

    #[test]
    fn bare_backlog_denies() {
        let r = denied("backlog task list");
        assert!(r.contains("fno backlog"), "names the remedy: {r}");
        assert!(r.contains("~/.bun/bin/backlog"), "names the impostor: {r}");
        denied("backlog init");
        // A chained, subshelled or wrapped bare call is still bare.
        denied("cd /tmp && backlog task list");
        denied("fno backlog get x; backlog task list");
        denied("(backlog task list)");
        denied("env backlog task list");
        denied("FOO=1 backlog task list");
        denied("if backlog init; then echo hi; fi");
        denied("while backlog task list; do :; done");
        // A later pipeline stage is still command position.
        denied("printf 'y\\n' | backlog init");
        denied("cat x | env backlog task list");
    }

    #[test]
    fn prefixed_and_prose_backlog_allows() {
        allowed("fno backlog get x");
        allowed("echo backlog task list");
        allowed("rg backlog init");
        allowed("cat backlog.md");
        allowed("ls backlog");
    }
}
