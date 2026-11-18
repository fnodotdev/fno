//! Integration tests for the loop-check session-binding gate (x-511e Change 1).
//!
//! Isolated `FNO_AGENT_HOME` per test; the live graph store and the shared
//! registry are never opened. gh/git/fno are pointed at nonexistent paths so
//! the engine degrades without network when an owner case reaches it.

use fno_agent::loopcheck::run_loop_check_capture;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Points `FNO_AGENT_HOME` at a fresh temp dir for one test and removes it
/// afterwards. A static mutex serializes the tests: the env var is
/// process-global and a concurrent test removing it mid-run would send the
/// gate at the LIVE machine registry, which these tests must never open.
struct HomeGuard {
    // Held only for its Drop: while alive it serializes the tests against
    // the process-global FNO_AGENT_HOME env var. No test reads the lock
    // itself, so the leading underscore (not #[allow(dead_code)]) is the
    // honest marker - a poisoned-lock guard genuinely carries no data.
    _lock: std::sync::MutexGuard<'static, ()>,
    dir: TempDir,
}

impl HomeGuard {
    fn new() -> Self {
        static ENV_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        let lock = ENV_LOCK.get_or_init(|| std::sync::Mutex::new(()));
        let _lock = lock.lock().unwrap_or_else(|p| p.into_inner());
        let dir = TempDir::new().unwrap();
        std::env::set_var("FNO_AGENT_HOME", dir.path());
        HomeGuard { _lock, dir }
    }

    fn seed_registry(&self, rows: &[String]) {
        fs::create_dir_all(self.dir.path()).unwrap();
        fs::write(
            self.dir.path().join("registry.json"),
            format!(
                r#"{{"schema_version":{},"agents":[{}]}}"#,
                fno_agent::state::REGISTRY_SCHEMA_VERSION,
                rows.join(",")
            ),
        )
        .unwrap();
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        std::env::remove_var("FNO_AGENT_HOME");
    }
}

fn row_json(harness: &str, sid: &str, cwd: &Path) -> String {
    let cwd = cwd.to_string_lossy().into_owned();
    format!(
        r#"{{"name":"w-{sid}","short_id":"w-{sid}","harness":"{harness}","harness_session_id":"{sid}","cwd":"{cwd}","log_path":"/tmp/{sid}.log","created_at":"2026-09-18T00:00:00Z","status":"live"}}"#
    )
}

fn row_json_with_node(harness: &str, sid: &str, cwd: &Path, node: &str) -> String {
    let mut row = row_json(harness, sid, cwd);
    row.insert(row.len() - 1, ',');
    row.insert_str(row.len() - 1, &format!(r#""node":"{node}""#));
    row
}

fn row_json_teamed(harness: &str, sid: &str, cwd: &Path) -> String {
    let mut row = row_json(harness, sid, cwd);
    row.insert(row.len() - 1, ',');
    row.insert_str(row.len() - 1, r#""role_level":1"#);
    row
}

fn manifest(input_node: &str) -> String {
    format!(
        "---\nsession_id: fno-own\ncreated_at: 2026-09-18T00:00:00Z\nattended: true\ninput: \"{input_node}\"\n---\n"
    )
}

fn transcript(text: &str) -> String {
    format!("{{\"message\":{{\"role\":\"assistant\",\"content\":\"{text}\"}}}}\n")
}

fn base_args(state: &Path, transcript_path: &Path, cwd: &Path) -> Vec<String> {
    vec![
        "loop-check".into(),
        "--state".into(),
        state.to_string_lossy().into_owned(),
        "--transcript".into(),
        transcript_path.to_string_lossy().into_owned(),
        "--cwd".into(),
        cwd.to_string_lossy().into_owned(),
        "--gh-bin".into(),
        "/nonexistent-gh".into(),
        "--git-bin".into(),
        "/nonexistent-git".into(),
        "--fno-bin".into(),
        "/nonexistent-fno".into(),
    ]
}

fn with_binding(args: Vec<String>, sid: &str) -> Vec<String> {
    let mut args = args;
    args.push("--harness".into());
    args.push("opencode".into());
    args.push("--harness-session".into());
    args.push(sid.into());
    args
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[test]
fn owner_session_proceeds_into_the_engine_unchanged() {
    let home = HomeGuard::new();
    let cwd_dir = TempDir::new().unwrap();
    let cwd = cwd_dir.path();
    home.seed_registry(&[row_json("opencode", "ses_owner", cwd)]);

    let manifest_path = cwd.join("target-state.md");
    fs::write(&manifest_path, manifest("x-511e")).unwrap();
    let transcript_path = cwd.join("transcript.jsonl");
    fs::write(&transcript_path, transcript("no promise yet")).unwrap();

    let (code, out) = run_loop_check_capture(&with_binding(
        base_args(&manifest_path, &transcript_path, cwd),
        "ses_owner",
    ));
    let v: Value = serde_json::from_str(&out).unwrap();
    // The gate let the owner through: whatever the engine decides with
    // nonexistent gh/git, it is the engine's own decision, not a refusal.
    assert_ne!(v["decision"], "refuse");
    assert!(code == 0 || code == 1);
    // A block names its continuation; an allow carries none.
    if v["decision"] == "block" {
        assert_eq!(v["continuation"], "/target --resume");
    } else {
        assert!(v.get("continuation").is_none());
    }
    let _ = &home;
}

#[test]
fn unrelated_session_is_refused_by_name_exit_zero() {
    let home = HomeGuard::new();
    let cwd_dir = TempDir::new().unwrap();
    let cwd = cwd_dir.path();
    home.seed_registry(&[row_json_with_node("opencode", "ses_owner", cwd, "x-511e")]);

    let manifest_path = cwd.join("target-state.md");
    fs::write(&manifest_path, manifest("x-511e")).unwrap();
    let transcript_path = cwd.join("transcript.jsonl");
    fs::write(&transcript_path, transcript("no promise yet")).unwrap();

    let (code, out) = run_loop_check_capture(&with_binding(
        base_args(&manifest_path, &transcript_path, cwd),
        "ses_unrelated",
    ));
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(code, 0);
    assert_eq!(v["decision"], "refuse");
    let msg = v["message"].as_str().unwrap();
    assert!(
        msg.contains("ses_owner"),
        "refusal must name the bound session: {out}"
    );
    assert!(
        msg.contains("ses_unrelated"),
        "refusal must name the asking session: {out}"
    );
}

#[test]
fn teamed_row_routes_a_real_drain_read_and_the_drain_answers() {
    // The Team route runs the lead's own reads under a fire stamped
    // driver=target. The drain reserve now arms on the shared lead entry
    // (not the driver string), and the drain answers through a scripted
    // fno-bin after a real sleep - it is never the 250ms floor that made
    // this route's floor kills look like load flakes.
    let home = HomeGuard::new();
    let cwd_dir = TempDir::new().unwrap();
    let cwd = cwd_dir.path();
    home.seed_registry(&[row_json_teamed("opencode", "ses_lead", cwd)]);
    // A lead manifest with a scope: the board reads in-process, then the
    // drain read shells to the scripted binary. created_at is NOW: a stale
    // stamp puts the team past its 96h default span and the term gate
    // blocks before the drain read ever runs. FNO_HOME carries a seeded
    // graph store and FNO_CONFIG a workspace config: the scope read demands
    // an epic entry and a work.workspaces mapping, and neither may lean on
    // an ambient machine's graph or config.
    let created = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let lead_md = cwd.join("lead.md");
    fs::write(
        &lead_md,
        format!("---\nfno_id: k-2440\ncreated_at: {created}\nscope: x-2440\n---\n"),
    )
    .unwrap();
    let graph_home = TempDir::new().unwrap();
    let graph = graph_home.path().join("graph.json");
    fno_agent::graph_store::seed_rows(&graph, &[
        json!({"id":"x-2440","slug":"x-2440","title":"scope","type":"epic","priority":"p1","status":"in_progress"})
    ]).unwrap();
    let config = graph_home.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "state_dir = {:?}\n[work.workspaces.test]\nprojects = [{{name = \"fno\"}}]\n",
            graph_home.path().to_string_lossy()
        ),
    )
    .unwrap();
    let saved_config = std::env::var_os("FNO_CONFIG");
    std::env::set_var("FNO_CONFIG", &config);
    std::env::set_var("FNO_HOME", graph_home.path());
    // The board's quiet verdict needs its own sources readable: gh
    // answers empty queues from a stub dir on PATH. The scripted drain sleeps
    // 400ms, then answers: a bound at the old 250ms floor would have killed
    // it. It rides --fno-bin, so the PATH stubs never cover it.
    let bin = cwd.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for (name, body) in [("gh", "#!/bin/sh\nprintf '[]\\n'\n")] {
        let path = bin.join(name);
        fs::write(&path, body).unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let saved_path = std::env::var_os("PATH");
    std::env::set_var(
        "PATH",
        std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
            &saved_path.clone().unwrap_or_default(),
        )))
        .unwrap(),
    );
    let stub = cwd.join("fno-stub.sh");
    fs::write(&stub, "#!/bin/sh\nsleep 0.4\necho '{\"undelivered\": 2}'\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let transcript_path = cwd.join("transcript.jsonl");
    fs::write(&transcript_path, transcript("x")).unwrap();
    let mut args = base_args(&lead_md, &transcript_path, cwd);
    args.push("--fno-bin".into());
    args.push(stub.to_string_lossy().into_owned());
    let (code, out) = run_loop_check_capture(&with_binding(args, "ses_lead"));
    if let Some(v) = saved_path {
        std::env::set_var("PATH", v);
    }
    match saved_config {
        Some(v) => std::env::set_var("FNO_CONFIG", v),
        None => std::env::remove_var("FNO_CONFIG"),
    }
    std::env::remove_var("FNO_HOME");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(code, 0);
    assert_ne!(v["decision"], "refuse");
    let msg = v["message"].as_str().unwrap();
    assert!(
        msg.contains("2 driven rows undelivered"),
        "expected the drain's own verdict through the Team route, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// The manifest-owner arm and the per-harness continuation (pi)
// ---------------------------------------------------------------------------
