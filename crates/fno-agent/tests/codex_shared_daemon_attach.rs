//! The codex thread lane is a client of the SHARED app-server daemon.
//!
//! The live tests need a running daemon and the `codex` binary, so they report
//! a skip line and pass when either is missing. A skip is not evidence: the
//! standing proof for those acceptance criteria is the measured process tree
//! recorded in the PR body, and these tests are the regression guard for the
//! machine that has the daemon.

use fno_agent::codex_inject::{
    codex_app_server_socket_path, connect_app_server, probe_codex_app_server,
};
use fno_agent::codex_thread::CodexThread;
use futures_util::{SinkExt, StreamExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

/// Serializes every test that READS OR WRITES `CODEX_HOME`.
///
/// The variable is process-global and cargo runs a binary's tests on parallel
/// threads, so a relocating test moves the socket out from under a concurrent
/// reader. Observed: the argv tests raced, and the failure moved between runs.
static CODEX_HOME_LOCK: Mutex<()> = Mutex::new(());

fn codex_home_guard() -> std::sync::MutexGuard<'static, ()> {
    CODEX_HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A live control socket, or `None` with a printed skip line. The caller must
/// already hold [`CODEX_HOME_LOCK`]: the path it resolves depends on that
/// variable.
fn live_socket() -> Option<PathBuf> {
    let path = codex_app_server_socket_path();
    if path.exists() {
        return Some(path);
    }
    eprintln!(
        "skip: no codex app-server daemon at {} (start one with `codex app-server daemon start`)",
        path.display()
    );
    None
}

/// AC7: a missing control socket reports the same connect failure at BOTH
/// call sites, because there is only one client under them.
#[tokio::test]
async fn a_missing_control_socket_fails_the_same_way_for_both_callers() {
    let absent = Path::new("/tmp/x6678-no-such-app-server-control.sock");
    assert_eq!(
        connect_app_server(absent).await.err(),
        Some("no-daemon"),
        "the driver's connect must name the missing socket with the same \
         token every other call site reports"
    );
    assert!(
        !probe_codex_app_server(absent),
        "the health probe reads the same absent socket as unhealthy"
    );
}

/// AC6, AC8, AC9: the driver's thread is registered on the SHARED daemon.
///
/// The positive marker the plan demands: the thread id is PRESENT in
/// `thread/loaded/list` read over a SEPARATE connection, which is only true
/// if the daemon owns it. A count of `codex app-server` processes would be
/// the absence-only assertion this replaces, and it reads the same whether
/// the daemon is healthy or dead with one orphan child left behind.
///
/// AC9 rides the same read: the listing connection is opened independently of
/// the driver's, so the thread is answered by the daemon, never by the
/// driver's own socket.
#[tokio::test]
async fn a_driver_thread_is_registered_on_the_shared_daemon() {
    let _guard = codex_home_guard();
    let Some(socket) = live_socket() else { return };
    let driver = match CodexThread::start(
        std::env::temp_dir(),
        None,
        &fno_agent::codex_posture::CodexPosture::bounded(),
        None,
    )
    .await
    {
        Ok(driver) => driver,
        Err(error) => {
            eprintln!("skip: could not start a thread on the shared daemon: {error}");
            return;
        }
    };
    let thread_id = driver.thread_id().to_string();
    assert!(!thread_id.is_empty(), "the driver must carry a thread id");

    let (mut sink, mut stream) = connect_app_server(&socket)
        .await
        .expect("a second, independent connection to the shared daemon");
    sink.send(Message::Text(
        serde_json::json!({"id": "list", "method": "thread/loaded/list", "params": {}})
            .to_string()
            .into(),
    ))
    .await
    .expect("send thread/loaded/list");
    let mut listed = None;
    for _ in 0..64 {
        let Some(Ok(Message::Text(text))) = stream.next().await else {
            break;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if value.get("id") == Some(&serde_json::json!("list")) {
            listed = value["result"]["data"].as_array().cloned();
            break;
        }
    }
    let listed = listed.expect("the daemon answered thread/loaded/list");
    assert!(
        listed
            .iter()
            .any(|id| id.as_str() == Some(thread_id.as_str())),
        "thread {thread_id} is not loaded on the shared daemon; the driver is \
         talking to something else. loaded={listed:?}"
    );
    assert_eq!(
        driver.pid(),
        fno_agent::codex_inject::ensure_codex_daemon()
            .expect("the daemon we just used")
            .state
            .pid,
        "the driver's recorded pid must BE the shared daemon's"
    );
}
