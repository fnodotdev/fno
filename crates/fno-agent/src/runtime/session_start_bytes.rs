//! `fno-agent session-start-bytes --gate-bytes N [--cwd PATH]` -- the
//! operator's TOTAL session-start byte count, so preamble overload
//! is a number, not a feeling.
//!
//! `--gate-bytes` is required rather than re-derived: `check-preamble-budget.sh`
//! already computes the fno gate figure (A) and this command must not carry a
//! second implementation of that parse. This command owns the other two pieces:
//! the user's global `~/.claude/CLAUDE.md` plus every file its top-level `@`
//! lines import (B), and the current project's memory index (C,
//! `~/.claude/projects/<slug>/memory/MEMORY.md`, the same slug
//! [`crate::claude_ask::claude_cwd_slug`] resolves for the transcript
//! store), and the user rules directory (D, `~/.claude/rules/*.md`, which
//! holds the `fno-decisions.md` copy a decision's `reach` writes). Best-effort:
//! a missing piece counts as zero bytes.

use crate::claude_ask::claude_cwd_slug;
use std::path::PathBuf;

fn user_claude_md_bytes(home: &std::path::Path) -> u64 {
    let claude_md = home.join(".claude").join("CLAUDE.md");
    let Ok(text) = std::fs::read_to_string(&claude_md) else {
        return 0;
    };
    let mut total = text.len() as u64;
    for line in text.lines() {
        let Some(rest) = line.strip_prefix('@') else {
            continue;
        };
        let name = rest.split_whitespace().next().unwrap_or(rest);
        if let Ok(meta) = std::fs::metadata(home.join(".claude").join(name)) {
            total += meta.len();
        }
    }
    total
}

/// Every `.md` file in the user rules directory, which Claude loads whole at
/// session start. The generated `fno-decisions.md` lands here.
fn rules_dir_bytes(home: &std::path::Path) -> u64 {
    let Ok(dir) = std::fs::read_dir(home.join(".claude").join("rules")) else {
        return 0;
    };
    dir.flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// `projects_dir` is the resolved Claude projects base (not read from the
/// environment here, so a test can point it at a fixture without mutating
/// process-wide state another test module's own tests read concurrently).
fn memory_index_bytes(projects_dir: &std::path::Path, cwd: &std::path::Path) -> u64 {
    let index = projects_dir
        .join(claude_cwd_slug(cwd))
        .join("memory")
        .join("MEMORY.md");
    std::fs::metadata(index).map(|m| m.len()).unwrap_or(0)
}

pub fn run_session_start_bytes(args: &[String]) -> i32 {
    let mut gate_bytes: Option<u64> = None;
    let mut cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--gate-bytes" => {
                i += 1;
                match args.get(i).and_then(|v| v.parse::<u64>().ok()) {
                    Some(v) => gate_bytes = Some(v),
                    None => {
                        eprintln!(
                            "fno-agent session-start-bytes: --gate-bytes needs a non-negative integer"
                        );
                        return 2;
                    }
                }
            }
            "--cwd" => {
                i += 1;
                match args.get(i) {
                    Some(p) => cwd = PathBuf::from(p),
                    None => {
                        eprintln!("fno-agent session-start-bytes: --cwd needs a path");
                        return 2;
                    }
                }
            }
            other => {
                eprintln!("fno-agent session-start-bytes: unknown flag {other}");
                return 2;
            }
        }
        i += 1;
    }
    let Some(gate_bytes) = gate_bytes else {
        eprintln!("fno-agent session-start-bytes: --gate-bytes is required");
        return 2;
    };

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let user_bytes = user_claude_md_bytes(&home);
    let memory_bytes = memory_index_bytes(&crate::claude_drive::claude_projects_dir(), &cwd);
    let rules_bytes = rules_dir_bytes(&home);
    let total = gate_bytes + user_bytes + memory_bytes + rules_bytes;

    println!(
        "preamble: fno gate {gate_bytes} B + user CLAUDE.md and imports {user_bytes} B + \
         project memory index {memory_bytes} B + user rules {rules_bytes} B = {total} B (~{} tok per session start)",
        total / 4
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_the_user_claude_md_and_its_import() {
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(home.path().join(".claude")).unwrap();
        std::fs::write(home.path().join(".claude/CLAUDE.md"), "abc\n@RTK.md\n").unwrap();
        std::fs::write(home.path().join(".claude/RTK.md"), "wxyz").unwrap();
        assert_eq!(user_claude_md_bytes(home.path()), 16); // 12 (CLAUDE.md) + 4 (RTK.md)
    }

    #[test]
    fn sums_every_md_file_in_the_user_rules_dir() {
        let home = tempfile::tempdir().expect("tempdir");
        let rules = home.path().join(".claude/rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("fno-decisions.md"), "abcde").unwrap();
        std::fs::write(rules.join("other.md"), "xyz").unwrap();
        std::fs::write(rules.join("note.txt"), "ignored").unwrap();
        assert_eq!(rules_dir_bytes(home.path()), 8);
        assert_eq!(rules_dir_bytes(&home.path().join("none")), 0);
    }

    #[test]
    fn missing_gate_bytes_is_a_usage_error() {
        assert_eq!(run_session_start_bytes(&[]), 2);
    }
}
