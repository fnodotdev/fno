//! Did the attesting session read the change it attests? The emit refuses a
//! clean `code-review` pass with no such evidence. Evidence comes from the
//! session's own Claude transcript, never from what the caller says about
//! itself, and only from tool results that succeeded:
//!
//! - diff output (`diff --git` or `+++ b/` header lines in any result, so a
//!   diff piped through `head`, saved and read back, or printed by `git show`
//!   names the files it covers),
//! - a `Read` call, or a `sed`/`cat`/`head`/`tail` call, on a file,
//! - a review subagent call whose brief names the diff.
//!
//! Evidence counts inside the review window (from the last review Skill call)
//! and after the last edit to its file. An edit does not void evidence when
//! the edits since it total under 100 lines, which is the interdiff a fix
//! commit may carry. A commit, a merge or a rebase voids nothing.

use crate::harness::{AsHarness, Harness};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Edit lines after a read that an earlier read still carries across.
const CARRY_LINES: usize = 100;

/// Shell readers whose file arguments count as read.
const READERS: [&str; 6] = ["sed", "cat", "head", "tail", "nl", "bat"];

fn segments(command: &str) -> impl Iterator<Item = Vec<&str>> {
    command
        .split(['\n', ';', '|', '&'])
        .map(|s| s.split_whitespace().collect::<Vec<_>>())
        .filter(|w| !w.is_empty())
}

fn same_file(a: &str, b: &str) -> bool {
    a == b || a.ends_with(&format!("/{b}")) || b.ends_with(&format!("/{a}"))
}

/// The text of one tool result, or none when it failed or printed nothing.
fn ok_text(block: &Value) -> Option<String> {
    if block["is_error"].as_bool().unwrap_or(false) {
        return None;
    }
    let text = match &block["content"] {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    (!text.trim().is_empty()).then_some(text)
}

/// Files named by diff header lines in one result. A `cat -n` gutter before
/// the header (digits, then a tab or arrow) is skipped.
fn header_paths(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let digits = trimmed.trim_start_matches(|c: char| c.is_ascii_digit());
        let body = if digits.len() < trimmed.len() {
            digits.trim_start_matches(['→', '\t', ' '])
        } else {
            trimmed
        };
        if let Some(rest) = body.strip_prefix("diff --git ") {
            if let Some(last) = rest.split_whitespace().last() {
                out.push(last.strip_prefix("b/").unwrap_or(last).to_string());
            }
        } else if let Some(rest) = body.strip_prefix("+++ b/") {
            out.push(rest.split('\t').next().unwrap_or(rest).trim().to_string());
        }
    }
    out
}

/// File arguments of one shell reader segment.
fn reader_paths(words: &[&str]) -> Vec<String> {
    if !words.first().is_some_and(|w| READERS.contains(w)) {
        return Vec::new();
    }
    words[1..]
        .iter()
        .map(|w| w.trim_matches(['\'', '"']))
        .filter(|w| !w.starts_with('-') && (w.contains('/') || w.contains('.')))
        .map(str::to_string)
        .collect()
}

struct Edit {
    index: usize,
    path: String,
    lines: usize,
}

fn edit_of(index: usize, block: &Value) -> Option<Edit> {
    let input = &block["input"];
    let path = input["file_path"]
        .as_str()
        .or_else(|| input["notebook_path"].as_str())?
        .to_string();
    let count = |v: &Value| v.as_str().map_or(0, |s| s.lines().count());
    let lines = match block["name"].as_str()? {
        "Edit" => count(&input["old_string"]).max(count(&input["new_string"])),
        "Write" => count(&input["content"]),
        "MultiEdit" => input["edits"].as_array().map_or(0, |edits| {
            edits.iter().map(|e| count(&e["new_string"])).sum::<usize>()
        }),
        "NotebookEdit" => count(&input["new_source"]),
        _ => return None,
    };
    Some(Edit { index, path, lines })
}

/// `{"kind": observed|unobserved, "covered": [paths], "agent": bool}`.
/// `covered` lists every file the session read inside the review window and
/// after its last edit (or within the carry budget of it).
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
    let blocks = |record: &Value| record["message"]["content"].as_array().cloned();
    let mut results: HashMap<String, Value> = HashMap::new();
    for record in &records {
        for block in blocks(record).into_iter().flatten() {
            if block["type"] == "tool_result" {
                if let Some(id) = block["tool_use_id"].as_str() {
                    results.insert(id.to_string(), block.clone());
                }
            }
        }
    }
    let mut window = 0;
    let mut edits: Vec<Edit> = Vec::new();
    let mut evidence: Vec<(usize, String)> = Vec::new();
    let mut agent_calls: Vec<usize> = Vec::new();
    for (index, record) in records.iter().enumerate() {
        for block in blocks(record).into_iter().flatten() {
            if block["type"] != "tool_use" {
                continue;
            }
            let id = block["id"].as_str().unwrap_or("");
            let result = results.get(id).and_then(ok_text);
            let name = block["name"].as_str().unwrap_or("");
            if let Some(edit) = edit_of(index, &block) {
                edits.push(edit);
                continue;
            }
            match name {
                "Skill" => {
                    let skill = block["input"]["skill"].as_str().unwrap_or("");
                    if skill.to_lowercase().contains("review") {
                        window = index;
                    }
                }
                "Read" => {
                    if let (Some(path), Some(_)) = (block["input"]["file_path"].as_str(), &result) {
                        evidence.push((index, path.to_string()));
                    }
                }
                "Bash" => {
                    let Some(out) = &result else { continue };
                    evidence.extend(header_paths(out).into_iter().map(|p| (index, p)));
                    let command = block["input"]["command"].as_str().unwrap_or("");
                    for words in segments(command) {
                        evidence.extend(reader_paths(&words).into_iter().map(|p| (index, p)));
                    }
                }
                "Agent" | "Task" => {
                    let brief = format!(
                        "{} {}",
                        block["input"]["prompt"], block["input"]["description"]
                    );
                    if brief.to_lowercase().contains("diff") && result.is_some() {
                        agent_calls.push(index);
                    }
                }
                _ => {}
            }
        }
    }
    let valid = |index: usize, path: &str| {
        let later: Vec<&Edit> = edits.iter().filter(|e| e.index > index).collect();
        !later.iter().any(|e| same_file(&e.path, path))
            || later.iter().map(|e| e.lines).sum::<usize>() < CARRY_LINES
    };
    let mut covered = Vec::new();
    let mut seen = HashSet::new();
    for (index, path) in &evidence {
        if *index >= window && valid(*index, path) && seen.insert(path.clone()) {
            covered.push(path.clone());
        }
    }
    let agent_seen = agent_calls.iter().any(|i| *i >= window);
    json!({"kind":"observed","covered":covered,"agent":agent_seen})
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF3: &str = "diff --git a/a.rs b/a.rs\n+x\ndiff --git a/b.rs b/b.rs\n+y\ndiff --git a/c.rs b/c.rs\n+z\n";

    /// A transcript of (tool name, input, result text, ok) steps.
    type Step = (&'static str, Value, &'static str, bool);

    fn bash(cmd: &'static str, out: &'static str) -> Step {
        ("Bash", json!({"command": cmd}), out, true)
    }

    fn read(path: &'static str) -> Step {
        ("Read", json!({"file_path": path}), "1\tfn main() {}", true)
    }

    fn skill() -> Step {
        ("Skill", json!({"skill": "fno:review"}), "loaded", true)
    }

    fn edit(path: &'static str, lines: usize) -> Step {
        let body = "x\n".repeat(lines);
        (
            "Write",
            json!({"file_path": path, "content": body}),
            "ok",
            true,
        )
    }

    fn scan_steps(steps: &[Step]) -> Value {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.jsonl");
        let mut lines = Vec::new();
        for (n, (name, input, out, ok)) in steps.iter().enumerate() {
            let id = format!("t{n}");
            lines.push(json!({"type":"assistant","message":{"content":[
                {"type":"tool_use","id":id,"name":name,"input":input}]}}));
            lines.push(json!({"type":"user","message":{"content":[
                {"type":"tool_result","tool_use_id":id,"is_error":!ok,"content":out}]}}));
        }
        let body: Vec<String> = lines.iter().map(Value::to_string).collect();
        std::fs::write(&path, body.join("\n") + "\n").unwrap();
        scan("claude", Some(&path))
    }

    fn covered(steps: &[Step]) -> Vec<String> {
        scan_steps(steps)["covered"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn stat_probes_failed_and_empty_commands_cover_nothing() {
        assert!(covered(&[bash("git diff --stat origin/main...HEAD", " a.rs | 2 +-")]).is_empty());
        assert!(covered(&[]).is_empty());
        let failed: Step = (
            "Bash",
            json!({"command": "git diff origin/main...HEAD"}),
            DIFF3,
            false,
        );
        assert!(covered(&[failed]).is_empty());
        assert!(covered(&[bash("git diff origin/main...HEAD > /dev/null", "")]).is_empty());
        assert!(covered(&[bash("git show HEAD:README", "# readme")]).is_empty());
    }

    #[test]
    fn diff_output_names_its_files_however_it_was_filtered_or_replayed() {
        let head = "FOO=1 git -C /wt diff origin/main...HEAD | head -400";
        assert_eq!(covered(&[bash(head, DIFF3)]), ["a.rs", "b.rs", "c.rs"]);
        let filtered = "git diff origin/main...HEAD -- w.rs | rg '^[+-]'";
        let out = "--- a/w.rs\n+++ b/w.rs\n+use x;";
        assert_eq!(covered(&[bash(filtered, out)]), ["w.rs"]);
        let saved = bash("git diff origin/main...HEAD > /tmp/review-diff.txt", "ok");
        let gutter: Step = (
            "Read",
            json!({"file_path": "/tmp/review-diff.txt"}),
            "     1→diff --git a/q.rs b/q.rs\n     2→+x",
            true,
        );
        assert!(covered(&[saved, gutter]).contains(&"q.rs".to_string()));
    }

    #[test]
    fn a_read_or_a_shell_reader_covers_the_file_it_names() {
        let sed = bash("sed -n '1195,1235p' crates/a/src/lib.rs", "fn f() {}");
        assert_eq!(covered(&[sed]), ["crates/a/src/lib.rs"]);
        assert_eq!(
            covered(&[read("/wt/crates/a/src/lib.rs")]),
            ["/wt/crates/a/src/lib.rs"]
        );
        assert!(covered(&[bash("rg -n foo crates/a/src/lib.rs", "x")]).is_empty());
    }

    #[test]
    fn only_evidence_inside_the_review_window_counts() {
        let steps = [read("/wt/a.rs"), skill(), read("/wt/b.rs")];
        assert_eq!(covered(&steps), ["/wt/b.rs"]);
    }

    #[test]
    fn an_edit_voids_earlier_evidence_only_past_the_carry_budget() {
        assert!(covered(&[read("/wt/a.rs"), edit("/wt/a.rs", 150)]).is_empty());
        assert_eq!(
            covered(&[read("/wt/a.rs"), edit("/wt/a.rs", 5)]),
            ["/wt/a.rs"]
        );
        assert_eq!(
            covered(&[read("/wt/a.rs"), edit("/wt/b.rs", 150)]),
            ["/wt/a.rs"]
        );
    }

    #[test]
    fn a_commit_or_a_merge_voids_nothing() {
        let steps = [
            bash("git diff origin/main...HEAD", DIFF3),
            bash("git -C /wt commit -m fix", "ok"),
            bash("git merge origin/main", "ok"),
        ];
        assert_eq!(covered(&steps).len(), 3);
    }

    #[test]
    fn only_a_successful_diff_briefed_agent_call_counts() {
        let call = |prompt: &'static str, ok: bool| -> Step {
            ("Agent", json!({"prompt": prompt}), "findings", ok)
        };
        assert_eq!(
            scan_steps(&[call("list the open TODOs", true)])["agent"],
            false
        );
        assert_eq!(
            scan_steps(&[call("review the diff", false)])["agent"],
            false
        );
        assert_eq!(
            scan_steps(&[call("review the diff for bugs", true)])["agent"],
            true
        );
    }

    #[test]
    fn non_claude_harnesses_are_unobserved() {
        assert_eq!(scan("codex", None), json!({"kind":"unobserved"}));
    }
}
