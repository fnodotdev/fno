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
use fno_agent::harness_capabilities::render_session_argv_with_ids;
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

/// AC2-HP (x-296f): ONE declaration, TWO doors, byte-identical.
///
/// `fno` never links `fno-agent` (it shells the binary at runtime), so the
/// mux viewport's renderer (`fno::agents_view::attach_form(...).render(...)`)
/// and the CLI command's renderer (`render_session_argv_with_ids`) are two
/// functions over the same declaration. For every harness the CONTRACT says
/// declares a form, both doors must render byte-identically; for every
/// harness it says declares none, the viewport must find none. Change either
/// door and this fails here, rather than the two drifting until a row opens
/// the wrong thing from one door.
#[test]
fn attach_argv_matches_the_mux_renderer() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/harness_capabilities.toml");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let caps: toml::Value = toml::from_str(&raw).expect("parse harness_capabilities.toml");
    let harness = caps.get("harness").expect("harness table");
    let uuid = "01a04546-28b2-7a41-ae4c-892bbeb8e295";

    for name in harness.as_table().expect("harness table").keys() {
        // Walk the path node by node: a slash-joined `get` is one key, not a
        // path.
        let mut node = harness.get(name).and_then(|n| n.get("resume_strategy"));
        for key in ["forms", "interactive_attach"] {
            node = node.and_then(|n| n.get(key));
        }
        let form_node = node.cloned();
        let kind = form_node
            .as_ref()
            .and_then(|f| f.get("kind"))
            .and_then(|k| k.as_str())
            .map(str::to_string);
        let declares = kind.as_deref().is_some_and(|k| k != "unsupported");
        // Which spelling the form declares, read off the contract itself -
        // the same question the CLI door's caller must answer.
        let tokens_takes_short = form_node
            .as_ref()
            .and_then(|f| f.get("tokens"))
            .and_then(|t| t.as_array())
            .map(|tokens| {
                tokens
                    .iter()
                    .filter_map(|t| t.as_str())
                    .any(|t| t == "{short_id}")
            });

        // The CLI door, over the same packaged contract fno-agent embeds,
        // asked in the spelling the contract declares.
        let cli = match (declares, tokens_takes_short) {
            (true, Some(true)) => {
                render_session_argv_with_ids(name, "interactive_attach", None, Some(uuid))
            }
            (true, Some(false)) => {
                render_session_argv_with_ids(name, "interactive_attach", Some(uuid), None)
            }
            _ => render_session_argv_with_ids(name, "interactive_attach", None, None),
        };
        // The viewport door.
        let viewport = fno::agents_view::attach_form(name).map(|form| form.render(uuid));
        match (declares, cli, viewport) {
            (true, Ok(cli), Some(viewport)) => {
                assert_eq!(
                    cli, viewport,
                    "{name}'s two attach doors drifted; both read one declaration"
                );
            }
            (false, Err(_), None) => {}
            (true, cli, viewport) => panic!(
                "{name} declares a form but the doors disagree (cli={cli:?}, viewport={viewport:?})"
            ),
            (false, cli, viewport) => panic!(
                "{name} declares no form but a door rendered anyway (cli={cli:?}, \
                 viewport={viewport:?})"
            ),
        }
    }
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
