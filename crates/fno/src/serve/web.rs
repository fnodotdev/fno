//! `fno mux serve --web` : the web bridge.
//!
//! A pure client. It attaches to a running mux session over the same per-session
//! unix socket the native TUI uses, as an OBSERVER (`Attach { rows: 0, cols: 0 }`,
//! which the server marks passive: excluded from the smallest-client clamp, and
//! fed EVERY pane's frames so a browser can view any pane without an upstream
//! message). It re-fans the `ServerMsg` broadcast to browser WebSocket
//! connections as JSON, unmodified. The browser paints the structured cells
//! directly (see `web_page.html`).
//!
//! The pane view is read-only by construction (Locked Decision 5): after sending
//! `Attach` the bridge `forget()`s the socket's write half, so no code path can
//! forward a browser byte upstream. The browser also never drives - it drops
//! every inbound WS message and only picks which already-arriving frame to draw
//! locally. The backlog board's writes are separate: guarded subprocess runs of
//! the same `fno` commands an agent runs (see `write_guard`), never socket input.
//!
//! Data flow, one direction only:
//!   vt::Pane --composite--> Frame --broadcast--> bridge --WS/JSON--> browser

use crate::pid::{Pid, SignalResult};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::backlog_model;
use crate::client::humanize_ago;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Json, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use tokio::net::unix::OwnedReadHalf;
use tokio::net::{TcpListener, UnixStream};
use tokio::sync::{broadcast, oneshot};

use crate::proto::{self, ClientMsg, ObserverHello, ServerMsg, BUILD_VERSION, PROTO_VERSION};
use crate::trusted_config::TrustedFile;
use crate::web_invites::{Grants, InviteSource};

/// The served page, vendored inline (no CDN) so the strict CSP holds offline.
pub(crate) const PAGE: &str = include_str!("web_page.html");
pub(crate) use crate::web_core::{
    house, recorded_pid_is_live, tcp_answers, web_state_path, BACKLOG_PAGE,
};
#[cfg(test)]
pub(crate) use crate::web_core::{print_pane_url, LiveBridge};
pub(crate) const HQ_PAGE: &str = include_str!("web_hq.html");
const TRACE_PAGE: &str = include_str!("web_trace.html");
/// The browser drives nothing, so anything it sends is dropped - but cap it so
/// a hostile client cannot OOM the bridge with one giant frame.
const INBOUND_WS_CAP: usize = 64 * 1024;
/// The longest an open feed goes without reading the invites again, so a
/// revoke ends it inside this bound.
const FEED_RECHECK: Duration = Duration::from_secs(1);
/// Per-subscriber buffered frames before a slow phone starts lag-dropping stale
/// ones (each browser drops independently; one slow viewer never blocks another).
const BROADCAST_CAP: usize = 256;
/// Max panes retained in the replay snapshot. Pane ids are monotonic and never
/// reused, and the wire has no "pane closed" signal, so a dead pane's last frame
/// would otherwise linger forever. Bounded by evicting the least-recently-updated
/// pane - a dead pane stops updating, so it ages out first (ponytail: fixed cap;
/// a proto-level pane-closed signal is the real fix, deferred with Locked 4).
const MAX_SNAPSHOT_PANES: usize = 128;
/// Reconnect backoff bounds (Errors: preserve the view on upstream EOF).
const BACKOFF_START: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(5);

/// Connect bound for the upstream attach. A wedged server (never accepts)
/// turns into a reconnect-with-backoff instead of blocking the bridge task
/// forever mid-loop.
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// A connection must stay up at least this long before its drop resets the
/// backoff. An accept-then-EOF flap stays below it, so the backoff keeps growing
/// (and each quick drop logs) instead of spinning a silent 250ms reconnect loop.
const MIN_HEALTHY_UPTIME: Duration = Duration::from_secs(2);

/// Reconnect pacing for an observer attach, shared by every bridge that
/// attaches: doubling from `BACKOFF_START` to `BACKOFF_MAX`, and reset only by
/// a connection that stayed up `MIN_HEALTHY_UPTIME`.
pub(crate) struct Backoff(Duration);

impl Backoff {
    pub(crate) fn new() -> Self {
        Backoff(BACKOFF_START)
    }

    /// Note a dropped connection. True for a quick accept-then-EOF flap,
    /// which keeps the delay growing and is worth a log line.
    pub(crate) fn dropped(&mut self, uptime: Duration) -> bool {
        if uptime >= MIN_HEALTHY_UPTIME {
            self.0 = BACKOFF_START;
            false
        } else {
            true
        }
    }

    pub(crate) async fn wait(&mut self) {
        tokio::time::sleep(self.0).await;
        self.0 = (self.0 * 2).min(BACKOFF_MAX);
    }
}

/// Parsed `fno mux serve --web` arguments.
#[derive(Debug, PartialEq, Eq)]
pub struct WebArgs {
    pub session: String,
    /// Bind address; loopback by default (Locked Decision 6). `--bind` widens it;
    /// remote reach is delegated to tailscale / a reverse proxy, not in-process TLS.
    pub bind: String,
    pub port: u16,
    /// `--stop`: kill the running bridge for this session instead of serving
    /// one. The state file names the pid; the pid's start token names the
    /// process.
    pub stop: bool,
    /// `--status`: read the state file and report who is serving (pid, bind,
    /// port, binary, build rev, start time, launching session id), flagging a
    /// bridge whose build rev predates the installed binary.
    pub status: bool,
    /// `--attention-api`: also serve the attention answer endpoint on its own
    /// loopback port (`--attention-port`, default 8724). Own router, never
    /// inside the read-only bridge.
    pub attention_api: bool,
    pub attention_port: u16,
}

impl Default for WebArgs {
    fn default() -> Self {
        WebArgs {
            session: proto::DEFAULT_SESSION.to_string(),
            bind: "127.0.0.1".to_string(),
            port: 8722,
            stop: false,
            status: false,
            attention_api: false,
            attention_port: crate::attention_api::DEFAULT_ATTENTION_PORT,
        }
    }
}

/// The latest server state, replayed to each freshly-connected browser so it
/// paints immediately instead of waiting for the next broadcast.
#[derive(Default)]
struct Snapshot {
    upstream_up: bool,
    /// Latest `Layout` JSON (the pane/agent catalog for the picker).
    layout: Option<String>,
    /// pane_id -> (update seq, latest `Frame` JSON). The seq drives LRU eviction
    /// so a dead pane (which stops updating) ages out of the replay set first.
    frames: HashMap<u64, (u64, String)>,
    frame_seq: u64,
}

#[derive(Clone)]
struct AppState {
    tx: broadcast::Sender<String>,
    snap: Arc<Mutex<Snapshot>>,
    token: Arc<str>,
    rundown_html: PathBuf,
    /// False when the state root came from a config form this mirror cannot
    /// expand (a template, a `~user` or relative anchor, an unset `$VAR`):
    /// /rundown then never republishes through the fallback path, so a
    /// project-isolated bridge cannot overwrite the global page.
    rundown_republish: bool,
    hq: Arc<crate::web_hq::HqCache>,
    /// True when the bound address is loopback, so the backlog page may act.
    /// Computed from the bound address, never from `--bind` text.
    writable: bool,
    /// Where `[[web.invites]]` is read per request. [`invite_gate`] hands the
    /// handler the read it decided on.
    invites: Arc<InviteSource>,
    /// Where a direct-local click lands. `None` records nothing.
    click_journal: Option<PathBuf>,
    /// The cached backlog read model (`None` until the first gather).
    model: Arc<tokio::sync::Mutex<Option<CachedModel>>>,
    /// Fires on Ctrl-C so every ws loop ends and axum's graceful shutdown can
    /// complete: an open browser tab holds a connection that never closes on
    /// its own, so without this arm the bridge hangs past the signal and the
    /// state-file Drop the hook exists to guarantee never runs.
    shutdown: tokio::sync::watch::Receiver<bool>,
}

/// One gathered read model, reused while fresh and the store version holds.
struct CachedModel {
    version: Option<i64>,
    at: Instant,
    inputs: Arc<backlog_model::Inputs>,
}

/// The rundown page path plus whether its root resolved FAITHFULLY (see
/// [`crate::org_root::lead_state_root`]): an unfaithful root is served as a
/// miss but never written through.
fn rundown_html_path() -> (PathBuf, bool) {
    #[cfg(not(test))]
    {
        let (root, faithful) = crate::org_root::lead_state_root();
        let page = crate::state_layout::place_new(&root, "rundown.html");
        let found = crate::state_layout::place(&root, "rundown.html");
        // The bridge serves and republishes one path, the home. A page from
        // before the move seeds it, so the first view is not empty.
        if found != page && found.exists() {
            let seeded = page
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::copy(&found, &page).map(|_| ()));
            if let Err(e) = seeded {
                eprintln!("fno mux serve --web: {} starts empty ({e})", page.display());
            }
        }
        (page, faithful)
    }
    #[cfg(test)]
    {
        let graph = crate::backlog_view::graph_path();
        let root = graph
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        (crate::state_layout::place_new(&root, "rundown.html"), true)
    }
}

/// The bridge's live-state marker: `web-<session>.json` beside the
/// session socket, holding the bind/port/token the bind-time print showed
/// once. Written 0600 (the token is the only URL guard); removed by `Drop`
/// on every exit path. A SIGKILLed bridge leaves it behind - the reader
/// (`web::print_pane_url`) trusts the URL only while the recorded pid is
/// live AND the port answers, so a corpse file reads as "no bridge" and a
/// stranger that took the dead bridge's port never reads as the bridge.
/// `serve --web --stop` is the other reader: it kills the recorded pid, and
/// the `started` token is what lets it refuse a recycled pid instead of
/// signalling a stranger.
struct WebStateFile(PathBuf);

impl WebStateFile {
    fn write(socket: &Path, bind: &str, port: u16, token: &str) -> Option<Self> {
        let path = web_state_path(socket)?;
        // `pid` makes the file's ownership checkable: two bridges may share a
        // session on different ports, the later bind owns the file, and an
        // exiting OLDER bridge must not delete the newer one's state (codex P2).
        // `started` is the pid's start-time token, so `--stop` can tell the
        // bridge it names from a process that later reused the pid.
        // `bin`/`rev`/`started_at`/`session` answer "who started this and how
        // old is its build": rev is the crates/ subtree rev this binary
        // baked, so --status can flag a bridge running pre-deploy code.
        let body = serde_json::json!({
            "bind": bind,
            "port": port,
            "token": token,
            "pid": std::process::id(),
            "started": proto::pid_start_time(std::process::id()),
            "bin": std::env::current_exe()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "unknown".into()),
            "rev": env!("FNO_MUX_CRATES_REV"),
            "started_at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            "session": launching_session_id(),
        });
        let wrote = {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .and_then(|mut f: std::fs::File| f.write_all(body.to_string().as_bytes()))
        };
        match wrote {
            Ok(()) => Some(WebStateFile(path)),
            Err(e) => {
                eprintln!("fno mux serve --web: cannot record bridge state: {e}");
                None
            }
        }
    }
}

impl Drop for WebStateFile {
    fn drop(&mut self) {
        // Remove only the file that still names THIS process. A newer bridge
        // for the same session overwrote it at its own bind; deleting that
        // one would make a live bridge read as absent.
        let still_ours = std::fs::read_to_string(&self.0)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|v| v.get("pid").and_then(|p| p.as_u64()))
            == Some(std::process::id() as u64);
        if still_ours {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

/// How long a SIGINT'd bridge gets to exit before `--stop` escalates to
/// SIGKILL. SIGINT is the bridge's own Ctrl-C path (browser sockets close,
/// the state file's Drop runs), so a healthy bridge is well inside this; a
/// wedged one is why the rung exists.
const WEB_STOP_GRACE: Duration = Duration::from_secs(3);

/// `serve --web --stop`: kill the running bridge for a session. The state
/// file names the pid; the pid's start token names the process. Exit 0 for
/// every "nothing to stop" shape - no state file, a corpse file, a recycled
/// pid - so an "ensure stopped" script converges, and 1 only for a failure
/// to act (unreadable state, a refused signal).
fn stop_web(session: &str, socket: &Path) -> i32 {
    let Some(state) = web_state_path(socket) else {
        eprintln!("fno mux serve --web: cannot place the bridge state beside {socket:?}");
        return 1;
    };
    let raw = match std::fs::read_to_string(&state) {
        Ok(r) => r,
        Err(_) => {
            println!("no web bridge recorded for session {session:?}");
            return 0;
        }
    };
    let v: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("fno mux serve --web: bridge state is not JSON: {e}");
            return 1;
        }
    };
    let Some(pid) = v
        .get("pid")
        .and_then(|p| p.as_u64())
        .and_then(|p| Pid::try_from(p).ok())
    else {
        // A state file that names no pid cannot be stopped or validated;
        // it is a corpse by definition.
        let _ = std::fs::remove_file(&state);
        println!(
            "no web bridge recorded for session {session:?} (state file named no pid; removed)"
        );
        return 0;
    };
    // Identity: a dead or zombie pid is gone whatever the file says, and a
    // recorded start token that no longer matches proves the pid was reused
    // after the bridge died - signalling it would hit a stranger.
    let recorded = v.get("started").and_then(|s| s.as_u64());
    let stale = proto::pid_confirmed_dead(pid.as_raw())
        || proto::pid_is_zombie(pid.as_raw())
        || recorded.is_some_and(|r| pid.start_time().is_none_or(|now| now != r));
    if stale {
        let _ = std::fs::remove_file(&state);
        println!("no live web bridge for session {session:?} (stale state file removed)");
        return 0;
    }
    // SIGINT, not SIGTERM: the bridge's graceful-shutdown arm listens for
    // Ctrl-C (tokio::signal::ctrl_c), so the same signal closes every browser
    // socket, lets the state file's Drop run, and a SIGTERM would bypass all
    // of it.
    if let SignalResult::Failed(e) = pid.signal(libc::SIGINT) {
        eprintln!("fno mux serve --web: cannot signal pid {pid}: {e}");
        return 1;
    }
    // The pid's start token at signal time; if it stops matching during the
    // grace window, the bridge was reaped and the pid reused - the bridge is
    // gone, and the newcomer must not inherit the escalation.
    let signalled_start = pid.start_time();
    let deadline = Instant::now() + WEB_STOP_GRACE;
    while Instant::now() < deadline {
        let gone = proto::pid_confirmed_dead(pid.as_raw())
            || proto::pid_is_zombie(pid.as_raw())
            || signalled_start.is_some_and(|s| pid.start_time().is_none_or(|now| now != s));
        if gone {
            println!("web bridge for session {session:?} stopped (pid {pid})");
            return 0;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    pid.signal(libc::SIGKILL);
    println!(
        "web bridge for session {session:?} ignored SIGINT; killed (pid {pid}); \
         the state file it could not remove reads as no bridge"
    );
    0
}

// ---------------------------------------------------------------------------
// --status and port-collision naming
// ---------------------------------------------------------------------------

/// The launching agent's harness session id, when the environment carries one.
/// `FNO_HARNESS_SESSION_ID` is what fno stamps; the rest are the per-harness
/// fallbacks the claim system recognizes.
fn launching_session_id() -> Option<String> {
    [
        "FNO_HARNESS_SESSION_ID",
        "CLAUDE_SESSION_ID",
        "CODEX_SESSION_ID",
        "GEMINI_SESSION_ID",
        "OPENCODE_SESSION_ID",
    ]
    .iter()
    .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
}

/// True when a bridge was built from a different, known crates/ rev than the
/// binary this command runs from, i.e. it is serving pre-deploy code. "unknown"
/// revs (a hand-built or stripped binary) are never called stale - we cannot
/// know, so we do not claim to.
fn rev_is_stale(recorded: &str) -> bool {
    recorded != "unknown" && recorded != env!("FNO_MUX_CRATES_REV")
}

/// Parse a `web-*.json` state file into (session, pid, port, bind, rev,
/// started_at, bin, launched-by). None for an unreadable or non-JSON file.
fn read_state_record(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Every live bridge recorded beside this mux's sockets: (session, record).
/// Same-dir scan only: sockets of one server live here, so this sees every
/// bridge that server's sessions started. A bridge under a DIFFERENT server's
/// socket dir is out of sight (ponytail: the collision note says so).
fn live_bridge_records(socket_dir: &Path) -> Vec<(String, serde_json::Value)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(socket_dir) else {
        return found;
    };
    let mut names: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    names.sort();
    for path in names {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(session) = name
            .strip_prefix("web-")
            .and_then(|s| s.strip_suffix(".json"))
        else {
            continue;
        };
        if let Some(v) = read_state_record(&path) {
            if recorded_pid_is_live(&v) {
                found.push((session.to_string(), v));
            }
        }
    }
    found
}

/// `serve --status`: report who is serving for a session - pid, bind, port,
/// binary, build rev, start time, launching session id - flagging a bridge
/// whose rev predates the installed binary. Exit 0 live, 1 none.
fn status_web(session: &str, socket: &Path) -> i32 {
    let Some(state) = web_state_path(socket) else {
        eprintln!("fno mux serve --web: cannot resolve the state-file path");
        return 1;
    };
    let record = read_state_record(&state).filter(|v| {
        // A live record owns the file; a stale one is a corpse from a killed
        // bridge (stop removes those; status only refuses to report it).
        recorded_pid_is_live(v)
    });
    let Some(v) = record else {
        println!("no live web bridge for session {session:?}");
        return 1;
    };
    let pid = v.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
    let bind = v.get("bind").and_then(|b| b.as_str()).unwrap_or("unknown");
    let port = v.get("port").and_then(|p| p.as_u64()).unwrap_or(0) as u16;
    println!("web bridge for session {session:?}: live (pid {pid})");
    println!("  bind:    {bind}:{port}");
    println!(
        "  binary:  {}",
        v.get("bin").and_then(|b| b.as_str()).unwrap_or("unknown")
    );
    let rev = v.get("rev").and_then(|r| r.as_str()).unwrap_or("unknown");
    if rev_is_stale(rev) {
        println!(
            "  build:   {rev} - STALE, installed build is {}; restart the bridge to serve current code",
            env!("FNO_MUX_CRATES_REV")
        );
    } else {
        println!("  build:   {rev}");
    }
    if let Some(at) = v.get("started_at").and_then(|s| s.as_u64()) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        println!(
            "  started: {at} ({} ago)",
            humanize_ago(now.saturating_sub(at))
        );
    }
    println!(
        "  started by session: {}",
        v.get("session")
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("(none recorded)")
    );
    if !tcp_answers(bind, port) {
        println!("  note:    port {port} is not answering (bridge may be mid-start or wedged)");
    }
    // Sibling sessions: the measured pain was TWO bridges nobody could find.
    // One line each for the other sessions this server's socket dir records.
    let dir = socket.parent().map(|p| p.to_path_buf());
    if let Some(dir) = dir {
        for (other, r) in live_bridge_records(&dir) {
            if other == session {
                continue;
            }
            println!(
                "  also live: session {other:?} pid {} port {}",
                r.get("pid").and_then(|p| p.as_u64()).unwrap_or(0),
                r.get("port").and_then(|p| p.as_u64()).unwrap_or(0)
            );
        }
    }
    0
}

/// On a taken port, name the recorded bridge holding it, so a second start
/// points at the process to stop instead of a bare EADDRINUSE. Only state
/// files beside THIS session's socket are checked; a port held by a bridge
/// under another server's socket dir (or by a non-fno process) falls through
/// to the generic error.
fn port_holder_note(socket: &Path, port: u16) {
    let Some(dir) = socket.parent() else { return };
    let holders: Vec<_> = live_bridge_records(dir)
        .into_iter()
        .filter(|(_, r)| r.get("port").and_then(|p| p.as_u64()) == Some(port as u64))
        .collect();
    if holders.is_empty() {
        eprintln!(
            "  no recorded web bridge names port {port}; another process likely holds it \
             (try: lsof -i :{port})"
        );
        return;
    }
    for (session, r) in holders {
        let pid = r.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
        let who = r
            .get("session")
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| format!("started by session {s}"))
            .unwrap_or_else(|| "starter session not recorded".into());
        eprintln!(
            "  port {port} is held by the web bridge of session {session:?} (pid {pid}, {who}); \
             stop it with: fno mux serve --stop --server {session}"
        );
    }
}

/// Entry point for the `mux serve --web` role. Owns its own runtime like the
/// server role; returns the process exit code.
pub fn serve(args: WebArgs) -> i32 {
    let socket = match proto::socket_path(&args.session) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("fno mux serve --web: {e}");
            return 2;
        }
    };
    if args.status {
        return status_web(&args.session, &socket);
    }
    if args.stop {
        return stop_web(&args.session, &socket);
    }
    // Taken before anything can change the cwd. The invite hold and the sink
    // notice compare this folder. An unreadable one has no project file.
    let startup = std::env::current_dir().unwrap_or_default();
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("fno mux serve --web: cannot start runtime: {e}");
            return 1;
        }
    };
    runtime.block_on(run(args, socket, startup))
}

async fn run(args: WebArgs, socket: PathBuf, startup: PathBuf) -> i32 {
    let token: Arc<str> = match mint_token() {
        Some(t) => t.into(),
        None => {
            eprintln!("fno mux serve --web: cannot read /dev/urandom to mint an auth token");
            return 1;
        }
    };

    let (tx, _rx0) = broadcast::channel::<String>(BROADCAST_CAP);
    let snap: Arc<Mutex<Snapshot>> = Arc::new(Mutex::new(Snapshot::default()));
    let (ready_tx, ready_rx) = oneshot::channel::<Result<(), String>>();

    // Attach upstream FIRST, then accept browsers (Concurrency: a browser that
    // connects before we are attached gets the disconnected banner, never a
    // half-open stream). The upstream task owns reconnect-with-backoff.
    {
        let (tx, snap, socket) = (tx.clone(), snap.clone(), socket.clone());
        tokio::spawn(async move { upstream_loop(socket, tx, snap, ready_tx).await });
    }
    match ready_rx.await {
        Ok(Ok(())) => {}
        // Startup failure (no server / refused attach): fail loud, and note that
        // NO HTTP listener was ever opened (AC1-ERR).
        Ok(Err(e)) => {
            eprintln!("fno mux serve --web: {e}");
            return 1;
        }
        Err(_) => {
            eprintln!("fno mux serve --web: upstream task exited before attaching");
            return 1;
        }
    }

    let addr = bind_addr(&args.bind, args.port);
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!("fno mux serve --web: cannot bind {addr}: {e}");
            port_holder_note(&socket, args.port);
            return 1;
        }
        Err(e) => {
            eprintln!("fno mux serve --web: cannot bind {addr}: {e}");
            return 1;
        }
    };

    let wide = args.bind == "0.0.0.0" || args.bind == "::";
    let host = if wide {
        "<this-host>"
    } else {
        args.bind.as_str()
    };
    // Writes ride the bound address, not the `--bind` text: `localhost` and
    // `::1` count, `0.0.0.0`, `::` and a tailscale address do not.
    let writable = listener.local_addr().is_ok_and(|a| a.ip().is_loopback());
    println!(
        "fno mux web ({}): http://{host}:{}/?t={}",
        if writable {
            "backlog writes on, loopback only"
        } else {
            "read-only"
        },
        args.port,
        token
    );
    if wide {
        println!(
            "  bound to all interfaces - reach it over tailscale/LAN; the URL token is the only guard."
        );
    }
    // Record the live bridge so `mux view <selector> --url` can
    // recover the URL after this one print. A write failure only costs the
    // --url door (it reports "no web bridge"), never the bridge itself; a
    // file left behind by a killed bridge is inert because the reader probes
    // the port before trusting it. Removed on every exit path via Drop.
    let _state_guard = WebStateFile::write(&socket, &args.bind, args.port, &token);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let (rundown_html, rundown_republish) = rundown_html_path();
    let state = AppState {
        tx,
        snap,
        token,
        rundown_html,
        rundown_republish,
        hq: Default::default(),
        writable,
        invites: InviteSource::for_bridge(TrustedFile::from_env(), &startup).into(),
        click_journal: crate::cli_usage::agents_journal(),
        model: Default::default(),
        shutdown: shutdown_rx,
    };
    let app = router(state);

    // The attention answer endpoint rides the serve process on its own
    // loopback router and port, never inside the read-only bridge.
    if args.attention_api {
        let port = args.attention_port;
        tokio::spawn(async move {
            if let Err(e) = crate::attention_api::serve(port, startup).await {
                eprintln!("fno mux serve --web: attention api: {e}");
            }
        });
    }

    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            // Ctrl-C is the NORMAL way a bridge ends. Without this hook the
            // process dies straight to the signal, no Drop runs, and the
            // state file outlives its bridge. Firing the watch makes
            // every ws loop close its tab first, so graceful shutdown can
            // actually complete instead of waiting out an open connection.
            let _ = tokio::signal::ctrl_c().await;
            let _ = shutdown_tx.send(true);
        })
        .await
    {
        eprintln!("fno mux serve --web: server error: {e}");
        return 1;
    }
    0
}

fn router(state: AppState) -> Router {
    let routes = Router::new()
        .route("/", get(page))
        .route("/backlog", get(backlog))
        .route("/backlog/model.json", get(backlog_model))
        .route("/backlog/node.json", get(backlog_node))
        .route("/backlog/act", post(backlog_act))
        .route("/rundown", get(team))
        .route("/team", get(team))
        .route("/hq", get(hq))
        .route("/hq/model.json", get(hq_model))
        .route("/hq/decisions.json", get(hq_decisions))
        .route("/hq/answer/{id}", post(hq_answer))
        .route("/hq/mine.json", get(hq_mine))
        .route("/hq/mine/look", post(hq_mine_look))
        .route("/command", get(command_redirect))
        .route("/command/model.json", get(command_model_redirect))
        .route("/command/answer/{id}", post(command_answer_redirect))
        .route("/trace", get(trace))
        .route("/trace/model.json", get(trace_model))
        .route("/ui/click", post(ui_click))
        .route("/ws", get(ws_handler))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            invite_gate,
        ))
        .with_state(state);
    routes.clone().nest(
        // The board answers at the root and under the /fno prefix: a
        // tailscale route that maps a subpath (the Oct 18 report's
        // /fno/backlog URL shape) reaches the same routes instead of 404.
        "/fno", routes,
    )
}

/// Every request needs at least a viewer invite when `web.invites` is set.
/// Per-route checks would let a new route slip past, so this one layer covers
/// them all. With no invite list the layer passes and the token rules stand.
/// A handler reads the grants decided here, so one request sees one list.
async fn invite_gate(
    State(st): State<AppState>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let grants = st.invites.read();
    match viewer(req.headers(), &st, &grants) {
        Ok(_) => {
            req.extensions_mut().insert(grants);
            next.run(req).await
        }
        Err(r) => refused(&r),
    }
}

/// The caller's name when `grants` let them read at all.
fn viewer(
    headers: &HeaderMap,
    st: &AppState,
    grants: &Grants,
) -> Result<String, crate::web_invites::Refusal> {
    crate::web_invites::decide(
        &caller_of(headers, st.writable),
        grants.list(),
        None,
        crate::web_invites::Role::Viewer,
    )
}

fn refused(r: &crate::web_invites::Refusal) -> Response {
    plain_status(
        StatusCode::from_u16(r.code).unwrap_or(StatusCode::FORBIDDEN),
        &r.text,
    )
}

/// The facts of one request that the invite decision reads.
pub(crate) fn caller_of(
    headers: &HeaderMap,
    loopback_bind: bool,
) -> crate::web_invites::Caller<'_> {
    crate::web_invites::Caller {
        // A value that is no text still says a proxy sent it. It names nobody.
        login: headers
            .get("tailscale-user-login")
            .map(|v| v.to_str().unwrap_or_default()),
        loopback_bind,
        // A proxy can rewrite Host, but tailscale serve and every common
        // reverse proxy add a forwarding header, so one of those marks the
        // request as proxied whatever Host says.
        loopback_host: !["x-forwarded-for", "x-forwarded-host", "forwarded"]
            .iter()
            .any(|h| headers.contains_key(*h))
            && headers
                .get(header::HOST)
                .and_then(|h| h.to_str().ok())
                .is_some_and(names_loopback),
    }
}

/// How much of the fold one request's answer may carry. invite_gate has
/// already admitted the request; this decides the read side, whose routes
/// serve whole boards a middleware could only refuse.
enum ReadScope {
    /// Every node: the local user, a token-only read with no invite list, or
    /// any invite with scope `*`.
    All,
    /// Only nodes whose chain carries one of these invite scopes.
    Scopes(Vec<String>),
}

impl ReadScope {
    /// True when the request came from a viewer who needs a filtered fold.
    fn is_scoped(&self) -> bool {
        matches!(self, ReadScope::Scopes(_))
    }

    /// True when a node whose chain is `chain` sits inside the viewer's
    /// invites. `chain_of` walks parents, so a scope on an epic covers the
    /// epic's descendants.
    fn covers(&self, chain: &[String]) -> bool {
        match self {
            ReadScope::All => true,
            ReadScope::Scopes(scopes) => scopes.iter().any(|s| chain.iter().any(|n| n == s)),
        }
    }
}

/// The read scope of one request. A proxied caller with no login cannot
/// exist past invite_gate, so the fallthrough there fails closed.
fn read_scope(headers: &HeaderMap, st: &AppState, grants: &Grants) -> ReadScope {
    let Some(invites) = grants.list() else {
        return ReadScope::All; // no invite list: token-only reads stay whole
    };
    let caller = caller_of(headers, st.writable);
    // The user at their own machine owns every scope (decide's first rule).
    if caller.is_direct_local() {
        return ReadScope::All;
    }
    let Some(login) = caller.identity() else {
        return ReadScope::Scopes(Vec::new());
    };
    let mine = invites.iter().filter(|i| i.email == login);
    if mine.clone().any(|i| i.scope == "*") {
        return ReadScope::All;
    }
    ReadScope::Scopes(mine.map(|i| i.scope.clone()).collect())
}

/// 403 when a scoped viewer names a node outside their invites.
fn scope_refusal(scope: &ReadScope, chain: &[String]) -> Option<Response> {
    if scope.covers(chain) {
        None
    } else {
        Some(plain_status(
            StatusCode::FORBIDDEN,
            "this invite does not cover that node",
        ))
    }
}

/// A scoped viewer's hq board: sections keyed by node keep the chain's rows,
/// a row with no node drops out, and the fleet grid (hosts, seats, sessions:
/// machine-wide, not node data) drops whole. The cache is untouched, so the
/// owner's page still reads the whole fold.
fn scope_hq_model(
    mut model: serde_json::Value,
    scope: &ReadScope,
    rows: &[crate::node_row::NodeRow],
) -> serde_json::Value {
    let Some(obj) = model.as_object_mut() else {
        return model;
    };
    obj.insert("machines".to_string(), serde_json::json!([]));
    let Some(waiting) = obj.get_mut("waiting") else {
        return model;
    };
    for key in ["questions", "prs", "next"] {
        let Some(items) = waiting
            .get_mut(key)
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        items.retain(|item| {
            // prs and questions key the node `node`; next rows key it `id`.
            // One typed parse per item (R1): the compiler checks the keys.
            let node = serde_json::from_value::<ScopedItemNode>(item.clone())
                .ok()
                .and_then(|keys| keys.node.or(keys.id));
            match node {
                Some(n) => scope.covers(&crate::web_answer::chain_of(rows, Some(&n))),
                None => false,
            }
        });
    }
    model
}

/// The node key one hq waiting item carries. Both fields are optional: a
/// next row names `id`, a pr or question row names `node`, and an item
/// naming neither drops out.
#[derive(serde::Deserialize)]
struct ScopedItemNode {
    #[serde(default)]
    node: Option<String>,
    #[serde(default)]
    id: Option<String>,
}

/// Keep only the rows a scoped viewer's invites cover, decided against the
/// full row set so parent walks still resolve. The board folds lanes,
/// offboard and stats from these rows alone.
fn filter_scoped_rows(inputs: &mut backlog_model::Inputs, scope: &ReadScope) {
    let keep: Vec<bool> = inputs
        .rows
        .iter()
        .map(|row| scope.covers(&crate::web_answer::chain_of(&inputs.rows, row.id())))
        .collect();
    let mut keep = keep.into_iter();
    inputs.rows.retain(|_| keep.next().unwrap_or(false));
}

// ---------------------------------------------------------------------------
// Upstream: attach as an observer, forward frames, reconnect on EOF
// ---------------------------------------------------------------------------

async fn upstream_loop(
    socket: PathBuf,
    tx: broadcast::Sender<String>,
    snap: Arc<Mutex<Snapshot>>,
    ready_tx: oneshot::Sender<Result<(), String>>,
) {
    let mut ready_tx = Some(ready_tx);
    let mut backoff = Backoff::new();
    loop {
        match connect_attach(&socket, None).await {
            Ok((reader, preamble)) => {
                let started = Instant::now();
                if let Some(rt) = ready_tx.take() {
                    let _ = rt.send(Ok(()));
                }
                snap.lock().unwrap().upstream_up = true;
                let _ = tx.send(bridge_status("connected"));
                forward(preamble, &tx, &snap);
                read_stream(reader, &tx, &snap).await;
                // Upstream dropped: mark stale so the last frame under the amber
                // banner is never presented as live (Errors invariant).
                snap.lock().unwrap().upstream_up = false;
                let _ = tx.send(bridge_status("disconnected"));
                // Only a session that stayed up a while resets the backoff; a
                // quick accept-then-EOF flap keeps growing it and logs, so a
                // misbehaving server never spins a silent tight reconnect loop.
                if backoff.dropped(started.elapsed()) {
                    eprintln!(
                        "fno mux serve --web: upstream dropped after {:?}; backing off",
                        started.elapsed()
                    );
                }
            }
            Err(e) => {
                // First attempt failing is a startup error the caller reports and
                // exits on. A later failure just retries - the browser already
                // shows the disconnected banner.
                if let Some(rt) = ready_tx.take() {
                    let _ = rt.send(Err(e));
                    return;
                }
                eprintln!("fno mux serve --web: upstream reconnect failed: {e}");
            }
        }
        backoff.wait().await;
    }
}

/// Connect, send the observer `Attach` (and the observer's own `hello` when it
/// names itself), relinquish the write half, and read the first reply. A `Bye`
/// here is a refused/skewed attach (`Err`); anything else means the attach
/// took, and the message is returned as preamble to forward.
pub(crate) async fn connect_attach(
    socket: &Path,
    hello: Option<ObserverHello>,
) -> Result<(OwnedReadHalf, ServerMsg), String> {
    let stream = tokio::time::timeout(UPSTREAM_CONNECT_TIMEOUT, UnixStream::connect(socket))
        .await
        .map_err(|_| {
            format!(
                "cannot connect to session socket {}: connect timed out (wedged server?)",
                socket.display()
            )
        })?
        .map_err(|e| {
            format!(
                "cannot connect to session socket {}: {e}\n  is the mux server running? list sessions with `fno mux ls`.",
                socket.display()
            )
        })?;
    let (reader, mut writer) = stream.into_split();

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    proto::write_msg(
        &mut writer,
        &ClientMsg::Attach {
            proto: PROTO_VERSION,
            build: BUILD_VERSION.to_string(),
            // (0,0) is the observer sentinel: excluded from the clamp, never
            // resizes a PTY, fed every pane's frames (server.rs passive path).
            rows: 0,
            cols: 0,
            cwd,
        },
    )
    .await
    .map_err(|e| format!("attach write failed: {e}"))?;
    if let Some(hello) = hello {
        proto::write_msg(&mut writer, &ClientMsg::Observe(hello))
            .await
            .map_err(|e| format!("observer hello write failed: {e}"))?;
    }
    // READ-ONLY (Locked Decision 5): drop all write ability. `forget()` releases
    // the write half WITHOUT the shutdown-on-drop that would half-close the
    // socket and make the server detach us; after this there is no handle that
    // could ever write a browser byte upstream.
    writer.forget();

    let mut reader = reader;
    // A busy server is still the user's server: say so once, keep waiting.
    let first = crate::client::attach_handshake::await_attach_reply(
        proto::read_msg::<_, ServerMsg>(&mut reader),
        crate::client::attach_handshake::ATTACH_BUSY_NOTICE,
        &mut false,
        || eprintln!("fno: server is busy, still waiting (Ctrl-C to stop; `fno mux ls`)"),
    )
    .await
    .map_err(|e| format!("attach read failed: {e}"))?;

    if let ServerMsg::Bye { reason } = &first {
        // Version skew or an immediate refusal - surface it, never hang or spin.
        return Err(format!("server refused the attach: {reason}"));
    }
    Ok((reader, first))
}

async fn read_stream(
    mut reader: OwnedReadHalf,
    tx: &broadcast::Sender<String>,
    snap: &Arc<Mutex<Snapshot>>,
) {
    loop {
        match proto::read_msg::<_, ServerMsg>(&mut reader).await {
            Ok(msg) => forward(msg, tx, snap),
            // EOF or a malformed message: return to reconnect. A malformed frame
            // is never forwarded as a half-grid.
            Err(_) => return,
        }
    }
}

/// Serialize one `ServerMsg` to its wire JSON, update the replay snapshot, and
/// broadcast it. A `Frame` failing `geometry_ok` at this trust boundary is
/// dropped (AC5-FR): the previous good frame stays drawn. A good frame gets
/// the same user-message band text fix the native client applies, because the
/// browser paints the raw cell colors.
fn forward(mut msg: ServerMsg, tx: &broadcast::Sender<String>, snap: &Arc<Mutex<Snapshot>>) {
    if let ServerMsg::Frame { frame, .. } = &mut msg {
        if !frame.geometry_ok() {
            return;
        }
        frame.cells.iter_mut().for_each(crate::band_remap::remap);
    }
    let json = match serde_json::to_string(&msg) {
        Ok(j) => j,
        Err(_) => return,
    };
    {
        let mut s = snap.lock().unwrap();
        match &msg {
            ServerMsg::Frame { pane_id, .. } => {
                s.frame_seq += 1;
                let seq = s.frame_seq;
                s.frames.insert(*pane_id, (seq, json.clone()));
                if s.frames.len() > MAX_SNAPSHOT_PANES {
                    // Evict the least-recently-updated pane (lowest seq = the one
                    // that has gone quiet longest - a dead pane).
                    if let Some(oldest) = s
                        .frames
                        .iter()
                        .min_by_key(|(_, (seq, _))| *seq)
                        .map(|(&pid, _)| pid)
                    {
                        s.frames.remove(&oldest);
                    }
                }
            }
            ServerMsg::Layout { .. } => s.layout = Some(json.clone()),
            _ => {}
        }
    }
    // Err only means no browser is subscribed yet - the snapshot already holds it.
    let _ = tx.send(json);
}

/// A bridge-injected control line (not a `ServerMsg`) the browser reads to drive
/// its connection banner.
pub(crate) fn bridge_status(state: &str) -> String {
    format!("{{\"_bridge\":{{\"state\":\"{state}\"}}}}")
}

/// `host:port`, bracketing an IPv6 literal so `[::1]:8722` parses - a bare
/// `::1:8722` does not (the colons are ambiguous).
pub(crate) fn bind_addr(bind: &str, port: u16) -> String {
    if bind.contains(':') {
        format!("[{bind}]:{port}")
    } else {
        format!("{bind}:{port}")
    }
}

// ---------------------------------------------------------------------------
// HTTP + WebSocket
// ---------------------------------------------------------------------------

async fn page() -> impl IntoResponse {
    let body = PAGE.replacen("<!--fno-nav-->", &nav_fragment(NavPage::Live), 1);
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body)
}

#[derive(serde::Deserialize)]
struct WsQuery {
    t: Option<String>,
}

async fn backlog(Query(q): Query<WsQuery>, State(st): State<AppState>) -> Response {
    if !token_ok(q.t.as_deref(), &st.token) {
        return unauthorized();
    }
    // The page's write controls exist only when the serving bridge can
    // write: one string replace, no second page.
    let page = house(BACKLOG_PAGE);
    let page = if st.writable {
        page.replacen("<body>", "<body data-writable=\"true\">", 1)
    } else {
        page
    };
    html_response(with_nav(&page, NavPage::Backlog))
}

/// `GET /hq`: the HQ page; it reads `/hq/model.json`.
async fn hq(Query(q): Query<WsQuery>, State(st): State<AppState>) -> Response {
    if !token_ok(q.t.as_deref(), &st.token) {
        return unauthorized();
    }
    html_response(with_nav(&house(HQ_PAGE), NavPage::Hq))
}

/// A 308 to the same query on `target`, a path relative to the old URL so a
/// mount prefix survives. The old `/command` URLs forward to `/hq`.
fn moved_to_hq(target: &str, query: Option<String>) -> Response {
    let location = match query {
        Some(q) if !q.is_empty() => format!("{target}?{q}"),
        _ => target.to_string(),
    };
    (
        StatusCode::PERMANENT_REDIRECT,
        [(header::LOCATION, location)],
    )
        .into_response()
}

async fn command_redirect(axum::extract::RawQuery(q): axum::extract::RawQuery) -> Response {
    moved_to_hq("hq", q)
}

async fn command_model_redirect(axum::extract::RawQuery(q): axum::extract::RawQuery) -> Response {
    moved_to_hq("../hq/model.json", q)
}

async fn command_answer_redirect(
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
) -> Response {
    moved_to_hq(&format!("../../hq/answer/{id}"), q)
}

#[derive(serde::Deserialize)]
struct TraceQuery {
    t: Option<String>,
    node: Option<String>,
}

/// `GET /trace`: the trace page for one node; it reads `/trace/model.json`.
async fn trace(Query(q): Query<TraceQuery>, State(st): State<AppState>) -> Response {
    if !token_ok(q.t.as_deref(), &st.token) {
        return unauthorized();
    }
    html_response(with_nav(&house(TRACE_PAGE), NavPage::Hq))
}

/// `GET /trace/model.json`: the fold for `node`, read per request.
async fn trace_model(
    Query(q): Query<TraceQuery>,
    State(st): State<AppState>,
    Extension(grants): Extension<Grants>,
    headers: HeaderMap,
) -> Response {
    if !token_ok(q.t.as_deref(), &st.token) {
        return unauthorized();
    }
    let node = q.node.as_deref().unwrap_or("");
    if !crate::web_trace::valid_node(node) {
        return plain_status(StatusCode::BAD_REQUEST, "node must be a node id");
    }
    let scope = read_scope(&headers, &st, &grants);
    if scope.is_scoped() {
        let inputs = model_inputs(&st).await;
        if let Some(reply) = scope_refusal(
            &scope,
            &crate::web_answer::chain_of(&inputs.rows, Some(node)),
        ) {
            return reply;
        }
    }
    match crate::web_trace::read_trace(node).await {
        Ok(body) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            body,
        )
            .into_response(),
        Err(reason) => plain_status(StatusCode::SERVICE_UNAVAILABLE, &reason),
    }
}

fn html_response(body: String) -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

async fn team(
    Query(q): Query<WsQuery>,
    State(st): State<AppState>,
    Extension(grants): Extension<Grants>,
    headers: HeaderMap,
) -> Response {
    let authorized = token_ok(q.t.as_deref(), &st.token);
    // The chart is org-wide: refuse a scoped viewer before any marker write,
    // so their visit never triggers a render they cannot read.
    if authorized && read_scope(&headers, &st, &grants).is_scoped() {
        return plain_status(
            StatusCode::FORBIDDEN,
            "scoped invites read a node-scoped fold; the rundown chart is machine-wide",
        );
    }
    let modified = std::fs::metadata(&st.rundown_html)
        .and_then(|m| m.modified())
        .ok();
    // An unfaithful root is never written through: the republish would pass
    // the fallback as `--out` and overwrite the global page with this
    // project's org data.
    if st.rundown_republish && team_needs_republish(authorized, modified, SystemTime::now()) {
        start_team_republish(&st.rundown_html);
    }
    // Option A (ruling recorded on the node): the daemon arm renders only
    // after a viewer was served the page, so an authorized serve stamps
    // the sibling marker the arm reads. Best effort: a failed stamp just
    // means the arm's next render waits for another serve.
    if authorized {
        let _ = std::fs::write(served_marker(&st.rundown_html), b"");
    }
    let notice = if st.rundown_republish {
        "fno agent hq chart rundown (a render has started; reload in about a minute)"
    } else {
        "rundown.html is not resolvable from this config (a template, ~user, relative or unset $VAR state_dir); fix state_dir or run fno agent hq chart rundown"
    };
    private_page_response(
        &st.rundown_html,
        q.t.as_deref(),
        &st.token,
        notice,
        NavPage::Team,
    )
    .await
}

/// `GET /hq/model.json`: the first read waits; later reads answer from the cache.
async fn hq_model(
    Query(q): Query<WsQuery>,
    State(st): State<AppState>,
    Extension(grants): Extension<Grants>,
    headers: HeaderMap,
) -> Response {
    if !token_ok(q.t.as_deref(), &st.token) {
        return unauthorized();
    }
    let scope = read_scope(&headers, &st, &grants);
    st.hq.first.get_or_init(|| refresh_hq(st.clone())).await;
    if st.hq.claim_refresh().await {
        tokio::spawn(refresh_hq(st.clone()));
    }
    let mut model = st.hq.model().await;
    // A read-only bridge shows the question drawer with no answer controls.
    model["writable"] = serde_json::json!(st.writable);
    if scope.is_scoped() {
        let rows = model_inputs(&st).await;
        model = scope_hq_model(model, &scope, &rows.rows);
    }
    json_response(&model)
}

/// The caller's name from the invite decision, or the refusal as a response.
fn mine_person(headers: &HeaderMap, st: &AppState, grants: &Grants) -> Result<String, Response> {
    viewer(headers, st, grants).map_err(|r| refused(&r))
}

/// `GET /hq/mine.json`: the caller's own views, filled inside their invite.
async fn hq_mine(
    Query(q): Query<WsQuery>,
    State(st): State<AppState>,
    Extension(grants): Extension<Grants>,
    headers: HeaderMap,
) -> Response {
    if !token_ok(q.t.as_deref(), &st.token) {
        return unauthorized();
    }
    let person = match mine_person(&headers, &st, &grants) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let root = crate::web_mine::store_root();
    let rows = if crate::web_mine::has_store(&root, &person) {
        Some(model_inputs(&st).await)
    } else {
        None
    };
    let out = tokio::task::spawn_blocking(move || {
        let rows = rows.as_ref().map_or(&[][..], |i| i.rows.as_slice());
        crate::web_mine::mine_json(&root, rows, grants.list(), &person)
    })
    .await;
    match out {
        Ok(v) => json_response(&v),
        Err(_) => plain_status(StatusCode::INTERNAL_SERVER_ERROR, "the Mine read failed"),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LookBody {
    project: String,
    look: crate::web_mine::Look,
}

/// `POST /hq/mine/look`: store the caller's look for one project.
async fn hq_mine_look(
    Query(q): Query<WsQuery>,
    State(st): State<AppState>,
    Extension(grants): Extension<Grants>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !token_ok(q.t.as_deref(), &st.token) {
        return unauthorized();
    }
    if let Err(msg) = crate::web_answer::same_origin(&headers) {
        return plain_status(StatusCode::FORBIDDEN, msg);
    }
    let person = match mine_person(&headers, &st, &grants) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let body: LookBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => {
            return plain_status(
                StatusCode::BAD_REQUEST,
                &format!(
                    "the body is {{\"project\", \"look\"}} with {}: {e}",
                    crate::web_mine::LOOK_VALUES
                ),
            )
        }
    };
    let out = tokio::task::spawn_blocking(move || {
        // The body arrived after the gate's read, so the invite may be gone.
        viewer(&headers, &st, &st.invites.read())
            .map_err(|r| crate::web_mine::Refused(r.code, r.text))?;
        crate::web_mine::set_look(
            &crate::web_mine::store_root(),
            &person,
            &body.project,
            body.look,
        )
    })
    .await;
    match out {
        Ok(Ok(look)) => json_response(&serde_json::json!({ "look": look })),
        Ok(Err(crate::web_mine::Refused(code, text))) => plain_status(
            StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            &text,
        ),
        Err(_) => plain_status(StatusCode::INTERNAL_SERVER_ERROR, "the look write failed"),
    }
}

/// `GET /hq/decisions.json`: every recorded decision plus the ids the
/// user marked reviewed. Read-only: a retract or an overrule needs a real
/// tty, so the page writes nothing.
async fn hq_decisions(
    Query(q): Query<WsQuery>,
    State(st): State<AppState>,
    Extension(grants): Extension<Grants>,
    headers: HeaderMap,
) -> Response {
    if !token_ok(q.t.as_deref(), &st.token) {
        return unauthorized();
    }
    // Decision rows name nodes across the machine; no per-node cut exists.
    if read_scope(&headers, &st, &grants).is_scoped() {
        return plain_status(
            StatusCode::FORBIDDEN,
            "scoped invites read a node-scoped fold; the decision log is machine-wide",
        );
    }
    let read = tokio::task::spawn_blocking(|| {
        crate::decisions_overlay::load().map(|rows| {
            let reviewed =
                crate::decisions_overlay::load_reviewed(&crate::decisions_table::store_path());
            serde_json::json!({"decisions": rows, "reviewed": reviewed})
        })
    })
    .await;
    match read {
        Ok(Ok(body)) => json_response(&body),
        Ok(Err(e)) => plain_status(StatusCode::BAD_GATEWAY, &e),
        Err(_) => plain_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the decisions read failed",
        ),
    }
}

async fn refresh_hq(st: AppState) {
    let inputs = model_inputs(&st).await;
    let (fleet, questions) = tokio::join!(
        crate::client::org_seats::read_fleet(),
        crate::needs_overlay::questions_now()
    );
    let measured_at = chrono::Utc::now().timestamp().max(0) as u64;
    let agents = layout_agents(&st.snap).unwrap_or_default();
    st.hq
        .board()
        .await
        .record(fleet, questions, &inputs, measured_at, &agents);
}

/// `GET /backlog/model.json`: the whole board as JSON behind the token.
async fn backlog_model(
    Query(raw): Query<Vec<(String, String)>>,
    State(st): State<AppState>,
    Extension(grants): Extension<Grants>,
    headers: HeaderMap,
) -> Response {
    if !token_ok(
        raw.iter().find(|(k, _)| k == "t").map(|(_, v)| v.as_str()),
        &st.token,
    ) {
        return unauthorized();
    }
    let scope = read_scope(&headers, &st, &grants);
    let q = match backlog_model::Query::from_pairs(&raw) {
        Ok(q) => q,
        Err(e) => return plain_status(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    // The question journal is 38 MB: read it only when the parsed query
    // actually asks `has:question`, over a clone of the cached inputs.
    let wants_questions =
        q.q.as_deref()
            .and_then(|text| {
                crate::search_query::parse(
                    text,
                    crate::search_query::Surface::Node,
                    crate::search_query::now_secs(),
                )
                .ok()
            })
            .is_some_and(|p| p.wants_questions());
    if wants_questions {
        let mut fresh = (*model_inputs(&st).await).clone();
        backlog_model::read_search_sources(&mut fresh, true);
        if scope.is_scoped() {
            filter_scoped_rows(&mut fresh, &scope);
        }
        let mut board = backlog_model::board(&fresh, &q);
        if raw.iter().any(|(k, v)| k == "keys" && v == "1") {
            board.search_keys = Some(crate::search_query::keys_json());
        }
        return json_response(&board);
    }
    let inputs = model_inputs(&st).await;
    // Only a scoped viewer pays the clone; the shared cache stays whole.
    let scoped_rows = if scope.is_scoped() {
        let mut owned = (*inputs).clone();
        filter_scoped_rows(&mut owned, &scope);
        Some(owned)
    } else {
        None
    };
    let mut board = backlog_model::board(scoped_rows.as_ref().unwrap_or(&inputs), &q);
    if raw.iter().any(|(k, v)| k == "keys" && v == "1") {
        board.search_keys = Some(crate::search_query::keys_json());
    }
    json_response(&board)
}

/// `GET /backlog/node.json?id=<id>`: one node's answer behind the token.
async fn backlog_node(
    Query(raw): Query<HashMap<String, String>>,
    State(st): State<AppState>,
    Extension(grants): Extension<Grants>,
    headers: HeaderMap,
) -> Response {
    if !token_ok(raw.get("t").map(String::as_str), &st.token) {
        return unauthorized();
    }
    let Some(id) = raw.get("id").filter(|s| !s.is_empty()) else {
        return plain_status(StatusCode::BAD_REQUEST, "id is required");
    };
    let scope = read_scope(&headers, &st, &grants);
    let inputs = model_inputs(&st).await;
    if let Some(reply) = scope_refusal(
        &scope,
        &crate::web_answer::chain_of(&inputs.rows, Some(id.as_str())),
    ) {
        return reply;
    }
    node_response(&inputs, id)
}

/// The pure half of `/backlog/node.json`: 503 on a failed source read, 404
/// when the read holds no such node, 200 with the body otherwise.
fn node_response(inputs: &backlog_model::Inputs, id: &str) -> Response {
    if let Some(err) = &inputs.rows_error {
        return plain_status(StatusCode::SERVICE_UNAVAILABLE, err);
    }
    match backlog_model::node(inputs, id) {
        Some(view) => json_response(&view),
        None => plain_status(StatusCode::NOT_FOUND, &format!("no node {id}")),
    }
}

fn plain_status(status: StatusCode, body: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body.to_string(),
    )
        .into_response()
}

fn json_response<T: serde::Serialize>(v: &T) -> Response {
    match serde_json::to_vec(v) {
        Ok(body) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            body,
        )
            .into_response(),
        Err(e) => plain_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("model serialization failed: {e}"),
        ),
    }
}

/// True when an HTTP authority names the loopback interface: `localhost`,
/// a loopback `IpAddr`, or either with a port, IPv6 bracketed or not.
fn names_loopback(authority: &str) -> bool {
    if let Ok(ip) = authority.parse::<std::net::IpAddr>() {
        return ip.is_loopback();
    }
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => authority,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The ruling's write guards, in order: the bridge must be bound to
/// loopback, the request's Host must name loopback (DNS-rebinding guard),
/// and the Origin header, when the browser sent one, must be an http URL
/// on loopback (cross-site guard; the value `null` included). Host must
/// always pass; Origin only when sent.
fn write_guard(headers: &HeaderMap, writable: bool) -> Result<(), (StatusCode, String)> {
    if !writable {
        return Err((
            StatusCode::FORBIDDEN,
            "the bridge is read-only: it is not bound to loopback".into(),
        ));
    }
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .filter(|h| names_loopback(h));
    if host.is_none() {
        return Err((
            StatusCode::FORBIDDEN,
            "write refused: Host does not name loopback".into(),
        ));
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) {
        let ok = origin.strip_prefix("http://").is_some_and(names_loopback);
        if !ok {
            return Err((
                StatusCode::FORBIDDEN,
                "write refused: Origin does not name loopback".into(),
            ));
        }
    }
    Ok(())
}

/// One backlog act the page can request.
#[derive(Debug, serde::Deserialize)]
#[serde(tag = "act", rename_all = "snake_case")]
enum Act {
    Rank {
        id: String,
        place: String,
    },
    Blueprint {
        id: String,
    },
    Target {
        id: String,
    },
    /// One encounter: the demand signal, POSTed with the operator's own
    /// evidence. The command refuses a repeat voter, so the page forwards the
    /// command's own answer.
    Encounter {
        id: String,
        evidence: String,
    },
}

impl Act {
    fn id(&self) -> &str {
        act_id(self)
    }
}

fn act_id(act: &Act) -> &str {
    match act {
        Act::Rank { id, .. }
        | Act::Blueprint { id }
        | Act::Target { id }
        | Act::Encounter { id, .. } => id,
    }
}

/// What a planned act runs: a backlog command's argv through
/// [`crate::backlog_write::run_command`], or a launch through
/// [`crate::server::agent_launch::run_dispatch_one`].
#[derive(Debug)]
enum Planned {
    Command(Vec<String>),
    Dispatch { plan: bool },
}

/// The pure half of `POST /backlog/act`: 503 on a failed source read, 404
/// when the read holds no such node, 409 when the card's backend cannot
/// answer the act or the node is already being worked, 400 on a bad field
/// value, else the planned argv or launch.
fn plan_act(inputs: &backlog_model::Inputs, act: &Act) -> Result<Planned, (StatusCode, String)> {
    if let Some(err) = &inputs.rows_error {
        return Err((StatusCode::SERVICE_UNAVAILABLE, err.clone()));
    }
    let id = act.id();
    let Some(view) = backlog_model::node(inputs, id) else {
        return Err((StatusCode::NOT_FOUND, format!("no node {id}")));
    };
    let unavailable = |feature: &str| {
        view.unavailable
            .iter()
            .find(|u| u.feature == feature)
            .map(|u| u.reason.clone())
    };
    match act {
        Act::Rank { place, .. } => {
            if let Some(reason) = unavailable(backlog_model::unavailable_features::CARD_MOVES) {
                return Err((StatusCode::CONFLICT, reason));
            }
            let place = match place.as_str() {
                "top" => crate::backlog_write::Place::Top,
                "bottom" => crate::backlog_write::Place::Bottom,
                other => return Err((StatusCode::BAD_REQUEST, format!("unknown place {other:?}"))),
            };
            crate::backlog_write::rank_argv(id, place, None)
                .map_err(|e| (StatusCode::BAD_REQUEST, e))
                .map(Planned::Command)
        }
        Act::Blueprint { .. } | Act::Target { .. } => {
            if view.card.claimed || view.card.live {
                return Err((
                    StatusCode::CONFLICT,
                    format!("{id} is already being worked; open its session instead"),
                ));
            }
            Ok(Planned::Dispatch {
                plan: matches!(act, Act::Blueprint { .. }),
            })
        }
        Act::Encounter { evidence, .. } => {
            let trimmed = evidence.trim();
            if trimmed.is_empty() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "encounter: evidence is required; name what the node cost".into(),
                ));
            }
            Ok(Planned::Command(vec![
                "backlog".into(),
                "encounter".into(),
                id.to_string(),
                "--operator".into(),
                "--evidence".into(),
                trimmed.to_string(),
            ]))
        }
    }
}

/// The effect half of `POST /backlog/act`: a planned command argv runs through
/// the shared shell-out; a launch runs the mux's own dispatch door and
/// carries its notice back. The launch arm's ok fact is `true`: the door
/// reports refusals through its notice text.
async fn run_planned(_st: &AppState, id: &str, planned: Planned) -> (bool, String) {
    match planned {
        Planned::Command(argv) => crate::backlog_write::run_command(&argv, None).await,
        Planned::Dispatch { plan } => {
            let notice = crate::server::agent_launch::run_dispatch_one(Some(id), None, plan).await;
            (true, notice)
        }
    }
}

/// The 200 body: the command's exit fact and its own last line.
fn act_response(ok: bool, notice: &str) -> Response {
    json_response(&serde_json::json!({ "ok": ok, "notice": notice }))
}

/// `POST /backlog/act?t=<token>`: guards, then the pure plan, then the
/// effect. The guard order is the ruling's: loopback bind, loopback Host,
/// loopback Origin when sent, then the token.
async fn backlog_act(
    Query(q): Query<WsQuery>,
    State(st): State<AppState>,
    Extension(grants): Extension<Grants>,
    headers: HeaderMap,
    Json(body): Json<Act>,
) -> Response {
    if let Err((code, msg)) = write_guard(&headers, st.writable) {
        return plain_status(code, &msg);
    }
    if !token_ok(q.t.as_deref(), &st.token) {
        return unauthorized();
    }
    // write_guard reads Host and Origin, both of which tailscale serve
    // rewrites or omits for a proxied caller, so it does not see the
    // invite: an act on any node would undo the read scoping. A scoped
    // viewer acts from their CLI session, never this bridge.
    if read_scope(&headers, &st, &grants).is_scoped() {
        return plain_status(
            StatusCode::FORBIDDEN,
            "scoped invites read a node-scoped fold; board acts run from the local machine",
        );
    }
    let id = body.id().to_string();
    let inputs = model_inputs(&st).await;
    let planned = match plan_act(&inputs, &body) {
        Ok(p) => p,
        Err((code, msg)) => return plain_status(code, &msg),
    };
    let (ok, notice) = run_planned(&st, &id, planned).await;
    act_response(ok, &notice)
}

/// `POST /ui/click`: one click or tap a served page reports, as the control's
/// stable id (`data-fno` attribute) and the screen (the page) it landed in.
/// The page's capture listener resolves the attribute, so a click that no
/// control names ships nothing. Ids only, never text (D3283); the bridge
/// writes the row through the same door the TUI client uses, and a dropped
/// id or failed write tells nobody. The body parses once into this struct
/// (R1); a body missing a field never reaches the handler.
///
/// Only the user's own click records: a direct loopback request with the
/// token and the same origin. An invitee or any other proxied caller gave no
/// opt-in, so they get the same 204 and no row.
#[derive(serde::Deserialize)]
struct UiClickBody {
    screen: String,
    control: String,
}

async fn ui_click(
    Query(q): Query<WsQuery>,
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UiClickBody>,
) -> Response {
    if !token_ok(q.t.as_deref(), &st.token) {
        return unauthorized();
    }
    if let Err(msg) = crate::web_answer::same_origin(&headers) {
        return plain_status(StatusCode::FORBIDDEN, msg);
    }
    if caller_of(&headers, st.writable).is_direct_local() {
        if let Some(journal) = &st.click_journal {
            crate::ui_usage::click_into(journal, &body.screen, &body.control);
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /hq/answer/{id}?t=<token>`: answer one open question from the
/// drawer. The refusals come in this order, and each one starts no process:
/// loopback bind, token, same origin, body shape, the question, the invite.
/// Then the answer door writes the one row. The route never records a
/// decision itself.
async fn hq_answer(
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(q): Query<WsQuery>,
    State(st): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<crate::web_answer::AnswerBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if !st.writable {
        return plain_status(
            StatusCode::FORBIDDEN,
            "the bridge is read-only: it is not bound to loopback",
        );
    }
    if !token_ok(q.t.as_deref(), &st.token) {
        return unauthorized();
    }
    if let Err(msg) = crate::web_answer::same_origin(&headers) {
        return plain_status(StatusCode::FORBIDDEN, msg);
    }
    if !crate::web_answer::valid_id(&id) {
        return plain_status(StatusCode::BAD_REQUEST, "that is not a question id");
    }
    let pick = match body {
        Ok(Json(b)) => match crate::web_answer::pick_of(b) {
            Ok(p) => p,
            Err(msg) => return plain_status(StatusCode::BAD_REQUEST, msg),
        },
        Err(_) => {
            return plain_status(
                StatusCode::BAD_REQUEST,
                "the body is JSON with one of option, words or done",
            )
        }
    };
    // The page only offers a question it already holds, so the cache answers
    // the node. A question the cache has not seen yet takes one fold read.
    let node = match st.hq.question_node(&id).await {
        Some(node) => node,
        None => match crate::needs_overlay::questions_now().await {
            Ok(fold) => match fold.items.iter().find(|i| i.id == id) {
                Some(item) => item.node.clone(),
                None => return plain_status(StatusCode::NOT_FOUND, "no such question"),
            },
            Err(e) => return plain_status(StatusCode::SERVICE_UNAVAILABLE, &e),
        },
    };
    let inputs = model_inputs(&st).await;
    let chain = crate::web_answer::chain_of(&inputs.rows, node.as_deref());
    // Read again after the last wait: the gate's read predates the body.
    let grants = st.invites.read();
    let person = match crate::web_invites::decide(
        &caller_of(&headers, st.writable),
        grants.list(),
        Some(&chain),
        crate::web_invites::Role::Editor,
    ) {
        Ok(person) => person,
        Err(r) => return refused(&r),
    };
    match crate::needs_overlay::answer_as(&id, pick, "command_page", Some(&person)).await {
        Ok(receipt) => {
            tokio::spawn(refresh_hq(st.clone()));
            let receipt = serde_json::from_str::<serde_json::Value>(&receipt)
                .unwrap_or_else(|_| serde_json::json!({ "recorded": true }));
            json_response(&receipt)
        }
        Err(f) => plain_status(
            if f.timed_out {
                StatusCode::SERVICE_UNAVAILABLE
            } else if f.refused {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            },
            &if f.refused {
                crate::web_answer::plain_refusal(&f.message)
            } else {
                f.message
            },
        ),
    }
}

/// The gathered inputs, cached under [`backlog_model::REGATHER_AFTER`] while
/// the store version holds; the lock is held for the whole call so one
/// gather runs at a time. A gather with a failed source read is returned
/// uncached, so the next request retries it.
async fn model_inputs(st: &AppState) -> Arc<backlog_model::Inputs> {
    let mut cached = st.model.lock().await;
    if let Some(c) = cached.as_ref() {
        if c.at.elapsed() < backlog_model::REGATHER_AFTER {
            let moved = match c.version {
                // An external backend has no version: age alone gates it.
                None => false,
                Some(v) => {
                    let graph = crate::backlog_view::graph_path();
                    tokio::task::spawn_blocking(move || crate::store_client::version(&graph))
                        .await
                        .map(|r| r.ok() != Some(v))
                        .unwrap_or(true)
                }
            };
            if !moved {
                return c.inputs.clone();
            }
        }
    }
    let (agents, roster_error) = match layout_agents(&st.snap) {
        Some(agents) => (agents, None),
        None => (
            Vec::new(),
            Some("no agent roster yet; live, lead and session actions are unknown".to_string()),
        ),
    };
    let graph = crate::backlog_view::graph_path();
    let mut inputs = backlog_model::gather(&graph, agents).await;
    if inputs.rows_error.is_some() {
        return Arc::new(inputs);
    }
    if let Some(e) = roster_error {
        inputs.errors.push(e);
    }
    backlog_model::read_search_sources(&mut inputs, false);
    let version = inputs.version;
    let inputs = Arc::new(inputs);
    *cached = Some(CachedModel {
        version,
        at: Instant::now(),
        inputs: inputs.clone(),
    });
    inputs
}

/// The roster from the snapshot's last `Layout` JSON
/// (`{"Layout": {"agents": [...]}}`). `None` when no layout has arrived yet.
fn layout_agents(snap: &Arc<Mutex<Snapshot>>) -> Option<Vec<proto::AgentRow>> {
    let guard = snap.lock().unwrap();
    let text = guard.layout.as_ref()?;
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let agents = v.get("Layout")?.get("agents")?.as_array()?;
    Some(
        agents
            .iter()
            .filter_map(|a| serde_json::from_value(a.clone()).ok())
            .collect(),
    )
}

/// Which page the bridge serves; the shared nav fragment marks the current
/// one so a reader always knows where they are.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NavPage {
    Live,
    Backlog,
    Team,
    Hq,
}

fn token_ok(supplied: Option<&str>, expected: &str) -> bool {
    supplied.is_some_and(|token| constant_time_eq(token.as_bytes(), expected.as_bytes()))
}

/// Republish-on-read: a render costs about a minute, so the bridge serves the
/// current file at once and starts ONE background render when the page is
/// stale. `team()` checks; this spawns through the fleet admission gate.
const TEAM_REPUBLISH_AFTER: Duration = Duration::from_secs(300);
const TEAM_REPUBLISH_TIMEOUT: Duration = Duration::from_secs(300);
static TEAM_REPUBLISHING: AtomicBool = AtomicBool::new(false);

/// Pure staleness verdict: unauthorized reads start nothing, a missing file
/// is maximally stale, a future mtime reads as fresh, and anything older
/// than the threshold needs a render.
fn team_needs_republish(authorized: bool, modified: Option<SystemTime>, now: SystemTime) -> bool {
    if !authorized {
        return false;
    }
    match modified {
        None => true,
        Some(m) if m > now => false,
        Some(m) => now
            .duration_since(m)
            .map(|age| age > TEAM_REPUBLISH_AFTER)
            .unwrap_or(false),
    }
}

/// The marker the /team route stamps on an authorized serve and the
/// daemon's team_ledger arm reads: marker mtime newer than the page mtime
/// means a viewer arrived since the last render (the option-A cadence, the
/// ruling recorded on the node).
fn served_marker(page: &Path) -> PathBuf {
    PathBuf::from(format!("{}.served", page.display()))
}

/// Single-flight background render. `--out` names the exact path the bridge
/// reads, so writer and reader agree whatever the Python state root resolves
/// to. Admission refusal, non-zero exit or timeout each log and clear the
/// flag, so a later read may retry.
fn start_team_republish(out: &Path) {
    if TEAM_REPUBLISHING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return; // a render is already in flight
    }
    let out = out.to_path_buf();
    tokio::spawn(async move {
        let fno = std::env::current_exe().unwrap_or_else(|_| "fno".into());
        let mut cmd = crate::process_admission::tokio_command(&fno);
        cmd.args(["agents", "hq", "chart", "rundown", "--out"])
            .arg(&out);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let result = match crate::process_admission::tokio_spawn(&mut cmd) {
            Err(e) => Err(format!("{e}")),
            Ok(mut child) => {
                let stderr = child.stderr.take();
                let tail = tokio::spawn(async move {
                    let mut buf = String::new();
                    if let Some(mut s) = stderr {
                        use tokio::io::AsyncReadExt;
                        let mut chunk = vec![0u8; 4096];
                        loop {
                            match s.read(&mut chunk).await {
                                Ok(0) | Err(_) => break,
                                Ok(n) => buf.push_str(&String::from_utf8_lossy(&chunk[..n])),
                            }
                        }
                    }
                    buf
                });
                match tokio::time::timeout(TEAM_REPUBLISH_TIMEOUT, child.wait()).await {
                    Err(_) => {
                        let _ = child.kill().await;
                        Err("timed out after 300s".to_string())
                    }
                    Ok(Err(e)) => Err(format!("{e}")),
                    Ok(Ok(status)) if !status.success() => {
                        let last = tail
                            .await
                            .unwrap_or_default()
                            .lines()
                            .last()
                            .unwrap_or_default()
                            .to_string();
                        Err(last)
                    }
                    Ok(Ok(_)) => Ok(()),
                }
            }
        };
        if let Err(e) = result {
            eprintln!("fno mux web: `fno agent hq chart rundown --out <rundown.html>` failed: {e}");
        }
        TEAM_REPUBLISHING.store(false, Ordering::SeqCst);
    });
}

/// One sticky nav shared by the live, backlog and team pages. Links are
/// computed in the browser from `location.pathname`, so any mount prefix
/// works, and no prefix works too.
fn nav_fragment(current: NavPage) -> String {
    let name = |p: NavPage| match p {
        NavPage::Live => "live",
        NavPage::Backlog => "backlog",
        NavPage::Team => "rundown",
        NavPage::Hq => "hq",
    };
    let link = |p: NavPage| {
        if p == current {
            format!(
                "<a data-page=\"{}\" aria-current=\"page\" href=\"#\">{}</a>",
                name(p),
                name(p)
            )
        } else {
            format!("<a data-page=\"{}\" href=\"#\">{}</a>", name(p), name(p))
        }
    };
    // The backlog page's own filter bar is sticky at top:0 and would slide
    // under the nav, so only that page's fragment lifts it.
    let controls = if current == NavPage::Backlog {
        ".controls{top:var(--fno-nav-h)}"
    } else {
        ""
    };
    format!(
        "<nav class=\"fno-nav\" data-current=\"{}\" aria-label=\"fno pages\">\
         <style>:root{{--fno-nav-h:42px}}\
         nav.fno-nav{{position:sticky;top:0;z-index:1000;display:flex;align-items:center;gap:18px;\
         padding:6px 14px;background:#14181a;color:#e3e8e4;\
         font:13px/1 system-ui,-apple-system,Segoe UI,sans-serif;box-sizing:border-box}}\
         nav.fno-nav *{{box-sizing:border-box}}\
         nav.fno-nav a{{color:#b9c2cf;text-decoration:none;display:flex;align-items:center;\
         min-height:30px;padding:0 4px;border-bottom:2px solid transparent}}\
         nav.fno-nav a[aria-current=\"page\"]{{color:#fff;border-bottom-color:#c99b45}}\
         nav.fno-nav a:hover{{color:#fff}}{controls}</style>\
         {}{}{}{}\
         <script>(function(){{var nav=document.querySelector(\"nav.fno-nav\");if(!nav)return;\
         var p=location.pathname,base;\
         if(nav.dataset.current===\"live\"){{base=p.endsWith(\"/\")?p:p+\"/\";}}\
         else{{base=p.slice(0,p.lastIndexOf(\"/\")+1);}}\
         nav.dataset.base=base;\
         var q=new URLSearchParams(location.search),t=q.get(\"t\")||\"\",pj=q.get(\"project\");\
         nav.querySelectorAll(\"a\").forEach(function(a){{\
         var page=a.dataset.page;\
         a.href=base+(page===\"live\"?\"\":page)+\"?t=\"+encodeURIComponent(t)\
         +(pj?\"&project=\"+encodeURIComponent(pj):\"\");}});}})();</script></nav>",
        name(current),
        link(NavPage::Live),
        link(NavPage::Backlog),
        link(NavPage::Team),
        link(NavPage::Hq),
    )
}

/// Insert the nav fragment right after the first `<body ...>` tag (ASCII
/// case-insensitive; a served page opens with `<body data-local="true">`).
/// With no body tag at all, prepend.
fn with_nav(html: &str, current: NavPage) -> String {
    let fragment = nav_fragment(current);
    let lower = html.to_ascii_lowercase();
    let injected = match lower
        .find("<body")
        .and_then(|start| lower[start..].find('>').map(|end| start + end + 1))
    {
        Some(at) => {
            let mut out = String::with_capacity(html.len() + fragment.len());
            out.push_str(&html[..at]);
            out.push_str(&fragment);
            out.push_str(&html[at..]);
            out
        }
        None => format!("{fragment}{html}"),
    };
    injected
}

/// The one 401 body, shared by every token-gated route.
fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "invalid or missing token",
    )
        .into_response()
}

async fn private_page_response(
    path: &Path,
    supplied: Option<&str>,
    expected: &str,
    render_hint: &str,
    nav: NavPage,
) -> Response {
    if !token_ok(supplied, expected) {
        return unauthorized();
    }
    match std::fs::read_to_string(path) {
        Ok(body) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            with_nav(&body, nav),
        )
            .into_response(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            format!("page not rendered; run {render_hint}"),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            format!("page unreadable: {err}"),
        )
            .into_response(),
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(q): Query<WsQuery>,
    State(st): State<AppState>,
    Extension(grants): Extension<Grants>,
    headers: HeaderMap,
) -> Response {
    let ok =
        q.t.as_deref()
            .is_some_and(|t| constant_time_eq(t.as_bytes(), st.token.as_bytes()));
    if !ok {
        // AC4-ERR. A browser cannot read the HTTP status of a FAILED WebSocket
        // handshake: `onclose` fires 1006 for any handshake failure, so a
        // 401-at-upgrade is indistinguishable from a network drop and would
        // retry forever instead of painting the auth-failed banner. Complete the
        // upgrade, then immediately close with 1008 (policy violation) + reason,
        // so the browser's `onclose.code === 1008` fires reliably. No ServerMsg
        // frame is ever sent, so "no frame on a bad token" still holds.
        return ws.on_upgrade(|mut socket| async move {
            let _ = socket
                .send(Message::Close(Some(CloseFrame {
                    code: close_code::POLICY,
                    reason: "invalid token".into(),
                })))
                .await;
        });
    }
    // Bridge frames have no node mapping, so no filtered stream exists: a
    // scoped viewer gets the reason instead, through the same
    // upgrade-then-close shape the token path uses (a browser cannot read
    // the HTTP status of a failed handshake).
    if read_scope(&headers, &st, &grants).is_scoped() {
        return ws.on_upgrade(|mut socket| async move {
            let _ = socket
                .send(Message::Close(Some(CloseFrame {
                    code: close_code::POLICY,
                    reason: "scoped invites read a node-scoped fold; the live bridge feed is machine-wide".into(),
                })))
                .await;
        });
    }
    ws.max_message_size(INBOUND_WS_CAP)
        .on_upgrade(move |socket| ws_conn(socket, st, headers))
}

/// An open feed's right to frames, under the invites as they are now. The
/// feed is machine-wide, so it needs a whole-board read.
fn feed_allowed(headers: &HeaderMap, st: &AppState) -> bool {
    let grants = st.invites.read();
    viewer(headers, st, &grants).is_ok() && !read_scope(headers, st, &grants).is_scoped()
}

async fn close_revoked(socket: &mut WebSocket) {
    // A client that already left needs no close frame.
    socket
        .send(Message::Close(Some(CloseFrame {
            code: close_code::POLICY,
            reason: "this caller may no longer read the live bridge feed".into(),
        })))
        .await
        .ok();
}

/// Send one frame, reading the invites first when the last read is a
/// [`FEED_RECHECK`] old. False ends the feed.
async fn send_frame(
    socket: &mut WebSocket,
    st: &AppState,
    headers: &HeaderMap,
    checked: &mut Instant,
    text: String,
) -> bool {
    if checked.elapsed() >= FEED_RECHECK {
        if !feed_allowed(headers, st) {
            close_revoked(socket).await;
            return false;
        }
        *checked = Instant::now();
    }
    socket.send(Message::Text(text.into())).await.is_ok()
}

async fn ws_conn(mut socket: WebSocket, st: AppState, headers: HeaderMap) {
    // The upgrade may have waited, and the invite may have gone meanwhile.
    if !feed_allowed(&headers, &st) {
        return close_revoked(&mut socket).await;
    }
    let mut checked = Instant::now();
    // A quiet feed sends nothing, so it reads the invites on a timer too.
    let first = tokio::time::Instant::now() + FEED_RECHECK;
    let mut recheck = tokio::time::interval_at(first, FEED_RECHECK);
    recheck.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Subscribe BEFORE snapshotting so no frame slips through the gap between
    // reading the snapshot and joining the live stream (a duplicate is harmless;
    // a gap would leave a stale grid).
    let mut rx = st.tx.subscribe();
    {
        let preamble: Vec<String> = {
            let s = st.snap.lock().unwrap();
            let mut p = Vec::with_capacity(s.frames.len() + 2);
            p.push(bridge_status(if s.upstream_up {
                "connected"
            } else {
                "disconnected"
            }));
            if let Some(l) = &s.layout {
                p.push(l.clone());
            }
            p.extend(s.frames.values().map(|(_, j)| j.clone()));
            p
        };
        for m in preamble {
            if !send_frame(&mut socket, &st, &headers, &mut checked, m).await {
                return;
            }
        }
    }

    loop {
        let mut shutdown = st.shutdown.clone();
        tokio::select! {
            r = rx.recv() => match r {
                Ok(json) => {
                    if !send_frame(&mut socket, &st, &headers, &mut checked, json).await {
                        return;
                    }
                }
                // Slow phone: skip the stale frames it missed, keep streaming.
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            },
            r = socket.recv() => match r {
                // Read-only: drop every inbound browser message. A Close frame,
                // an error, or EOF ends the connection.
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
            _ = recheck.tick() => {
                if !feed_allowed(&headers, &st) {
                    return close_revoked(&mut socket).await;
                }
                checked = Instant::now();
            }
            _ = shutdown.changed() => {
                let _ = socket
                    .send(Message::Close(Some(CloseFrame {
                        code: close_code::NORMAL,
                        reason: "bridge shutting down".into(),
                    })))
                    .await;
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Auth helpers
// ---------------------------------------------------------------------------

/// 32 random bytes from `/dev/urandom` as hex. `None` if the device is
/// unreadable (fail closed - never mint a guessable token).
pub(crate) fn mint_token() -> Option<String> {
    use std::io::Read;
    let mut buf = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .ok()?;
    Some(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Constant-time equality over the compared bytes. The token length is not
/// secret (always 64 hex chars), so an early length mismatch is fine; the byte
/// comparison itself never short-circuits.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
#[path = "web_invite_tests.rs"]
mod invite_tests;

#[cfg(test)]
mod tests {
    use super::invite_tests::invite_source;
    use super::*;

    #[test]
    fn layout_agents_parses_the_snapshot_layout() {
        let layout = serde_json::json!({
            "Layout": {
                "agents": [{
                    "squad": null, "name": "w1", "pane_id": null,
                    "badge": null, "reason": null, "exited": false,
                    "harness_session_id": "s1"
                }]
            }
        });
        let snap = Snapshot {
            layout: Some(layout.to_string()),
            ..Default::default()
        };
        let snap = Arc::new(Mutex::new(snap));
        let agents = layout_agents(&snap).expect("the layout parses");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "w1");
        // No layout yet: None, never an empty roster that reads as truth.
        let snap = Arc::new(Mutex::new(Snapshot::default()));
        assert!(layout_agents(&snap).is_none());
    }
    #[tokio::test]
    async fn backlog_json_routes_refuse_bad_tokens_and_queries() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir = std::env::temp_dir().join(format!("fno-web-model-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, _) = broadcast::channel(4);
        let (_shutdown_tx, shutdown) = tokio::sync::watch::channel(false);
        let state = AppState {
            tx,
            snap: Arc::new(Mutex::new(Snapshot::default())),
            token: Arc::<str>::from("right"),
            rundown_html: dir.join("lead.html"),
            rundown_republish: true,
            hq: Default::default(),
            writable: true,
            invites: invite_source(None),
            click_journal: None,
            model: Default::default(),
            shutdown,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router(state)).await.unwrap();
        });
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        // AC9-ERR: no token, 401 on both routes, before any store read.
        stream
            .write_all(b"GET /backlog/model.json HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 401"), "{reply}");
        assert!(reply.contains("invalid or missing token"), "{reply}");
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                b"GET /backlog/node.json?t=right HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 400"), "no id: {reply}");
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                b"GET /backlog/model.json?t=right&lanes=sideways HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 400"), "bad lanes: {reply}");
        assert!(reply.contains("unknown lanes"), "{reply}");
        server.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// A temp dir per test; tests using it must clean up after themselves.
    fn temp_state_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("web-stop-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn stop_state_rows() {
        let dir = temp_state_dir("none");
        let socket = dir.join("t.sock");
        assert_eq!(stop_web("t", &socket), 0);
        let _ = std::fs::remove_dir_all(&dir);

        let dir = temp_state_dir("corpse");
        let socket = dir.join("t.sock");
        // A pid proven dead: spawned, reaped, gone.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(proto::pid_confirmed_dead(pid as i32));
        let state = web_state_path(&socket).unwrap();
        std::fs::write(
            &state,
            serde_json::json!({"pid": pid, "started": 1}).to_string(),
        )
        .unwrap();
        assert_eq!(stop_web("t", &socket), 0);
        assert!(!state.exists(), "the corpse file is gone");
        let _ = std::fs::remove_dir_all(&dir);

        let dir = temp_state_dir("recycled");
        let socket = dir.join("t.sock");
        // pid names THIS live test process, but the recorded start token is
        // wrong: the identity check must read stale, and this process must
        // survive the command.
        let own_start =
            proto::pid_start_time(std::process::id()).expect("start time on test platform");
        let state = web_state_path(&socket).unwrap();
        std::fs::write(
            &state,
            serde_json::json!({
                "pid": std::process::id(),
                "started": own_start.wrapping_add(1),
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(stop_web("t", &socket), 0);
        assert!(!state.exists(), "the stale file is gone");

        let dir = temp_state_dir("token");
        let socket = dir.join("t.sock");
        let guard = WebStateFile::write(&socket, "127.0.0.1", 8722, "tok").unwrap();
        let raw = std::fs::read_to_string(web_state_path(&socket).unwrap()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["pid"].as_u64(), Some(std::process::id() as u64));
        assert_eq!(
            v["started"].as_u64(),
            proto::pid_start_time(std::process::id())
        );
        drop(guard);
        assert!(
            !web_state_path(&socket).unwrap().exists(),
            "Drop removes the file"
        );
        let _ = std::fs::remove_dir_all(&dir);

        // The record answers "who started this and how old is its build":
        // binary path, build rev, wall-clock start, launcher session.
        let dir = temp_state_dir("own");
        let socket = dir.join("t.sock");
        let guard = WebStateFile::write(&socket, "127.0.0.1", 8944, "tok").expect("wrote state");
        let raw = std::fs::read_to_string(web_state_path(&socket).unwrap()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["port"], 8944);
        assert_eq!(v["pid"], u64::from(std::process::id()));
        assert_eq!(v["rev"], env!("FNO_MUX_CRATES_REV"));
        assert_eq!(v["bin"], std::env::current_exe().unwrap().to_str().unwrap());
        assert!(v["started_at"].as_u64().unwrap() > 0);
        match launching_session_id() {
            Some(id) => assert_eq!(v["session"], id),
            None => assert!(
                v["session"].is_null(),
                "no launcher id -> the field is null"
            ),
        }
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pane_url_refuses_when_the_recorded_pid_is_gone_even_if_a_stranger_answers() {
        // A dead bridge's port can be taken by another process; a bare TCP
        // answer is not ownership. A dead or recycled pid in the state file
        // must refuse the URL even though the port answers.
        let stranger = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = stranger.local_addr().unwrap().port();
        // A pid proven dead: spawned, reaped, gone.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = child.id();
        child.wait().unwrap();
        assert!(proto::pid_confirmed_dead(dead_pid as i32));
        assert!(
            tcp_answers("127.0.0.1", port),
            "repro premise: the stranger holds the port"
        );
        // The test mux dir is a cleaned path, not a created one: make it.
        std::fs::create_dir_all(proto::mux_dir()).unwrap();
        let state = proto::mux_dir().join("web-turldead.json");
        std::fs::write(
            &state,
            serde_json::json!({
                "bind": "127.0.0.1", "port": port, "token": "tok",
                "pid": dead_pid, "started": 1
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(print_pane_url("fno mux pane url", "turldead", 0), 1);
        // A recycled pid reads the same: this live process, wrong start token.
        let own_start =
            proto::pid_start_time(std::process::id()).expect("start time on test platform");
        std::fs::write(
            &state,
            serde_json::json!({
                "bind": "127.0.0.1", "port": port, "token": "tok",
                "pid": std::process::id(), "started": own_start.wrapping_add(1)
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(print_pane_url("fno mux pane url", "turldead", 0), 1);
        let _ = std::fs::remove_file(&state);
        drop(stranger);
    }

    #[test]
    fn pane_url_prints_when_the_recorded_bridge_is_live() {
        // The identity gate must not over-refuse: live pid, matching start
        // token, answering port -> the URL prints and the command exits 0.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let own_start =
            proto::pid_start_time(std::process::id()).expect("start time on test platform");
        std::fs::create_dir_all(proto::mux_dir()).unwrap();
        let state = proto::mux_dir().join("web-turllive.json");
        std::fs::write(
            &state,
            serde_json::json!({
                "bind": "127.0.0.1", "port": port, "token": "tok",
                "pid": std::process::id(), "started": own_start
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(print_pane_url("fno mux pane url", "turllive", 3), 0);
        let _ = std::fs::remove_file(&state);
        drop(listener);
    }
    /// The served page must stay pinch-zoomable. A pane grid is as wide
    /// as its terminal, so on a phone zooming is the only way to read a grid
    /// wider than the screen; `maximum-scale` / `user-scalable=no` take that away.
    /// Anchored on the meta line itself, so a page that lost the tag entirely
    /// fails here rather than passing on an absence.
    #[test]
    fn served_page_rows() {
        let meta = PAGE
            .lines()
            .find(|l| l.contains(r#"name="viewport""#))
            .expect("the served page declares a viewport meta");
        assert!(
            meta.contains("width=device-width"),
            "viewport still maps the layout to the device: {meta}"
        );
        assert!(
            !meta.contains("maximum-scale"),
            "maximum-scale disables pinch-zoom on iOS: {meta}"
        );
        assert!(
            !meta.contains("user-scalable"),
            "user-scalable=no disables pinch-zoom on iOS: {meta}"
        );

        let note = PAGE
            .lines()
            .find(|l| l.contains("const KEPT_NOTE ="))
            .expect("the page names the retained region");
        assert!(
            note.contains("not terminal scrollback"),
            "the retained region disclaims scrollback: {note}"
        );

        assert!(
            PAGE.contains("new WebSocket("),
            "the page still opens the read-only socket"
        );
        // The one send is the share knock, and only in share mode. The mux
        // view sends nothing upstream (Locked Decision 5), and the share
        // bridge never writes to the mux socket either.
        let sends: Vec<&str> = PAGE.lines().filter(|l| l.contains(".send(")).collect();
        assert!(
            sends.len() == 1 && sends[0].contains("if (SHARE) ws.send(JSON.stringify({ knock:"),
            "only the share knock is sent: {sends:?}"
        );
        assert!(
            !PAGE.contains(r#""Resize""#),
            "the page never names the Resize message: a passive observer must not drive PTY geometry"
        );
        assert!(
            !PAGE.contains("ClientMsg"),
            "the page never builds an upstream message of any kind"
        );

        assert!(!PAGE.contains("\"/backlog?t="));
        assert!(!PAGE.contains("\"/team?t="));
        assert!(!PAGE.contains("\"/hq?t="));
        assert!(!PAGE.contains("${location.host}/ws"));
        assert!(PAGE.contains("<!--fno-nav-->"));
        assert!(PAGE.contains("const base = document.querySelector(\"nav.fno-nav\").dataset.base;"));
        assert!(PAGE.contains("${location.host}${base}ws?t="));
    }

    /// Lift one top-level `function <name>(` body out of the served page.
    /// Brace-balanced, which is exact for the page as written (no brace lives
    /// inside a string or comment in the lifted functions) and is the ceiling:
    /// a future `"{"` inside one of them would cut the slice short, and the
    /// node run then fails loudly on a syntax error rather than passing.
    fn lift_js_fn(src: &str, name: &str) -> String {
        let head = format!("function {name}(");
        let start = src
            .find(&head)
            .unwrap_or_else(|| panic!("the served page defines {name}()"));
        let rest = &src[start..];
        let open = rest.find('{').expect("a function body opens");
        let mut depth = 0usize;
        for (i, c) in rest[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return rest[..open + i + c.len_utf8()].to_string();
                    }
                }
                _ => {}
            }
        }
        panic!("unbalanced braces lifting {name}()");
    }

    /// The retention diff decides how many rows scrolled off between
    /// two frames, and getting it wrong silently corrupts what the operator
    /// reads as recent output. Exercise the SHIPPED source, not a Rust
    /// re-implementation, by running the lifted function under node.
    ///
    /// node is preinstalled on the CI runners. Where it is absent the test
    /// cannot assert anything, so it says so on stdout instead of passing
    /// quietly - a skip that looks like a pass is the failure mode here.
    #[test]
    fn evicted_row_count_tracks_the_grid_shift() {
        let asserts = r#"
const eq = (got, want, what) => {
  if (got !== want) { console.error(`FAIL ${what}: got ${got}, want ${want}`); process.exit(1); }
};
const g = (...rows) => rows.map((s) => ({ key: s }));

// --- proven scrolls: a non-negative row count ---
// An unmoved grid evicts nothing, so redrawing one frame stays idempotent.
eq(evictedRowCount(g("a","b","c"), g("a","b","c")), 0, "identical");
// Scrolled up by one: the top row left, a new row arrived at the bottom.
eq(evictedRowCount(g("a","b","c"), g("b","c","d")), 1, "shift by 1");
eq(evictedRowCount(g("a","b","c","d"), g("c","d","e","f")), 2, "shift by 2");
eq(evictedRowCount(g("a","b","c","d"), g("d","w","x","y")), 3, "shift by 3");
// A blank screen staying blank is unmoved, never a screenful of evictions.
eq(evictedRowCount(g("","",""), g("","","")), 0, "blank stays blank");
// Repeated rows leave the shift ambiguous. The smallest fit wins, which is
// the conservative read: an ambiguous shift retains fewer rows rather than
// inventing evictions that never happened.
eq(evictedRowCount(g("x","x","x","p"), g("x","x","p","q")), 1, "ambiguous shift takes the smallest fit");

// --- edited in place: 0 rows evicted, and NO gap ---
// A terminal edits rows in place constantly, and none of it is a scroll. These
// are the cases that made an earlier version dump a duplicate of the visible
// screen into the retained region on every single frame. Reading the last rows
// as a pinned tail answers 0 here, which is stronger than "unprovable": nothing
// scrolled off, so nothing is retained AND no gap is claimed.
eq(evictedRowCount(g("hdr","body","* work"), g("hdr","body","- work")), 0, "spinner tick on the last row");
eq(evictedRowCount(g("$ ls","a.txt","",""), g("$ ls","a.txt","$ ","")), 0, "prompt appears on an unfilled screen");

// --- not provably a scroll: -1, and the caller must retain NOTHING ---
// A live HEADER is not covered: only a pinned TAIL is looked past, because that
// is where an agent pane puts its input box. Under-reporting, never a guess.
eq(evictedRowCount(g("t=1","a","b"), g("t=2","b","c")), -1, "body scrolls under a live header");
// A whole new screen shares nothing: a repaint, or output outrunning the
// frame rate. Unprovable either way, so it is never treated as a shift.
eq(evictedRowCount(g("a","b","c"), g("x","y","z")), -1, "no overlap");
// Geometry changed under us. Note this is the SHRINK direction, which an
// earlier version silently concatenated because it compared the shift against
// the old row count rather than the new one.
eq(evictedRowCount(g("a","b","c","d","e"), g("c","d","e")), -1, "terminal shrank");
eq(evictedRowCount(g("a","b"), g("a","b","c")), -1, "terminal grew");
eq(evictedRowCount(null, g("a","b","c")), -1, "no previous frame");

// --- a pinned bottom region: the pane an operator actually opens this for ---
// An agent pane keeps an input box or a footer line on its last rows. Requiring
// the WHOLE grid to shift made retention permanently inert on every one of them,
// and silently, because the note only appears once a row is retained.
// A static footer, body scrolled by one.
eq(evictedRowCount(g("a","b","c","foot"), g("b","c","d","foot")), 1, "static footer, body scrolls");
// A LIVE footer, body scrolled by one: the tail differs too, so nothing about
// the last row can be assumed, only that it is not part of the scrolling body.
eq(evictedRowCount(g("a","b","c","spin |"), g("b","c","d","spin /")), 1, "live footer, body scrolls");
// A live footer over a body that did NOT move evicts nothing. This is the
// spinner case again, one row deeper, and it must not read as a scroll.
eq(evictedRowCount(g("a","b","c","spin |"), g("a","b","c","spin /")), 0, "live footer, body still");
// Two-row footer, body scrolled by two.
eq(evictedRowCount(g("a","b","c","f1","f2"), g("c","d","e","f1","f2")), 2, "two-row footer");
// The tail we look past is bounded, so a grid that is mostly footer is still
// reported honestly rather than explained away by an ever-deeper tail.
eq(evictedRowCount(g("a","b","c","d","e","f","g","h"), g("z","y","x","w","v","u","t","s")), -1, "unrelated beats any tail");
console.log("evictedRowCount: 18 cases ok");
"#;
        let src = format!(
            "{}\n{}\n{}\n{}",
            // MAX_FIXED_TAIL is a const the lifted function closes over.
            PAGE.lines()
                .find(|l| l.contains("const MAX_FIXED_TAIL"))
                .expect("the page bounds the fixed tail it looks past"),
            lift_js_fn(PAGE, "scrollWithin"),
            lift_js_fn(PAGE, "evictedRowCount"),
            asserts
        );
        let path = std::env::temp_dir().join(format!("fno-evicted-{}.mjs", std::process::id()));
        std::fs::write(&path, src).expect("temp dir writable");
        let out = std::process::Command::new("node").arg(&path).output();
        let _ = std::fs::remove_file(&path);
        match out {
            Err(e) => {
                // On CI a missing node means the assertions never ran, and a
                // skip that reads as a pass is exactly the failure this guard
                // exists to prevent. The runners ship node, so demand it there.
                assert!(
                    std::env::var_os("CI").is_none(),
                    "node is required on CI to exercise the shipped evictedRowCount: {e}"
                );
                println!(
                    "SKIPPED evicted_row_count_tracks_the_grid_shift: node not runnable ({e}); \
                     nothing was asserted"
                );
            }
            Ok(o) => {
                // The end-of-harness marker is the whole verdict, and it is
                // strictly stronger than the exit code: node printing it means
                // every case passed, and nothing else prints it. A failed case,
                // a syntax error, and a node that somehow exits 0 without
                // running all fail the same way, with both streams shown.
                let stdout = String::from_utf8_lossy(&o.stdout);
                assert!(
                    stdout.contains("evictedRowCount: 18 cases ok"),
                    "the shipped evictedRowCount did not clear every case:\n{}{}",
                    stdout,
                    String::from_utf8_lossy(&o.stderr)
                );
            }
        }
    }

    /// The pacer in the served page: an idle frame paints at once, a burst
    /// keeps the newest frame per pane, and a non-frame message never overtakes
    /// a held frame. The head regex reads the bridge's real serialization, so a
    /// field reorder on the wire fails here and not as a silent loss of pacing.
    #[test]
    fn frame_pacer_keeps_newest_per_pane_and_order() {
        let wire = |pane_id: u64, scroll_offset: u16| {
            let frame = proto::Frame {
                scroll_offset,
                ..proto::Frame::default()
            };
            serde_json::to_string(&ServerMsg::Frame { pane_id, frame }).unwrap()
        };
        let raws = serde_json::json!({
            "a1": wire(1, 1), "a2": wire(1, 2), "a3": wire(1, 3),
            "b1": wire(22, 1), "b2": wire(22, 2),
        });
        let asserts = r#"
const eq = (got, want, what) => {
  const g = JSON.stringify(got), w = JSON.stringify(want);
  if (g !== w) { console.error(`FAIL ${what}: got ${g}, want ${w}`); process.exit(1); }
};
const rig = (cap = 128) => {
  const seen = [], ticks = [];
  const send = framePacer((r) => seen.push(r), (f) => ticks.push(f), cap);
  return { seen, send, tick: () => ticks.shift()(), ticks };
};
let t = rig();
// Idle: the frame is delivered in the same call, before any animation frame.
t.send(RAW.a1);
eq(t.seen, [RAW.a1], "idle paints at once");
// Burst: the newest frame per pane wins, and neither pane is dropped.
t.send(RAW.a2); t.send(RAW.b1); t.send(RAW.a3); t.send(RAW.b2);
eq(t.seen.length, 1, "a burst waits for the animation frame");
t.tick();
eq(t.seen.slice(1), [RAW.a3, RAW.b2], "newest per pane, oldest update first");
// One empty animation frame does not close the window: a saturated page sees
// one between any two messages, and closing there would paint every frame.
t.send(RAW.a1);
eq(t.seen.length, 3, "a frame right after a flush still waits");
t.tick();
eq(t.seen.length, 4, "and lands on the next animation frame");
t.tick();
t.send(RAW.a2);
eq(t.seen.length, 4, "one empty animation frame keeps the window open");
t.tick();
eq(t.seen.at(-1), RAW.a2, "the held frame lands");
t.tick(); t.tick(); t.tick();
eq(t.ticks.length, 0, "three empty animation frames close the window");
t.send(RAW.b1);
eq(t.seen.at(-1), RAW.b1, "idle again paints at once");
// A non-frame message flushes first, so a Layout cannot overtake a frame.
t = rig();
t.send(RAW.a1); t.send(RAW.a2); t.send('{"Layout":{}}');
eq(t.seen, [RAW.a1, RAW.a2, '{"Layout":{}}'], "held frame lands before Layout");
// A throw while delivering must not stop later animation frames.
const ticks = []; let boom = true; const seen = [];
const send = framePacer((r) => { if (r === RAW.a2 && boom) { boom = false; throw new Error("x"); } seen.push(r); }, (f) => ticks.push(f), 128);
send(RAW.a1); send(RAW.a2);
try { ticks.shift()(); } catch (_) {}
send(RAW.a3); ticks.shift()();
eq(seen, [RAW.a1, RAW.a3], "a throw keeps the pacer running");
// No animation frames (hidden tab): the cap bounds what is held.
t = rig(2);
t.send(RAW.a1); t.send(RAW.a2); t.send(RAW.b1);
eq(t.seen, [RAW.a1, RAW.a2, RAW.b1], "the cap flushes without an animation frame");
console.log("framePacer: 12 cases ok");
"#;
        let src = format!(
            "const RAW = {raws};\n{}\n{}\n{asserts}",
            PAGE.lines()
                .filter(|l| l.contains("const FRAME_HEAD") || l.contains("const QUIET_TICKS"))
                .collect::<Vec<_>>()
                .join("\n"),
            lift_js_fn(PAGE, "framePacer"),
        );
        let path = std::env::temp_dir().join(format!("fno-pacer-{}.mjs", std::process::id()));
        std::fs::write(&path, src).expect("temp dir writable");
        let out = std::process::Command::new("node").arg(&path).output();
        let _ = std::fs::remove_file(&path);
        match out {
            Err(e) => {
                assert!(
                    std::env::var_os("CI").is_none(),
                    "node is required on CI to exercise the shipped framePacer: {e}"
                );
                println!("SKIPPED frame_pacer: node not runnable ({e}); nothing was asserted");
            }
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                assert!(
                    stdout.contains("framePacer: 12 cases ok"),
                    "the shipped framePacer did not clear every case:\n{}{}",
                    stdout,
                    String::from_utf8_lossy(&o.stderr)
                );
            }
        }
    }

    /// The page must keep calling retention what it is. A protocol
    /// history request is unreachable while `writer.forget()` stands, so the
    /// visible label must not promise scrollback the wire never carries.

    /// Fit-to-width is client-side only. The bridge attaches passive
    /// with rows==0/cols==0 so it never shrinks a PTY, and `writer.forget()`
    /// leaves no upstream handle. A page that learned to ask for a resize would
    /// collapse every terminal user's pane to phone width.
    ///
    /// Anchored on the WIRE vocabulary and on sending, not on the bare word
    /// "Resize". A bare match also banned `ResizeObserver`, which is a local DOM
    /// API and the right way to refit when the screen box changes without a
    /// window resize event - the guard would have refused it with a message
    /// about PTY geometry it has nothing to do with.

    #[test]
    fn bind_rows() {
        assert_eq!(bind_addr("127.0.0.1", 8722), "127.0.0.1:8722");
        assert_eq!(bind_addr("0.0.0.0", 80), "0.0.0.0:80");
        assert_eq!(bind_addr("::1", 8722), "[::1]:8722");
        assert_eq!(bind_addr("::", 8722), "[::]:8722");

        let a = WebArgs::default();
        assert_eq!(a.bind, "127.0.0.1");
        assert_eq!(a.session, proto::DEFAULT_SESSION);
    }
    /// One HTML sink fed from the model is script injection on a page that
    /// holds the operator's token: node titles, details, notes and decisions
    /// are free text any session writes. The page must therefore never build
    /// HTML from data at all.
    #[test]
    fn backlog_page_writes_no_html_from_data() {
        for sink in [
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "document.write",
        ] {
            assert!(
                !BACKLOG_PAGE.contains(sink),
                "the backlog page must not build HTML from data via {sink}"
            );
        }
        assert!(
            BACKLOG_PAGE.contains("connect-src 'self'"),
            "the page fetches only same-origin JSON"
        );
        assert!(
            BACKLOG_PAGE.contains(r#"class="controls""#),
            "the filter bar keeps the controls class the nav offset targets"
        );
        assert!(
            BACKLOG_PAGE.contains(r#"data-view="graph""#)
                && BACKLOG_PAGE.contains("function drawGraph()"),
            "the live board exposes its graph view"
        );
        assert!(
            BACKLOG_PAGE.contains("createElementNS")
                && BACKLOG_PAGE.contains("graph.edges")
                && BACKLOG_PAGE.contains("graph.nodes"),
            "the browser builds SVG nodes and paths from the model answer"
        );
        assert!(
            BACKLOG_PAGE.contains("graph view needs the live board (fno mux serve --web)")
                && BACKLOG_PAGE.contains("if (SNAPSHOT)"),
            "the static page explains its disabled graph view"
        );
        assert!(
            BACKLOG_PAGE.contains("graph.edges")
                && BACKLOG_PAGE.contains("graph.nodes")
                && BACKLOG_PAGE.contains("openNode(id)"),
            "the graph draws model edges and nodes into the existing detail action"
        );
        assert!(
            BACKLOG_PAGE.contains("const wrapGraphTitle")
                && BACKLOG_PAGE.contains("wrapGraphTitle(card.title, 32)"),
            "the SVG keeps a long card title visible on a second line"
        );
        assert!(
            BACKLOG_PAGE.contains("svg.setAttribute(\"role\", \"group\")")
                && BACKLOG_PAGE.contains("group.setAttribute(\"role\", \"button\")"),
            "graph nodes stay accessible as controls inside a labeled group"
        );
        // The copied-id surface: the three render sites bind the one copy
        // handler, and no fourth site appeared unbound.
        assert!(
            BACKLOG_PAGE.contains("function bindIdCopy(el, id)"),
            "the one copy handler exists"
        );
        assert!(
            BACKLOG_PAGE.contains("bindIdCopy(cidEl, card.id)"),
            "kanban card ids copy"
        );
        assert!(
            BACKLOG_PAGE.contains("bindIdCopy(lidEl, card.id)"),
            "list row ids copy"
        );
        assert!(
            BACKLOG_PAGE.contains("bindIdCopy(idSpan, card.id)"),
            "the panel id copies"
        );
        assert_eq!(
            BACKLOG_PAGE.matches("bindIdCopy(").count(),
            4,
            "the handler binds at exactly the three render sites plus its definition"
        );
        assert!(
            BACKLOG_PAGE.contains("ev.stopPropagation()"),
            "an id tap must not open the row behind it"
        );
        // The search bar is the first control on the page and holds the one
        // find input, its clear button and the focus key.
        let bar = BACKLOG_PAGE
            .find(r#"id="searchbar""#)
            .expect("the search bar exists");
        let controls = BACKLOG_PAGE
            .find(r#"id="controls""#)
            .expect("controls exist");
        assert!(bar < controls, "the search bar leads the page");
        assert!(
            BACKLOG_PAGE.contains(r#"placeholder="search: s:ready h:codex -t:epic, ? for keys""#),
            "the placeholder names what search covers"
        );
        assert!(
            BACKLOG_PAGE.contains(r#"id="f-q-clear""#),
            "the clear button exists"
        );
        assert!(
            BACKLOG_PAGE.contains(r#"id="searchkey""#),
            "the focus key shows in the box"
        );
        assert!(
            !BACKLOG_PAGE.contains(r#"size="14""#),
            "the old hidden find input is gone"
        );
        assert!(
            BACKLOG_PAGE.contains("b.dataset.id = card.id;"),
            "board rows carry their id for the j/k selection"
        );
        // The filter state reads every key FILTER_KEYS iterates: one missing
        // key made filterParams throw "state[k] is not iterable" on every
        // fresh served board, and the route drew no cards at all (the
        // snapshot copy tolerates the undefined, which is why only the
        // bridge showed it). Found live on the deployed binary 2026-10-01.
        for key in [
            "project", "epic", "status", "priority", "size", "lead", "kind",
        ] {
            let line = format!(r#"{}: initial.getAll("{}")"#, key, key);
            assert!(
                BACKLOG_PAGE.contains(&line),
                "the filter state must read {key} or filterParams throws at boot"
            );
        }
        // The recent-search store: one guarded localStorage key, recorded on
        // every executed search, with the dropdown element present.
        assert!(
            BACKLOG_PAGE.contains("const RECENT_KEY = \"fno-backlog-recent-searches\";"),
            "the recent-search key exists"
        );
        assert!(
            BACKLOG_PAGE.contains("recordRecent(state.q);"),
            "every executed search is recorded"
        );
        assert!(
            BACKLOG_PAGE.contains(r#"id="recent""#),
            "the recent-search dropdown exists"
        );
        assert!(
            BACKLOG_PAGE.contains("function lsGet(k)")
                && BACKLOG_PAGE.contains("function lsSet(k, v)"),
            "localStorage access stays guarded by lsGet/lsSet"
        );
    }

    /// The three pure helpers must hold their contracts when run for real,
    /// not when re-implemented in Rust: lift them from the shipped page and
    /// run the cases under node (same rule as evicted_row_count above).
    #[test]
    fn backlog_page_helpers_hold_under_node() {
        let asserts = r#"
const eq = (got, want, what) => {
  if (got !== want) { console.error("FAIL " + what + ": got " + got + ", want " + want); process.exit(1); }
};
// cellHead: "<column> <total>"
eq(cellHead({column: "Now", total: 12}), "Now 12", "cell head");
// laneTotal: the lane's whole count from its cells' (uncapped) totals.
eq(laneTotal({cells: [{total: 3}, {total: 4}, {}]}), 7, "lane total");
// mergeBoard: an errors-only answer keeps the last lanes and stamps staleness.
const last = {lanes: [{key: "p"}], fetched_at: 111};
const bad = mergeBoard(last, {errors: ["boom"], lanes: []}, 222);
eq(bad.lanes.length, 1, "errors-only keeps the last lanes");
eq(bad.stale_since, 111, "stale_since names the kept board fetch time");
eq(bad.errors[0], "boom", "the error lines carry");
eq(bad.fetched_at, 111, "the kept board keeps its fetch time");
// A good answer is taken whole, stamped with its own fetch time.
const good = mergeBoard(last, {errors: [], lanes: [{key: "q"}]}, 333);
eq(good.lanes.length, 1, "a good answer lanes carry");
eq(good.errors.length, 0, "a good answer clears the errors");
eq(good.fetched_at, 333, "a good answer is stamped at its fetch time");
// hiddenLine: the off-board disclosure line, empty when nothing is hidden.
eq(hiddenLine(null), "", "no hidden field reads empty");
eq(hiddenLine({total: 0}), "", "a zero total reads empty");
eq(hiddenLine({total: 3, deferred: 1, superseded: 1, roadmap: 1}),
   "off-board 3 (deferred 1, superseded 1, roadmap 1)", "the buckets carry");
// offBoardFacts: the served model's fields pass through; the snapshot's
// flat cards compute the same facts, capped at 200 cards.
const served = offBoardFacts({hidden: {total: 2, deferred: 2, superseded: 0, roadmap: 0}, offboard: [{id: "a"}]});
eq(served.hidden.total, 2, "served hidden passes through");
eq(served.cards.length, 1, "served offboard passes through");
const snap = offBoardFacts({cards: [
  {id: "a", column: "Off-board", status: "deferred"},
  {id: "b", column: "Off-board", status: "superseded"},
  {id: "c", column: "Off-board", kind: "roadmap", status: "ready"},
  {id: "d", column: "Next", status: "ready"},
]});
eq(snap.hidden.total, 3, "snapshot cards compute the total");
eq(snap.hidden.deferred, 1, "snapshot deferred bucket");
eq(snap.hidden.roadmap, 1, "the roadmap kind buckets first");
eq(snap.cards.length, 3, "the snapshot reveal lists the off-board cards");
console.log("backlog page helpers: 18 cases ok");
"#;
        let src = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            lift_js_fn(BACKLOG_PAGE, "cellHead"),
            lift_js_fn(BACKLOG_PAGE, "laneTotal"),
            lift_js_fn(BACKLOG_PAGE, "mergeBoard"),
            lift_js_fn(BACKLOG_PAGE, "hiddenLine"),
            lift_js_fn(BACKLOG_PAGE, "offBoardFacts"),
            asserts
        );
        let path =
            std::env::temp_dir().join(format!("fno-backlog-helpers-{}.mjs", std::process::id()));
        std::fs::write(&path, src).expect("temp dir writable");
        let out = std::process::Command::new("node").arg(&path).output();
        let _ = std::fs::remove_file(&path);
        match out {
            Err(e) => {
                // On CI a missing node means the assertions never ran, and a
                // skip that reads as a pass is exactly the failure to prevent.
                assert!(
                    std::env::var_os("CI").is_none(),
                    "node is required on CI to exercise the shipped backlog helpers: {e}"
                );
                println!(
                    "SKIPPED backlog_page_helpers_hold_under_node: node not runnable ({e}); \
                     nothing was asserted"
                );
            }
            Ok(o) => {
                // The end-of-harness marker is the whole verdict.
                let stdout = String::from_utf8_lossy(&o.stdout);
                assert!(
                    stdout.contains("backlog page helpers: 18 cases ok"),
                    "the shipped backlog helpers did not clear every case:\n{}{}",
                    stdout,
                    String::from_utf8_lossy(&o.stderr)
                );
            }
        }
    }
    #[test]
    fn nav_fragment_rows() {
        for (page, name) in [
            (NavPage::Live, "live"),
            (NavPage::Backlog, "backlog"),
            (NavPage::Team, "rundown"),
            (NavPage::Hq, "hq"),
        ] {
            let frag = nav_fragment(page);
            // The CSS selector also names the attribute; count the link tags.
            assert_eq!(frag.matches("aria-current=\"page\" href=\"#\"").count(), 1);
            assert!(frag.contains(&format!("data-current=\"{name}\"")));
            assert!(frag.contains(&format!(
                "<a data-page=\"{name}\" aria-current=\"page\" href=\"#\">{name}</a>"
            )));
            assert!(frag.contains("encodeURIComponent(t)"));
            assert!(frag.contains("encodeURIComponent(pj)"));
        }

        let out = with_nav(
            "<html><body data-local=\"true\"><p>x</p></body></html>",
            NavPage::Backlog,
        );
        assert!(out.contains("<body data-local=\"true\"><nav class=\"fno-nav\""));
        let out = with_nav("<html><BODY><p>x</p></BODY></html>", NavPage::Team);
        assert!(out.contains("<BODY><nav class=\"fno-nav\""));
        let out = with_nav("<p>no body</p>", NavPage::Live);
        assert!(out.starts_with("<nav class=\"fno-nav\""));

        assert!(nav_fragment(NavPage::Backlog).contains(".controls{top:var(--fno-nav-h)}"));
        assert!(!nav_fragment(NavPage::Live).contains(".controls"));
        assert!(!nav_fragment(NavPage::Team).contains(".controls"));
    }

    #[tokio::test]
    async fn team_requires_token_and_serves_private_file_without_cache() {
        let dir = std::env::temp_dir().join(format!("fno-web-team-{}-serve", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lead.html");
        std::fs::write(&path, "<body><p>PRIVATE-TEAM-MARKER</p></body>").unwrap();
        let response = private_page_response(
            &path,
            Some("right"),
            "right",
            "fno agent hq chart rundown",
            NavPage::Team,
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&body).contains("PRIVATE-TEAM-MARKER"));
        // The served team page carries the shared nav (inserted after <body>).
        let text = String::from_utf8_lossy(&body).to_string();
        assert!(text.contains("nav class=\"fno-nav\" data-current=\"rundown\""));
        let denied = private_page_response(
            &path,
            Some("wrong"),
            "right",
            "fno agent hq chart rundown",
            NavPage::Team,
        )
        .await;
        assert_eq!(denied.status(), axum::http::StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(denied.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("PRIVATE-TEAM-MARKER"));
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn team_republish_truth_table() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        // Unauthorized never starts a render.
        assert!(!team_needs_republish(false, None, now));
        // A missing file is maximally stale.
        assert!(team_needs_republish(true, None, now));
        // Fresh, and a future mtime, read as fresh.
        let fresh = now.checked_sub(Duration::from_secs(10)).unwrap();
        assert!(!team_needs_republish(true, Some(fresh), now));
        let future = now.checked_add(Duration::from_secs(10)).unwrap();
        assert!(!team_needs_republish(true, Some(future), now));
        // Older than the threshold needs a render.
        let stale = now
            .checked_sub(TEAM_REPUBLISH_AFTER + Duration::from_secs(1))
            .unwrap();
        assert!(team_needs_republish(true, Some(stale), now));
        // Exactly at the threshold is still fresh ("older than" is strict).
        let boundary = now.checked_sub(TEAM_REPUBLISH_AFTER).unwrap();
        assert!(!team_needs_republish(true, Some(boundary), now));
        // The served marker names a page sibling by this exact shape, and
        // the authorized serve stamps it: drift on either side stands the
        // daemon arm down forever (the feature fails silent in production,
        // every test green).
        let dir = tempfile::tempdir().unwrap();
        let page = dir.path().join("rundown.html");
        assert_eq!(served_marker(&page), dir.path().join("rundown.html.served"));
        std::fs::write(&page, b"<html></html>").unwrap();
        assert!(!served_marker(&page).exists());
        let _ = std::fs::write(served_marker(&page), b"");
        assert!(served_marker(&page).exists());
    }

    #[tokio::test]
    async fn republish_is_single_flight() {
        // With the flag held, a second start returns without spawning: no task
        // ever runs, so the flag survives the call untouched.
        TEAM_REPUBLISHING.store(true, Ordering::SeqCst);
        start_team_republish(Path::new("/tmp/never-written-lead.html"));
        tokio::task::yield_now().await;
        assert!(TEAM_REPUBLISHING.load(Ordering::SeqCst));
        TEAM_REPUBLISHING.store(false, Ordering::SeqCst);
    }
    #[tokio::test]
    async fn router_gates_hq_model_on_the_token_and_drops_fleet() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (tx, _) = broadcast::channel(4);
        let (_shutdown_tx, shutdown) = tokio::sync::watch::channel(false);
        let hq = Arc::new(crate::web_hq::HqCache::default());
        // A recorded, spent first read: the route spawns no fleet read.
        let fleet = serde_json::json!({"machines": [{"host": "u", "sessions": []}]});
        let none = backlog_model::Inputs::default();
        hq.board()
            .await
            .record(Ok(fleet), Err("x".into()), &none, 1, &[]);
        hq.first.set(()).unwrap();
        let state = AppState {
            tx,
            snap: Arc::new(Mutex::new(Snapshot::default())),
            token: Arc::<str>::from("right"),
            rundown_html: std::env::temp_dir().join("fno-web-hq-lead.html"),
            rundown_republish: false,
            hq,
            writable: true,
            invites: invite_source(None),
            click_journal: None,
            model: Default::default(),
            shutdown,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router(state)).await.unwrap();
        });
        let ask = |path: &'static str| async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let req =
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
            stream.write_all(req.as_bytes()).await.unwrap();
            let mut reply = String::new();
            stream.read_to_string(&mut reply).await.unwrap();
            reply
        };
        let reply = ask("/command?t=right&question=q-1").await;
        assert!(reply.starts_with("HTTP/1.1 308"), "{reply}");
        assert!(
            reply.contains("location: hq?t=right&question=q-1"),
            "{reply}"
        );
        let reply = ask("/command/model.json?t=right").await;
        assert!(reply.starts_with("HTTP/1.1 308"), "{reply}");
        assert!(
            reply.contains("location: ../hq/model.json?t=right"),
            "{reply}"
        );
        let reply = ask("/hq/model.json").await;
        assert!(reply.starts_with("HTTP/1.1 401"), "{reply}");
        let reply = ask("/hq/model.json?t=right").await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(reply.contains("cache-control: no-store"), "{reply}");
        assert!(reply.contains("\"host\":\"u\""), "{reply}");
        assert!(reply.contains("\"questions_error\":\"x\""), "{reply}");
        assert!(ask("/hq/decisions.json").await.starts_with("HTTP/1.1 401"));
        assert!(ask("/hq").await.starts_with("HTTP/1.1 401"));
        let reply = ask("/hq?t=right").await;
        assert!(reply.contains("data-current=\"hq\""), "{reply}");
        let reply = ask("/fleet?t=right").await;
        assert!(reply.starts_with("HTTP/1.1 404"), "{reply}");
        server.abort();
    }

    // AC3-HP second half: the deep link opens the question even under the
    // /fno path prefix a tailscale route can mount (the Oct 18 report's
    // /fno/backlog shape). The prefix must reach the auth wall, a green
    // token, and the page itself, or the phone lands on a 404.
    #[tokio::test]
    async fn the_board_answers_under_the_fno_prefix_mount() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (tx, _) = broadcast::channel(4);
        let (_shutdown_tx, shutdown) = tokio::sync::watch::channel(false);
        let hq = Arc::new(crate::web_hq::HqCache::default());
        let fleet = serde_json::json!({"machines": [{"host": "u", "sessions": []}]});
        let none = backlog_model::Inputs::default();
        hq.board()
            .await
            .record(Ok(fleet), Err("x".into()), &none, 1, &[]);
        hq.first.set(()).unwrap();
        let state = AppState {
            tx,
            snap: Arc::new(Mutex::new(Snapshot::default())),
            token: Arc::<str>::from("right"),
            rundown_html: std::env::temp_dir().join("fno-web-hq-lead.html"),
            rundown_republish: false,
            hq,
            writable: true,
            invites: invite_source(None),
            click_journal: None,
            model: Default::default(),
            shutdown,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router(state)).await.unwrap();
        });
        let ask = |path: &'static str| async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let req =
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
            stream.write_all(req.as_bytes()).await.unwrap();
            let mut reply = String::new();
            stream.read_to_string(&mut reply).await.unwrap();
            reply
        };
        assert!(
            ask("/fno/backlog").await.starts_with("HTTP/1.1 401"),
            "the prefix reaches the auth wall"
        );
        let reply = ask("/fno/backlog?t=right").await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        let reply = ask("/fno/hq/model.json?t=right").await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        let reply = ask("/fno/hq?t=right").await;
        assert!(reply.contains("data-current=\"hq\""), "{reply}");
        server.abort();
    }
    fn tiny_frame() -> proto::Frame {
        proto::Frame {
            rows: 1,
            cols: 1,
            cells: vec![proto::Cell::default()],
            cursor_row: 0,
            cursor_col: 0,
            cursor_visible: false,
            scroll_offset: 0,
            history_lines: 0,
            blocks: Vec::new(),
        }
    }

    fn feed(snap: &Arc<Mutex<Snapshot>>, pane_id: u64) {
        let (tx, _rx) = broadcast::channel::<String>(16);
        forward(
            ServerMsg::Frame {
                pane_id,
                frame: tiny_frame(),
            },
            &tx,
            snap,
        );
    }
    #[test]
    fn snapshot_rows() {
        let (tx, _rx) = broadcast::channel::<String>(16);
        let snap = Arc::new(Mutex::new(Snapshot::default()));
        // rows*cols == 4 but only one cell: geometry_ok() is false.
        let bad = proto::Frame {
            rows: 2,
            cols: 2,
            cells: vec![proto::Cell::default()],
            ..tiny_frame()
        };
        forward(
            ServerMsg::Frame {
                pane_id: 7,
                frame: bad,
            },
            &tx,
            &snap,
        );
        assert!(
            snap.lock().unwrap().frames.is_empty(),
            "a geometry-inconsistent frame is dropped, never stored"
        );

        let snap = Arc::new(Mutex::new(Snapshot::default()));
        for pid in 0..(MAX_SNAPSHOT_PANES as u64 + 5) {
            feed(&snap, pid);
        }
        let s = snap.lock().unwrap();
        assert_eq!(s.frames.len(), MAX_SNAPSHOT_PANES, "bounded to the cap");
        assert!(!s.frames.contains_key(&0), "the stalest pane was evicted");
        assert!(
            s.frames.contains_key(&(MAX_SNAPSHOT_PANES as u64 + 4)),
            "the newest pane is retained"
        );

        let snap = Arc::new(Mutex::new(Snapshot::default()));
        feed(&snap, 0);
        for pid in 1..(MAX_SNAPSHOT_PANES as u64) {
            feed(&snap, pid);
        }
        feed(&snap, 0); // touch pane 0 again -> now the freshest
        for pid in MAX_SNAPSHOT_PANES as u64..(MAX_SNAPSHOT_PANES as u64 + 5) {
            feed(&snap, pid);
        }
        assert!(
            snap.lock().unwrap().frames.contains_key(&0),
            "a pane that keeps updating survives the eviction sweep"
        );
    }

    #[test]
    fn rev_is_stale_only_flags_known_different_revs() {
        assert!(!rev_is_stale(env!("FNO_MUX_CRATES_REV")));
        assert!(!rev_is_stale("unknown"), "cannot know, so never stale");
        assert!(rev_is_stale("deadbeef-old-build"));
    }

    #[test]
    fn live_bridge_records_reads_only_live_sibling_sessions() {
        let dir = temp_state_dir("scan");
        // Live: this test process under its real start token.
        let live = dir.join("web-live.json");
        std::fs::write(
            &live,
            serde_json::json!({
                "pid": std::process::id(),
                "started": proto::pid_start_time(std::process::id()),
                "port": 8945,
            })
            .to_string(),
        )
        .unwrap();
        // Corpse: a pid proven gone.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = child.id();
        child.wait().unwrap();
        let corpse = dir.join("web-corpse.json");
        std::fs::write(
            &corpse,
            serde_json::json!({"pid": dead_pid, "started": 1, "port": 8946}).to_string(),
        )
        .unwrap();
        // Unrelated file shape: ignored.
        std::fs::write(dir.join("other.json"), "{}").unwrap();
        let found: Vec<_> = live_bridge_records(&dir)
            .into_iter()
            .map(|(s, _)| s)
            .collect();
        assert_eq!(
            found,
            vec!["live".to_string()],
            "corpse and non-bridge skipped"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn status_web_rows() {
        // No state file: no bridge.
        let dir = temp_state_dir("status-none");
        assert_eq!(status_web("t", &dir.join("t.sock")), 1);
        let _ = std::fs::remove_dir_all(&dir);

        // A live record under this process's own pid and start token: up.
        let dir = temp_state_dir("status-live");
        let socket = dir.join("t.sock");
        let state = web_state_path(&socket).unwrap();
        std::fs::write(
            &state,
            serde_json::json!({
                "pid": std::process::id(),
                "started": proto::pid_start_time(std::process::id()),
                "bind": "127.0.0.1",
                "port": 8948,
                "rev": env!("FNO_MUX_CRATES_REV"),
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(status_web("t", &socket), 0);
        let _ = std::fs::remove_dir_all(&dir);

        // This live test process under a WRONG start token: the record reads
        // stale, so status reports no bridge instead of a stranger's pid.
        let dir = temp_state_dir("status-recycled");
        let socket = dir.join("t.sock");
        let state = web_state_path(&socket).unwrap();
        std::fs::write(
            &state,
            serde_json::json!({
                "pid": std::process::id(),
                "started": 1,
                "bind": "127.0.0.1",
                "port": 8949,
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(status_web("t", &socket), 1);
        assert!(state.exists(), "status is a read door: it never deletes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_loopback_accepts_only_the_loopback_names() {
        for good in [
            "localhost",
            "localhost:8722",
            "127.0.0.1",
            "127.0.0.1:8722",
            "[::1]",
            "[::1]:8722",
            "::1",
        ] {
            assert!(names_loopback(good), "{good} is loopback");
        }
        for bad in [
            "evil.example",
            "evil.example:8722",
            "127.0.0.1.example",
            "0.0.0.0",
            "0.0.0.0:8722",
            "[::]",
            "null",
        ] {
            assert!(!names_loopback(bad), "{bad} is not loopback");
        }
    }

    // The ruling's guard order: loopback bind, loopback Host, loopback
    // Origin when sent. The token gate sits after these in the handler.
    #[test]
    fn write_guard_refuses_in_ruling_order() {
        use axum::http::{header, HeaderMap};
        let mut h = HeaderMap::new();
        // AC6-ERR: a non-loopback bind is read-only, every request refused.
        let err = write_guard(&h, false).unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        assert_eq!(
            err.1,
            "the bridge is read-only: it is not bound to loopback"
        );
        // Host must always name loopback.
        let err = write_guard(&h, true).unwrap_err();
        assert_eq!(err.1, "write refused: Host does not name loopback");
        h.insert(header::HOST, "evil.example:8722".parse().unwrap());
        let err = write_guard(&h, true).unwrap_err();
        assert_eq!(err.1, "write refused: Host does not name loopback");
        h.insert(header::HOST, "127.0.0.1:8722".parse().unwrap());
        assert!(write_guard(&h, true).is_ok(), "no Origin header: pass");
        // AC5-ERR: an Origin that names another site is refused; `null` too.
        h.insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        let err = write_guard(&h, true).unwrap_err();
        assert_eq!(err.1, "write refused: Origin does not name loopback");
        h.insert(header::ORIGIN, "null".parse().unwrap());
        let err = write_guard(&h, true).unwrap_err();
        assert_eq!(err.1, "write refused: Origin does not name loopback");
        h.insert(header::ORIGIN, "http://127.0.0.1:8722".parse().unwrap());
        assert!(write_guard(&h, true).is_ok());
        h.insert(header::ORIGIN, "http://localhost:8722".parse().unwrap());
        assert!(write_guard(&h, true).is_ok());
    }

    // AC4, AC9, AC10, and the 503/404/400 map of the pure half.
    #[test]
    fn plan_act_maps_errors_and_plans_the_argv() {
        let inp = backlog_model::fixture(vec![serde_json::json!({
            "id": "x-1", "status": "ready", "priority": "p1"
        })]);
        // AC9-EDGE: a claimed card refuses a launch, naming the node.
        let claimed = backlog_model::fixture(vec![serde_json::json!({
            "id": "x-2", "status": "in_progress"
        })]);
        let err = plan_act(&claimed, &Act::Blueprint { id: "x-2".into() }).unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(
            err.1,
            "x-2 is already being worked; open its session instead"
        );
        // AC10-ERR: an external backend's field-edits reason is gone with
        // the field act itself; fields are agent-owned.
        // A failed read is 503; a miss is 404 naming the id; a bad field is
        // 400.
        let mut bad = backlog_model::fixture(Vec::new());
        bad.rows_error = Some("the store read failed".into());
        assert_eq!(
            plan_act(&bad, &Act::Target { id: "x-1".into() })
                .unwrap_err()
                .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            plan_act(
                &inp,
                &Act::Target {
                    id: "x-nope".into()
                }
            )
            .unwrap_err()
            .0,
            StatusCode::NOT_FOUND
        );
        // A field POST no longer deserializes: the Act enum has no field
        // variant, so the act route refuses it as unknown (AC7).
        let raw = serde_json::json!({
            "act": "field", "id": "x-1", "field": "color", "value": "blue"
        });
        let parsed: Result<Act, _> = serde_json::from_value(raw);
        assert!(parsed.is_err(), "act field is not an act any more");
        let err = plan_act(
            &inp,
            &Act::Rank {
                id: "x-nope".into(),
                place: "before".into(),
            },
        )
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    /// One request over a real listener; returns the whole reply.
    pub(super) async fn raw_request(addr: std::net::SocketAddr, head: &str, body: &[u8]) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        reply
    }

    pub(super) async fn serve_state(
        state: AppState,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router(state)).await.unwrap();
        });
        (addr, server)
    }

    pub(super) fn answer_state(
        writable: bool,
        invites: Option<Vec<crate::web_invites::Invite>>,
    ) -> AppState {
        let (tx, _) = broadcast::channel(4);
        let (_shutdown_tx, shutdown) = tokio::sync::watch::channel(false);
        AppState {
            tx,
            snap: Arc::new(Mutex::new(Snapshot::default())),
            token: Arc::<str>::from("right"),
            rundown_html: std::env::temp_dir().join("fno-web-answer-lead.html"),
            rundown_republish: true,
            hq: Default::default(),
            writable,
            invites: invite_source(invites),
            click_journal: None,
            model: Default::default(),
            shutdown,
        }
    }

    // Every refusal that comes before the question read: none starts the
    // door, so none can land a row. A bad token, a cross-origin page, a bad
    // body and a read-only bridge all stop here.
    #[tokio::test]
    async fn web_answer_route_refuses_before_any_process_starts() {
        let body = br#"{"option":1}"#;
        let post = |addr: std::net::SocketAddr, extra: &str, path: &str, body: &'static [u8]| {
            let head = format!(
                "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{extra}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                addr.port(),
                body.len()
            );
            async move { raw_request(addr, &head, body).await }
        };
        let (addr, server) = serve_state(answer_state(true, None)).await;
        let reply = post(addr, "", "/hq/answer/q-1?t=wrong", body).await;
        assert!(reply.starts_with("HTTP/1.1 401"), "{reply}");
        let reply = post(
            addr,
            "Origin: https://evil.example\r\n",
            "/hq/answer/q-1?t=right",
            body,
        )
        .await;
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
        assert!(reply.contains("Origin does not match Host"), "{reply}");
        let reply = post(addr, "", "/hq/answer/q-1?t=right", br#"{"option":0}"#).await;
        assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");
        let reply = post(addr, "", "/hq/answer/q-1?t=right", br#"{"delegate":true}"#).await;
        assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");
        let reply = post(addr, "", "/hq/answer/a%2Fb?t=right", body).await;
        assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");
        server.abort();
        let (addr, server) = serve_state(answer_state(false, None)).await;
        let reply = post(addr, "", "/hq/answer/q-1?t=right", body).await;
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
        assert!(reply.contains("read-only"), "{reply}");
        server.abort();
    }

    // The read-side route table for a scoped viewer: board reads carry only
    // the invited chain, a foreign node refuses with the reason, and the
    // machine-wide surfaces name why they refuse.
    fn invite(email: &str, scope: &str) -> crate::web_invites::Invite {
        crate::web_invites::Invite {
            email: email.into(),
            scope: scope.into(),
            role: crate::web_invites::Role::Viewer,
        }
    }

    // Pin the read model the routes serve, so no test reads the real graph.
    pub(super) async fn seed_model(
        st: &mut AppState,
        rows: Vec<serde_json::Value>,
    ) -> std::sync::Arc<backlog_model::Inputs> {
        let inputs = std::sync::Arc::new(backlog_model::fixture(rows));
        *st.model.lock().await = Some(CachedModel {
            version: None,
            at: std::time::Instant::now(),
            inputs: std::sync::Arc::clone(&inputs),
        });
        inputs
    }

    #[tokio::test]
    async fn scoped_invites_read_a_filtered_backlog_board() {
        let list = vec![
            invite("ann@example.com", "x-b1"),
            invite("pat@example.com", "*"),
        ];
        let mut st = answer_state(true, Some(list));
        seed_model(
            &mut st,
            vec![
                serde_json::json!({"id": "x-b1", "status": "ready", "type": "epic"}),
                serde_json::json!({"id": "x-kid", "status": "ready", "parent": "x-b1"}),
                serde_json::json!({"id": "x-oth", "status": "ready"}),
            ],
        )
        .await;
        let (addr, server) = serve_state(st).await;
        let get = |addr: std::net::SocketAddr, login: &str| {
            let head = format!(
                "GET /backlog/model.json?t=right HTTP/1.1\r\nHost: machine.tail1.ts.net\r\nTailscale-User-Login: {login}\r\nConnection: close\r\n\r\n"
            );
            async move { raw_request(addr, &head, b"").await }
        };
        let reply = get(addr, "ann@example.com").await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(
            reply.contains("\"x-b1\"") && reply.contains("\"x-kid\""),
            "{reply}"
        );
        assert!(!reply.contains("\"x-oth\""), "{reply}");
        let reply = get(addr, "pat@example.com").await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(
            reply.contains("\"x-oth\""),
            "the star invite reads whole: {reply}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn scoped_invites_refuse_a_node_outside_their_invite() {
        let list = vec![invite("ann@example.com", "x-b1")];
        let mut st = answer_state(true, Some(list));
        seed_model(
            &mut st,
            vec![
                serde_json::json!({"id": "x-b1", "status": "ready", "type": "epic"}),
                serde_json::json!({"id": "x-kid", "status": "ready", "parent": "x-b1"}),
                serde_json::json!({"id": "x-oth", "status": "ready"}),
            ],
        )
        .await;
        let (addr, server) = serve_state(st).await;
        let get = |addr: std::net::SocketAddr, path: &str| {
            let head = format!(
                "GET {path} HTTP/1.1\r\nHost: machine.tail1.ts.net\r\nTailscale-User-Login: ann@example.com\r\nConnection: close\r\n\r\n"
            );
            async move { raw_request(addr, &head, b"").await }
        };
        let reply = get(addr, "/backlog/node.json?t=right&id=x-oth").await;
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
        assert!(
            reply.contains("this invite does not cover that node"),
            "{reply}"
        );
        let reply = get(addr, "/backlog/node.json?t=right&id=x-kid").await;
        assert!(
            reply.starts_with("HTTP/1.1 200"),
            "descendant serves: {reply}"
        );
        assert!(reply.contains("\"x-kid\""), "{reply}");
        // An in-scope trace serves through the shell-out, so only the
        // refusal runs here.
        let reply = get(addr, "/trace/model.json?t=right&node=x-oth").await;
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
        assert!(
            reply.contains("this invite does not cover that node"),
            "{reply}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn scoped_invites_refuse_the_machine_wide_reads() {
        let list = vec![
            invite("ann@example.com", "x-b1"),
            invite("pat@example.com", "*"),
        ];
        // A fresh page file: the serve never starts a republish render, and
        // the star-invite control reads it back verbatim.
        let rundown = std::env::temp_dir().join("fno-web-answer-lead.html");
        std::fs::write(&rundown, "<html>the org chart</html>").unwrap();
        let st = answer_state(true, Some(list));
        let (addr, server) = serve_state(st).await;
        let get = |addr: std::net::SocketAddr, path: &str, login: &str| {
            let head = format!(
                "GET {path} HTTP/1.1\r\nHost: machine.tail1.ts.net\r\nTailscale-User-Login: {login}\r\nConnection: close\r\n\r\n"
            );
            async move { raw_request(addr, &head, b"").await }
        };
        let reply = get(addr, "/rundown?t=right", "ann@example.com").await;
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
        assert!(
            reply.contains("the rundown chart is machine-wide"),
            "{reply}"
        );
        let reply = get(addr, "/hq/decisions.json?t=right", "ann@example.com").await;
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
        assert!(
            reply.contains("the decision log is machine-wide"),
            "{reply}"
        );
        let reply = get(addr, "/rundown?t=right", "pat@example.com").await;
        assert!(reply.starts_with("HTTP/1.1 200"), "star invite: {reply}");
        assert!(reply.contains("the org chart"), "{reply}");
        server.abort();
    }

    #[tokio::test]
    async fn scoped_invites_read_a_filtered_hq_board() {
        // The one section the filter's contract names (R1: one typed parse).
        #[derive(serde::Deserialize)]
        struct HqBody {
            #[serde(default)]
            machines: serde_json::Value,
        }
        let list = vec![
            invite("ann@example.com", "x-hq1"),
            invite("pat@example.com", "*"),
        ];
        let mut st = answer_state(true, Some(list));
        let inputs = seed_model(
            &mut st,
            vec![
                serde_json::json!({"id": "x-hq1", "status": "in_review", "pr_number": 42}),
                serde_json::json!({"id": "x-oth", "status": "in_review", "pr_number": 43}),
            ],
        )
        .await;
        // Seed the cache and stamp read_at, so claim_refresh never starts a
        // real fleet read inside the test.
        st.hq.first.set(()).unwrap();
        st.hq.board().await.record(
            Ok(serde_json::json!({"machines": [
                {"host": "u", "local": true, "fresh": true, "sessions": [
                    {"name": "kara", "session_id": "976b7b64", "harness": "claude", "role": "lead"}
                ]},
                {"host": "mac2", "fresh": false, "age_s": 900, "sessions": [
                    {"name": "t-two", "harness": "codex"}
                ]}
            ]})),
            Err("the test seeds no questions".into()),
            &inputs,
            0,
            &[],
        );
        let (addr, server) = serve_state(st).await;
        let get = |addr: std::net::SocketAddr, login: &str| {
            let head = format!(
                "GET /hq/model.json?t=right HTTP/1.1\r\nHost: machine.tail1.ts.net\r\nTailscale-User-Login: {login}\r\nConnection: close\r\n\r\n"
            );
            async move { raw_request(addr, &head, b"").await }
        };
        let reply = get(addr, "pat@example.com").await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        let body = reply.split("\r\n\r\n").nth(1).unwrap_or_default();
        let hq: HqBody = serde_json::from_str(body).unwrap();
        assert_eq!(
            hq.machines.as_array().map(Vec::len),
            Some(2),
            "star invite reads the fleet grid"
        );
        assert!(
            reply.contains("\"x-oth\""),
            "star invite reads whole: {reply}"
        );
        let reply = get(addr, "ann@example.com").await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        let body = reply.split("\r\n\r\n").nth(1).unwrap_or_default();
        let hq: HqBody = serde_json::from_str(body).unwrap();
        assert_eq!(
            hq.machines,
            serde_json::json!([]),
            "the fleet grid drops whole"
        );
        assert!(reply.contains("\"x-hq1\""), "{reply}");
        assert!(!reply.contains("\"x-oth\""), "{reply}");
        server.abort();
    }

    // Frames have no node mapping, so the bridge feed closes the upgraded
    // socket with the 1008 policy code and the reason in the close text.
    #[allow(clippy::indexing_slicing)]
    #[tokio::test]
    async fn scoped_invites_get_a_policy_close_on_the_bridge_feed() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let list = vec![invite("ann@example.com", "x-b1")];
        let (addr, server) = serve_state(answer_state(true, Some(list))).await;
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let head = "GET /ws?t=right HTTP/1.1\r\nHost: machine.tail1.ts.net\r\nTailscale-User-Login: ann@example.com\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
        stream.write_all(head.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(text.starts_with("HTTP/1.1 101"), "{text}");
        let frame_at = text.find("\r\n\r\n").map_or(buf.len(), |i| i + 4);
        let frame = &buf[frame_at..];
        assert_eq!(
            frame.first().copied(),
            Some(0x88),
            "a close frame ends the feed: {text}"
        );
        assert!(frame.len() >= 4, "{text:?}");
        let len = frame[1] as usize;
        let code = u16::from_be_bytes([frame[2], frame[3]]);
        assert_eq!(code, 1008, "policy close: {text:?}");
        let reason = std::str::from_utf8(&frame[4..2 + len]).unwrap_or("");
        assert!(reason.contains("machine-wide"), "{reason}");
        server.abort();
    }

    // A login's look lands in its own store file with three prior versions;
    // a request that names nobody gets no look and no views.
    #[allow(clippy::indexing_slicing)]
    #[tokio::test]
    async fn hq_mine_routes_answer_per_login() {
        use crate::web_invites::{Invite, Role};
        let login = "mine-route@example.com";
        let dir = crate::web_mine::store_root().join(crate::web_mine::login_key(login));
        let _ = std::fs::remove_dir_all(&dir);
        let list = vec![Invite {
            email: login.into(),
            scope: "*".into(),
            role: Role::Viewer,
        }];
        let post = |addr: std::net::SocketAddr, login: &str, body: String| {
            let head = format!(
                "POST /hq/mine/look?t=right HTTP/1.1\r\nHost: machine.tail1.ts.net\r\nTailscale-User-Login: {login}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            async move { raw_request(addr, &head, body.as_bytes()).await }
        };
        let (addr, server) = serve_state(answer_state(true, Some(list))).await;
        for theme in ["light", "dark", "auto", "dark"] {
            let body = format!(
                r#"{{"project":"fno","look":{{"theme":"{theme}","density":"compact","accent":"blue"}}}}"#
            );
            let reply = post(addr, login, body).await;
            assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        }
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("fno.json")).unwrap()).unwrap();
        assert_eq!(doc["look"]["theme"], "dark");
        assert_eq!(doc["history"].as_array().unwrap().len(), 3);
        let reply = post(
            addr,
            login,
            r#"{"project":"fno","look":{"accent":"url(https://x)"}}"#.into(),
        )
        .await;
        assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");
        assert!(reply.contains("accent ink|blue|green|violet"), "{reply}");
        server.abort();

        let (addr, server) = serve_state(answer_state(true, None)).await;
        let reply = post(
            addr,
            "eve@example.com",
            r#"{"project":"fno","look":{}}"#.into(),
        )
        .await;
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
        assert!(
            reply.contains("no login on this request: personal look needs an invite"),
            "{reply}"
        );
        let head = "GET /hq/mine.json?t=right HTTP/1.1\r\nHost: machine.tail1.ts.net\r\nTailscale-User-Login: eve@example.com\r\nConnection: close\r\n\r\n";
        let reply = raw_request(addr, head, b"").await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(reply.contains(r#""projects":[]"#), "{reply}");
        server.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }
    // AC6's page half: the act controls exist only on a writing bridge.
    #[tokio::test]
    async fn backlog_page_flags_writability() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir = std::env::temp_dir().join(format!("fno-web-flag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, _) = broadcast::channel(4);
        let (_shutdown_tx, shutdown) = tokio::sync::watch::channel(false);
        let state = AppState {
            tx,
            snap: Arc::new(Mutex::new(Snapshot::default())),
            token: Arc::<str>::from("right"),
            rundown_html: dir.join("lead.html"),
            rundown_republish: true,
            hq: Default::default(),
            writable: true,
            invites: invite_source(None),
            click_journal: None,
            model: Default::default(),
            shutdown,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router_state = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, router(router_state)).await.unwrap();
        });
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET /backlog?t=right HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        assert!(reply.contains("data-writable=\"true\""), "{reply}");
        server.abort();
        // The read-only state: no flag anywhere in the page.
        let ro_state = AppState {
            writable: false,
            invites: invite_source(None),
            click_journal: None,
            ..state.clone()
        };
        let ro_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ro_addr = ro_listener.local_addr().unwrap();
        let ro_server = tokio::spawn(async move {
            axum::serve(ro_listener, router(ro_state)).await.unwrap();
        });
        let mut stream = tokio::net::TcpStream::connect(ro_addr).await.unwrap();
        stream
            .write_all(b"GET /backlog?t=right HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        assert!(!reply.contains("data-writable"), "{reply}");
        ro_server.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn web_inputs_turn_autocorrect_off() {
        let share = crate::share::share_fragment(1);
        let pages: [(&str, &str); 5] = [
            ("web_page.html", PAGE),
            ("web_backlog.html", BACKLOG_PAGE),
            ("web_hq.html", HQ_PAGE),
            ("web_trace.html", TRACE_PAGE),
            ("share fragment", share.as_str()),
        ];
        let off = [
            "autocorrect=\"off\"",
            "autocapitalize=\"off\"",
            "spellcheck=\"false\"",
        ];
        let untyped = [
            "checkbox", "date", "radio", "range", "hidden", "file", "button", "submit",
        ];
        let mut checked = 0;
        let mut bad = Vec::new();
        for (name, html) in pages {
            let mut rest = html;
            while let Some(at) = rest.find("<input") {
                let tag = &rest[at..];
                let end = tag.find('>').unwrap_or(tag.len());
                let tag = &tag[..end];
                rest = &rest[at + 6..];
                if untyped
                    .iter()
                    .any(|t| tag.contains(&format!("type=\"{t}\"")))
                {
                    continue;
                }
                checked += 1;
                if !off.iter().all(|a| tag.contains(a)) {
                    bad.push(format!("{name}: {tag}"));
                }
            }
            let mut rest = html;
            while let Some(at) = rest.find("createElement(\"input\")") {
                let site = &rest[at..];
                let mut cut = site.len().min(600);
                while !site.is_char_boundary(cut) {
                    cut -= 1;
                }
                let window = &site[..cut];
                rest = &rest[at + 10..];
                checked += 1;
                for a in ["autocorrect", "autocapitalize", "spellcheck"] {
                    if !window.contains(&format!("setAttribute(\"{a}\"")) {
                        bad.push(format!("{name}: createElement input lacks {a}"));
                    }
                }
            }
        }
        assert!(checked >= 4, "scan found only {checked} inputs");
        assert!(bad.is_empty(), "inputs without autocorrect off: {bad:#?}");
    }
}
