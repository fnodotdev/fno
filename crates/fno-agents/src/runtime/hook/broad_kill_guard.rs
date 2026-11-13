//! `fno-agents hook broad-kill-guard` - advise on a broad process kill.
//!
//! `pkill` matches every process this user can signal whose name (or, with
//! `-f`, whose full argv) carries the pattern, and `killall <name>` matches
//! every process of that name. Another session's argv can carry your
//! pattern text without naming your process: on 2026-11-13 a worker's
//! `pkill -f` on a cargo pattern killed another session's node command
//! whose argv carried law text naming cargo.
//!
//! Advisory only (the D3141 posture): the call runs, and the note names the
//! scoped stop. A broad kill harms shared-machine work, never data or
//! money, so a refusal here would only wedge legitimate fleet work.
//!
//! Parse-only, like every probe here: a token walk in command position per
//! pipeline stage, with `bash -c` payload recursion, and it fails open on
//! anything unexpected.

use serde_json::Value;
use std::path::PathBuf;

use super::bg_process_guard::{is_separator, shlex_tokens, strip_heredocs};
use super::test_run_guard::head_of;

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];

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
    if !crate::agents_config::guard_enabled(&cwd, "broad-kill") {
        return super::emit_allow();
    }
    let note = judge(&payload);
    let decision = if note.is_some() { "nudge" } else { "allow" };
    super::emit_guard_row(&cwd, "broad-kill-guard", "Bash", decision);
    match note {
        Some(reason) => super::emit_warn(
            &super::refusal::Refusal::new(super::refusal::Guard::BroadKill, reason).render(),
        ),
        None => super::emit_allow(),
    }
}

/// The whole verdict for one payload: a note, or None to allow.
pub(super) fn judge(payload: &Value) -> Option<String> {
    if payload.get("tool_name").and_then(Value::as_str) != Some("Bash") {
        return None;
    }
    let command = payload
        .pointer("/tool_input/command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())?;
    decide(command)
}

/// The whole verdict for one command string: a note, or None to allow.
/// Unbalanced quotes cannot tell command position, so they allow.
fn decide(command: &str) -> Option<String> {
    let tokens = shlex_tokens(&strip_heredocs(command))?;
    let cmd = broad_kill_segment(&tokens, 0)?;
    Some(reason(&cmd))
}

/// True when this argv carries a non-flag operand: for `pkill` the match
/// pattern, for `killall` a process name. Flag spellings start with `-`, so
/// `pkill -f cargo` and `pkill cargo` both answer true while `pkill -l` and
/// a bare `killall` answer false. A value-taking option's next token can
/// read as the operand (`pkill -g 42`), which only widens an advisory.
fn names_a_target(argv: &[String]) -> bool {
    argv.iter().any(|t| !t.starts_with('-'))
}

/// The script text a shell was handed with `-c`. Short options bundle:
/// `bash -lc '...'` is the same call.
fn shell_payload(head: &str, argv: &[String]) -> Option<String> {
    if !SHELLS.contains(&head) {
        return None;
    }
    argv.iter().enumerate().find_map(|(idx, tok)| {
        let is_c_bundle = tok.len() >= 2
            && tok.starts_with('-')
            && tok.ends_with('c')
            && tok[1..tok.len() - 1]
                .chars()
                .all(|c| c.is_ascii_alphabetic());
        if is_c_bundle {
            argv.get(idx + 1).cloned()
        } else {
            None
        }
    })
}

/// First command string that runs a broad kill, or None. Read in command
/// position, per pipeline stage, with both wrapper-flag readings. A
/// `bash -c` payload recurses as its own command text, bounded by depth.
fn broad_kill_segment(tokens: &[String], depth: usize) -> Option<String> {
    if depth > 2 {
        return None;
    }
    let mut segments: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for tok in tokens {
        if is_separator(tok) {
            if !current.is_empty() {
                segments.push(std::mem::take(&mut current));
            }
        } else {
            current.push(tok.clone());
        }
    }
    if !current.is_empty() {
        segments.push(current);
    }
    for segment in &segments {
        let mut parts: Vec<Vec<String>> = Vec::new();
        let mut stage: Vec<String> = Vec::new();
        for tok in segment {
            if tok == "|" || tok == "|&" {
                if !stage.is_empty() {
                    parts.push(std::mem::take(&mut stage));
                }
            } else {
                stage.push(tok.clone());
            }
        }
        if !stage.is_empty() {
            parts.push(stage);
        }
        for part in &parts {
            for greedy in [false, true] {
                let Some((head, argv, _)) = head_of(part, greedy) else {
                    continue;
                };
                let broad = (head == "pkill" || head == "killall") && names_a_target(&argv);
                if broad {
                    return Some(part.join(" "));
                }
                if let Some(payload) = shell_payload(&head, &argv).filter(|p| !p.is_empty()) {
                    let Some(inner) = shlex_tokens(&strip_heredocs(&payload)) else {
                        continue;
                    };
                    if let Some(found) = broad_kill_segment(&inner, depth + 1) {
                        return Some(found);
                    }
                }
            }
        }
    }
    None
}

fn reason(cmd: &str) -> String {
    format!(
        "`{cmd}` matches every process this user can signal that carries the pattern in its name or argv, across sessions. On 2026-11-13 a worker's `pkill -f` on a cargo pattern killed another session's command whose argv carried law text naming cargo. The command still runs; prefer the scoped stop."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nudges_each_broad_kill_form() {
        for cmd in [
            "pkill -f cargo",
            "pkill cargo",
            "pkill -TERM -f 'cargo build'",
            "pkill -9f pattern",
            "pkill --full cargo",
            "/usr/bin/pkill -f cargo",
            "sudo pkill -f cargo",
            "sudo -u me killall node",
            "killall node",
            "killall -9 node",
            "killall -TERM node",
            "pkill -f x | cat",
            "cat list | killall node",
            "echo hi\npkill cargo",
            "bash -c 'pkill -f cargo'",
            "bash -lc 'killall node'",
            "FOO=1 pkill -f x",
            "timeout 300 pkill -f x",
            "caffeinate -t 3600 pkill -f x",
            "nohup pkill cargo &",
        ] {
            assert!(decide(cmd).is_some(), "should nudge: {cmd:?}");
        }
    }

    #[test]
    fn allows_scoped_and_unrelated_commands() {
        for cmd in [
            "kill 1234",
            "kill -- -1234",
            "kill -9 1234",
            "kill %1",
            "pkill -l",
            "pkill --help",
            "killall -l",
            "pkill",
            "killall",
            "grep pkill file.txt",
            "echo pkill cargo",
            "rg -n 'pkill -f' crates/",
            "echo 'unterminated pkill -f",
            "case $x in y) echo pick;; esac",
            "cat notes.md | head -20",
        ] {
            assert!(decide(cmd).is_none(), "should allow: {cmd:?}");
        }
    }

    #[test]
    fn other_tools_and_blank_input_allow() {
        assert!(judge(&Value::Null).is_none());
        assert!(judge(&serde_json::json!({
            "tool_name": "Read",
            "tool_input": {"command": "pkill -f cargo"}
        }))
        .is_none());
        assert!(judge(&serde_json::json!({
            "tool_name": "Bash",
            "tool_input": {"command": "  "}
        }))
        .is_none());
    }
}
