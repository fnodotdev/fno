//! The refresh leaf's consent boundary: enablement reads the HOME-global
//! config only, never the cwd project-config chain. Before this gate, any
//! repo whose project config enabled pr_watch turned `fno update --adopt`
//! run in a fresh HOME into a watcher bootstrap there - the leaf is the
//! update tail, so a bare leaf run stands in for it hermetically: the
//! disabled skip returns before any plist write, bounce or launchctl call.

use std::process::Command;

fn fresh_home(tag: &str) -> std::path::PathBuf {
    let base =
        std::env::temp_dir().join(format!("fno-refresh-gate-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("home")).unwrap();
    std::fs::create_dir_all(base.join("state")).unwrap();
    base
}

/// Run the leaf with HOME pinned to a configless fresh HOME, cwd sitting in
/// a repo whose PROJECT config enables pr_watch, and the global read pinned
/// to a config that stays silent about pr_watch.
fn run_leaf(base: &std::path::Path, cwd: &std::path::Path) -> (String, i32) {
    let global = base.join("global.toml");
    std::fs::write(&global, "schema_version = 1\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agent"))
        .args(["pr-watch", "refresh"])
        .current_dir(cwd)
        .env("HOME", base.join("home"))
        .env("FNO_GLOBAL_SETTINGS_PATH", &global)
        .env("FNO_STATE_DIR", base.join("state"))
        .env("FNO_TEST_LAUNCHD_SUFFIX", "testhome")
        .envs(fno_agent::test_run::self_owner_env())
        .output()
        .expect("pr-watch refresh runs");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn a_project_config_at_cwd_does_not_enable_the_refresh() {
    let base = fresh_home("project-no");
    let cwd = base.join("cwd");
    std::fs::create_dir_all(cwd.join(".fno")).unwrap();
    std::fs::write(
        cwd.join(".fno/config.toml"),
        "[pr_watch]\nenabled = true\ninterval_seconds = 600\n",
    )
    .unwrap();
    let (stdout, code) = run_leaf(&base, &cwd);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("pr-watch: disabled; nothing to refresh."),
        "the project enablement must not authorize the refresh, got: {stdout}"
    );
    assert!(
        !cwd.join("LaunchAgents").exists() && !base.join("home/Library/LaunchAgents").exists(),
        "no LaunchAgents dir may appear"
    );
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn a_global_enable_without_project_config_still_enables_the_refresh() {
    let base = fresh_home("global-yes");
    let cwd = base.join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let global = base.join("global.toml");
    std::fs::write(
        &global,
        "[pr_watch]\nenabled = true\ninterval_seconds = 600\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agent"))
        .args(["pr-watch", "refresh", "--fno-binary", "/usr/local/bin/fno"])
        .current_dir(&cwd)
        .env("HOME", base.join("home"))
        .env("FNO_GLOBAL_SETTINGS_PATH", &global)
        .env("FNO_STATE_DIR", base.join("state"))
        .env("FNO_TEST_LAUNCHD_SUFFIX", "testhome")
        .envs(fno_agent::test_run::self_owner_env())
        .env(
            "FNO_TEST_PR_WATCH_LAUNCH_AGENTS_DIR",
            base.join("LaunchAgents"),
        )
        .env("FNO_TEST_PR_WATCH_TICK_PID", "0")
        .env(
            "FNO_TEST_PR_WATCH_LAUNCHCTL",
            "[[0,false],[0,false],[0,false]]",
        )
        .output()
        .expect("pr-watch refresh runs");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    // Enabled: the leaf proceeds past the gate. With no prior plist the
    // renderer writes one; the bounce runs through the launchctl seam pinned
    // above, so the leaf having passed the gate shows in the refresh output
    // shape and in the absence of the disabled skip line.
    assert!(
        stdout.starts_with("pr-watch refresh:"),
        "the global enable must pass the gate and refresh, got: {stdout}"
    );
    assert!(
        !stdout.contains("pr-watch: disabled; nothing to refresh."),
        "the disabled skip must not appear, got: {stdout}"
    );
    assert!(
        !stdout.contains("pr-watch: disabled; nothing to refresh."),
        "the global enable must pass the gate, got: {stdout}"
    );
    std::fs::remove_dir_all(&base).unwrap();
}
