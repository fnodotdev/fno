//! End-to-end daemon tests with real subprocesses (Wave 3, Discretion #5).
//!
//! These are the load-bearing proofs of the wave: a PTY-managed agent survives
//! the daemon's death (Outcome B), and a restarted daemon reconnects to the
//! still-live worker via socket discovery. No monkeypatching — real `daemon`,
//! `worker`, and `sleep` processes.

use fno_agent::client::{call, ensure_daemon, ClientError};
use fno_agent::paths::AgentsHome;
use fno_agent::protocol::Request;
use fno_agent::state;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

mod common;

const DAEMON_BIN: &str = env!("CARGO_BIN_EXE_fno-agent-daemon");
const WORKER_BIN: &str = env!("CARGO_BIN_EXE_fno-agent-worker");
const CLIENT_BIN: &str = env!("CARGO_BIN_EXE_fno-agent");

/// Short home root (Unix-socket `sun_path` is ~104 bytes; `/var/folders/...` is
/// too long on macOS).
fn short_home() -> AgentsHome {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    // pid + counter: collision-proof across parallel tests, and short enough
    // for the Unix-socket sun_path limit.
    AgentsHome::at(PathBuf::from(format!(
        "/tmp/fnoe{}_{}",
        std::process::id(),
        n
    )))
}

/// A daemon started by a test, killed and reaped when the test's binding goes
/// out of scope.
///
/// Every daemon here runs with `FNO_AGENT_IDLE_EXIT_SECS=3600`, so one that is
/// spawned and never waited on outlives the whole binary. Measured 2026-08-25:
/// a single run of this file left two live daemons behind, and the stress
/// harness runs the file twenty times in a row, so one CI job ended with about
/// forty of them alive at once. That is the pid-exhaustion shape this suite
/// exists to close, and per-test discipline had already missed it three times.
/// Reaping in `Drop` makes the obligation structural: a test added later cannot
/// forget.
///
/// The reap asserts the POSITIVE marker. `wait` returns the child's exit
/// status, so a returning `Drop` means the daemon was collected, not merely
/// signalled. A test that has already waited leaves an `ESRCH` kill and an
/// error from the second wait, both harmless.
struct DaemonChild(std::process::Child);

impl std::ops::Deref for DaemonChild {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for DaemonChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for DaemonChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spawn the daemon as a tracked child (so the test holds its PID) and wait for
/// the socket. The worker-bin override is passed through the env.
fn start_daemon(home: &AgentsHome) -> DaemonChild {
    start_daemon_env(home, &[])
}

/// Like [`start_daemon`] but with extra env on the daemon process. Used by tests
/// that seed a precise registry status the startup reconcile sweep (Architecture
/// B) would otherwise settle -- e.g. `FNO_AGENT_NO_STARTUP_RECONCILE=1` to keep
/// an artificially-seeded mid-flight source row intact for a promote-admission
/// assertion.
fn start_daemon_env(home: &AgentsHome, extra: &[(&str, &str)]) -> DaemonChild {
    start_daemon_env_in(home, home.root(), extra, Path::new(DAEMON_BIN))
}

/// [`start_daemon_env`] with an explicit launch cwd: the daemon keeps whoever
/// started it as its cwd, and the cwd-pin test needs a launch from inside a
/// git worktree to prove the pin.
fn start_daemon_env_in(
    home: &AgentsHome,
    dir: &Path,
    extra: &[(&str, &str)],
    daemon_bin: &Path,
) -> DaemonChild {
    let seen = common::count_events(home, "daemon_started");
    let stderr =
        std::fs::File::create(home.root().join("daemon.stderr")).expect("daemon.stderr creates");
    let mut cmd = Command::new(daemon_bin);
    cmd.env("FNO_AGENT_HOME", home.root())
        .env("HOME", home.root())
        .envs(fno_agent::test_run::self_owner_env())
        .env("FNO_AGENT_WORKER_BIN", WORKER_BIN)
        .env("FNO_AGENT_IDLE_EXIT_SECS", "3600")
        .env("FNO_EVENTS_PATH", home.root().join(".fno/events.jsonl"))
        // Outside any git checkout, so the daily reclaim sweep's cargo-build-dirs
        // lane (`crate::reclaim::maybe_run_daily`) finds no workspace manifest to
        // resolve and never reaches the real machine's build base.
        .current_dir(dir)
        .stderr(std::process::Stdio::from(stderr));
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let child = cmd.spawn().expect("daemon spawns");
    common::wait_for_path(&home.supervisor_sock(), Duration::from_secs(10));
    common::wait_for_event_count(home, "daemon_started", seen + 1, Duration::from_secs(10));
    DaemonChild(child)
}

/// A stand-in daemon binary that records that it ran, then exits 0 without
/// binding -- the race loser's shape.
///
/// It leaves a FILE rather than signalling through its exit code, because the
/// exit code no longer separates the cases: a clean exit is now a legitimate
/// outcome the client waits through rather than an error it reports. "Did the
/// child run" is the question, so the child answers it directly.
fn fake_daemon_bin(home: &AgentsHome) -> PathBuf {
    let path = home.root().join("fake-daemon.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\ntouch \"$FNO_AGENT_HOME/fake-daemon-ran\"\nexit 0\n",
    )
    .expect("write fake daemon");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake daemon");
    path
}

/// Quote `value` as one single-quoted shell word.
///
/// The escape is `'\''`: close, escaped quote, reopen. What stood here emitted
/// `'\"'\"'` - the `'"'"'` idiom, which is itself perfectly valid, but with two
/// stray BACKSLASHES in the Rust literal that produced it. Those turn the
/// rewritten apostrophe into a double quote, so a path or value containing one
/// produced a wrapper whose meaning had quietly changed, surfacing much later
/// as a 10s socket timeout that named nothing.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn daemon_env_bin(
    home: &AgentsHome,
    label: &str,
    marker: Option<&Path>,
    extra: &[(&str, &str)],
) -> PathBuf {
    let path = home.root().join(format!("{label}-daemon.sh"));
    // Only the worker-bin override is cleared, and only because the export two
    // lines down replaces it. The delay variable is deliberately NOT unset: it
    // is the thing the probe MEASURES, and clearing it first made the sibling's
    // `delay=unset` a constant this test printed about itself. With the unset
    // gone the printf reports the environment the daemon actually handed down,
    // so a leak has somewhere to show up.
    let mut script = String::from("#!/bin/sh\nunset FNO_AGENT_WORKER_BIN\n");
    script.push_str(&format!(
        "export FNO_AGENT_WORKER_BIN={}\n",
        shell_quote(WORKER_BIN),
    ));
    // Every successor forked through this wrapper inherits the test binary as
    // its owner, so the daemon's watchdog reaps it when the test dies - even
    // under a bare `cargo test`, which sets no owner env of its own. Emitted
    // before `extra` so a caller's explicit owner still wins.
    for (key, value) in fno_agent::test_run::self_owner_env() {
        script.push_str(&format!("export {key}={}\n", shell_quote(&value)));
    }
    for (key, value) in extra {
        script.push_str(&format!("export {key}={}\n", shell_quote(value)));
    }
    if let Some(marker) = marker {
        script.push_str(&format!(
            "printf '%s delay=%s pid=%s\\n' '{}' \"${{FNO_AGENT_STARTUP_RECONCILE_DELAY_MS:-unset}}\" \"$$\" >> {}\n",
            label,
            shell_quote(&marker.display().to_string()),
        ));
    }
    script.push_str(&format!("exec {} \"$@\"\n", shell_quote(DAEMON_BIN)));
    std::fs::write(&path, script).expect("write environment probe daemon");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod environment probe daemon");
    path
}

/// AC1-HP (Architecture B, plan ab-70faa65b): a cold daemon start runs ONE
/// bounded reconcile sweep BEFORE serving, so the first `list` reads truthful
/// liveness. A stale `ask` row recorded `live` at creation (its one-shot process
/// long gone) settles to `exited` -- even though its provider session id makes it
/// "resumable" -- and surfaces that resumability via `session_id`. The startup
/// sweep is what flips it: no explicit `reconcile` RPC is issued here.
#[tokio::test]
async fn cold_start_reconciles_stale_ask_row_to_exited() {
    let home = short_home();
    home.ensure_root().unwrap();
    let daemon_bin = PathBuf::from(DAEMON_BIN);

    // Seed a stale ask row (empty short_id + no pid = one-shot ask) while the
    // daemon is DOWN, status recorded `live` and never reconciled.
    state::update_registry(&home.registry_json(), |r| {
        r.entries.push(state::RegistryEntry {
            substrate: None,
            node: None,
            spawned_by_session: None,
            spawned_by_harness: None,
            spawned_by_cwd: None,
            launch_account: None,
            related_session_id: None,
            // Fixture: makes no claim about what created the row.
            origin: None,
            name: "stale-ask".into(),
            short_id: String::new(),
            legacy_provider: "codex".into(),
            provider: None,
            model: None,
            model_basis: None,
            effort: None,
            harness: None,
            harness_session_id: None,
            predecessor_session_ids: Vec::new(),
            forked_from_session_id: None,
            route_provider_id: None,
            model_name: None,
            account_record_id: None,
            cwd: "/tmp".into(),
            project_root: String::new(),
            session_id: None,
            spawn_trigger: None,
            legacy_claude_short_id: None,
            claude_session_uuid: None,
            messaging_socket_path: None,
            codex_session_id: Some("resume-uuid-xyz".into()),
            gemini_session_id: None,
            mcp_channel_id: None,
            host_mode: None,
            cc_session_id: None,
            status: fno_agent::AgentStatus::Live,
            last_message_at: None,
            created_at: "2026-05-29T00:00:00Z".into(),
            pid: None,
            pid_start_time: None,
            keeper_child_pid: None,
            log_path: None,
            last_reconciled_at: None,
            inside_leg: None,
            exited_at: None,
            mux: None,
            screen_state: None,
            role_level: None,
            role_scope: None,
            role_grantor: None,
            route_settings_path: None,
            fno_id: None,
            delivery_policy: None,
            sandbox_posture: None,
            ..Default::default()
        });
    })
    .unwrap();

    let mut daemon = start_daemon(&home);

    // The sweep now runs concurrently with the accept loop (x-ef7f), so a served
    // RPC no longer implies it has landed. This test is about WHAT the sweep
    // settles, not when, so wait for the sweep's own event before reading.
    common::wait_for_event(&home, "startup_reconcile_done", common::RECONCILE_BUDGET);
    // Until the truth warm pass ends, every row reads `warming`.
    common::wait_for_event(&home, "truth_warm_done", common::RECONCILE_BUDGET);

    let resp = call(
        &home,
        &daemon_bin,
        &Request::new(1, "agent.list", json!({"all": true})),
    )
    .await
    .expect("list call");
    assert!(!resp.is_err(), "list failed: {:?}", resp.error());
    let agents = resp.result().unwrap()["agents"].as_array().unwrap().clone();
    let row = agents
        .iter()
        .find(|a| a["name"] == "stale-ask")
        .expect("stale-ask row present");
    // The rendered word comes from truth, never the stored status: the
    // recorded exit falsifies the row, so it reads unreachable.
    assert_eq!(
        row["status"], "orphaned",
        "rendered liveness must not inherit the stored lifecycle status"
    );
    // Resumability (session_id) is independent of liveness (status) -- AC3-EDGE.
    assert_eq!(row["session_id"], "resume-uuid-xyz");
    // AC4-EDGE: a one-shot ask has no managed process, so pid is null in --json.
    assert!(row["pid"].is_null(), "ask row must have null pid: {row}");

    // The startup sweep wrote the registry: status exited + CHECKED stamped.
    let reg = state::load_registry(&home.registry_json()).unwrap();
    let entry = reg.find("stale-ask").unwrap();
    assert_eq!(entry.status, fno_agent::AgentStatus::Exited);
    assert!(
        entry.last_reconciled_at.is_some(),
        "startup sweep must stamp last_reconciled_at (CHECKED freshens, AC1-UI)"
    );
    assert_eq!(entry.pid, None, "ask row never carries a pid");

    let events = common::event_text(&home);
    assert!(
        events.contains("startup_reconcile_done"),
        "startup_reconcile_done event not emitted"
    );

    unsafe {
        libc::kill(daemon.id() as libc::pid_t, libc::SIGTERM);
    }
    let _ = daemon.wait();
    std::fs::remove_dir_all(home.root()).ok();
}

/// x-ef7f, the client half: a client whose connect fails while something already
/// holds the singleton lock WAITS for the incumbent instead of forking a
/// competitor for it.
///
/// `call()` reaches `ensure_daemon` from every client command, so with a fleet
/// firing loop-check at each turn boundary, a daemon too busy to accept in time
/// minted a new supervisor per timed-out command. A failed connect has two
/// explanations, dead and merely busy, and only the lock separates them.
///
/// The two outcomes are separated by whether the child RAN, read from a file
/// the child itself writes. The unlocked half is the positive control: it
/// proves the marker mechanism works, so the locked half's missing marker means
/// "declined to spawn" rather than "the marker never worked".
#[tokio::test]
async fn client_declines_to_spawn_while_the_singleton_lock_is_held() {
    let home = short_home();
    home.ensure_root().unwrap();
    let fake = fake_daemon_bin(&home);
    let marker = home.root().join("fake-daemon-ran");

    // Positive control: with the lock free, the client does spawn the child.
    let _ = ensure_daemon(&home, &fake).await;
    assert!(
        marker.exists(),
        "control: a free lock must spawn the daemon bin"
    );
    std::fs::remove_file(&marker).expect("clear the marker");

    // Now hold the lock, as a live daemon holds it for its whole lifetime.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(home.supervisor_lock())
        .expect("open singleton lock");
    lock.try_lock().expect("test takes the singleton lock");

    match ensure_daemon(&home, &fake).await {
        Err(ClientError::DaemonBusy(_)) => {}
        other => panic!("a held lock must report a busy incumbent, got {other:?}"),
    }
    assert!(
        !marker.exists(),
        "a held lock must stop the client forking a competitor for it"
    );

    drop(lock);
    std::fs::remove_dir_all(home.root()).ok();
}

/// status (Wave 5, US6.10, AC10-ERR): with no daemon running, the `fno-agent
/// status` client exits 13 and does NOT lazy-start a daemon.
#[tokio::test]
async fn status_client_exits_13_when_daemon_down() {
    const CLIENT_BIN: &str = env!("CARGO_BIN_EXE_fno-agent");
    let home = short_home();
    home.ensure_root().unwrap();

    let out = Command::new(CLIENT_BIN)
        .arg("status")
        .envs(fno_agent::test_run::self_owner_env())
        .env("FNO_AGENT_HOME", home.root())
        .env("HOME", home.root())
        .output()
        .expect("client runs");
    assert_eq!(
        out.status.code(),
        Some(13),
        "status with no daemon must exit 13; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    // It must NOT have lazy-started a daemon (no socket left behind).
    assert!(
        !home.supervisor_sock().exists(),
        "status must not start a daemon when one is down"
    );
    std::fs::remove_dir_all(home.root()).ok();
}

/// The x-a879 repro (change 4): a real daemon restart over a real-shaped
/// registry, never a constructed summary. The seeded home is a fresh /tmp
/// tree; the live `~/.fno/agents` is never touched.
#[test]
fn a_daemon_restart_over_a_loss_shaped_registry_loses_no_rows() {
    use common::{seed_loss_shaped_registry, short_home};

    let home = short_home();
    seed_loss_shaped_registry(&home);

    let child = start_daemon(&home);
    common::wait_for_event(&home, "startup_reconcile_done", common::RECONCILE_BUDGET);
    drop(child);

    let reg = state::load_registry(&home.registry_json()).unwrap();
    assert_eq!(
        reg.entries.len(),
        29,
        "every seeded row survives the restart"
    );
    let kept = reg.entries.iter().find(|e| e.name == "row-07").unwrap();
    assert_eq!(
        kept.harness_session_id.as_deref(),
        Some("sess-0007"),
        "a surviving row keeps its identity, not just its slot"
    );
    let events = common::event_text(&home);
    assert!(
        !events.contains("registry_row_removed"),
        "a restart that keeps every row must announce nothing: {events}"
    );
}

/// Change 4 acceptance 3: a row the current binary cannot represent (a
/// future schema) is read forward, never silently dropped, and the existing
/// write-side refusal fires instead of a degraded rewrite.
#[test]
fn a_future_schema_registry_is_refused_not_dropped_on_restart() {
    use common::{loss_shaped_rows, short_home};

    let home = short_home();
    std::fs::create_dir_all(home.root()).unwrap();
    let body = format!(
        r#"{{"schema_version":{},"agents":[{}]}}"#,
        state::REGISTRY_SCHEMA_VERSION + 1,
        loss_shaped_rows().join(",")
    );
    seed_registry(&home, &body).unwrap();
    // The seed mints fno ids for session-less rows; compare against the
    // stored document, not the input text.
    let seeded = registry_text(&home.registry_json()).unwrap();

    let child = start_daemon(&home);
    // The sweep reads the store, computes changes, then refuses the write.
    // The meta-event substitution still names the intended kind, so the
    // substring matches either the plain or the capped form.
    common::wait_for_event(&home, "startup_reconcile_failed", common::RECONCILE_BUDGET);
    drop(child);

    assert_eq!(
        registry_text(&home.registry_json()).unwrap(),
        seeded,
        "a future-schema store must not be rewritten behind the reader"
    );
    let reg = state::load_registry(&home.registry_json()).unwrap();
    assert_eq!(reg.entries.len(), 29, "no row is silently dropped");
    let events = common::event_text(&home);
    assert!(
        !events.contains("registry_row_removed"),
        "a refusal keeps every row, so nothing is announced: {events}"
    );
}

/// Seed a pane-hosted row: the shape whose identity keys the list projection
/// used to drop. Holds the mux ref INSTEAD of a transport key (mux XOR worker
/// XOR bg), so `short_id` is empty and `harness_session_id` is the only id.
fn seed_pane_row(home: &AgentsHome, name: &str) {
    seed_pane_row_with_session(home, name, "e6f78b98-e594-47ed-ad81-84f8a78b8bb7");
}

fn seed_pane_row_with_session(home: &AgentsHome, name: &str, session_id: &str) {
    state::update_registry(&home.registry_json(), |r| {
        r.entries.push(fno_agent::state::RegistryEntry {
            substrate: None,
            node: None,
            spawned_by_session: None,
            spawned_by_harness: None,
            spawned_by_cwd: None,
            launch_account: None,
            related_session_id: None,
            // Fixture: makes no claim about what created the row.
            origin: None,
            name: name.into(),
            short_id: String::new(),
            legacy_provider: "claude".into(),
            provider: None,
            model: None,
            model_basis: None,
            effort: None,
            harness: Some("claude".into()),
            harness_session_id: Some(session_id.into()),
            predecessor_session_ids: Vec::new(),
            forked_from_session_id: None,
            route_provider_id: None,
            model_name: None,
            account_record_id: None,
            cwd: "/tmp".into(),
            project_root: String::new(),
            session_id: None,
            spawn_trigger: None,
            legacy_claude_short_id: None,
            claude_session_uuid: None,
            messaging_socket_path: None,
            codex_session_id: None,
            gemini_session_id: None,
            mcp_channel_id: None,
            host_mode: None,
            cc_session_id: None,
            status: fno_agent::AgentStatus::Live,
            last_message_at: None,
            created_at: "2026-07-30T00:00:00Z".into(),
            pid: None,
            pid_start_time: None,
            keeper_child_pid: None,
            log_path: None,
            last_reconciled_at: None,
            inside_leg: None,
            exited_at: None,
            mux: Some(fno_agent::state::MuxRef {
                session: "main".into(),
                pane_id: 10,
            }),
            screen_state: None,
            role_level: None,
            role_scope: None,
            role_grantor: None,
            route_settings_path: None,
            fno_id: None,
            delivery_policy: None,
            sandbox_posture: None,
            ..Default::default()
        });
    })
    .unwrap();
}

fn write_executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[tokio::test]
async fn rm_reaps_registry_claude_and_mux_surfaces_in_one_call() {
    let home = short_home();
    home.ensure_root().unwrap();
    seed_pane_row(&home, "three-surface-worker");
    state::update_registry(&home.registry_json(), |registry| {
        let row = registry.find_mut("three-surface-worker").unwrap();
        row.status = fno_agent::AgentStatus::Exited;
        row.exited_at = None;
    })
    .unwrap();

    let shim_dir = home.root().join("shims");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let claude_state = home.root().join("claude-row");
    let mux_state = home.root().join("mux-pane");
    std::fs::write(&claude_state, "e6f78b98\n").unwrap();
    std::fs::write(&mux_state, "main:10\n").unwrap();
    write_executable(
        &shim_dir.join("claude"),
        r#"#!/bin/sh
if [ "$1" = "agents" ]; then
  if [ -f "$CLAUDE_STATE" ]; then
    printf '[{"kind":"background","id":"e6f78b98","state":"stopped"}]\n'
  else
    printf '[]\n'
  fi
  exit 0
fi
if [ "$1" = "rm" ] && [ "$2" = "e6f78b98" ]; then
  /bin/rm -f "$CLAUDE_STATE"
  exit 0
fi
exit 2
"#,
    );
    write_executable(
        &shim_dir.join("fno"),
        r#"#!/bin/sh
if [ "$1" = "mux" ] && [ "$2" = "pane" ] && [ "$3" = "kill" ] && \
   [ "$4" = "--server" ] && [ "$5" = "main" ] && [ "$6" = "10" ]; then
  /bin/rm -f "$MUX_STATE"
  exit 0
fi
if [ "$1" = "mux" ] && [ "$2" = "retire-session" ] && [ "$3" = "main" ] && \
   [ "$4" = "--harness" ] && [ "$5" = "claude" ] && \
   [ "$6" = "--session-id" ] && [ "$7" = "e6f78b98-e594-47ed-ad81-84f8a78b8bb7" ]; then
  /bin/rm -f "$MUX_STATE"
  printf '{"retired":0,"panes_closed":1,"skipped_typing":0}\n'
  exit 0
fi
if [ "$1" = "mux" ] && [ "$2" = "pane" ] && [ "$3" = "ls" ]; then
  if [ -f "$MUX_STATE" ]; then
    printf '[{"session":"main","pane_id":10}]\n'
  else
    printf '[]\n'
  fi
  exit 0
fi
exit 2
"#,
    );

    let path = format!(
        "{}:{}",
        shim_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let claude_state_env = claude_state.to_string_lossy().into_owned();
    let mux_state_env = mux_state.to_string_lossy().into_owned();
    let mut daemon = start_daemon_env(
        &home,
        &[
            ("PATH", &path),
            ("CLAUDE_STATE", &claude_state_env),
            ("MUX_STATE", &mux_state_env),
            ("FNO_AGENT_NO_STARTUP_RECONCILE", "1"),
        ],
    );

    assert_eq!(
        state::load_registry(&home.registry_json())
            .unwrap()
            .entries
            .len(),
        1
    );
    let pre_claude = Command::new(shim_dir.join("claude"))
        .args(["agents", "--json", "--all"])
        .env("CLAUDE_STATE", &claude_state_env)
        .output()
        .unwrap();
    let pre_mux = Command::new(shim_dir.join("fno"))
        .args(["mux", "pane", "ls"])
        .env("MUX_STATE", &mux_state_env)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&pre_claude.stdout).contains("e6f78b98"));
    assert!(String::from_utf8_lossy(&pre_mux.stdout).contains("pane_id"));

    let out = Command::new(CLIENT_BIN)
        .args(["rm", "three-surface-worker"])
        .envs(fno_agent::test_run::self_owner_env())
        .env("FNO_AGENT_HOME", home.root())
        .env("HOME", home.root())
        .output()
        .expect("rm client runs");
    assert!(
        out.status.success(),
        "rm failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("fno + claude + mux"),
        "receipt: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    let post_registry = state::load_registry(&home.registry_json()).unwrap();
    let post_claude = Command::new(shim_dir.join("claude"))
        .args(["agents", "--json", "--all"])
        .env("CLAUDE_STATE", &claude_state_env)
        .output()
        .unwrap();
    let post_mux = Command::new(shim_dir.join("fno"))
        .args(["mux", "pane", "ls"])
        .env("MUX_STATE", &mux_state_env)
        .output()
        .unwrap();
    assert_eq!(post_registry.entries.len(), 0, "registry row survived");
    assert_eq!(String::from_utf8_lossy(&post_claude.stdout).trim(), "[]");
    assert_eq!(String::from_utf8_lossy(&post_mux.stdout).trim(), "[]");

    let removed = common::event_text(&home)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| event.get("type").and_then(|value| value.as_str()) == Some("agent_removed"))
        .expect("successful removal must leave an audit event");
    let data = removed.get("data").expect("agent_removed data");
    assert_eq!(data.get("registry_changed"), Some(&json!(true)));
    assert_eq!(
        data.get("harness_session_id"),
        Some(&json!("e6f78b98-e594-47ed-ad81-84f8a78b8bb7"))
    );
    for field in [
        "actor",
        "reason",
        "request_id",
        "worktree_touched",
        "reclaimed_bytes",
    ] {
        assert!(
            data.get(field).is_some(),
            "audit event missing {field}: {data}"
        );
    }

    unsafe {
        libc::kill(daemon.id() as libc::pid_t, libc::SIGTERM);
    }
    let _ = daemon.wait();
    std::fs::remove_dir_all(home.root()).ok();
}

// ---------------------------------------------------------------------------
// Daemon binary-version drift restart (ab-1891cdff): US2 (restart swaps the
// daemon), US3 (PTY workers survive -- Outcome B), US1/US4 (drift warned on
// list, stderr-only so --json stdout stays clean).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn restart_force_refuses_to_signal_a_recycled_pid() {
    // x-3498 acceptance: a lockfile whose pid has been recycled by an unrelated
    // process must NOT be signalled. The positive marker is the stranger
    // SURVIVING the command that had every reason to kill it.
    let home = short_home();
    home.ensure_root().unwrap();
    let daemon_bin = daemon_env_bin(&home, "restart-recycled", None, &[]);

    // A live, unrelated process the corrupt lockfile will name. Start time "1"
    // never matches a real process, so the pid-reuse guard must refuse it.
    let mut stranger = Command::new("sleep")
        .arg("300")
        .spawn()
        .expect("spawn sleep");
    std::fs::write(home.supervisor_lock(), format!("{} 1\n", stranger.id())).unwrap();

    let outcome = fno_agent::client::restart_daemon(&home, &daemon_bin, true)
        .await
        .expect("force-with-recycled-pid still restarts");
    assert!(!outcome.forced, "no kill was performed");
    let note = outcome.note.expect("the refusal is reported, not silent");
    assert!(
        note.contains(&stranger.id().to_string()),
        "the note names the pid it refused: {note}"
    );
    assert!(
        stranger.try_wait().unwrap().is_none(),
        "the stranger SURVIVED: a recycled pid is never signalled"
    );

    // The pid-ONLY shape (no start token in the lockfile): bare existence is
    // not a basis for SIGKILL either - the same standard the daemon applies
    // to worker kills. A second stranger proves it.
    let mut stranger2 = Command::new("sleep")
        .arg("300")
        .spawn()
        .expect("spawn sleep 2");
    std::fs::write(home.supervisor_lock(), format!("{}\n", stranger2.id())).unwrap();
    // Retire the daemon the first restart started, so the second force starts
    // from the planted lockfile against a free socket.
    unsafe {
        libc::kill(outcome.new_pid as libc::pid_t, libc::SIGTERM);
    }
    std::thread::sleep(Duration::from_millis(300));
    let outcome2 = fno_agent::client::restart_daemon(&home, &daemon_bin, true)
        .await
        .expect("force-with-pid-only still restarts");
    assert!(!outcome2.forced, "no kill on a startless lockfile record");
    let note2 = outcome2.note.expect("the refusal is reported");
    assert!(
        note2.contains(&stranger2.id().to_string()),
        "the note names the pid it declined: {note2}"
    );
    assert!(
        stranger2.try_wait().unwrap().is_none(),
        "the second stranger SURVIVED: no start token, no SIGKILL"
    );
    unsafe {
        libc::kill(stranger2.id() as libc::pid_t, libc::SIGKILL);
        libc::kill(outcome2.new_pid as libc::pid_t, libc::SIGTERM);
    }
    let _ = stranger2.wait();

    // Cleanup: the first stranger by hand (the first daemon was already
    // SIGTERMed above, the second in the block above).
    unsafe {
        libc::kill(stranger.id() as libc::pid_t, libc::SIGKILL);
    }
    let _ = stranger.wait();
    std::thread::sleep(Duration::from_millis(200));
    std::fs::remove_dir_all(home.root()).ok();
}

// ---------------------------------------------------------------------------
// x-4c87: a nonempty registry must never decode to an empty roster silently.
// The live outage (2026-08-16): a stale daemon whose v11 reader refused the
// v14 store swallowed the read failure into `Registry::default()` and answered
// `count: 0` beside a healthy `discovered_count`, while mail lookup printed
// `agent '<name>' not found` for live workers. The fixtures below are the
// sanitized same-schema equivalent: real worker row shapes, one row carrying a
// value the typed model cannot represent (an unknown status variant).
// ---------------------------------------------------------------------------

/// The argv-gate half of the merge-gate hop repair: a hop that mis-resolved
/// the daemon binary fires `fno-agent-daemon pr-worktree` with the payload
/// on stdin. The gate must refuse fast, name the argv, and publish no serving
/// socket - the old silent ignore booted a full resident daemon that won a
/// freed singleton and served as the control plane under misleading argv.
#[tokio::test]
async fn daemon_refuses_merge_gate_hop_argv_instead_of_serving() {
    let home = short_home();
    home.ensure_root().unwrap();

    let mut child = Command::new(DAEMON_BIN)
        .arg("pr-worktree")
        .env("FNO_AGENT_HOME", home.root())
        .env("HOME", home.root())
        .envs(fno_agent::test_run::self_owner_env())
        .env("FNO_AGENT_WORKER_BIN", WORKER_BIN)
        .env("FNO_AGENT_IDLE_EXIT_SECS", "3600")
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("daemon spawns");

    // Bounded wait: the refusal happens before the runtime builds, so the
    // process dies in well under a second. A daemon that serves instead (the
    // bug this test replaced) would hang until the idle exit, and the
    // deadline turns that into a failure here rather than a stuck test.
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait().expect("daemon is waitable") {
            Some(status) => break status,
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            None => {
                unsafe {
                    libc::kill(child.id() as libc::pid_t, libc::SIGKILL);
                }
                let _ = child.wait();
                panic!("daemon served merge-gate hop argv instead of refusing it");
            }
        }
    };
    assert_eq!(status.code(), Some(2), "the argv gate exits 2: {status}");
    assert!(
        !home.supervisor_sock().exists(),
        "no serving socket may be published under hop argv"
    );
    let mut stderr_buf = String::new();
    if let Some(mut s) = child.stderr.take() {
        use std::io::Read;
        let _ = s.read_to_string(&mut stderr_buf);
    }
    let stderr = stderr_buf;
    assert!(
        stderr.contains("unknown argument"),
        "stderr names the unknown argv: {stderr}"
    );
    assert!(
        stderr.contains("pr-worktree"),
        "stderr echoes the offending argv: {stderr}"
    );
    std::fs::remove_dir_all(home.root()).ok();
}

fn registry_text(path: &std::path::Path) -> std::io::Result<String> {
    fno_agent::registry_store::read(path)
        .map(|doc| serde_json::to_string_pretty(&doc).unwrap())
        .map_err(|e| std::io::Error::other(e.to_string()))
}

fn seed_registry(home: &AgentsHome, body: impl AsRef<str>) -> Result<(), String> {
    let doc = serde_json::from_str(body.as_ref()).map_err(|e| e.to_string())?;
    fno_agent::registry_store::replace_document(&home.registry_json(), doc);
    Ok(())
}
