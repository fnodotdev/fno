//! Every must-work command in fno's must-enable list passes the built Bash
//! guard chain under both presets. The list lives in the fno crate; this
//! test reads only the hook's stdout, so a refusal shape of deny, ask or
//! block fails it the same way.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::json;

const TABLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../fno/src/setup/must_enable.toml"
));

/// The spawn-side vendored copy `auto_approve` ships. Byte-identical to the
/// canonical table, pinned below, so a drift can never land quietly.
const VENDORED: &str = include_str!("../src/spawn/must_enable.toml");

/// The rows of the vendored table, rendered the way `auto_approve`
/// renders them for a spawn's `permissions.allow`.
fn vendored_rows() -> Vec<String> {
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Word {
        One(String),
        Any(Vec<String>),
    }
    #[derive(serde::Deserialize)]
    struct Row {
        prefix: Vec<Word>,
    }
    #[derive(serde::Deserialize)]
    struct RuleTable {
        #[serde(default)]
        rule: Vec<Row>,
    }
    let table: RuleTable = toml::from_str(VENDORED).expect("the vendored table parses");
    table
        .rule
        .iter()
        .flat_map(|row| {
            let mut out: Vec<Vec<String>> = vec![Vec::new()];
            for word in &row.prefix {
                let alts: Vec<&String> = match word {
                    Word::One(w) => vec![w],
                    Word::Any(ws) => ws.iter().collect(),
                };
                out = out
                    .iter()
                    .flat_map(|head| {
                        alts.iter().map(move |w| {
                            let mut next = head.clone();
                            next.push((*w).clone());
                            next
                        })
                    })
                    .collect();
            }
            out
        })
        .map(|words| format!("Bash({} *)", words.join(" ")))
        .collect()
}

fn row_admits(rule: &str, seg: &[&str]) -> bool {
    let Some(prefix) = rule
        .strip_prefix("Bash(")
        .and_then(|r| r.strip_suffix(" *)"))
    else {
        return false;
    };
    let cmd = seg.join(" ");
    cmd == prefix || cmd.starts_with(&format!("{prefix} "))
}

/// The plain-word segments of a compound command, split where all three
/// harnesses split one: `&&`, `||`, `;` and `|` (crate fno's `segments`).
fn segments(command: &str) -> Vec<Vec<&str>> {
    command
        .split("&&")
        .flat_map(|s| s.split("||"))
        .flat_map(|s| s.split(';'))
        .flat_map(|s| s.split('|'))
        .map(|s| s.split_whitespace().collect::<Vec<_>>())
        .filter(|w| !w.is_empty())
        .collect()
}

fn refusal(command: &str, preset: &str) -> Option<String> {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("home dir");
    let payload = json!({"tool_name": "Bash", "cwd": repo, "tool_input": {"command": command}});
    let mut child = Command::new(env!("CARGO_BIN_EXE_fno-agent"))
        .args(["hook", "pretooluse-bash"])
        .current_dir(&repo)
        .envs(fno_agent::test_run::self_owner_env())
        .env("CLAUDECODE", "1")
        .env("FNO_REPO_ROOT", &repo)
        .env("FNO_EVENTS_PATH", temp.path().join("events.jsonl"))
        .env("FNO_GUARD_PRESET", preset)
        .env("HOME", &home)
        .env("CARGO_HOME", home.join(".cargo"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("fno-agent hook process");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.to_string().as_bytes())
        .expect("write payload");
    let out = child.wait_with_output().expect("hook output");
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        return Some(format!(
            "exit {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let Ok(v) = serde_json::from_str::<HookOut>(text.trim()) else {
        return None;
    };
    let decision = v.hook_specific_output.permission_decision.as_str();
    if v.decision == "block" || matches!(decision, "deny" | "ask") {
        return Some(text.into_owned());
    }
    None
}

#[derive(serde::Deserialize, Default)]
struct HookOut {
    #[serde(default)]
    decision: String,
    #[serde(default, rename = "hookSpecificOutput")]
    hook_specific_output: HookSpecific,
}

#[derive(serde::Deserialize, Default)]
struct HookSpecific {
    #[serde(default, rename = "permissionDecision")]
    permission_decision: String,
}

#[derive(serde::Deserialize)]
struct Table {
    corpus: Vec<String>,
}

#[test]
fn every_must_work_command_passes_the_bash_guards_under_both_presets() {
    let corpus = toml::from_str::<Table>(TABLE)
        .expect("must_enable.toml parses")
        .corpus;
    assert!(corpus.len() > 10, "the corpus shrank to {}", corpus.len());
    // The vendored spawn copy pins byte-identical to crate fno's canonical
    // table: a drift here breaks the rules the workers launch with.
    assert_eq!(
        TABLE, VENDORED,
        "the vendored must-enable copy drifted from crates/fno/src/setup/must_enable.toml"
    );
    // The spawn renderer admits every corpus command: the same contract
    // crate fno's renderer test pins, held against this side's copy.
    let rows = vendored_rows();
    for command in &corpus {
        for seg in segments(command) {
            assert!(
                rows.iter().any(|r| row_admits(r, &seg)),
                "no vendored row admits `{command}` segment `{}`",
                seg.join(" ")
            );
        }
    }
    let mut refused = Vec::new();
    for preset in ["standard", "strict"] {
        for command in &corpus {
            if let Some(why) = refusal(command, preset) {
                refused.push(format!("{preset}: `{command}`: {why}"));
            }
        }
    }
    assert!(
        refused.is_empty(),
        "refused must-work commands:\n{}",
        refused.join("\n")
    );
}
