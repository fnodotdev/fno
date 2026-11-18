//! The keeper stop arm's cursor leg: a cursor-agent keeper-thread
//! row's detached worker-server is captured before the Kill and reaped after
//! the confirmation, and a reap that cannot end a captured server refuses
//! the terminal stamp. The non-cursor arm contracts (confirm-then-stamp,
//! refuse-on-silence) stay owned by keeper_sweep.rs.

use super::*;
use crate::daemon::tests::keeper_sweep::{
    expect_row, keeper_sweep_home, load_registry_tolerating_wipe, spawn_killable_keeper,
};
use crate::daemon::tests::test_ctx;
use crate::pid::Pid;

/// Teardown that runs even when an assert above it fails. A panicked body
/// used to skip the worker SIGKILL and the keeper stop: the SIG_IGN child
/// then lived out its 30s sleep and the keeper thread parked forever, and
/// nextest reported the process FAIL + LEAK.
struct StopFixture {
    worker_pid: u32,
    stop: Arc<std::sync::atomic::AtomicBool>,
    keeper: Option<std::thread::JoinHandle<()>>,
}

impl Drop for StopFixture {
    fn drop(&mut self) {
        // Teardown of a test-spawned child; already-reaped pids
        // answer ESRCH and the error is ignored.
        if let Some(pid) = Pid::new(self.worker_pid) {
            pid.signal(libc::SIGKILL);
        }
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(keeper) = self.keeper.take() {
            let _ = keeper.join();
        }
    }
}

/// A cursor-agent lane-B row whose ownership anchor is THIS test process:
/// the census proves worker-server ownership through `pid` +
/// `pid_start_time`, so both must read back a live, real process.
fn cursor_thread_row(
    name: &str,
    session: &str,
    sock: &Path,
    worker_pid: Option<u32>,
) -> RegistryEntry {
    RegistryEntry {
        origin: Some("spawn".into()),
        name: name.into(),
        short_id: String::new(),
        legacy_provider: String::new(),
        harness: Some("cursor-agent".into()),
        harness_session_id: Some(session.into()),
        cwd: "/repo".into(),
        pid: Some(std::process::id()),
        pid_start_time: super::process_start_time(std::process::id()),
        keeper_child_pid: worker_pid,
        messaging_socket_path: Some(sock.to_string_lossy().into_owned()),
        host_mode: Some("interactive".into()),
        status: AgentStatus::Live,
        created_at: "2026-10-01T00:00:00Z".into(),
        ..Default::default()
    }
}

/// A real child whose ps command line names Cursor's worker-server: exec -a
/// keeps the marker on argv[0] across the exec, so the surviving image (a
/// plain `sleep`) still matches `is_cursor_worker_server_command`. `pre_exec`
/// runs BEFORE the exec (a trap set after it would never fire, and SIG_IGN
/// for TERM survives the exec into the sleep image). The start delay makes
/// the census deterministic: `ps` must list the child before stop's capture
/// runs. Returns the pid only: a reaper thread waits the child like a real
/// parent, because a reaped-then-dead pid would otherwise answer kill(0)
/// as a zombie and read as a survivor.
fn spawn_worker_server(pre_exec: &str) -> u32 {
    let mut child = std::process::Command::new("/bin/bash")
        .arg("-c")
        .arg(format!(
            "{pre_exec}; exec -a 'cursor-agent worker-server fake' /bin/sleep 30"
        ))
        .spawn()
        .expect("spawn worker-server fake");
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let listed = loop {
        let output = std::process::Command::new("ps")
            .args(["-axo", "command="])
            .output()
            .expect("ps");
        let seen = String::from_utf8_lossy(&output.stdout).contains("worker-server fake");
        if seen || std::time::Instant::now() > deadline {
            break seen;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    assert!(listed, "the marker child never appeared in ps");
    pid
}

/// Serializes the two tests below around their fake worker-servers. Both
/// markers share one command string by design, so a census running while the
/// other test's TERM-immune child is alive would capture and reap it too and
/// read the survivor as a failure. One test process, one census at a time.
static WORKER_SERVER_CENSUS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[tokio::test(flavor = "current_thread")]
async fn cursor_keeper_stop_refuses_the_stamp_when_a_server_survives_the_reap() {
    // AC1-ERR: a server that shrugs off SIGTERM (SIG_IGN crosses the exec)
    // fails the reap, the response is an error naming it, and the row stays
    // non-terminal so a retry can reach the leak.
    let home = keeper_sweep_home("kperr");
    let _census = WORKER_SERVER_CENSUS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let sock = home
        .root()
        .join("mux")
        .join("threads")
        .join("cur-hard.sock");
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let keeper = spawn_killable_keeper(&sock, Arc::clone(&stop));
    let worker_pid = spawn_worker_server("trap \"\" TERM");
    let _fixture = StopFixture {
        worker_pid,
        stop: Arc::clone(&stop),
        keeper: Some(keeper),
    };
    state::update_registry(&home.registry_json(), |r| {
        r.entries.push(cursor_thread_row(
            "cur-hard",
            "csess-2",
            &sock,
            Some(worker_pid),
        ));
    })
    .unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));

    let response = handle_stop(
        &ctx,
        &Request::new(2, "agent.stop", json!({"name": "cur-hard"})),
    )
    .await;
    assert!(
        response.result().is_none(),
        "a failed reap must error, not report a stop"
    );
    let message = response
        .error()
        .map(|error| error.message.clone())
        .unwrap_or_default();
    assert!(
        message.contains("worker-server reap failed"),
        "the error names the reap: {message}"
    );
    // The keeper DID confirm; only the reap failed, so the row must stay
    // live (a terminal stamp here would bury the surviving server).
    if let Some(registry) = load_registry_tolerating_wipe(&home).await {
        assert_eq!(
            expect_row(&registry, "cur-hard", &home).status,
            AgentStatus::Live
        );
    }
    assert!(
        Pid::new(worker_pid).is_some_and(|p| p.probe()),
        "the stubborn server is the reason the reap failed"
    );

    // The kill, the keeper stop and the join ride StopFixture's Drop, so
    // they also ran when an assert above this line failed.
    std::fs::remove_dir_all(home.root()).ok();
}
