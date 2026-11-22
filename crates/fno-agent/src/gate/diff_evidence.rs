//! Did the attesting session read the change it attests? The emit refuses a
//! clean `code-review` pass with no such evidence. Evidence comes from the
//! session's own Claude transcript, never from what the caller says about
//! itself: a diff command counts only when its tool result holds real
//! `diff --git` output after the session's last head-moving command, and a
//! review subagent counts only when its call succeeded.

use crate::harness::{AsHarness, Harness};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Flags that make a diff command a size probe, not a read of the change.
const SIZE_ONLY: [&str; 8] = [
    "--stat",
    "--name-only",
    "--name-status",
    "--numstat",
    "--shortstat",
    "--summary",
    "--quiet",
    "--check",
];

/// Git subcommands that move HEAD, so an earlier read no longer covers it.
const HEAD_MOVERS: [&str; 6] = ["commit", "merge", "rebase", "cherry-pick", "am", "revert"];

/// The git subcommand of one shell segment: the first word that is no flag
/// and no `-C <dir>` / `-c <kv>` value, after any `VAR=value` prefix.
fn git_subcommand(words: &[&str]) -> Option<String> {
    let mut rest = words
        .iter()
        .skip_while(|w| w.contains('=') && !w.starts_with('-'));
    if rest.next() != Some(&"git") {
        return None;
    }
    while let Some(word) = rest.next() {
        if matches!(*word, "-C" | "-c") {
            rest.next();
        } else if !word.starts_with('-') {
            return Some((*word).to_string());
        }
    }
    None
}

fn is_diff_read(words: &[&str]) -> bool {
    let body: Vec<&str> = words
        .iter()
        .copied()
        .skip_while(|w| w.contains('=') && !w.starts_with('-'))
        .collect();
    let reads = git_subcommand(&body).as_deref() == Some("diff")
        || (body.first() == Some(&"gh")
            && body.get(1) == Some(&"pr")
            && body.get(2) == Some(&"diff"));
    reads && !body.iter().any(|w| SIZE_ONLY.contains(w))
}

fn segments(command: &str) -> impl Iterator<Item = Vec<&str>> {
    command
        .split(['\n', ';', '|', '&'])
        .map(|s| s.split_whitespace().collect::<Vec<_>>())
        .filter(|w| !w.is_empty())
}

/// The text of one tool result, and whether the harness marked it an error.
fn result_text(block: &Value) -> (bool, String) {
    let text = match &block["content"] {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    (block["is_error"].as_bool().unwrap_or(false), text)
}

/// Paths named by `diff --git a/X b/Y` headers in one diff output.
fn diff_paths(text: &str) -> HashSet<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("diff --git "))
        .filter_map(|l| l.split_whitespace().last())
        .map(str::to_string)
        .collect()
}

enum Evidence {
    Diff(HashSet<String>),
    Agent,
}

/// `{"kind": none|stale|read|unobserved, "files": N, "agent": bool}`.
/// `files` is the distinct changed files whose diff output the session read
/// after its last head-moving command.
pub(super) fn scan(agent: &str, path: Option<&Path>) -> Value {
    let unobserved = json!({"kind":"unobserved"});
    if agent.as_harness() != Harness::Claude {
        return unobserved;
    }
    let Some(text) = path.and_then(|p| std::fs::read_to_string(p).ok()) else {
        return unobserved;
    };
    let records: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let mut results: HashMap<String, (bool, String)> = HashMap::new();
    for record in &records {
        for block in record["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if block["type"] == "tool_result" {
                if let Some(id) = block["tool_use_id"].as_str() {
                    results.insert(id.to_string(), result_text(block));
                }
            }
        }
    }
    let ok_text = |id: &str| {
        results
            .get(id)
            .filter(|(is_error, text)| !is_error && !text.trim().is_empty())
            .map(|(_, text)| text.as_str())
    };
    let mut last_move = None;
    let mut evidence: Vec<(usize, Evidence)> = Vec::new();
    for (index, record) in records.iter().enumerate() {
        for block in record["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if block["type"] != "tool_use" {
                continue;
            }
            let id = block["id"].as_str().unwrap_or("");
            match block["name"].as_str() {
                Some("Bash") => {
                    let command = block["input"]["command"].as_str().unwrap_or("");
                    for words in segments(command) {
                        if git_subcommand(&words).is_some_and(|s| HEAD_MOVERS.contains(&s.as_str()))
                        {
                            last_move = Some(index);
                        }
                        if is_diff_read(&words) {
                            let paths = ok_text(id).map(diff_paths).unwrap_or_default();
                            if !paths.is_empty() {
                                evidence.push((index, Evidence::Diff(paths)));
                            }
                        }
                    }
                }
                Some("Agent" | "Task") => {
                    let brief = format!(
                        "{} {}",
                        block["input"]["prompt"], block["input"]["description"]
                    );
                    if brief.to_lowercase().contains("diff") && ok_text(id).is_some() {
                        evidence.push((index, Evidence::Agent));
                    }
                }
                _ => {}
            }
        }
    }
    let after = |index: usize| last_move.is_none_or(|moved| index > moved);
    let mut files = HashSet::new();
    let mut agent_seen = false;
    let mut any = false;
    for (index, item) in &evidence {
        any = true;
        if !after(*index) {
            continue;
        }
        match item {
            Evidence::Diff(paths) => files.extend(paths.iter().cloned()),
            Evidence::Agent => agent_seen = true,
        }
    }
    let kind = if files.is_empty() && !agent_seen {
        if any {
            "stale"
        } else {
            "none"
        }
    } else {
        "read"
    };
    json!({"kind":kind,"files":files.len(),"agent":agent_seen})
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF3: &str = "diff --git a/a.rs b/a.rs\n+x\ndiff --git a/b.rs b/b.rs\n+y\ndiff --git a/c.rs b/c.rs\n+z\n";

    /// One transcript: each step is a Bash command with its result, or an
    /// Agent call with its prompt and result. `ok` false marks an error.
    enum Step {
        Bash(&'static str, &'static str, bool),
        Agent(&'static str, &'static str, bool),
    }

    fn scan_steps(steps: &[Step]) -> Value {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.jsonl");
        let mut lines = Vec::new();
        for (n, step) in steps.iter().enumerate() {
            let id = format!("t{n}");
            let (name, input, out, ok) = match step {
                Step::Bash(cmd, out, ok) => ("Bash", json!({"command": cmd}), *out, *ok),
                Step::Agent(prompt, out, ok) => ("Agent", json!({"prompt": prompt}), *out, *ok),
            };
            lines.push(json!({"type":"assistant","message":{"content":[
                {"type":"tool_use","id":id,"name":name,"input":input}]}}));
            lines.push(json!({"type":"user","message":{"content":[
                {"type":"tool_result","tool_use_id":id,"is_error":!ok,"content":out}]}}));
        }
        let body: Vec<String> = lines.iter().map(Value::to_string).collect();
        std::fs::write(&path, body.join("\n") + "\n").unwrap();
        scan("claude", Some(&path))
    }

    fn kind(steps: &[Step]) -> String {
        scan_steps(steps)["kind"].as_str().unwrap().to_string()
    }

    /// The PR 512 shape: the skill loads, a stat probe runs, the emit follows.
    #[test]
    fn a_stat_probe_or_no_command_is_no_read() {
        assert_eq!(
            kind(&[
                Step::Bash("git commit -m x", "ok", true),
                Step::Bash("git diff --stat origin/main...HEAD", " a.rs | 2 +-", true),
            ]),
            "none"
        );
        assert_eq!(kind(&[]), "none");
    }

    #[test]
    fn a_diff_whose_result_is_empty_failed_or_headerless_is_no_read() {
        for (out, ok) in [
            ("", true),
            ("fatal: bad revision", false),
            ("no changes", true),
        ] {
            assert_eq!(
                kind(&[Step::Bash("git diff origin/main...HEAD", out, ok)]),
                "none"
            );
        }
        assert_eq!(
            kind(&[Step::Bash(
                "git diff origin/main...HEAD > /dev/null",
                "",
                true
            )]),
            "none"
        );
    }

    #[test]
    fn a_real_diff_counts_and_names_its_files() {
        let scanned = scan_steps(&[
            Step::Bash("git commit -m x", "ok", true),
            Step::Bash(
                "FOO=1 git -C /wt diff origin/main...HEAD | head -400",
                DIFF3,
                true,
            ),
        ]);
        assert_eq!(scanned["kind"], "read");
        assert_eq!(scanned["files"], 3);
    }

    #[test]
    fn show_and_path_limited_diffs_cover_only_the_files_they_print() {
        assert_eq!(
            kind(&[Step::Bash("git show HEAD:README", "# readme", true)]),
            "none"
        );
        let scanned = scan_steps(&[Step::Bash(
            "git diff origin/main...HEAD -- a.rs",
            "diff --git a/a.rs b/a.rs\n+x\n",
            true,
        )]);
        assert_eq!(
            (scanned["kind"].as_str(), scanned["files"].as_u64()),
            (Some("read"), Some(1))
        );
    }

    #[test]
    fn a_read_before_the_last_head_move_is_stale_whatever_the_git_spelling() {
        for mv in [
            "git commit -m fix",
            "git -C /wt commit -m fix",
            "git merge origin/main",
        ] {
            assert_eq!(
                kind(&[
                    Step::Bash("git diff origin/main...HEAD", DIFF3, true),
                    Step::Bash(mv, "ok", true)
                ]),
                "stale",
                "{mv}"
            );
        }
        assert_eq!(
            kind(&[
                Step::Bash("git diff origin/main...HEAD", DIFF3, true),
                Step::Bash("echo git commit", "git commit", true),
            ]),
            "read"
        );
    }

    #[test]
    fn only_a_successful_diff_briefed_agent_call_counts() {
        assert_eq!(
            kind(&[Step::Agent("list the open TODOs", "done", true)]),
            "none"
        );
        assert_eq!(
            kind(&[Step::Agent("review the diff", "boom", false)]),
            "none"
        );
        let scanned = scan_steps(&[Step::Agent("review the diff for bugs", "no findings", true)]);
        assert_eq!(
            (scanned["kind"].as_str(), scanned["agent"].as_bool()),
            (Some("read"), Some(true))
        );
    }

    #[test]
    fn non_claude_harnesses_are_unobserved() {
        assert_eq!(scan("codex", None), json!({"kind":"unobserved"}));
    }
}
