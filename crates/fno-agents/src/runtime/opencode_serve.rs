//! The serve-HTTP opencode worker lane : `spawn --harness opencode
//! --substrate bg` as a real, unattended, persistent provider.
//!
//! opencode hosts two unattended shapes before this module: the headless
//! one-shot (`opencode_ask::dispatch_opencode_once`, stateless, 600s cap) and
//! the PTY pane (attended; every pane capability bit is false, so autonomous
//! dispatch refuses). A worker needs a third shape: a session that outlives the
//! spawn call, runs long turns without an operator, and captures structured
//! output. `opencode serve` is that shape - the node's reframe from PTY pane to
//! HTTP API, verified against opencode v1.14.50 (live probes 2026-08-23):
//!
//! - `opencode serve --port 0 --print-logs` prints `listening on
//!   http://127.0.0.1:<port>`. fno writes no permission config into it: the
//!   serve reads the user's own opencode config, so the user's grants bind
//!   (an `OPENCODE_CONFIG` allow-all landed last in the merged map and beat
//!   the user's denies; measured 2026-11-11).
//! - `POST /session?directory=<abs>` mints a session bound to that directory,
//!   so ONE shared serve hosts workers across worktrees (a payload `directory`
//!   field is ignored - only the query param binds; probed both ways). The
//!   create body carries the session rules: `question`, `plan_enter` and
//!   `plan_exit` deny (no person answers them), and `external_directory`
//!   allow for the worktree and every `FNO_WORKER_ADD_DIRS` root (the
//!   state-root grant: opencode has no additive dir flag, `--dir` SETS cwd).
//! - Each turn runs under `fno-agents-worker --opencode-turn`
//!   (`opencode_turn.rs`): it sends the turn over HTTP and answers each
//!   permission ask through the fnod ladder (`thread_permission.rs`) until
//!   the session goes idle. Structured capture is the serve's own message
//!   readback (`GET /session/:id/message`), never scraped from a pane or pipe.
//!
//! Serve sessions land in the same global store as every other opencode
//! session, so the existing reachability probe (`opencode_reachable_with`)
//! covers serve rows unchanged.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::claude_ask::{emit_event, validate_spawn_inputs};
use crate::opencode_ask::{
    apply_opencode_variant, require_qualified_model, AskOutcome, OPENCODE_DEFAULT_MODEL,
};
use crate::paths::AgentsHome;
use crate::state::load_registry;

/// Everything the spawn needs from a live serve: the loopback base URL, the
/// auth token the serve was booted with, and the pid (with its start time, so
/// a displaced-serve reap can never SIGTERM a recycled pid).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeHandle {
    pub base_url: String,
    pub token: String,
    pub pid: u32,
    pub pid_start: Option<u64>,
}

/// Budgets: how long to wait for a fresh serve to print its port, and the
/// per-call HTTP timeouts. A serve boot is a node CLI start (~1-2s idle) that
/// reached a healthy port at ~40s wall on a machine pinned at 98.9% CPU, so
/// 60s absorbs a loaded host while a wedged binary still fails fast enough.
const SERVE_BOOT_BUDGET: Duration = Duration::from_secs(60);
const HTTP_CALL_TIMEOUT: Duration = Duration::from_secs(10);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);
const STEER_POLL_INTERVAL: Duration = Duration::from_millis(250);

// ===========================================================================
// Minimal HTTP/1.1 client (loopback, `Connection: close`)
// ===========================================================================

/// One request/response exchange against the serve. Blocking, std-only: the
/// serve is always 127.0.0.1, every call here is short (create / merge /
/// health), and the fno-agents spawn path is synchronous - a full HTTP stack
/// would be a dependency bought for nothing. `auth_token` rides every request
/// as Basic auth: the serve is booted with `OPENCODE_SERVER_PASSWORD`, so a
/// loopback listener with an allow-all permission posture is not reachable by
/// arbitrary local processes.
pub(crate) fn http_json(
    base_url: &str,
    method: &str,
    path_and_query: &str,
    body: Option<&serde_json::Value>,
    auth_token: Option<&str>,
    timeout: Duration,
) -> Result<(u16, String), String> {
    let after_scheme = base_url
        .strip_prefix("http://")
        .ok_or_else(|| format!("serve base url {base_url:?} is not http:// loopback"))?;
    let authority = after_scheme.trim_end_matches('/');
    // Every resolved address, as TcpStream::connect tries them: `localhost`
    // can resolve ::1 first while the server binds 127.0.0.1.
    let mut last = format!("resolve {authority}: no address");
    let mut stream = None;
    for addr in std::net::ToSocketAddrs::to_socket_addrs(authority)
        .map_err(|e| format!("resolve {authority}: {e}"))?
    {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = format!("connect {authority}: {e}"),
        }
    }
    let mut stream = stream.ok_or(last)?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("read timeout: {e}"))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| format!("write timeout: {e}"))?;
    let body_bytes = body.map(|v| v.to_string()).unwrap_or_default();
    let auth_header = auth_token
        .map(|t| {
            use base64::Engine as _;
            let encoded = base64::engine::general_purpose::STANDARD.encode(format!("opencode:{t}"));
            format!("Authorization: Basic {encoded}\r\n")
        })
        .unwrap_or_default();
    let request = format!(
        "{method} {path_and_query} HTTP/1.1\r\nHost: {authority}\r\nAccept: application/json\r\nContent-Type: application/json\r\n{auth_header}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body_bytes.len(),
    );
    stream
        .write_all(request.as_bytes())
        .and_then(|_| stream.write_all(body_bytes.as_bytes()))
        .map_err(|e| format!("write to {authority}: {e}"))?;
    let mut raw = Vec::new();
    // Connection: close means EOF terminates the body; read_to_end is the
    // whole story for every response shape this server produces.
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("read from {authority}: {e}"))?;
    let text = String::from_utf8_lossy(&raw);
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("malformed status line from {authority}"))?;
    let body_out = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    Ok((status, body_out))
}

/// GET /global/health -> true iff `{healthy: true}`. Reused as the gate for a
/// recorded serve, so a single answer never decides a live serve is dead: a
/// busy node event loop can miss one 3s probe while workers run turns, and
/// the unhealthy branch SIGTERMs the serve (killing every worker session on
/// it). Three attempts, 1s apart - slow answers survive, dead ones do not.
fn serve_healthy(base_url: &str, token: &str) -> bool {
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_secs(1));
        }
        if matches!(
            http_json(
                base_url,
                "GET",
                "/global/health",
                None,
                Some(token),
                HEALTH_TIMEOUT
            ),
            Ok((200, body)) if body.contains("\"healthy\":true")
        ) {
            return true;
        }
    }
    false
}

// ===========================================================================
// Serve lifecycle
// ===========================================================================

fn serve_state_path(home: &AgentsHome) -> std::path::PathBuf {
    home.root().join("opencode-serve.json")
}

fn serve_log_path(home: &AgentsHome) -> std::path::PathBuf {
    home.root()
        .join("agents")
        .join("logs")
        .join("opencode-serve.log")
}

/// 32 hex chars from /dev/urandom - the per-serve Basic-auth password. No
/// rand crate; the OS CSPRNG is one read_exact away and the token never needs
/// reproduction.
fn new_serve_token() -> Result<String, String> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom").map_err(|e| format!("open urandom: {e}"))?;
    let mut buf = [0u8; 16];
    f.read_exact(&mut buf)
        .map_err(|e| format!("read urandom: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Extract the port from a serve log line (`opencode server listening on
/// http://127.0.0.1:59971`, measured shape). Free function so tests feed
/// canned logs; manual scan because the one pattern does not buy a regex
/// dependency this module otherwise avoids.
fn serve_port_from_log(text: &str) -> Option<u16> {
    let marker = "listening on http://127.0.0.1:";
    let idx = text.rfind(marker)?;
    let tail = &text[idx + marker.len()..];
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Ensure a shared serve is up and return its handle. Reuses the recorded
/// instance when it answers `/global/health`; otherwise writes the generated
/// config, boots a NEW serve detached (its own session via setsid, so it
/// outlives this client process - the worker turns continue on it after the
/// spawn returns), tails the boot log for the port, and records the state
/// file. One serve per agents-home, shared across every opencode worker on
/// the machine; it is idle-cheap between turns.
///
/// The whole check-or-boot runs under an flock on a sidecar lock file, so two
/// concurrent spawns can never both boot a replacement and leave the loser's
/// serve running orphaned; a displaced unhealthy serve (recorded pid that no
/// longer answers health) is SIGTERMed before the replacement boots.
pub fn ensure_serve(home: &AgentsHome) -> Result<ServeHandle, String> {
    let adapter = OpenCodeDaemonAdapter::new(home);
    let result = crate::harness_daemon::ensure_harness_daemon(&adapter)
        .map_err(|error| error.to_string())?;
    let record = &result.state.raw;
    Ok(ServeHandle {
        base_url: record
            .get("base_url")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        token: record
            .get("token")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        pid: result.state.pid.unwrap_or_default(),
        pid_start: result.state.process_start_time,
    })
}

struct OpenCodeDaemonAdapter<'a> {
    home: &'a AgentsHome,
    state_path: PathBuf,
    lock_path: PathBuf,
}

impl<'a> OpenCodeDaemonAdapter<'a> {
    fn new(home: &'a AgentsHome) -> Self {
        Self {
            home,
            state_path: serve_state_path(home),
            lock_path: home.root().join("opencode-serve.lock"),
        }
    }
}

impl crate::harness_daemon::HarnessDaemonAdapter for OpenCodeDaemonAdapter<'_> {
    fn harness(&self) -> &str {
        "opencode"
    }

    fn state_path(&self) -> &Path {
        &self.state_path
    }

    fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    fn parse_state(&self, raw: &str) -> Result<crate::harness_daemon::DaemonState, String> {
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(|error| error.to_string())?;
        let token = value
            .get("token")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let base_url = value
            .get("base_url")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        let pid = value
            .get("pid")
            .and_then(|value| value.as_u64())
            .filter(|pid| *pid > 0)
            .map(|pid| pid as u32);
        let pid_start = value
            .get("pid_start")
            .and_then(|value| value.as_u64())
            .filter(|start| *start > 0);
        let endpoint = if base_url.is_empty() {
            "unresolved://opencode".to_string()
        } else {
            base_url
        };
        let incarnation = match (pid, pid_start) {
            (Some(pid), Some(start)) => format!("{pid}:{start}"),
            _ if !token.is_empty() && endpoint != "unresolved://opencode" => endpoint.clone(),
            _ => "opencode-unresolved".to_string(),
        };
        crate::harness_daemon::DaemonState::with_optional_process_identity(
            value,
            pid,
            pid_start,
            endpoint,
            incarnation,
        )
    }

    fn is_healthy(&self, state: &crate::harness_daemon::DaemonState) -> bool {
        let token = state
            .raw
            .get("token")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        !token.is_empty()
            && state.endpoint != "unresolved://opencode"
            && serve_healthy(&state.endpoint, token)
    }

    fn boot(&self) -> Result<crate::harness_daemon::DaemonState, String> {
        // Fresh token per serve: Basic-auth material for every request (the serve
        // is booted with OPENCODE_SERVER_PASSWORD), generated from /dev/urandom.
        let token = new_serve_token()?;
        let log_path = serve_log_path(self.home);
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // TRUNCATE, not append: the boot loop below scans this file for the
        // serving port, and a previous boot's `listening on` line would win
        // rfind while the new serve is still starting - recording the OLD url
        // with the NEW pid. Truncating also bounds the file per boot.
        let log = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log_path)
            .map_err(|e| format!("open serve log {:?}: {e}", log_path))?;
        let log_err = log
            .try_clone()
            .map_err(|e| format!("clone serve log handle: {e}"))?;
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        let mut cmd = Command::new("opencode");
        cmd.args(["serve", "--port", "0", "--print-logs"])
            // A config file left by an older fno must not ride in.
            .env_remove("OPENCODE_CONFIG")
            .env("OPENCODE_SERVER_PASSWORD", &token)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        // The serve is shared and long-lived: its plugin reports every later
        // session, so the booting session's identity must not ride into it.
        // With none, the plugin claims by session title.
        crate::claims::scrub_identity_env(&mut cmd);
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = cmd
            .spawn()
            .map_err(|e| format!("spawn opencode serve: {e}"))?;
        let pid = child.id();
        // Leak the handle on purpose: the serve must outlive this process. The
        // detach is real (setsid + no wait); dropping the Rust handle only stops
        // reaping, which is the point.
        std::mem::forget(child);
        crate::orphan_reap::release_child(pid);
        let pid_start = crate::daemon::process_start_time(pid);

        let deadline = std::time::Instant::now() + SERVE_BOOT_BUDGET;
        loop {
            std::thread::sleep(Duration::from_millis(250));
            if let Ok(log_text) = std::fs::read_to_string(&log_path) {
                if let Some(port) = serve_port_from_log(&log_text) {
                    let base_url = format!("http://127.0.0.1:{port}");
                    if serve_healthy(&base_url, &token) {
                        let record = serde_json::json!({
                            "base_url": base_url,
                            "port": port,
                            "pid": pid,
                            "pid_start": pid_start,
                            "token": token,
                        });
                        let incarnation = pid_start
                            .map(|start| format!("{pid}:{start}"))
                            .unwrap_or_else(|| base_url.clone());
                        return crate::harness_daemon::DaemonState::with_optional_process_identity(
                            record,
                            Some(pid),
                            pid_start,
                            base_url,
                            incarnation,
                        );
                    }
                }
            }
            if std::time::Instant::now() >= deadline {
                // The serve was setsid'd into its own group: without this kill
                // every boot timeout leaks one serve, and the next spawn boots
                // another beside it (measured: two orphans after two refusals).
                unsafe {
                    libc::killpg(pid as libc::pid_t, libc::SIGKILL);
                }
                return Err(format!(
                "opencode serve did not report a healthy port within {}s (log: {:?}, pid {pid})",
                SERVE_BOOT_BUDGET.as_secs(),
                log_path
            ));
            }
        }
    }
}

// ===========================================================================
// Spawn dispatch
// ===========================================================================

/// The session rules a worker session is created with. `question`,
/// `plan_enter` and `plan_exit` deny: no person answers them on a thread.
/// `external_directory` allows the worktree and each granted root, the
/// state-root grant the serve lane owes (the user's own rules decide every
/// other call).
fn session_rules(cwd: &Path, state_dirs: &[String]) -> serde_json::Value {
    let mut rules: Vec<serde_json::Value> = ["question", "plan_enter", "plan_exit"]
        .iter()
        .map(|p| serde_json::json!({"permission": p, "pattern": "*", "action": "deny"}))
        .collect();
    let cwd = cwd.to_string_lossy().into_owned();
    for dir in std::iter::once(&cwd).chain(state_dirs) {
        let dir = dir.trim_end_matches('/');
        for pattern in [dir.to_string(), format!("{dir}/*")] {
            rules.push(serde_json::json!({
                "permission": "external_directory",
                "pattern": pattern,
                "action": "allow",
            }));
        }
    }
    serde_json::Value::Array(rules)
}

/// `POST /session?directory=<cwd>` titled `name` with the worker rules;
/// returns the minted `ses_` id.
fn create_session(
    serve: &ServeHandle,
    cwd: &Path,
    name: &str,
    state_dirs: &[String],
) -> Result<String, String> {
    let path = format!(
        "/session?directory={}",
        crate::opencode_permission::encode_query_path(&cwd.to_string_lossy())
    );
    let body = serde_json::json!({"title": name, "permission": session_rules(cwd, state_dirs)});
    let (status, text) = http_json(
        &serve.base_url,
        "POST",
        &path,
        Some(&body),
        Some(&serve.token),
        HTTP_CALL_TIMEOUT,
    )?;
    if status != 200 {
        return Err(format!(
            "session create answered {status}: {}",
            tail_reason(&text, status)
        ));
    }
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("id").and_then(|id| id.as_str()).map(str::to_string))
        .filter(|id| crate::provider::is_opencode_session_id(id))
        .ok_or_else(|| {
            format!(
                "session create returned no ses_ id: {}",
                tail_reason(&text, status)
            )
        })
}

/// The turn driver binary: `fno-agents-worker`, resolved the way the MCP
/// lazy-start resolves it.
fn driver_binary() -> Result<String, String> {
    crate::mcp::client::worker_executable()
        .map(|p| p.to_string_lossy().into_owned())
        .ok_or_else(|| "fno-agents-worker not found beside this binary or on PATH".to_string())
}

/// Start one detached turn driver (`<driver_bin> --opencode-turn <spec>`)
/// for an existing serve session. Launch and steering both use this path, so
/// turn sending, ask answering and logging cannot drift between the two.
#[allow(clippy::too_many_arguments)]
fn spawn_turn_driver(
    home: &AgentsHome,
    serve: &ServeHandle,
    session_id: &str,
    message: &str,
    model: Option<&str>,
    cwd: &Path,
    log_path: &Path,
    driver_bin: &str,
    name: Option<&str>,
) -> Result<u32, String> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let spec = crate::opencode_turn::TurnSpec {
        base_url: serve.base_url.clone(),
        token: serve.token.clone(),
        session_id: session_id.to_string(),
        cwd: cwd.to_path_buf(),
        worker: name.map(str::to_string),
        message: message.to_string(),
        model: model.filter(|m| !m.is_empty()).map(str::to_string),
        events: home.events_jsonl(),
    };
    let dir = home.root().join("agents").join("turns");
    std::fs::create_dir_all(&dir).map_err(|e| format!("turn spec dir: {e}"))?;
    let spec_path = dir.join(format!("{session_id}-{}.json", crate::claims::now_ms()));
    // The spec carries the serve token: owner-only.
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&spec_path)
        .and_then(|mut f| f.write_all(serde_json::to_string(&spec).unwrap_or_default().as_bytes()))
        .map_err(|e| format!("turn spec {}: {e}", spec_path.display()))?;
    let argv = crate::spawn_gate::qos_wrap(
        cwd,
        vec![
            driver_bin.to_string(),
            "--opencode-turn".to_string(),
            spec_path.to_string_lossy().into_owned(),
        ],
    );
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.stdin(Stdio::null());
    let open_log = || {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
    };
    if let (Ok(out), Ok(err)) = (open_log(), open_log()) {
        cmd.stdout(Stdio::from(out));
        cmd.stderr(Stdio::from(err));
    }
    cmd.current_dir(cwd);
    cmd.env("PWD", cwd);
    crate::claims::stamp_command_env(&mut cmd, name, "opencode", Some(session_id));
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()
        .map(|child| {
            let pid = child.id();
            std::mem::forget(child);
            crate::orphan_reap::release_child(pid);
            pid
        })
        .map_err(|e| e.to_string())
}

/// The first passthrough token the serve lane cannot carry, else None. It
/// carries `--model M`, `--model=M` and `-m M`, nothing else.
pub fn uncarried_flag(tokens: &[String]) -> Option<&str> {
    let mut it = tokens.iter();
    while let Some(tok) = it.next() {
        if tok == "--model" || tok == "-m" {
            it.next();
        } else if !tok.starts_with("--model=") {
            return Some(tok);
        }
    }
    None
}

/// Orchestrate one `spawn --harness opencode --substrate bg`: validate,
/// fail-closed registry + collision check, ensure the shared serve, mint the
/// session with the worker rules, then launch the turn driver. The launch
/// record goes first and the row binds at the worker's session report.
/// Fire-and-forget by design: the turn runs on the serve; the driver only
/// sends it and answers its asks.
pub fn dispatch_opencode_serve(
    home: &AgentsHome,
    name: &str,
    message: &str,
    from_name: &str,
    cwd: &Path,
    model: Option<&str>,
    effort: Option<&str>,
    node: Option<&str>,
) -> AskOutcome {
    let driver = match driver_binary() {
        Ok(path) => path,
        Err(error) => return AskOutcome::err(error, 13),
    };
    dispatch_opencode_serve_inner(
        home,
        name,
        message,
        from_name,
        cwd,
        model,
        effort,
        node,
        &crate::claude_ask::state_dirs_from_env(),
        &driver,
    )
}

/// The test seam: the writable-dir set and the driver binary are injected so
/// unit tests never mutate process-global env (the suite runs parallel in one
/// process; a set_var here races every other env-reading test).
#[allow(clippy::too_many_arguments)]
fn dispatch_opencode_serve_inner(
    home: &AgentsHome,
    name: &str,
    message: &str,
    from_name: &str,
    cwd: &Path,
    model: Option<&str>,
    effort: Option<&str>,
    node: Option<&str>,
    state_dirs: &[String],
    driver_bin: &str,
) -> AskOutcome {
    if let Err(msg) = validate_spawn_inputs(name, from_name) {
        return AskOutcome::err(msg, 2);
    }
    if let Err(msg) = require_qualified_model(model) {
        return AskOutcome::err(msg, 2);
    }
    let events = home.events_jsonl();

    let registry = match load_registry(&home.registry_json()) {
        Ok(r) => r,
        Err(e) => {
            emit_event(
                &events,
                "agent_ask_failed",
                &[
                    ("stage", "registry-read".into()),
                    ("name", name.into()),
                    ("provider", "opencode".into()),
                    ("error", e.to_string().into()),
                ],
            );
            return AskOutcome::err(format!("registry read failed: {e}"), 12);
        }
    };
    if registry.find(name).is_some() {
        return AskOutcome::err(
            format!(
                "agent {name:?} already exists; use 'fno agents rm {name}' first or pick another name"
            ),
            2,
        );
    }

    if let Some(effort) = effort {
        let selected_model = model
            .filter(|model| !model.is_empty())
            .unwrap_or(OPENCODE_DEFAULT_MODEL);
        if let Err(error) = apply_opencode_variant(selected_model, effort) {
            return AskOutcome::err(error, 2);
        }
    }

    let serve = match ensure_serve(home) {
        Ok(s) => s,
        Err(msg) => {
            emit_event(
                &events,
                "agent_ask_failed",
                &[
                    ("stage", "opencode-serve".into()),
                    ("name", name.into()),
                    ("provider", "opencode".into()),
                    ("error", msg.clone().into()),
                ],
            );
            return AskOutcome::err(msg, 13);
        }
    };

    // fno mints the session (titled with the worker name, so the serve
    // plugin's session report binds the row by title) and drives the turn.
    let effective_message = crate::provider::render_command_seed(
        if message.is_empty() { "hello" } else { message },
        "opencode",
    );
    let full_prompt = crate::opencode_ask::opencode_envelope(&effective_message, from_name);
    // An unnamed model takes the lane default.
    let model_value = model
        .filter(|m| !m.is_empty())
        .unwrap_or(OPENCODE_DEFAULT_MODEL);
    let session_id = match create_session(&serve, cwd, name, state_dirs) {
        Ok(id) => id,
        Err(error) => {
            emit_event(
                &events,
                "agent_ask_failed",
                &[
                    ("stage", "opencode-session-create".into()),
                    ("name", name.into()),
                    ("provider", "opencode".into()),
                    ("error", error.clone().into()),
                ],
            );
            return AskOutcome::err(error, 13);
        }
    };

    // The record first: the launch reservation and the parent edge the adopt
    // names the worker from, so a session that never reports still holds the
    // lane until its window lapses.
    let lineage = crate::spawn_lineage::ambient_lineage();
    if let Err(error) = crate::launch_record::write_record(
        home.root(),
        crate::launch_record::Launch {
            harness: "opencode".to_string(),
            resume: None,
            name: Some(name.to_string()),
            node: node.filter(|v| !v.is_empty()).map(str::to_string),
            slot: None,
            provider: Some("opencode".to_string()),
            cwd: cwd.to_string_lossy().into_owned(),
        },
        lineage.session.as_deref().unwrap_or(""),
        lineage.harness.as_deref(),
        lineage.cwd.as_deref().unwrap_or(""),
        crate::claims::now_ms(),
    ) {
        return AskOutcome::err(format!("launch record: {error}"), 12);
    }

    let log_path = home
        .root()
        .join("agents")
        .join("logs")
        .join(format!("{name}.jsonl"));
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(error) = spawn_turn_driver(
        home,
        &serve,
        &session_id,
        &full_prompt,
        Some(model_value),
        cwd,
        &log_path,
        driver_bin,
        Some(name),
    ) {
        return AskOutcome::err(format!("opencode turn driver launch: {error}"), 2);
    }

    AskOutcome::ok_reply(
        serde_json::json!({
            "ok": true,
            "name": name,
            "serve_url": serve.base_url,
            "session_id": session_id,
            "short_id": "",
            "permission_posture": "user grants; fno answers asks (thread_permission)",
            "durability": "daemon-owned",
            "incarnation": format!("{}:{}", serve.pid, serve.pid_start.unwrap_or_default()),
            "endpoint": serve.base_url,
            "log_path": log_path.to_string_lossy(),
            // The model the launch carries, and where it came from: a model
            // the caller typed, or the lane's own default.
            "model": model_value,
            "model_basis": if model.is_some_and(|m| !m.is_empty()) {
                "requested"
            } else {
                "lane-default"
            },
            // fno minted the id; the row still binds at the worker's session
            // report, by title.
            "binding": "session-report",
        })
        .to_string(),
    )
}
/// Short failure reason for a non-2xx merge answer (HTTP codes carry no text
/// of their own here; the body's first line does).
fn tail_reason(body: &str, code: u16) -> String {
    let first = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let trimmed = first.trim().chars().take(120).collect::<String>();
    if trimmed.is_empty() {
        format!("HTTP {code}")
    } else {
        format!("HTTP {code}: {trimmed}")
    }
}

// ===========================================================================
// Read/teardown surface (the journey test's eyes and broom)
// ===========================================================================

/// `GET /session/:id` as parsed JSON. The session object carries the merged
/// `permission` rules, so a caller can assert the writable-dirs grant landed.
pub fn fetch_session(
    base_url: &str,
    token: &str,
    session_id: &str,
) -> Result<serde_json::Value, String> {
    let (status, body) = http_json(
        base_url,
        "GET",
        &format!("/session/{session_id}"),
        None,
        Some(token),
        HTTP_CALL_TIMEOUT,
    )?;
    if status != 200 {
        return Err(format!(
            "GET session answered {status}: {}",
            tail_reason(&body, status)
        ));
    }
    serde_json::from_str(&body).map_err(|e| format!("session body parse: {e}"))
}

/// Read liveness for a serve row from the serve's session endpoint. A 404 is
/// a definitive absent session; an unreadable state file, unreachable server,
/// or malformed response is UNKNOWN and must not be converted to dead.
pub fn serve_session_reachable(session_id: &str, timeout: Duration) -> Result<bool, String> {
    serve_session_reachable_in(&AgentsHome::from_env(), session_id, timeout)
}

fn serve_session_reachable_in(
    home: &AgentsHome,
    session_id: &str,
    timeout: Duration,
) -> Result<bool, String> {
    if !crate::provider::is_opencode_session_id(session_id) {
        return Err(format!("malformed opencode session id {session_id:?}"));
    }
    let raw = std::fs::read_to_string(serve_state_path(&home))
        .map_err(|e| format!("serve state unreadable: {e}"))?;
    let state: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("serve state parse: {e}"))?;
    let base_url = state
        .get("base_url")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "serve state has no base_url".to_string())?;
    let token = state
        .get("token")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "serve state has no token".to_string())?;
    let (status, body) = http_json(
        base_url,
        "GET",
        &format!("/session/{session_id}"),
        None,
        Some(token),
        timeout,
    )?;
    if status == 404 {
        return Ok(false);
    }
    if status != 200 {
        return Err(format!(
            "GET session answered {status}: {}",
            tail_reason(&body, status)
        ));
    }
    let session: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("session body parse: {e}"))?;
    if session.get("id").and_then(|value| value.as_str()) == Some(session_id) {
        Ok(true)
    } else {
        Err(format!("session readback id does not match {session_id}"))
    }
}

/// `GET /session/:id/message` as parsed JSON (structured capture readback).
pub fn fetch_messages(
    base_url: &str,
    token: &str,
    session_id: &str,
) -> Result<serde_json::Value, String> {
    let (status, body) = http_json(
        base_url,
        "GET",
        &format!("/session/{session_id}/message"),
        None,
        Some(token),
        HTTP_CALL_TIMEOUT,
    )?;
    if status != 200 {
        return Err(format!(
            "GET messages answered {status}: {}",
            tail_reason(&body, status)
        ));
    }
    serde_json::from_str(&body).map_err(|e| format!("messages body parse: {e}"))
}

/// Send one turn to an already-minted serve session and confirm it by a new
/// message id in the serve readback. The writer's exit is not delivery proof.
pub fn steer_existing_session(
    home: &AgentsHome,
    session_id: &str,
    message: &str,
    cwd: &Path,
    model: Option<&str>,
    timeout: Duration,
) -> Result<String, String> {
    require_qualified_model(model)?;
    let driver = driver_binary()?;
    steer_existing_session_with_binary(home, session_id, message, cwd, model, timeout, &driver)
}

fn steer_existing_session_with_binary(
    home: &AgentsHome,
    session_id: &str,
    message: &str,
    cwd: &Path,
    model: Option<&str>,
    timeout: Duration,
    driver_bin: &str,
) -> Result<String, String> {
    if !crate::provider::is_opencode_session_id(session_id) {
        return Err(format!(
            "opencode serve steering refused session {session_id:?}: expected a full ses_<alnum> id"
        ));
    }
    let serve = ensure_serve(home)?;
    let before = fetch_messages(&serve.base_url, &serve.token, session_id)?;
    let log_path = home
        .root()
        .join("agents")
        .join("logs")
        .join(format!("steer-{session_id}.jsonl"));
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("steering log directory: {e}"))?;
    }
    spawn_turn_driver(
        home, &serve, session_id, message, model, cwd, &log_path, driver_bin, None,
    )
    .map_err(|error| {
        format!("opencode serve steering driver launch failed for session {session_id}: {error}")
    })?;

    let deadline = std::time::Instant::now() + timeout;
    let mut last_seen = latest_message_id(&before);
    while std::time::Instant::now() < deadline {
        if let Ok(messages) = fetch_messages(&serve.base_url, &serve.token, session_id) {
            if let Some(id) = new_message_id_after(&messages, &before) {
                return Ok(id);
            }
            last_seen = latest_message_id(&messages);
        }
        std::thread::sleep(STEER_POLL_INTERVAL);
    }
    Err(format!(
        "opencode serve steering UNKNOWN for session {session_id}: no new message within {}s; last message id {}",
        timeout.as_secs(),
        last_seen.as_deref().unwrap_or("<none>")
    ))
}

/// Resolve a full opencode serve row and steer it. This is the shared mail
/// entry point; callers cannot supply a URL or cwd that disagrees with the row.
pub fn steer_registered_session(
    session_id: &str,
    message: &str,
    timeout: Duration,
) -> Result<String, String> {
    let home = AgentsHome::from_env();
    let registry =
        load_registry(&home.registry_json()).map_err(|e| format!("registry read failed: {e}"))?;
    let entry = registry
        .entries
        .iter()
        .find(|entry| {
            entry.harness_name() == "opencode"
                && (entry.harness_session_id.as_deref() == Some(session_id)
                    || entry.session_id.as_deref() == Some(session_id))
        })
        .ok_or_else(|| {
            format!("opencode serve steering refused: no row for session {session_id}")
        })?;
    if entry.substrate.as_deref() != Some("thread") {
        return Err(format!(
            "opencode serve steering refused session {session_id}: row is not a serve thread"
        ));
    }
    let cwd = Path::new(&entry.cwd);
    if !cwd.is_dir() {
        return Err(format!(
            "opencode serve steering refused session {session_id}: cwd {} is not a directory",
            cwd.display()
        ));
    }
    steer_existing_session(
        &home,
        session_id,
        message,
        cwd,
        entry.model.as_deref(),
        timeout,
    )
}

/// `fno agents ask <name> --answer allow_once|deny` on a serve row: reply to
/// the oldest pending permission ask of the row's session. Exit 2 when no
/// ask is pending, so the user learns nothing was sent.
pub fn answer_registered_session(home: &AgentsHome, name: &str, choice: &str) -> AskOutcome {
    let registry = match load_registry(&home.registry_json()) {
        Ok(registry) => registry,
        Err(error) => return AskOutcome::err(format!("registry read failed: {error}"), 12),
    };
    let Some(entry) = registry.find_name_or_full_session_id(name) else {
        return AskOutcome::err(format!("agent {name:?} has no registry row"), 13);
    };
    let Some(session_id) = entry
        .harness_session_id
        .as_deref()
        .or(entry.session_id.as_deref())
    else {
        return AskOutcome::err(format!("agent {name:?} has no full session id"), 2);
    };
    let serve = match ensure_serve(home) {
        Ok(serve) => serve,
        Err(error) => return AskOutcome::err(error, 13),
    };
    match crate::opencode_turn::answer_for_user(
        &serve.base_url,
        &serve.token,
        Path::new(&entry.cwd),
        session_id,
        choice,
        &crate::opencode_turn::held_ids(&home.root().join("agents").join("turns"), session_id),
    ) {
        Ok(request_id) => AskOutcome::ok_reply(
            serde_json::json!({"ok": true, "answered": request_id, "answer": choice}).to_string(),
        ),
        Err(error) => AskOutcome::err(format!("fno-agents: {name}: {error}"), 2),
    }
}

/// Handle `fno agents ask` for a serve row. Steering confirms the new stored
/// message, then this wrapper waits for a new assistant message and returns its
/// text so ask callers receive the reply rather than the writer receipt.
pub fn ask_registered_session(
    home: &AgentsHome,
    name: &str,
    message: &str,
    from_name: &str,
    model: Option<&str>,
    timeout: Duration,
) -> AskOutcome {
    let registry = match load_registry(&home.registry_json()) {
        Ok(registry) => registry,
        Err(error) => return AskOutcome::err(format!("registry read failed: {error}"), 12),
    };
    let Some(entry) = registry.find_name_or_full_session_id(name) else {
        return AskOutcome::err(format!("agent {name:?} has no registry row"), 13);
    };
    if entry.harness_name() != "opencode" || entry.substrate.as_deref() != Some("thread") {
        return AskOutcome::err(format!("agent {name:?} is not an opencode serve thread"), 2);
    }
    let Some(session_id) = entry
        .harness_session_id
        .as_deref()
        .or(entry.session_id.as_deref())
    else {
        return AskOutcome::err(
            format!("agent {name:?} is an opencode serve row with no full session id"),
            2,
        );
    };
    let cwd = Path::new(&entry.cwd);
    if !cwd.is_dir() {
        return AskOutcome::err(
            format!(
                "agent {name:?} recorded cwd {} is not a directory",
                cwd.display()
            ),
            13,
        );
    }
    let rendered = crate::provider::render_command_seed(message, "opencode");
    let full_prompt = crate::opencode_ask::opencode_envelope(&rendered, from_name);
    let serve = match ensure_serve(home) {
        Ok(serve) => serve,
        Err(error) => return AskOutcome::err(error, 13),
    };
    let before = match fetch_messages(&serve.base_url, &serve.token, session_id) {
        Ok(messages) => messages,
        Err(error) => return AskOutcome::err(error, 12),
    };
    if let Err(error) = steer_existing_session(
        home,
        session_id,
        &full_prompt,
        cwd,
        model.or(entry.model.as_deref()),
        timeout,
    ) {
        return AskOutcome::err(error, 12);
    }
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Ok(messages) = fetch_messages(&serve.base_url, &serve.token, session_id) {
            if let Some(reply) = new_assistant_text_after(&messages, &before) {
                return AskOutcome::ok_reply(reply);
            }
        }
        std::thread::sleep(STEER_POLL_INTERVAL);
    }
    AskOutcome::err(
        format!(
            "opencode serve ask UNKNOWN for session {session_id}: no new assistant reply within {}s",
            timeout.as_secs()
        ),
        12,
    )
}

fn latest_message_id(messages: &serde_json::Value) -> Option<String> {
    messages
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message.get("info"))
        .filter_map(|info| info.get("id"))
        .filter_map(|id| id.as_str())
        .last()
        .map(str::to_string)
}

fn new_assistant_text_after(
    messages: &serde_json::Value,
    high_water: &serde_json::Value,
) -> Option<String> {
    let prior = high_water
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message.get("info"))
        .filter_map(|info| info.get("id"))
        .filter_map(|id| id.as_str())
        .collect::<std::collections::HashSet<_>>();
    messages
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| {
            message
                .get("info")
                .and_then(|info| info.get("role"))
                .and_then(|role| role.as_str())
                == Some("assistant")
        })
        .filter(|message| {
            message
                .get("info")
                .and_then(|info| info.get("id"))
                .and_then(|id| id.as_str())
                .is_some_and(|id| !prior.contains(id))
        })
        .filter_map(|message| message.get("parts"))
        .filter_map(|parts| parts.as_array())
        .flat_map(|parts| parts.iter())
        .filter(|part| part.get("type").and_then(|kind| kind.as_str()) == Some("text"))
        .filter_map(|part| part.get("text").and_then(|text| text.as_str()))
        .map(str::to_string)
        .next()
}

/// Return the first message id that was not present at the caller's
/// high-water mark. A non-empty response is not delivery evidence: the id is
/// the serve's positive marker for a newly stored message.
fn new_message_id_after(
    messages: &serde_json::Value,
    high_water: &serde_json::Value,
) -> Option<String> {
    let prior = high_water
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message.get("info"))
        .filter_map(|info| info.get("id"))
        .filter_map(|id| id.as_str())
        .collect::<std::collections::HashSet<_>>();
    messages
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message.get("info"))
        .filter_map(|info| info.get("id"))
        .filter_map(|id| id.as_str())
        .find(|id| !prior.contains(id))
        .map(str::to_string)
}

/// `DELETE /session/:id` - best-effort teardown; an error is returned, not
/// panicked on, so a caller can still finish its other cleanup.
pub fn delete_session(base_url: &str, token: &str, session_id: &str) -> Result<(), String> {
    let (status, body) = http_json(
        base_url,
        "DELETE",
        &format!("/session/{session_id}"),
        None,
        Some(token),
        HTTP_CALL_TIMEOUT,
    )?;
    if status == 200 || status == 204 {
        Ok(())
    } else {
        Err(format!(
            "DELETE session answered {status}: {}",
            tail_reason(&body, status)
        ))
    }
}

// ===========================================================================
// The archive op: a history-preserving active-surface removal
// ===========================================================================

/// The opencode version the archive op was measured against (v1.14.50,
/// 2026-09-08). `PATCH /session/{id}` with `{"time":{"archived":<epoch_ms>}}`
/// answers 200, a separate GET still returns the session with the field set
/// and its messages intact, and `session.list` drops it from the active
/// listing. Below this version the field is UNVERIFIED, not known-absent, so a
/// caller skips instead of guessing: an older serve that ignored the field
/// would turn a retirement that works today into one that holds its row
/// forever. Above [`OPENCODE_V2_FLOOR`] the argument binds upward: the 2.x
/// HTTP API is a different contract, and an unverified version is skipped,
/// not guessed at.
pub const ARCHIVE_MIN_VERSION: (u32, u32, u32) = (1, 14, 50);

/// First opencode 2.x release. The archive op was measured against the 1.x
/// HTTP API only, so a serve at or above this version is skipped and told why.
pub const OPENCODE_V2_FLOOR: (u32, u32, u32) = (2, 0, 0);

/// What one archive attempt measured. `Survived` is an outcome, not an error:
/// the write was accepted and the stored record still carries no
/// `time.archived`, which is a positive finding about the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveOutcome {
    Archived,
    AlreadyArchived,
    Gone,
    Survived,
}

/// Compare the leading three dotted integers of `reported` against `min`.
/// Anything before the first digit is skipped (`opencode v2.0.19`, bare
/// `1.18.33`); an unparseable version answers false: a version nobody can
/// read is not evidence of a supported one.
pub(crate) fn version_at_least(reported: &str, min: (u32, u32, u32)) -> bool {
    let trimmed = reported.trim();
    let from_digit = match trimmed.find(|c: char| c.is_ascii_digit()) {
        Some(i) => &trimmed[i..],
        None => "",
    };
    let nums: Vec<u32> = from_digit
        .split('.')
        .take(3)
        .map(|part| {
            part.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
        })
        .filter_map(|digits| digits.parse::<u32>().ok())
        .collect();
    nums.len() == 3 && (nums[0], nums[1], nums[2]) >= min
}

fn is_archived(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("time")
                .and_then(|time| time.get("archived"))
                .cloned()
        })
        .is_some_and(|value| !value.is_null())
}

/// The recorded serve, when it answers health AND reports a version carrying
/// the archive op. Returns `None` for every other case, including a missing
/// state file and an unreadable one.
///
/// It never boots a serve: a GC sweep must not start a server as a side
/// effect. One health read answers both questions, because the health body
/// carries the version, and one read is the right budget here - a false
/// "unhealthy" only skips the archive, which is what the caller did before
/// this op existed. The three-attempt [`serve_healthy`] belongs to the spawn
/// path, where the unhealthy branch SIGTERMs the serve.
pub fn archive_capable_serve(home: &AgentsHome) -> Option<ServeHandle> {
    let raw = std::fs::read_to_string(serve_state_path(home)).ok()?;
    let state: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let base_url = state.get("base_url")?.as_str()?.to_string();
    let token = state.get("token")?.as_str()?.to_string();
    if base_url.is_empty() || token.is_empty() {
        return None;
    }
    let (status, body) = http_json(
        &base_url,
        "GET",
        "/global/health",
        None,
        Some(&token),
        HEALTH_TIMEOUT,
    )
    .ok()?;
    if status != 200 {
        return None;
    }
    let health: serde_json::Value = serde_json::from_str(&body).ok()?;
    if health.get("healthy") != Some(&serde_json::Value::Bool(true)) {
        return None;
    }
    let version = health.get("version")?.as_str()?.to_string();
    if !version_at_least(&version, ARCHIVE_MIN_VERSION) {
        return None;
    }
    if version_at_least(&version, OPENCODE_V2_FLOOR) {
        eprintln!(
            "opencode serve reports {version}; the archive op is measured on 1.14.50 up to but not including 2.0.0, so it is skipped"
        );
        return None;
    }
    Some(ServeHandle {
        base_url,
        token,
        pid: state.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        pid_start: state
            .get("pid_start")
            .and_then(|v| v.as_u64())
            .filter(|start| *start > 0),
    })
}

/// Archive one session: stamp `time.archived` and read the STORED record back.
///
/// The PATCH response body is the server's own projection of a write it has
/// not necessarily persisted, so the second GET is the positive marker. An
/// `Err` here means the attempt could not be judged; a write that was accepted
/// and left the session unarchived answers [`ArchiveOutcome::Survived`].
pub fn archive_session(
    base_url: &str,
    token: &str,
    session_id: &str,
) -> Result<ArchiveOutcome, String> {
    let path = format!("/session/{session_id}");
    let (status, body) = http_json(base_url, "GET", &path, None, Some(token), HTTP_CALL_TIMEOUT)?;
    if status == 404 {
        return Ok(ArchiveOutcome::Gone);
    }
    if status != 200 {
        return Err(format!(
            "GET session answered {status}: {}",
            tail_reason(&body, status)
        ));
    }
    if is_archived(&body) {
        return Ok(ArchiveOutcome::AlreadyArchived);
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .map_err(|e| format!("clock before the epoch: {e}"))?;
    let (status, body) = http_json(
        base_url,
        "PATCH",
        &path,
        Some(&serde_json::json!({"time": {"archived": now_ms}})),
        Some(token),
        HTTP_CALL_TIMEOUT,
    )?;
    if status != 200 && status != 204 {
        return Err(format!(
            "PATCH archive answered {status}: {}",
            tail_reason(&body, status)
        ));
    }
    let (status, body) = http_json(base_url, "GET", &path, None, Some(token), HTTP_CALL_TIMEOUT)?;
    if status != 200 {
        return Err(format!(
            "archive readback answered {status}: {}",
            tail_reason(&body, status)
        ));
    }
    if is_archived(&body) {
        Ok(ArchiveOutcome::Archived)
    } else {
        Ok(ArchiveOutcome::Survived)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(dir: &Path) -> AgentsHome {
        AgentsHome::at(dir.to_path_buf())
    }

    // A registry row for the name would produce the "already exists" refusal
    // if the model gate ran after the lookup; the bare-model refusal must win
    // so no serve ever starts for a launch that cannot run a turn.
    #[test]
    fn bare_model_refuses_before_the_registry_gate() {
        let dir = tempfile::tempdir().unwrap();
        let h = home(dir.path());
        let reg = serde_json::json!({
            "schema_version": crate::state::REGISTRY_SCHEMA_VERSION,
            "agents": [{
                "name": "oc",
                "harness": "opencode",
                "provider": "opencode",
                "cwd": "/x",
                "status": "live",
                "created_at": "2026-01-01T00:00:00Z",
            }],
        });
        crate::registry_store::seed_raw(&h.registry_json(), reg.to_string());
        let out = dispatch_opencode_serve_inner(
            &h,
            "oc",
            "hi",
            "fno/test",
            dir.path(),
            Some("glm-5.3-flash"),
            None,
            None,
            &[],
            "fno-agents-worker",
        );
        assert_eq!(out.exit_code, 2);
        assert!(
            out.stderr.contains("provider/model"),
            "names the shape: {}",
            out.stderr
        );
        assert!(
            out.stderr.contains("zai-coding-plan/glm-5.3-flash"),
            "names the remedy: {}",
            out.stderr
        );
    }

    // A serve boot reached a healthy port at ~40s wall on a machine pinned at
    // 98.9% CPU; anything under 30s wedges spawns on loaded hosts again.
    #[test]
    fn boot_budget_absorbs_a_loaded_machine() {
        assert!(SERVE_BOOT_BUDGET.as_secs() >= 30);
    }

    #[test]
    fn port_scan_finds_the_measured_log_line() {
        let log = "INFO stuff\nwarn noise\nopencode server listening on http://127.0.0.1:59971\nlater lines";
        assert_eq!(serve_port_from_log(log), Some(59971));
        assert_eq!(serve_port_from_log("no port here"), None);
        assert_eq!(serve_port_from_log(""), None);
    }

    #[test]
    fn steering_readback_requires_a_message_id_after_high_water_mark() {
        let before = serde_json::json!([
            {"info": {"id": "msg_old"}, "parts": []}
        ]);
        let after = serde_json::json!([
            {"info": {"id": "msg_old"}, "parts": []},
            {"info": {"id": "msg_new"}, "parts": []}
        ]);

        assert_eq!(
            new_message_id_after(&after, &before).as_deref(),
            Some("msg_new")
        );
        assert_eq!(new_message_id_after(&before, &before), None);
    }

    use crate::opencode_permission::tests::Fake;

    #[test]
    fn unreadable_opencode_state_does_not_authorize_a_replacement() {
        use crate::harness_daemon::HarnessDaemonAdapter;

        let dir = tempfile::tempdir().unwrap();
        let h = home(dir.path());
        let adapter = OpenCodeDaemonAdapter::new(&h);

        assert!(!adapter.unreadable_state_may_boot());
    }

    #[test]
    fn dispatch_mints_the_session_with_rules_and_launches_the_turn_driver() {
        let dir = tempfile::tempdir().unwrap();
        let h = home(dir.path());
        let fake = Fake::start(vec![
            ("GET /global/health", "200 OK", r#"{"healthy":true}"#.into()),
            (
                "POST /session?",
                "200 OK",
                r#"{"id":"ses_dispatchtest1","directory":"/w"}"#.into(),
            ),
        ]);

        // Point ensure_serve at the fake by pre-writing the state file, so no
        // real opencode boots in a unit test.
        let record = serde_json::json!({
            "base_url": fake.url,
            "pid": 4242,
            "pid_start": 1,
            "token": "test-token",
        });
        std::fs::write(serve_state_path(&h), record.to_string()).unwrap();

        // A stub driver that records its argv beside the worker cwd: the seam
        // points the launch at it, so no real worker runs.
        let stub = crate::write_exec_stub(
            dir.path(),
            "driver-stub",
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > argv.txt\n",
        );

        let cwd = dir.path().join("w");
        std::fs::create_dir_all(&cwd).unwrap();
        let outcome = dispatch_opencode_serve_inner(
            &h,
            "wk-serve",
            "hello there",
            "op",
            &cwd,
            None,
            None,
            Some("x-aaaa"),
            &["/state/root".to_string()],
            &stub.to_string_lossy(),
        );

        assert_eq!(outcome.exit_code, 0, "stderr: {}", outcome.stderr);
        let receipt: serde_json::Value = serde_json::from_str(outcome.stdout.trim()).unwrap();
        assert_eq!(receipt["session_id"], "ses_dispatchtest1");
        assert_eq!(
            receipt["permission_posture"],
            "user grants; fno answers asks (thread_permission)"
        );
        let reg = load_registry(&h.registry_json()).unwrap();
        assert!(reg.find("wk-serve").is_none(), "the adopt owns the row");
        let pending = crate::launch_record::pending_for_provider(
            h.root(),
            "opencode",
            crate::claims::now_ms(),
        );
        assert_eq!(
            pending,
            vec!["wk-serve".to_string()],
            "the record holds the lane"
        );

        // The create body carries the worker rules: three denies and one
        // external_directory allow pair per writable dir.
        let seen = fake.seen.lock().unwrap();
        let (_, body) = seen
            .iter()
            .find(|(l, _)| l.starts_with("POST /session?directory="))
            .expect("fno mints the session");
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["title"], "wk-serve");
        let rules = body["permission"].as_array().unwrap();
        let denied: Vec<&str> = rules
            .iter()
            .filter(|r| r["action"] == "deny" && r["pattern"] == "*")
            .filter_map(|r| r["permission"].as_str())
            .collect();
        assert_eq!(denied, vec!["question", "plan_enter", "plan_exit"]);
        assert!(
            !body.to_string().contains("\"*\":\"allow\""),
            "no allow-all rides a session"
        );
        let allows: Vec<&str> = rules
            .iter()
            .filter(|r| r["permission"] == "external_directory")
            .filter_map(|r| r["pattern"].as_str())
            .collect();
        assert!(allows.contains(&"/state/root/*"), "{allows:?}");
        assert!(allows.iter().any(|p| p.ends_with("/w/*")), "{allows:?}");
        drop(seen);

        // The launch is detached, so the stub may not have written its file yet.
        let argv_path = cwd.join("argv.txt");
        for _ in 0..100 {
            if std::fs::metadata(&argv_path).is_ok_and(|m| m.len() > 0) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let launched = std::fs::read_to_string(&argv_path).unwrap();
        let lines: Vec<&str> = launched.lines().collect();
        assert_eq!(lines[0], "--opencode-turn", "the driver lane: {launched}");
        assert!(
            !launched.contains("opencode run") && !launched.contains("dangerously"),
            "no opencode writer and no skip flag: {launched}"
        );
        let spec: crate::opencode_turn::TurnSpec =
            serde_json::from_str(&std::fs::read_to_string(lines[1]).unwrap()).unwrap();
        assert_eq!(spec.session_id, "ses_dispatchtest1");
        assert_eq!(spec.worker.as_deref(), Some("wk-serve"));
        assert!(spec.message.contains("hello there"));
        assert_eq!(spec.model.as_deref(), Some(OPENCODE_DEFAULT_MODEL));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(lines[1]).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the spec holds the token: owner-only");
    }

    #[test]
    fn dispatch_refuses_name_collision() {
        let dir = tempfile::tempdir().unwrap();
        let h = home(dir.path());
        std::fs::create_dir_all(h.registry_json().parent().unwrap()).unwrap();
        std::fs::write(
            &h.registry_json(),
            serde_json::json!({
                "schema_version": crate::state::REGISTRY_SCHEMA_VERSION,
                "agents": [{
                    "name": "wk-taken",
                    "harness": "opencode",
                    "cwd": "/x",
                    "status": "live",
                    "created_at": "2026-01-01T00:00:00Z",
                }],
            })
            .to_string(),
        )
        .unwrap();
        let out = dispatch_opencode_serve_inner(
            &h,
            "wk-taken",
            "m",
            "op",
            Path::new("/tmp"),
            None,
            None,
            None,
            &[],
            "fno-agents-worker",
        );
        assert_eq!(out.exit_code, 2);
    }
}

#[cfg(test)]
#[path = "opencode_archive_tests.rs"]
mod opencode_archive_tests;
