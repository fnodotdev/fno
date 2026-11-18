//! parity-stage: characterization
//! parity-oracle: fno.paths.plan_doc_filename
//!
//! Characterization for the plans-path port: the Rust chain
//! (`plans_path`, reached through the `fno-agent state plan-path` client
//! command) is pinned by the frozen goldens under tests/golden/plans-path/.
//! While the Python leg lived, this file ran as a LIVE differential: both
//! implementations answered the same fixture with the same env pins and the
//! run asserted byte-identity across all nine chain-step cases before the
//! goldens froze. The Python leg (`plan_doc_filename`, `plans_dir`, the
//! `plans_content_dir` chain) was deleted in the same change that landed the
//! port, so the goldens now stand as the contract and capture mode refuses.
//! `plans_content_dir` and `plan_doc_path` survive in `fno.paths` as thin
//! pydoor forwards; they are not the oracle - the oracle is the deleted
//! renderer.
//!
//! Fixture roots move every run, so every golden output is rendered with
//! the fixture root masked first. The pinned instant (2026-09-27 11:30 UTC)
//! renders `20260927` on any UTC machine; date-code goldens depend on that.

use common::{assert_golden as assert_golden_common, Golden};
use std::process::Command;

mod common;

/// Serialize FNO_CONFIG/FNO_STATE_DIR/FNO_SPACES_DIR mutation across the
/// parallel test threads.
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// The fixture: one git repo `proj` plus the pinned config file, both under
/// a tempdir. `proj` is a git repo so the canonical-root slug both legs hash
/// is the same.
struct Fixture {
    tmp: tempfile::TempDir,
}

/// 2026-09-11 13:30:00 UTC; renders `20260911` under any TZ that keeps the
/// local date on 2026-09-11. UTC CI and the capture machine agree.
const NOW: &str = "1789138200";

/// The env pins BOTH legs inherit: the config walk collapses to the fixture
/// file, and the state roots stay inside it.
fn pin_env(fx: &Fixture) -> Vec<(String, String)> {
    vec![
        (
            "FNO_CONFIG".to_string(),
            fx.tmp.path().join("config.toml").display().to_string(),
        ),
        (
            "FNO_STATE_DIR".to_string(),
            fx.tmp.path().join("state").display().to_string(),
        ),
        (
            "FNO_SPACES_DIR".to_string(),
            fx.tmp.path().join("spaces").display().to_string(),
        ),
    ]
}

/// Mask the moving fixture root (both spellings macOS offers) and the
/// space slug it mints, so the golden describes the chain, not this run's
/// tempdir name.
fn masked(text: String, fx: &Fixture) -> String {
    let root = fx.tmp.path();
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let slug = fno_agent::paths::space_slug(&canonical);
    text.replace(&canonical.to_string_lossy().to_string(), "<fixture>")
        .replace(&root.to_string_lossy().to_string(), "<fixture>")
        .replace(&slug, "<fixture-slug>")
}

/// Run the Rust leg through the built binary: the exact surface the Python
/// command forwards to.
fn rust_plan_path(fx: &Fixture, pins: &[(String, String)]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agent"))
        .args(["state", "plan-path", "--slug", "s", "--node", "x-aaeb"])
        .arg("--now")
        .arg(NOW)
        .arg(fx.tmp.path().join("proj"))
        .envs(fno_agent::test_run::self_owner_env())
        .envs(pins.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .output()
        .expect("run fno-agent state plan-path");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// One case: run the Rust leg and assert it against the frozen golden.
fn assert_case(label: &str, fx: &Fixture, pins: &[(String, String)]) {
    let _env = env_lock();
    let (code, out, err) = rust_plan_path(fx, pins);
    assert_eq!(code, 0, "rust leg failed: {err:?}");
    let out = masked(out, fx);
    let err = masked(err, fx);
    let golden = Golden {
        exit: Some(code),
        streams: vec![out, err],
    };
    assert_golden_common("plans-path", label, &golden, None);
}

fn build(config: &str, settings: Option<&str>) -> Fixture {
    let tmp = tempfile::TempDir::new().unwrap();
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(proj.join(".claude")).unwrap();
    std::fs::create_dir_all(proj.join(".fno")).unwrap();
    let run = |args: &[&str]| {
        Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args(["-C", proj.to_str().unwrap()])
            .args(args)
            .status()
            .unwrap()
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@t"]);
    run(&["config", "user.name", "t"]);
    run(&["commit", "-q", "--allow-empty", "-m", "init"]);
    std::fs::write(tmp.path().join("config.toml"), config).unwrap();
    if let Some(settings) = settings {
        std::fs::write(proj.join(".claude/settings.local.json"), settings).unwrap();
    }
    Fixture { tmp }
}

#[test]
fn local_relative() {
    let fx = build("", Some(r#"{"plansDirectory": "docs/plans"}"#));
    assert_case("local_relative", &fx, &pin_env(&fx));
}

#[test]
fn config_vault_template() {
    let fx = build("plans_dir = \"plans\"\n", None);
    let vault_dir = fx.tmp.path().join("vault");
    std::fs::write(
        fx.tmp.path().join("config.toml"),
        format!(
            "plans_dir = \"{{vault}}/plans\"\n\n[obsidian]\nenabled = true\nvault = \"{}\"\n",
            vault_dir.display()
        ),
    )
    .unwrap();
    assert_case("config_vault_template", &fx, &pin_env(&fx));
}

#[test]
fn filename_template_dated() {
    let fx = build("", None);
    assert_case("filename_template_dated", &fx, &pin_env(&fx));
}
