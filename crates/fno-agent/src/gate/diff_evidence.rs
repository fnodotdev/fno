//! Did the attesting session read the change it attests? The emit refuses a
//! clean `code-review` pass with no such evidence. Evidence comes from the
//! session's own Claude transcript, never from what the caller says about
//! itself, and only from tool results that succeeded:
//!
//! - diff output (`diff --git` or `+++ b/` header lines in any result, so a
//!   diff piped through `head`, or saved and read back, names its files),
//! - a `Read` call, or a `sed`/`cat`/`head`/`tail` call that only reads, on
//!   a file,
//! - a review subagent call whose brief names the diff.
//!
//! Evidence counts only after the session's last review Skill call. A session
//! with no such call has no review window and no evidence. The emit decides
//! whether each file's evidence is still fresh against the git tree.

use crate::harness::{AsHarness, Harness};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

/// Shell readers whose file arguments count as read.
const READERS: [&str; 6] = ["sed", "cat", "head", "tail", "nl", "bat"];

/// One piece of evidence: the record index, the file, the transcript time.
type Evidence = (usize, String, String);

/// The string at a JSON pointer, or "".
fn text_at<'a>(v: &'a Value, pointer: &str) -> &'a str {
    v.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}

fn segments(command: &str) -> impl Iterator<Item = Vec<&str>> {
    command
        .split(['\n', ';', '|', '&'])
        .map(|s| s.split_whitespace().collect::<Vec<_>>())
        .filter(|w| !w.is_empty())
}

/// The text of one tool result, or none when it failed or printed nothing.
fn ok_text(block: &Value) -> Option<String> {
    if block.get("is_error").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let text = match block.get("content")? {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
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

/// File arguments of one shell reader segment. A segment that writes
/// (`sed -i`, a `>` redirect) reads nothing.
fn reader_paths(words: &[&str]) -> Vec<String> {
    if !words.first().is_some_and(|w| READERS.contains(w)) {
        return Vec::new();
    }
    let writes = words
        .iter()
        .any(|w| w.starts_with('>') || w.starts_with("-i") || *w == "--in-place");
    if writes {
        return Vec::new();
    }
    words
        .iter()
        .skip(1)
        .map(|w| w.trim_matches(['\'', '"']))
        .filter(|w| !w.starts_with('-') && (w.contains('/') || w.contains('.')))
        .map(str::to_string)
        .collect()
}

fn blocks(record: &Value) -> Vec<Value> {
    record
        .pointer("/message/content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Successful tool results keyed by tool_use id.
fn results_by_id(records: &[Value]) -> HashMap<String, Value> {
    let mut results = HashMap::new();
    for block in records.iter().flat_map(blocks) {
        if text_at(&block, "/type") == "tool_result" {
            results.insert(text_at(&block, "/tool_use_id").to_string(), block);
        }
    }
    results
}

/// The files one tool call read, judged by its own successful result.
fn files_read(block: &Value, result: &str) -> Vec<String> {
    let mut found = Vec::new();
    match text_at(block, "/name") {
        "Read" => {
            found.push(text_at(block, "/input/file_path").to_string());
            found.extend(header_paths(result));
        }
        "Bash" => {
            found.extend(header_paths(result));
            for words in segments(text_at(block, "/input/command")) {
                found.extend(reader_paths(&words));
            }
        }
        _ => {}
    }
    found.retain(|p| !p.is_empty());
    found
}

/// What the session did: the window start, its evidence, its agent calls.
fn collect(records: &[Value]) -> (Option<usize>, Vec<Evidence>, Vec<usize>) {
    let results = results_by_id(records);
    let (mut window, mut evidence, mut agents) = (None, Vec::new(), Vec::new());
    for (index, record) in records.iter().enumerate() {
        let at = text_at(record, "/timestamp");
        for block in blocks(record) {
            if text_at(&block, "/type") != "tool_use" {
                continue;
            }
            let result = results.get(text_at(&block, "/id")).and_then(ok_text);
            match text_at(&block, "/name") {
                "Skill"
                    if text_at(&block, "/input/skill")
                        .to_lowercase()
                        .contains("review") =>
                {
                    window = Some(index);
                }
                "Agent" | "Task" => {
                    let brief = format!(
                        "{} {}",
                        text_at(&block, "/input/prompt"),
                        text_at(&block, "/input/description")
                    );
                    if brief.to_lowercase().contains("diff") && result.is_some() {
                        agents.push(index);
                    }
                }
                _ => {
                    let files = result.map(|r| files_read(&block, &r)).unwrap_or_default();
                    evidence.extend(files.into_iter().map(|p| (index, p, at.to_string())));
                }
            }
        }
    }
    (window, evidence, agents)
}

/// `{"kind": observed|unobserved, "window": bool, "agent": bool,
/// "covered": [{"path", "at"}]}`. `at` is the transcript time of the latest
/// evidence for that path.
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
    let (window, evidence, agents) = collect(&records);
    let Some(start) = window else {
        return json!({"kind":"observed","window":false,"agent":false,"covered":[]});
    };
    let mut latest: Vec<(String, String)> = Vec::new();
    for (_, file, at) in evidence.into_iter().filter(|e| e.0 >= start) {
        match latest.iter_mut().find(|e| e.0 == file) {
            Some(slot) => slot.1 = at,
            None => latest.push((file, at)),
        }
    }
    let covered: Vec<Value> = latest
        .into_iter()
        .map(|(file, at)| json!({"path": file, "at": at}))
        .collect();
    let agent_seen = agents.iter().any(|i| *i >= start);
    json!({"kind":"observed","window":true,"agent":agent_seen,"covered":covered})
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

    fn paths_of(scanned: &Value) -> Vec<String> {
        let all = scanned.get("covered").and_then(Value::as_array).unwrap();
        all.iter()
            .map(|v| text_at(v, "/path").to_string())
            .collect()
    }

    /// Paths covered after a review Skill call opens the window.
    fn covered(steps: &[Step]) -> Vec<String> {
        let mut all = vec![skill()];
        all.extend(steps.iter().cloned());
        paths_of(&scan_steps(&all))
    }

    fn agent_seen(prompt: &'static str, ok: bool) -> Option<bool> {
        let call = ("Agent", json!({"prompt": prompt}), "findings", ok);
        scan_steps(&[skill(), call])
            .get("agent")
            .and_then(Value::as_bool)
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
    }

    #[test]
    fn a_saved_diff_read_back_with_read_or_a_shell_reader_covers_its_files() {
        let saved = bash("git diff origin/main...HEAD > /tmp/review-diff.txt", "ok");
        let gutter: Step = (
            "Read",
            json!({"file_path": "/tmp/review-diff.txt"}),
            "     1→diff --git a/q.rs b/q.rs\n     2→+x",
            true,
        );
        assert!(covered(&[saved.clone(), gutter]).contains(&"q.rs".to_string()));
        let sed = bash("sed -n '1,40p' /tmp/review-diff.txt", "+++ b/z.rs\n+x");
        assert!(covered(&[saved, sed]).contains(&"z.rs".to_string()));
    }

    #[test]
    fn a_read_or_a_reading_shell_command_covers_the_file_it_names() {
        let sed = bash("sed -n '1195,1235p' crates/a/src/lib.rs", "fn f() {}");
        assert_eq!(covered(&[sed]), ["crates/a/src/lib.rs"]);
        assert_eq!(
            covered(&[read("/wt/crates/a/src/lib.rs")]),
            ["/wt/crates/a/src/lib.rs"]
        );
        assert!(covered(&[bash("rg -n foo crates/a/src/lib.rs", "x")]).is_empty());
    }

    #[test]
    fn a_writing_shell_command_reads_nothing() {
        assert!(covered(&[bash("sed -i 's/a/b/' crates/a/src/lib.rs", "x")]).is_empty());
        assert!(covered(&[bash("cat > crates/a/src/lib.rs", "x")]).is_empty());
        assert!(covered(&[bash("cat in.txt >> crates/a/src/lib.rs", "x")]).is_empty());
    }

    #[test]
    fn evidence_before_the_review_skill_call_or_with_no_skill_call_does_not_count() {
        let early = scan_steps(&[read("/wt/a.rs"), skill(), read("/wt/b.rs")]);
        assert_eq!(paths_of(&early), ["/wt/b.rs"]);
        let none = scan_steps(&[read("/wt/a.rs")]);
        assert_eq!(none.get("window").and_then(Value::as_bool), Some(false));
        assert!(paths_of(&none).is_empty());
    }

    #[test]
    fn only_a_successful_diff_briefed_agent_call_counts() {
        assert_eq!(agent_seen("list the open TODOs", true), Some(false));
        assert_eq!(agent_seen("review the diff", false), Some(false));
        assert_eq!(agent_seen("review the diff for bugs", true), Some(true));
    }

    #[test]
    fn non_claude_harnesses_are_unobserved() {
        assert_eq!(scan("codex", None), json!({"kind":"unobserved"}));
    }
}
