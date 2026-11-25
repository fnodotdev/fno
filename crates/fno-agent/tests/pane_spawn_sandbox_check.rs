//! The pane door runs the spawn's first sandbox leg. A required provider
//! refuses a `--as pane` spawn at the real bin's door (exit 2, the sandbox
//! refusal on stderr) before any pane opens; a clean policy admits past it,
//! so the pane door's own refusals answer instead. Both arms end at refusal
//! doors: no mux server, pane or worker ever launches.

#![cfg(unix)]

use std::fs;
use std::process::Command;

/// A fresh scratch root per run: the agents home, the spawn's --cwd and the
/// pinned config all live under it, so no sibling test or machine state
/// leaks in. The env travels in the child only; the parent pins nothing.
fn run_spawn(pin: &str, provider: &str, tag: &str) -> (Option<i32>, String) {
    let root = std::env::temp_dir().join(format!(
        "fno-pane-sandbox-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let home = root.join("home");
    let cwd = root.join("work");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let pin = root.join("config.toml");
    fs::write(&pin, pin).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agent"))
        .args([
            "spawn",
            "--name",
            tag,
            "--as",
            "pane",
            "--provider",
            provider,
            "--cwd",
            cwd.to_str().unwrap(),
            "seed text",
        ])
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("FNO_AGENT_HOME", &home)
        .env("FNO_CONFIG", &pin)
        .envs(fno_agent::test_run::self_owner_env())
        .output()
        .unwrap();
    let code = out.status.code();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = fs::remove_dir_all(&root);
    (code, stderr)
}

#[test]
fn a_required_provider_refuses_the_pane_spawn_at_the_sandbox_door() {
    let (code, stderr) = run_spawn("[sandbox]\nprovider = \"docker\"\n", "codex", "refused");
    assert_eq!(code, Some(2), "stderr: {stderr}");
    assert!(
        stderr.contains("fno-agent spawn: [sandbox]"),
        "the sandbox door must refuse the pane spawn: {stderr}"
    );
    assert!(
        !stderr.contains("no pane arm"),
        "the sandbox door must fire before the pane arm door: {stderr}"
    );
}

#[test]
fn a_clean_policy_admits_the_pane_spawn_past_the_sandbox_door() {
    let (code, stderr) = run_spawn("schema_version = 1\n", "gemini", "admitted");
    assert_eq!(code, Some(2), "stderr: {stderr}");
    assert!(
        stderr.contains("no pane arm"),
        "the gemini pane refusal must answer a clean policy: {stderr}"
    );
    assert!(
        !stderr.contains("[sandbox]"),
        "the sandbox door must not refuse a clean policy: {stderr}"
    );
}
