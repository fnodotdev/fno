//! Every `fno-agents` command the pr-watch sweep shells out to must still
//! dispatch. The sweep calls its own binary through `clock::door`, so a command
//! folded into another one fails at runtime ("unknown command") and is only
//! noticed in the watcher's error log. This test reads the literal command
//! heads out of the watcher source and runs each one.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn watcher_sources() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gate/pr_watch");
    std::fs::read_dir(&dir)
        .expect("pr_watch dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect()
}

/// The first string literal of every `clock::door(&[...` call: the command head.
fn door_heads(src: &str) -> BTreeSet<String> {
    let mut heads = BTreeSet::new();
    for marker in ["clock::door(", "clock::door_raw("] {
        for (at, _) in src.match_indices(marker) {
            let rest = &src[at + marker.len()..];
            let Some(open) = rest.find("&[") else {
                continue;
            };
            let Some(close) = rest[open..].find(']') else {
                continue;
            };
            let list = &rest[open + 2..open + close];
            if let Some(start) = list.find('"') {
                if let Some(len) = list[start + 1..].find('"') {
                    heads.insert(list[start + 1..start + 1 + len].to_string());
                }
            }
        }
    }
    heads
}

fn sandboxed(args: &[&str]) -> Command {
    let dir = tempfile::tempdir().expect("tempdir").into_path();
    let config = dir.join("config.toml");
    std::fs::write(&config, format!("state_dir = \"{}\"", dir.display())).expect("config");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
    cmd.args(args)
        .env("FNO_CONFIG", &config)
        .env("FNO_HOME", &dir)
        .env("FNO_STATE_DIR", &dir)
        .env("FNO_GLOBAL_SETTINGS_PATH", "/dev/null")
        .env("FNO_TRACKER_BACKEND", "graph")
        .envs(fno_agents::test_run::self_owner_env())
        .stdin(Stdio::null());
    cmd
}

#[test]
fn every_command_the_watcher_calls_still_dispatches() {
    let mut heads = BTreeSet::new();
    for path in watcher_sources() {
        heads.extend(door_heads(
            &std::fs::read_to_string(path).expect("read source"),
        ));
    }
    assert!(heads.len() > 5, "found too few door heads: {heads:?}");
    for head in &heads {
        let out = sandboxed(&[head]).output().expect("run fno-agents");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !err.contains("unknown command"),
            "pr-watch calls `fno-agents {head}` but the binary rejects it: {err}"
        );
    }
}

#[test]
fn the_draft_flip_door_answers_through_backlog_get() {
    use std::io::Write;
    let mut child = sandboxed(&["backlog", "get"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(br#"{"pr_draft_ready":{}}"#)
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("pr_draft_ready needs"));
}
