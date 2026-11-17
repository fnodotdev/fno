//! `-H footnote --substrate thread` against real processes: the worker
//! binary hosts the session, a stand-in `footnote acp` speaks ACP stdio, and
//! every steer is a typed message, never a keystroke.

use fno_agent::footnote_harness::thread::{deliver_rpc, dispatch_thread, maybe_ask_host};
use fno_agent::paths::AgentsHome;
use fno_agent::protocol::Request;
use fno_agent::state::{load_registry, update_registry};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

/// Every test here mutates the process env the spawns inherit; one at a time.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The stand-in reads the spec's id, opens that session, takes one prompt,
/// asks once, and holds the turn until a cancel arrives. Each frame it reads
/// lands in a capture file. It has no `serve`, so the host falls back to it.
const FAKE_ACP: &str = r#"#!/bin/sh
[ "$1" = serve ] && exit 3
sid=$(sed -n 's/.*"fno_id":"\([^"]*\)".*/\1/p' "$2")
read -r init; printf '%s\n' "$init" > "$CAP/init.json"
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"name":"footnote"}}}'
read -r new
printf '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"%s"}}\n' "$sid"
read -r prompt; printf '%s\n' "$prompt" > "$CAP/prompt.json"
printf '%s\n' '{"jsonrpc":"2.0","id":90,"method":"session/request_permission","params":{"toolCall":{"title":"Write: a.txt"},"options":[{"optionId":"allow_once","kind":"allow_once"},{"optionId":"reject_once","kind":"reject_once"}]}}'
read -r answer; printf '%s\n' "$answer" > "$CAP/answer.json"
read -r cancel; printf '%s\n' "$cancel" > "$CAP/cancel.json"
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"cancelled"}}'
read -r eof
"#;

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn captured(dir: &Path, name: &str) -> Value {
    let path = dir.join(name);
    wait_for(name, || {
        std::fs::read_to_string(&path).is_ok_and(|t| t.ends_with('\n'))
    });
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

/// A spawn with no footnote binary refuses with the install command and
/// leaves no row. A thread spawn registers the fnh session id and a live
/// host pid. Mail reaches fnh as `session/prompt` whose origin names the
/// sender's full session id and the message id. `--answer deny` rejects the
/// pending ask. A stop SIGINT to the host's group sends `session/cancel`,
/// closes the socket and marks the row Exited.
#[test]
fn footnote_thread_takes_mail_an_answer_and_a_stop_over_acp_stdio() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let cap = tmp.path().join("cap");
    std::fs::create_dir_all(&cap).unwrap();
    let fake = tmp.path().join("footnote");
    std::fs::write(&fake, FAKE_ACP).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    for (k, v) in [
        (
            "FNO_AGENT_HOME",
            tmp.path().join("agents").to_str().unwrap(),
        ),
        (
            "FNO_AGENT_WORKER_BIN",
            env!("CARGO_BIN_EXE_fno-agent-worker"),
        ),
        ("FNO_FOOTNOTE_BIN", "/nonexistent/footnote"),
        ("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic"),
        ("ANTHROPIC_AUTH_TOKEN", "k"),
        ("CAP", cap.to_str().unwrap()),
        // No serve.json here, so no live footnote serve on this machine.
        ("HOME", tmp.path().join("home").to_str().unwrap()),
    ] {
        std::env::set_var(k, v);
    }
    let home = AgentsHome::from_env();
    let cwd = tmp.path();
    let missing = dispatch_thread(
        &home,
        "fx-gone",
        "",
        "tester",
        cwd,
        Some("glm-test"),
        &json!({}),
    );
    assert_eq!(missing.exit_code, 2, "{}", missing.stderr);
    assert!(
        missing.stderr.contains("cargo install --locked"),
        "{}",
        missing.stderr
    );
    assert!(load_registry(&home.registry_json())
        .unwrap()
        .find("fx-gone")
        .is_none());

    std::env::set_var("FNO_FOOTNOTE_BIN", &fake);
    let spawned = dispatch_thread(
        &home,
        "fx-thread",
        "",
        "tester",
        cwd,
        Some("glm-test"),
        &json!({}),
    );
    assert_eq!(spawned.exit_code, 0, "{}", spawned.stderr);
    let receipt: Value = serde_json::from_str(spawned.stdout.trim()).unwrap();
    let sid = receipt["session_id"].as_str().unwrap().to_string();
    let row = load_registry(&home.registry_json())
        .unwrap()
        .find("fx-thread")
        .cloned()
        .unwrap();
    let pid = row.pid.unwrap();
    assert_eq!(receipt["host_pid"], pid);
    assert_eq!(receipt["host"], "child", "{receipt}");
    assert_eq!(row.substrate.as_deref(), Some("thread"));
    assert_eq!(row.harness_session_id.as_deref(), Some(sid.as_str()));
    // SAFETY: signal 0 only probes that the pid exists.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, 0);
    let init = captured(&cap, "init.json");
    let meta = &init["params"]["clientCapabilities"]["_meta"];
    assert_eq!(meta["footnote/records"], true);
    assert_eq!(meta["footnote/author"], "agent");

    // The sender's row holds its full id; the RPC carries only its name.
    let lead_id = "0b73a1c0-6bd0-4bf7-a30f-9b1fddac1e58";
    // A clone keeps none of the thread row's handles, or the registry refuses
    // the collision.
    let mut lead = row.clone();
    lead.name = "lead".into();
    lead.short_id = String::new();
    lead.session_id = Some(lead_id.into());
    lead.harness_session_id = None;
    lead.fno_id = None;
    update_registry(&home.registry_json(), |reg| {
        reg.entries.push(lead.clone());
        true
    })
    .unwrap();
    let body = "`@lead · fmail-0123456789ab`\nhello";
    let params = json!({"name": "fx-thread", "body": body, "from_name": "lead"});
    let delivered = deliver_rpc(&home, &Request::new(7, "agent.deliver", params));
    let result = delivered.result().cloned().unwrap_or_default();
    assert_eq!(result["delivered"], true, "{result}");
    let prompt = captured(&cap, "prompt.json");
    let origin = &prompt["params"]["_meta"]["footnote/origin"];
    assert_eq!(origin["kind"], "mail");
    assert_eq!(origin["sender_session_id"], lead_id);
    assert_eq!(origin["message_id"], "fmail-0123456789ab");
    assert_eq!(prompt["params"]["sessionId"], sid.as_str());
    assert!(prompt["params"]["prompt"][0]["text"]
        .as_str()
        .unwrap()
        .ends_with("hello"));

    let socket = row.messaging_socket_path.clone().unwrap();
    let status = || {
        fno_agent::footnote_harness::thread::host_call(Path::new(&socket), &json!({"op": "status"}))
    };
    wait_for("the ask", || status().is_ok_and(|s| !s["ask"].is_null()));
    assert_eq!(maybe_ask_host(&row, &json!({"answer": "deny"})), Some(0));
    let answer = captured(&cap, "answer.json");
    assert_eq!(answer["id"], 90);
    assert_eq!(answer["result"]["outcome"]["optionId"], "reject_once");

    // What `fno agent stop` sends a footnote row's pid group.
    // SAFETY: the host called setsid, so its pid is its group id.
    unsafe { libc::killpg(pid as libc::pid_t, libc::SIGINT) };
    let cancel = captured(&cap, "cancel.json");
    assert_eq!(cancel["method"], "session/cancel");
    assert_eq!(cancel["params"]["sessionId"], sid.as_str());
    wait_for("the row to exit", || {
        load_registry(&home.registry_json())
            .ok()
            .and_then(|r| {
                r.find("fx-thread")
                    .map(|e| e.status == fno_agent::AgentStatus::Exited)
            })
            .unwrap_or(false)
    });
    assert!(!Path::new(&socket).exists());
    let mut code = 0;
    // SAFETY: reaps the host this process started.
    unsafe { libc::waitpid(pid as libc::pid_t, &mut code, 0) };
}

/// A probe that quits before the handshake; its one job is printing the
/// `FNO_GRAPH_JSON` it was handed.
const GRAPH_PROBE: &str = "#!/bin/sh\nprintenv FNO_GRAPH_JSON > \"$CAP/graph-env.txt\"\n";

/// A thread spawn names fnh its graph store: like the headless lane, the
/// host command carries the spawner's resolved `FNO_GRAPH_JSON`, so a
/// sandboxed spawn reaches the sandbox store and never `$HOME/.fno`.
#[test]
fn the_thread_spawn_names_fnh_the_graph_store() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let cap = tmp.path().join("cap");
    std::fs::create_dir_all(&cap).unwrap();
    let fake = tmp.path().join("footnote");
    std::fs::write(&fake, GRAPH_PROBE).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let state = tmp.path().join("state");
    for (k, v) in [
        (
            "FNO_AGENT_HOME",
            tmp.path().join("agents").to_str().unwrap(),
        ),
        (
            "FNO_AGENT_WORKER_BIN",
            env!("CARGO_BIN_EXE_fno-agent-worker"),
        ),
        ("FNO_FOOTNOTE_BIN", fake.to_str().unwrap()),
        ("FNO_STATE_DIR", state.to_str().unwrap()),
        ("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic"),
        ("ANTHROPIC_AUTH_TOKEN", "k"),
        ("CAP", cap.to_str().unwrap()),
    ] {
        std::env::set_var(k, v);
    }
    let saved_graph = std::env::var_os("FNO_GRAPH_JSON");
    std::env::remove_var("FNO_GRAPH_JSON");
    let home = AgentsHome::from_env();
    let refused = dispatch_thread(
        &home,
        "fx-graph",
        "",
        "tester",
        tmp.path(),
        Some("glm-test"),
        &json!({}),
    );
    // The probe quits before the ACP handshake, so the host exits before the
    // session opens; the probe's capture is already on disk, and no row
    // registered.
    assert_eq!(refused.exit_code, 2, "{}", refused.stderr);
    match saved_graph {
        Some(v) => std::env::set_var("FNO_GRAPH_JSON", v),
        None => std::env::remove_var("FNO_GRAPH_JSON"),
    }
    let got = std::fs::read_to_string(cap.join("graph-env.txt")).unwrap();
    assert!(
        got.trim().starts_with(state.to_str().unwrap()),
        "the sandbox store, never empty or $HOME/.fno: {got:?}"
    );
    assert!(
        got.trim().ends_with("graph.json"),
        "the graph anchor: {got:?}"
    );
}

/// A stand-in `footnote serve` on 127.0.0.1. It takes one WebSocket peer and
/// reports the bearer header it sent. It answers initialize, and session/new
/// from the spec file `_meta["footnote/spec"]` names, read and removed as
/// serve does. It holds the prompt's turn until a cancel. Every frame goes to
/// `frames`, then `{"closed": true}` when the peer goes.
fn fake_serve(frames: std::sync::mpsc::Sender<Value>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://{}/acp", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
        use tokio_tungstenite::tungstenite::Message;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let (tcp, _) = listener.accept().await.unwrap();
            let auth = frames.clone();
            let check = move |req: &Request, resp: Response| {
                let got = req
                    .headers()
                    .get("authorization")
                    .and_then(|v| v.to_str().ok());
                let _ = auth.send(json!({"authorization": got}));
                Ok::<_, ErrorResponse>(resp)
            };
            let mut ws = tokio_tungstenite::accept_hdr_async(tcp, check)
                .await
                .unwrap();
            let mut held = Value::Null;
            while let Some(Ok(msg)) = ws.next().await {
                let Message::Text(text) = msg else { continue };
                let frame: Value = serde_json::from_str(text.as_str()).unwrap();
                let id = frame["id"].clone();
                let reply = match frame["method"].as_str() {
                    Some("initialize") => Some(json!({"jsonrpc": "2.0", "id": id, "result":
                        {"protocolVersion": 1, "agentInfo": {"name": "footnote"}}})),
                    Some("session/new") => {
                        use std::os::unix::fs::PermissionsExt;
                        let path = frame["params"]["_meta"]["footnote/spec"]
                            .as_str()
                            .unwrap_or("");
                        let mode = std::fs::metadata(path).map(|m| m.permissions().mode() & 0o777);
                        let spec: Value =
                            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                        std::fs::remove_file(path).unwrap();
                        let _ = frames.send(json!({"spec_mode": mode.unwrap_or(0)}));
                        Some(json!({"jsonrpc": "2.0", "id": id,
                            "result": {"sessionId": spec["fno_id"]}}))
                    }
                    Some("session/prompt") => {
                        held = id;
                        None
                    }
                    Some("session/cancel") if !held.is_null() => Some(json!({"jsonrpc": "2.0",
                        "id": std::mem::take(&mut held), "result": {"stopReason": "cancelled"}})),
                    _ => None,
                };
                let _ = frames.send(frame);
                if let Some(r) = reply {
                    if ws.send(Message::Text(r.to_string().into())).await.is_err() {
                        break;
                    }
                }
            }
            let _ = frames.send(json!({"closed": true}));
        });
    });
    url
}

fn frame_where(
    rx: &std::sync::mpsc::Receiver<Value>,
    what: &str,
    hit: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(frame) if hit(&frame) => return frame,
            Ok(_) => {}
            Err(_) => panic!("timed out waiting for {what}"),
        }
    }
}

/// With a live serve, the host attaches with serve.json's token. It declares
/// author agent and opens the session with its spec as a 0600 `.tui-` file.
/// Mail reaches serve as `session/prompt` with its origin. A stop SIGINT
/// cancels the turn and closes the connection. Serve is never signalled, and
/// no child file names it.
#[test]
fn footnote_thread_attaches_to_a_live_serve_and_a_stop_detaches() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let fake = tmp.path().join("footnote");
    std::fs::write(&fake, FAKE_ACP).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let url = fake_serve(tx);
    let footnote_dir = tmp.path().join("home/.footnote");
    std::fs::create_dir_all(&footnote_dir).unwrap();
    // This test process stands in for the daemon pid serve.json names.
    let discovery = json!({"pid": std::process::id(), "url": url, "token": "tok-1"});
    std::fs::write(footnote_dir.join("serve.json"), discovery.to_string()).unwrap();
    for (k, v) in [
        (
            "FNO_AGENT_HOME",
            tmp.path().join("agents").to_str().unwrap(),
        ),
        (
            "FNO_AGENT_WORKER_BIN",
            env!("CARGO_BIN_EXE_fno-agent-worker"),
        ),
        ("FNO_FOOTNOTE_BIN", fake.to_str().unwrap()),
        ("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic"),
        ("ANTHROPIC_AUTH_TOKEN", "k"),
        ("CAP", tmp.path().to_str().unwrap()),
        ("HOME", tmp.path().join("home").to_str().unwrap()),
    ] {
        std::env::set_var(k, v);
    }
    // A declared sandbox keeps the private child; this test declares none.
    let saved: Vec<_> = ["FNO_STATE_DIR", "FNO_CONFIG"]
        .into_iter()
        .map(|k| (k, std::env::var_os(k)))
        .collect();
    for (k, _) in &saved {
        std::env::remove_var(k);
    }
    let home = AgentsHome::from_env();
    let spawned = dispatch_thread(
        &home,
        "fx-serve",
        "",
        "tester",
        tmp.path(),
        Some("glm-test"),
        &json!({}),
    );
    assert_eq!(spawned.exit_code, 0, "{}", spawned.stderr);
    let receipt: Value = serde_json::from_str(spawned.stdout.trim()).unwrap();
    assert_eq!(receipt["host"], "serve", "{receipt}");
    let sid = receipt["session_id"].as_str().unwrap().to_string();
    let auth = frame_where(&rx, "the upgrade", |f| f.get("authorization").is_some());
    assert_eq!(auth["authorization"], "Bearer tok-1");
    let init = frame_where(&rx, "initialize", |f| f["method"] == "initialize");
    assert_eq!(
        init["params"]["clientCapabilities"]["_meta"]["footnote/author"],
        "agent"
    );
    let mode = frame_where(&rx, "the spec mode", |f| f.get("spec_mode").is_some());
    assert_eq!(mode["spec_mode"], 0o600);
    let new = frame_where(&rx, "session/new", |f| f["method"] == "session/new");
    let spec = new["params"]["_meta"]["footnote/spec"].as_str().unwrap();
    let name = Path::new(spec).file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with(".tui-") && name.ends_with(".0.json"),
        "{name}"
    );

    let body = "`@lead · fmail-0123456789ab`\nhello";
    let params = json!({"name": "fx-serve", "body": body, "from_name": "lead"});
    let delivered = deliver_rpc(&home, &Request::new(8, "agent.deliver", params));
    let result = delivered.result().cloned().unwrap_or_default();
    assert_eq!(result["delivered"], true, "{result}");
    let prompt = frame_where(&rx, "the mail prompt", |f| f["method"] == "session/prompt");
    assert_eq!(prompt["params"]["sessionId"], sid.as_str());
    assert_eq!(prompt["params"]["_meta"]["footnote/origin"]["kind"], "mail");

    let row = load_registry(&home.registry_json())
        .unwrap()
        .find("fx-serve")
        .cloned()
        .unwrap();
    let pid = row.pid.unwrap();
    let socket = std::path::PathBuf::from(row.messaging_socket_path.clone().unwrap());
    assert!(!socket.with_extension("child").exists());
    // SAFETY: the host called setsid, so its pid is its group id.
    unsafe { libc::killpg(pid as libc::pid_t, libc::SIGINT) };
    let cancel = frame_where(&rx, "session/cancel", |f| f["method"] == "session/cancel");
    assert_eq!(cancel["params"]["sessionId"], sid.as_str());
    frame_where(&rx, "the close", |f| f["closed"] == true);
    wait_for("the row to exit", || {
        load_registry(&home.registry_json())
            .ok()
            .and_then(|r| {
                r.find("fx-serve")
                    .map(|e| e.status == fno_agent::AgentStatus::Exited)
            })
            .unwrap_or(false)
    });
    assert!(!socket.exists());
    assert!(!socket.with_extension("child").exists());
    let mut code = 0;
    // SAFETY: reaps the host this process started.
    unsafe { libc::waitpid(pid as libc::pid_t, &mut code, 0) };
    for (k, v) in saved {
        if let Some(v) = v {
            std::env::set_var(k, v);
        }
    }
}
