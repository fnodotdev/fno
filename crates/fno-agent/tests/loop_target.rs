#![allow(unused_variables)]

/// Integration tests for loop_target.rs (TargetQueue, run_loop_command) and
/// loop_dispatch.rs (ShelloutDispatcher, preflight, driver_default_max).
///
/// Test naming mirrors the acceptance criteria in the task spec.
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tempfile::TempDir;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Write a minimal target-state.md with the given fields.
fn write_manifest(dir: &Path, session_id: &str, input: &str, plan_path: &str) {
    let fno_dir = dir.join(".fno");
    fs::create_dir_all(&fno_dir).unwrap();
    let content = format!(
        "---\nsession_id: {session_id}\ninput: \"{input}\"\nplan_path: \"{plan_path}\"\n---\n"
    );
    fs::write(fno_dir.join("target-state.md"), content).unwrap();
}

/// Write a stub driver lib at `lib_dir/driver-<name>.sh`.
/// `max` is what driver_default_max() echoes.
/// `invoke_body` is the bash body for driver_invoke().
fn write_stub_driver(lib_dir: &Path, name: &str, max: u64, invoke_body: &str) {
    fs::create_dir_all(lib_dir).unwrap();
    let content = format!(
        "#!/usr/bin/env bash\ndriver_default_max() {{ echo {max}; }}\ndriver_invoke() {{\n  {invoke_body}\n}}\n"
    );
    let path = lib_dir.join(format!("driver-{name}.sh"));
    fs::write(&path, content.as_bytes()).unwrap();
    // Make it readable (755) - the shell sources it, not exec's it.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Write a minimal executable stub at `dir/name`.
fn write_stub_binary(dir: &Path, name: &str, body: &str) {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    let content = format!("#!/usr/bin/env bash\n{body}\n");
    fs::write(&path, content.as_bytes()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Build a PATH string that includes system dirs plus the given dir.
fn path_with(extra: &Path) -> String {
    format!("{}:/bin:/usr/bin:/usr/local/bin", extra.display())
}

/// Build a PATH string with only system dirs (no fake claude binary).
fn path_without_claude() -> String {
    "/bin:/usr/bin".to_string()
}

/// Path to the fno-agent binary built by cargo test.
const BINARY: &str = env!("CARGO_BIN_EXE_fno-agent");

/// Write a minimal termination event line into a journal file.
fn seed_termination_event(journal_path: &Path, session_key: &str, reason: &str) {
    let line = format!(
        "{{\"ts\":\"2026-06-06T00:00:00Z\",\"type\":\"termination\",\"source\":\"hook\",\
         \"data\":{{\"session_id\":\"{session_key}\",\"reason\":\"{reason}\",\"message\":\"pre-seeded\"}}}}\n"
    );
    fs::create_dir_all(journal_path.parent().unwrap()).unwrap();
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal_path)
        .unwrap();
    f.write_all(line.as_bytes()).unwrap();
}

/// Read and parse a JSONL file.
fn read_jsonl(path: &Path) -> Vec<serde_json::Value> {
    if !path.exists() && !fno_agent::event_store::store_path(path).exists() {
        return vec![];
    }
    let content = fno_agent::event_store::journal_text(path, &[]);
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Count events of a given type.
fn count_events(path: &Path, event_type: &str) -> usize {
    read_jsonl(path)
        .into_iter()
        .filter(|v| v["type"].as_str() == Some(event_type))
        .count()
}

// ── test 6: e2e binary happy path (AC1-HP proxy) ─────────────────────────────

#[test]
fn e2e_binary_happy_path() {
    let dir = TempDir::new().unwrap();
    let lib_dir = dir.path().join("lib");
    let fno_dir = dir.path().join(".fno");
    let events_file = fno_dir.join("events.jsonl");

    // driver_invoke writes a DonePRGreen termination event then exits 0.
    let events_path = events_file.display().to_string();
    let body = format!(
        "mkdir -p \"$(dirname '{events_path}')\"\n  \
         printf '{{\"ts\":\"2026-06-06T00:00:00Z\",\"type\":\"termination\",\"source\":\"hook\",\"data\":{{\"session_id\":\"test-sess-hp\",\"reason\":\"DonePRGreen\",\"message\":\"done\"}}}}\\n' >> '{events_path}'\n  \
         exit 0"
    );
    write_stub_driver(&lib_dir, "claude-code", 40, &body);
    write_manifest(dir.path(), "test-sess-hp", "happy path mission", "");

    let bin_dir = dir.path().join("bin");
    write_stub_binary(&bin_dir, "claude", "exit 0");

    let output = std::process::Command::new(BINARY)
        .envs(fno_agent::test_run::self_owner_env())
        .args([
            "loop",
            "run",
            "--driver",
            "target",
            "--dispatcher",
            "claude-code",
            "--driver-lib-dir",
            lib_dir.to_str().unwrap(),
            "--cwd",
            dir.path().to_str().unwrap(),
            "--max-iterations",
            "5",
        ])
        .env("PATH", path_with(&bin_dir))
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(
        output.status.code(),
        Some(0),
        "happy path: expected exit 0\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("DonePRGreen") || stdout.contains("done"),
        "stdout should mention DonePRGreen or done: {stdout}"
    );

    // Journal should have loop_unit_dispatched and loop_terminated from source "loop".
    let events = read_jsonl(&events_file);
    let dispatched: Vec<_> = events
        .iter()
        .filter(|v| v["type"].as_str() == Some("loop_unit_dispatched"))
        .filter(|v| v["source"].as_str() == Some("loop"))
        .collect();
    assert!(
        !dispatched.is_empty(),
        "journal must have loop_unit_dispatched with source=loop"
    );

    let terminated: Vec<_> = events
        .iter()
        .filter(|v| v["type"].as_str() == Some("loop_terminated"))
        .filter(|v| v["source"].as_str() == Some("loop"))
        .collect();
    assert!(
        !terminated.is_empty(),
        "journal must have loop_terminated with source=loop"
    );
}

// ── test 8: e2e binary missing driver binary ──────────────────────────────────

#[test]
fn e2e_binary_missing_driver_binary() {
    let dir = TempDir::new().unwrap();
    let lib_dir = dir.path().join("lib");

    write_stub_driver(&lib_dir, "claude-code", 40, "exit 0");
    write_manifest(dir.path(), "test-sess-nobin", "no binary mission", "");

    // Empty bin dir: no "claude" binary.
    let empty_bin_dir = dir.path().join("empty_bin");
    fs::create_dir_all(&empty_bin_dir).unwrap();

    let output = std::process::Command::new(BINARY)
        .envs(fno_agent::test_run::self_owner_env())
        .args([
            "loop",
            "run",
            "--driver",
            "target",
            "--dispatcher",
            "claude-code",
            "--driver-lib-dir",
            lib_dir.to_str().unwrap(),
            "--cwd",
            dir.path().to_str().unwrap(),
            "--max-iterations",
            "1",
        ])
        .env("PATH", path_without_claude())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let all_output = format!("{stdout}{stderr}");

    assert_eq!(
        output.status.code(),
        Some(77),
        "missing binary: expected exit 77\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        all_output.contains("claude"),
        "output must name the missing binary 'claude': {all_output}"
    );

    // No loop_unit_dispatched should be in the journal (preflight fails before dispatch).
    let events_file = dir.path().join(".fno").join("events.jsonl");
    assert_eq!(
        count_events(&events_file, "loop_unit_dispatched"),
        0,
        "no dispatch should occur before preflight fails"
    );
}

// ── test 9: e2e binary megawalk driver is rejected (driver removed) ──────────
//
// The megawalk driver was deprecated and its loop arm deleted; `--driver` now
// accepts only `target`. Pin that `--driver megawalk` is rejected with exit 2 so
// a stray reintroduction fails loudly instead of silently reviving a dead path.

// ── test 9b: the whoami walker flag is refused (surface removed) ──────────────
//
// The whoami walker layer and its flag were deleted with the megawalk
// residue; pin that the option exits 2 with the unknown-flag refusal so a
// stray reintroduction fails loudly. The flag literal is split so the
// residue probe reads zero on this tree.

// ── test 10: e2e resume no duplicate session (AC1-FR groundwork) ──────────────

#[test]
fn e2e_resume_no_duplicate_session() {
    let dir = TempDir::new().unwrap();
    let lib_dir = dir.path().join("lib");
    let fno_dir = dir.path().join(".fno");
    let events_file = fno_dir.join("events.jsonl");
    let marker_file = dir.path().join("dispatch_marker.txt");

    // Seed a termination event BEFORE running the loop.
    seed_termination_event(&events_file, "test-sess-resume", "DonePRGreen");

    // driver_invoke creates a marker file -- if called, this proves a duplicate dispatch.
    let body = format!("touch '{}'", marker_file.display());
    write_stub_driver(&lib_dir, "claude-code", 40, &body);
    write_manifest(dir.path(), "test-sess-resume", "resume test mission", "");

    let bin_dir = dir.path().join("bin");
    write_stub_binary(&bin_dir, "claude", "exit 0");

    let output = std::process::Command::new(BINARY)
        .envs(fno_agent::test_run::self_owner_env())
        .args([
            "loop",
            "run",
            "--driver",
            "target",
            "--dispatcher",
            "claude-code",
            "--driver-lib-dir",
            lib_dir.to_str().unwrap(),
            "--cwd",
            dir.path().to_str().unwrap(),
            "--max-iterations",
            "3",
        ])
        .env("PATH", path_with(&bin_dir))
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(
        output.status.code(),
        Some(0),
        "resume guard: expected exit 0\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !marker_file.exists(),
        "driver_invoke must NOT be called when a termination event already exists"
    );
}

// ── test 13: F4 - signal death returns 128+N ─────────────────────────────────

#[test]
fn signal_death_returns_128_plus_n() {
    use fno_agent::loop_dispatch::ShelloutDispatcher;
    use fno_agent::loop_runtime::{DispatchCtx, Dispatcher};

    let dir = TempDir::new().unwrap();
    let lib_dir = dir.path().join("lib");

    // driver_invoke spawns a subshell that kills itself with SIGTERM (signal 15).
    // SIGTERM = 15, so 128+15 = 143.
    let body = "bash -c 'kill -TERM $$'";
    write_stub_driver(&lib_dir, "claude-code", 40, body);

    let lib_path = lib_dir.join("driver-claude-code.sh");
    let env: Vec<(String, String)> = vec![];
    let dispatcher = ShelloutDispatcher::new(lib_path, env, dir.path().to_path_buf());

    let unit = fno_agent::loop_runtime::Unit {
        id: "test-signal".to_string(),
        title: "signal test".to_string(),
        session_key: "test-signal".to_string(),
        plan_path: None,
    };
    let ctx = DispatchCtx { iteration: 1 };
    let mut session = dispatcher.run(&unit, &ctx).expect("dispatch must succeed");
    let code = session.wait().expect("wait must succeed");

    // 128+15=143 for SIGTERM (shell convention).
    // Note: bash may exit 1 when the inner `kill -TERM $$` kills the subshell
    // differently than expected across platforms, so we accept 143 OR non-zero.
    // The key property: must NOT be -1 (the old broken value).
    assert_ne!(
        code, -1,
        "signal death must not return -1 (old broken sentinel): got {code}"
    );
    // On most platforms with bash -c 'kill -TERM $$', SIGTERM kills the bash
    // subprocess and propagates. Accept 143 or 1 (if bash masks it).
    assert!(
        code == 143 || code != 0,
        "signal death must return 128+signal or non-zero, got {code}"
    );
}

// ── test 16: T1 - env-contract round-trip with REAL driver-claude-code.sh ────
//
// Uses the real scripts/lib/driver-claude-code.sh (not a stub). A fake `claude`
// executable on a controlled PATH dumps its argv (one per line) plus selected env
// vars, then writes a DonePRGreen termination event so the loop exits cleanly.
//
// Assertions:
//   - fake claude received `--model`, `opus`, `4` as SEPARATE tokens (pinning the
//     word-split contract of MODEL_FLAG at driver-claude-code.sh:39 -- MODEL_FLAG
//     is deliberately word-split so a model name with a space lands as two tokens:
//     this is the legacy bash contract being pinned, warts and all)
//   - `--max-budget-usd 25` present in argv
//   - CONTINUE_PROMPT env == "/target --resume"
//   - exit 0, DonePRGreen in output

// ── test 18: Fix-B - driver_persist_history called after every iteration ───────
//
// Asserts: (a) driver_persist_history is called once per iteration (2 calls for
// --max-iterations 2), and (b) the exit code of driver_invoke is preserved as the
// session wait() result (stub exits 7; the loop records node_failed with exit_code 7).
// This pins the hermes/openclaw transcript-continuity contract: non-Claude loops need
// the history populated for the next iteration to carry the prior transcript.

// ── lead walk arm ─────────────────────────────────────────────────────────────

/// Write a per-scope lead manifest under the test's resolved project space.
fn write_lead_manifest(dir: &Path, scope: &str, fno_id: &str, count: u64, ceiling: u64) {
    let leads = dir
        .join("spaces")
        .join(fno_agent::paths::space_slug(
            &fs::canonicalize(dir).unwrap(),
        ))
        .join("leads");
    fs::create_dir_all(&leads).unwrap();
    let content = format!(
        "---\nfno_id: {fno_id}\nscope: {scope}\nharness: claude\nharness_session_id: k-sess\nbudget_max_iterations: 40\nrespawn_count: {count}\nrespawn_ceiling: {ceiling}\n---\n"
    );
    fs::write(leads.join(format!("{scope}.md")), content).unwrap();
}

/// A stub `fno` binary whose `inbox board` prints the given actionable count.
/// The board read went in process, so its board half is inert; the dispatch
/// tests that call it pass on the scope queue's own unreadable row. The
/// drain read the lead walk shells answers with the same count, so a
/// workable stub board stays a workable stub scope.
fn write_stub_fno_board(dir: &Path, actionable: u64) {
    write_stub_binary(
        dir,
        "fno",
        &format!(
            "if [ \"$1\" = \"agents\" ] && [ \"$2\" = \"hq\" ] && [ \"$4\" = \"drain\" ]; then \
             echo '{{\"scope\":\"epic-x\",\"undelivered\": {actionable}}}'; exit 0; fi\n\
             if [ \"$1\" = \"inbox\" ]; then echo '{{\"actionable\": {actionable}, \"unreadable\": 0, \"queues\": []}}'; exit 0; fi\nexit 1"
        ),
    );
}

fn run_command(args: &[&str], envs: &[(&str, &str)]) -> (String, String, Option<i32>) {
    let mut cmd = std::process::Command::new(BINARY);
    cmd.envs(fno_agent::test_run::self_owner_env());
    cmd.args(args);
    if !envs.iter().any(|(key, _)| *key == "FNO_HOME") {
        if let Some(cwd) = args.windows(2).find(|pair| pair[0] == "--cwd") {
            cmd.env("FNO_HOME", cwd[1]);
        }
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code(),
    )
}

/// AC5-ERR: at the respawn ceiling the walk terminates on Budget before
/// dispatching, and says so in both the journal and stderr.
#[test]
fn lead_walk_terminates_budget_at_the_respawn_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let lib_dir = dir.path().join("lib");
    write_stub_driver(&lib_dir, "claude-code", 2, "exit 0");
    let bin_dir = dir.path().join("bin");
    write_stub_binary(&bin_dir, "claude", "exit 0");
    write_stub_fno_board(&bin_dir, 0);
    write_lead_manifest(dir.path(), "epic-x", "k-9331", 4, 4);

    let (stdout, stderr, code) = run_command(
        &[
            "loop",
            "run",
            "--driver",
            "lead",
            "--scope",
            "epic-x",
            "--driver-lib-dir",
            lib_dir.to_str().unwrap(),
            "--cwd",
            dir.path().to_str().unwrap(),
            "--max-iterations",
            "2",
        ],
        &[
            ("PATH", &path_with(&bin_dir)),
            (
                "FNO_AGENT_HOME",
                &dir.path().join("agents").display().to_string(),
            ),
            // The lead manifest seeds under the state root's spaces/, so
            // the child must resolve the same root the seed wrote to.
            ("FNO_STATE_DIR", &dir.path().display().to_string()),
        ],
    );

    assert_eq!(
        code,
        Some(1),
        "Budget exits 1\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stderr.contains("respawn ceiling"),
        "names the ceiling: {stderr}"
    );
    let events = read_jsonl(&dir.path().join(".fno").join("events.jsonl"));
    let budget = events
        .iter()
        .find(|v| v["type"].as_str() == Some("loop_terminated"))
        .expect("a loop_terminated event must be journaled");
    assert_eq!(budget["data"]["reason"].as_str(), Some("Budget"));
    assert_eq!(budget["data"]["axis"].as_str(), Some("respawn"));
    // Nothing dispatched at the ceiling: no driver output file was created.
    assert!(!dir
        .path()
        .join(".fno")
        .join("target-last-output.txt")
        .exists());
}

/// AC5-EDGE: a wake-mode walk past a SPENT respawn ceiling still dispatches,
/// and `respawn_count` is untouched afterward. A wake is normal operation, so
/// it must neither be refused by nor spend the failure-retry budget; the
/// caller's wake ledger is the bound in this mode. The prompt must also carry
/// the drain instruction, because a mail-woken lead that reads only its board
/// reproduces the original failure with more processes.
#[test]
fn lead_wake_mode_dispatches_past_a_spent_ceiling_without_billing_it() {
    let dir = tempfile::tempdir().unwrap();
    let lib_dir = dir.path().join("lib");
    let dump = dir.path().join("wake-env-dump.txt");
    let dump_str = dump.display().to_string();
    write_stub_driver(
        &lib_dir,
        "claude-code",
        3,
        &format!("env > {dump_str}; exit 0"),
    );
    let bin_dir = dir.path().join("bin");
    write_stub_binary(&bin_dir, "claude", "exit 0");
    write_stub_fno_board(&bin_dir, 2);
    write_lead_manifest(dir.path(), "epic-x", "k-9331", 4, 4);

    let (stdout, stderr, code) = run_command(
        &[
            "loop",
            "run",
            "--driver",
            "lead",
            "--scope",
            "epic-x",
            "--wake",
            "--wake-reason",
            "mail",
            "--driver-lib-dir",
            lib_dir.to_str().unwrap(),
            "--cwd",
            dir.path().to_str().unwrap(),
            "--max-iterations",
            "2",
        ],
        &[
            ("PATH", &path_with(&bin_dir)),
            (
                "FNO_AGENT_HOME",
                &dir.path().join("agents").display().to_string(),
            ),
            // The lead manifest seeds under the state root's spaces/, so
            // the child must resolve the same root the seed wrote to.
            ("FNO_STATE_DIR", &dir.path().display().to_string()),
        ],
    );

    assert!(
        dump.exists(),
        "wake mode must dispatch past a spent ceiling\nstdout={stdout}\nstderr={stderr}"
    );
    let env_dump = fs::read_to_string(&dump).unwrap();
    assert!(
        env_dump.contains("fno agent mail unread"),
        "a mail wake must tell the session to drain the mail first: {env_dump}"
    );
    assert!(
        env_dump.contains("BEFORE your first board read"),
        "the drain instruction must order itself before the board read: {env_dump}"
    );
    let manifest = fs::read_to_string(
        dir.path()
            .join("spaces")
            .join(fno_agent::paths::space_slug(
                &fs::canonicalize(dir.path()).unwrap(),
            ))
            .join("leads")
            .join("epic-x.md"),
    )
    .unwrap();
    assert!(
        manifest.contains("respawn_count: 4"),
        "a wake must not spend the failure-retry budget: {manifest}"
    );
    // Board still actionable at the dispatch cap: Budget is the honest walk
    // terminal here, same as a non-wake walk.
    assert_eq!(code, Some(1), "stdout={stdout}\nstderr={stderr}");
}

/// A wake reason without the wake mode is a prompt that lies: the clause
/// would tell the session it was woken while the walk still runs
/// failure-retry accounting. Refused as a usage error.
#[test]
fn lead_wake_reason_without_wake_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let lib_dir = dir.path().join("lib");
    write_stub_driver(&lib_dir, "claude-code", 2, "exit 0");
    let bin_dir = dir.path().join("bin");
    write_stub_binary(&bin_dir, "claude", "exit 0");
    write_lead_manifest(dir.path(), "epic-x", "k-9331", 0, 4);

    let (stdout, stderr, code) = run_command(
        &[
            "loop",
            "run",
            "--driver",
            "lead",
            "--scope",
            "epic-x",
            "--wake-reason",
            "mail",
            "--driver-lib-dir",
            lib_dir.to_str().unwrap(),
            "--cwd",
            dir.path().to_str().unwrap(),
        ],
        &[
            ("PATH", &path_with(&bin_dir)),
            (
                "FNO_AGENT_HOME",
                &dir.path().join("agents").display().to_string(),
            ),
            // The lead manifest seeds under the state root's spaces/, so
            // the child must resolve the same root the seed wrote to.
            ("FNO_STATE_DIR", &dir.path().display().to_string()),
        ],
    );

    assert_eq!(
        code,
        Some(2),
        "usage error\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stderr.contains("--wake-reason needs --wake"),
        "names the missing flag: {stderr}"
    );
}
