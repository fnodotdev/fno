//! The web bridge's axum-free half: the marker a running bridge leaves
//! beside its socket, the liveness reads over it, the house stylesheet and
//! the backlog page constant. The desktop app and the CLI read these
//! without linking the HTTP stack, which lives behind the `web` feature.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::pid::Pid;
use crate::proto;

/// The backlog board page, vendored like PAGE. It draws the read model
/// ([`crate::backlog_model`]) through `/backlog/model.json` and
/// `/backlog/node.json` and computes nothing the model already answered.
pub(crate) const BACKLOG_PAGE: &str = include_str!("web_backlog.html");

/// The house stylesheet the backlog and command pages share. The CSP allows
/// inline style only, so [`house`] writes it into each page's `<style>`.
pub(crate) const HOUSE_CSS: &str = include_str!("web_house.css");

/// `page` with its `/* fno-house */` marker replaced by [`HOUSE_CSS`].
pub(crate) fn house(page: &str) -> String {
    page.replacen("/* fno-house */", HOUSE_CSS, 1)
}

/// Path of the bridge's live-state marker beside a session socket. The one
/// construction for writer and readers, so the file name cannot drift.
pub(crate) fn web_state_path(socket: &Path) -> Option<PathBuf> {
    let session = socket
        .file_stem()
        .and_then(|s| s.to_str())
        // socket_path() names the file <session>.sock; the stem is the name.
        .unwrap_or(proto::DEFAULT_SESSION);
    socket
        .parent()
        .map(|p| p.join(format!("web-{session}.json")))
}

/// The state-file path from a session name; the one lookup for readers
/// outside this module (`--stop` resolves the socket itself).
pub(crate) fn web_state_path_for_session(session: &str) -> Option<PathBuf> {
    web_state_path(&proto::socket_path(session).ok()?)
}

/// A session's live web bridge, as its state file records it.
pub(crate) struct LiveBridge {
    /// `http://<host>:<port>`, the host this machine reaches it at.
    pub(crate) base: String,
    pub(crate) token: String,
    /// True when the bridge listens on loopback only.
    pub(crate) loopback: bool,
}

/// Read a session's web-bridge state file. The file's existence alone
/// proves nothing, and neither does an answering port: the recorded pid
/// must still be live and the port must answer. `Err` is the start hint.
pub(crate) fn live_bridge(session: &str) -> Result<LiveBridge, String> {
    let hint = format!(
        "no web bridge for session {session}; start one with: fno mux serve --web --session {session}"
    );
    let state: serde_json::Value = web_state_path_for_session(session)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .ok_or_else(|| hint.clone())?;
    let bind = state
        .get("bind")
        .and_then(|v| v.as_str())
        .unwrap_or("127.0.0.1");
    let port = state
        .get("port")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| hint.clone())?;
    // A wide bind is reachable locally too; the URL says where THIS machine
    // finds it, mirroring the bind-time print's host hint.
    let wide = bind == "0.0.0.0" || bind == "::";
    let host = if wide { "127.0.0.1" } else { bind };
    // A bare TCP answer is not ownership - a dead bridge's port can be taken
    // by a stranger - so the record's pid must still be live, the same
    // identity --stop and --status already enforce.
    if !recorded_pid_is_live(&state) || !tcp_answers(host, port as u16) {
        return Err(hint);
    }
    // Bracket a literal IPv6 host; a bare ::1 in a URL truncates at the colon.
    let url_host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    Ok(LiveBridge {
        base: format!("http://{url_host}:{port}"),
        token: state["token"].as_str().unwrap_or_default().to_string(),
        loopback: !wide && (host.starts_with("127.") || host == "::1" || host == "localhost"),
    })
}

/// Print the pasteable per-pane URL. Exit codes follow the mux commands'
/// (0 printed, 1 no usable bridge).
pub(crate) fn print_pane_url(command: &str, session: &str, pane: u64) -> i32 {
    match live_bridge(session) {
        Ok(b) => {
            println!("{}/?t={}&pane={pane}", b.base, b.token);
            0
        }
        Err(hint) => {
            eprintln!("{command}: {hint}");
            1
        }
    }
}

/// A state-file record whose pid is alive under its recorded start token.
/// Dead, zombie, or a reused pid all read as not-live, so a corpse file never
/// passes as a bridge. The one liveness read for every state-file consumer
/// (`--url`, `--stop`, `--status`, and the corpse sweep).
pub(crate) fn recorded_pid_is_live(v: &serde_json::Value) -> bool {
    let Some(pid) = v
        .get("pid")
        .and_then(|p| p.as_u64())
        .and_then(|p| Pid::try_from(p).ok())
    else {
        return false;
    };
    let recorded = v.get("started").and_then(|s| s.as_u64());
    !proto::pid_confirmed_dead(pid.as_raw())
        && !proto::pid_is_zombie(pid.as_raw())
        && recorded.is_none_or(|r| pid.start_time().is_some_and(|now| now == r))
}

/// The bridge marker's typed read (`web-<session>.json`, written by
/// `WebStateFile`): each reader deserializes it once at its boundary,
/// beside the raw Value the pid-liveness check consumes.
#[derive(Debug, serde::Deserialize)]
pub(crate) struct BridgeMarker {
    #[serde(default = "default_bridge_bind")]
    pub(crate) bind: String,
    pub(crate) port: u16,
    pub(crate) pid: Option<u32>,
}

fn default_bridge_bind() -> String {
    "127.0.0.1".to_string()
}

/// Does something answer TCP on host:port right now?
pub(crate) fn tcp_answers(host: &str, port: u16) -> bool {
    use std::net::ToSocketAddrs;
    (host, port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut it| it.next())
        .is_some_and(|addr| {
            std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
        })
}

/// The contract's default port (the OpenAPI servers block).
pub const DEFAULT_ATTENTION_PORT: u16 = 8724;

/// Every syntactically valid journal line, torn tails skipped.
pub(crate) fn journal_rows(text: &str) -> impl Iterator<Item = Value> + '_ {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
}

/// The `.fno` state base, the same anchor as agents_view's (`FNO_AGENT_HOME`'s
/// parent > `$HOME/.fno`). Mirrored here: agents_view is over the shrink-only
/// line budget, so it takes no new surface.
fn fno_dir() -> PathBuf {
    if let Some(v) = std::env::var_os("FNO_AGENT_HOME") {
        return PathBuf::from(&v)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".fno"));
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".fno")
}

pub(crate) fn journal_path() -> PathBuf {
    fno_dir().join("questions.jsonl")
}
