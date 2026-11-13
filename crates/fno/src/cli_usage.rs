//! The `cli_command` row: one per command the `fno` front door runs, with
//! the command path, the exit code and the wall time. The usage events arm
//! ships it (docs/telemetry.md).
//!
//! The path is menu words only. A root word must be a live row of
//! `command-names.tsv`, and a longer path must be a line of
//! `cli-usage-paths.txt`, the command paths the committed inventories name.
//! The first word that makes no known path ends it, so an argument (a node
//! id, a search term, a message) is never written, even one that happens to
//! be a command word.
//!
//! A command the front door answers in-process writes its row in
//! [`exit`]. A command it execs into `fno-agents` cannot come back, so the
//! path and start time ride the exec's environment and `fno-agents` writes
//! the row at its own exit.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;

const PATHS: &str = include_str!("../cli-usage-paths.txt");
const MAX_WORDS: usize = 3;
/// The command path an exec hands to `fno-agents`.
pub const PATH_ENV: &str = "FNO_CLI_PATH";
/// The front door's start, in Unix ms, an exec hands to `fno-agents`.
pub const START_ENV: &str = "FNO_CLI_START_MS";

static STARTED: OnceLock<(String, i64)> = OnceLock::new();

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn paths() -> HashSet<&'static str> {
    PATHS
        .lines()
        .map(str::trim)
        .filter(|p| !p.is_empty() && !p.starts_with('#'))
        .collect()
}

/// The command path `args` names, or None when the root is not a live name.
pub fn command_path(args: &[OsString]) -> Option<String> {
    let mut tokens = args
        .iter()
        .map(|a| a.to_str())
        .skip_while(|a| matches!(a, Some("-J" | "--json")));
    let root = tokens.next()??;
    if !crate::command_names::live_roots()
        .iter()
        .any(|r| *r == root)
    {
        return None;
    }
    let known = paths();
    let mut path = root.to_string();
    for token in tokens.take(MAX_WORDS - 1) {
        let Some(word) = token else { break };
        let longer = format!("{path} {word}");
        if !known.contains(longer.as_str()) {
            break;
        }
        path = longer;
    }
    Some(path)
}

/// Note the command at the front door's start. `main` calls this once.
pub fn start(args: &[OsString]) {
    if let Some(path) = command_path(args) {
        if STARTED.set((path, now_ms())).is_err() {
            eprintln!("cli usage: the command start was already noted");
        }
    }
}

/// The environment an exec into `fno-agents` carries, so the row is written
/// at that process's exit.
pub fn exec_env() -> Vec<(&'static str, String)> {
    match STARTED.get() {
        Some((path, start)) => vec![(PATH_ENV, path.clone()), (START_ENV, start.to_string())],
        None => Vec::new(),
    }
}

/// The agents journal, or None for a hermetic test run that pinned no
/// agents home: a test must never write a row into a real home. Shared with
/// the `ui_click` row's writer ([`crate::ui_usage`]).
pub(crate) fn agents_journal() -> Option<PathBuf> {
    let hermetic = std::env::var("FNO_TEST_HERMETIC").as_deref() == Ok("1")
        || std::env::var_os("PYTEST_CURRENT_TEST").is_some();
    let home = match std::env::var_os("FNO_AGENTS_HOME").filter(|v| !v.is_empty()) {
        Some(declared) => PathBuf::from(declared),
        None if hermetic => return None,
        None => PathBuf::from(std::env::var_os("HOME").filter(|h| !h.is_empty())?)
            .join(".fno")
            .join("agents"),
    };
    Some(home.join("events.jsonl"))
}

/// The row for a command that ran `duration_ms` and exited `code`.
pub fn envelope(command: &str, code: i32, duration_ms: i64) -> serde_json::Value {
    serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "type": "cli_command",
        "source": "cli",
        "data": {"command": command, "exit": code, "duration_ms": duration_ms},
    })
}

/// Write the row for the noted command, then exit with `code`. A command
/// that never noted a start (no live root) exits with no row.
pub fn exit(code: i32) -> ! {
    if let (Some((path, start)), Some(journal)) = (STARTED.get(), agents_journal()) {
        let row = envelope(path, code, (now_ms() - start).max(0));
        if let Err(e) = crate::event_store::append_envelope(&journal, &row.to_string(), None) {
            eprintln!("cli usage: the cli_command row did not land: {e}");
        }
    }
    std::process::exit(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(args: &[&str]) -> Option<String> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        command_path(&args)
    }

    /// Known command paths ship and the first argument ends the path: a node
    /// id, a search term or a message never rides in it. Every word of every
    /// listed path is a word the committed command inventories name.
    #[test]
    fn the_path_is_a_known_command_path_and_every_path_is_in_the_inventories() {
        assert_eq!(
            path(&["backlog", "get", "x-aaaa"]).as_deref(),
            Some("backlog get")
        );
        assert_eq!(
            path(&["-J", "do", "pr", "merge", "141"]).as_deref(),
            Some("do pr merge")
        );
        assert_eq!(
            path(&["backlog", "find", "secret words"]).as_deref(),
            Some("backlog find")
        );
        assert_eq!(
            path(&["mail", "send", "margit", "hi"]).as_deref(),
            Some("mail send")
        );
        assert_eq!(path(&["backlog", "x-aaaa"]).as_deref(), Some("backlog"));
        // A command word inside free text ships nothing: no known path holds it.
        assert_eq!(
            path(&["backlog", "idea", "review", "the", "cache"]).as_deref(),
            Some("backlog idea")
        );
        assert_eq!(path(&["not-a-root", "get"]), None);
        assert_eq!(path(&[]), None);

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut inventory = String::new();
        for rel in [
            "scripts/ci/command-baseline.txt",
            "scripts/ci/native-command-tree.txt",
            "scripts/ci/command-collapse-map.tsv",
            "crates/fno/command-names.tsv",
            "crates/fno-agents/src/backlog/commands.rs",
        ] {
            inventory.push_str(&std::fs::read_to_string(root.join(rel)).unwrap());
            inventory.push('\n');
        }
        let named: HashSet<&str> = inventory
            .split(|c: char| !(c.is_ascii_lowercase() || c == '-'))
            .collect();
        for listed in paths() {
            for word in listed.split(' ') {
                assert!(
                    named.contains(word),
                    "{listed}: {word} is in no command inventory"
                );
            }
        }
    }
}
