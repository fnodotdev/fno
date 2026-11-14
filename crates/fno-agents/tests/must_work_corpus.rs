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

fn refusal(command: &str, preset: &str) -> Option<String> {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("home dir");
    let payload = json!({"tool_name": "Bash", "cwd": repo, "tool_input": {"command": command}});
    let mut child = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .args(["hook", "pretooluse-bash"])
        .current_dir(&repo)
        .envs(fno_agents::test_run::self_owner_env())
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
        .expect("fno-agents hook process");
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
