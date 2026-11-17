//! The door: a loopback TCP listener that `tailscale serve --tcp` puts on
//! the tailnet (ruling D2513: no relay). Per connection it runs the Noise
//! IK responder, checks the device store, then answers sealed requests one
//! at a time: an allowed method goes to the daemon through
//! [`client::call_if_running`] on a fresh socket per request (the daemon
//! serves one request per connection), anything else is refused here.
//!
//! The door never binds a non-loopback address and never starts a daemon.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;

use super::allow;
use super::channel::{self, Keypair};
use super::pairing::{self, now_unix};
use crate::client::{self, ClientError};
use crate::paths::AgentsHome;
use crate::protocol::{ErrorCode, Request, Response};

/// The door's port when none is given: next to the attention door's 8724.
pub const DEFAULT_PORT: u16 = 8725;
/// A handshake that does not finish in this long is dropped.
const HANDSHAKE_BOUND: Duration = Duration::from_secs(10);
/// A session with no request for this long is closed. The phone polls
/// every 2 s while open, so this only reaps a backgrounded app.
const SESSION_IDLE: Duration = Duration::from_secs(120);
/// How often the status file is rewritten.
const BEAT: Duration = Duration::from_secs(60);
/// The message prefix a client reads as "the daemon is down".
pub const DAEMON_DOWN: &str = "daemon_down";

#[derive(Debug, thiserror::Error)]
pub enum DoorError {
    #[error("refusing to bind {0}: the door binds loopback only; tailscale serve exposes it")]
    NotLoopback(SocketAddr),
    #[error("bind {0}: {1}")]
    Bind(SocketAddr, std::io::Error),
    #[error(transparent)]
    Pair(#[from] pairing::PairError),
}

/// The status file the deployed door rewrites each beat.
pub fn status_path(home: &AgentsHome) -> PathBuf {
    home.remote_dir().join("door-status.json")
}

/// What `door-status.json` holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DoorStatus {
    pub state: String,
    pub bind: String,
    pub port: u16,
    pub tailnet_dns: Option<String>,
    pub pid: u32,
    pub updated_at: u64,
    pub devices: usize,
}

/// Bind the door. Anything but a loopback address is refused before the
/// socket exists.
pub async fn bind(addr: SocketAddr) -> Result<TcpListener, DoorError> {
    if !addr.ip().is_loopback() {
        return Err(DoorError::NotLoopback(addr));
    }
    TcpListener::bind(addr)
        .await
        .map_err(|e| DoorError::Bind(addr, e))
}

fn write_status(home: &AgentsHome, bind: SocketAddr, tailnet_dns: &Option<String>) {
    let devices = pairing::with_store(home, |s| {
        s.devices.iter().filter(|d| d.revoked_at.is_none()).count()
    })
    .unwrap_or(0);
    let status = DoorStatus {
        state: "listening".into(),
        bind: bind.to_string(),
        port: bind.port(),
        tailnet_dns: tailnet_dns.clone(),
        pid: std::process::id(),
        updated_at: now_unix(),
        devices,
    };
    let path = status_path(home);
    let body = serde_json::to_vec_pretty(&status).expect("status serializes");
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, body).and_then(|()| std::fs::rename(&tmp, &path)) {
        eprintln!("fno-agent remote: status write {}: {e}", path.display());
    }
}

/// Serve the door on a bound listener until the process ends.
pub async fn serve(
    listener: TcpListener,
    home: AgentsHome,
    tailnet_dns: Option<String>,
) -> Result<(), DoorError> {
    let daemon = pairing::load_or_create_daemon_key(&home)?;
    let local = listener
        .local_addr()
        .map_err(|e| DoorError::Bind(SocketAddr::from(([127, 0, 0, 1], 0)), e))?;
    write_status(&home, local, &tailnet_dns);
    eprintln!("fno-agent remote: listening on {local}");
    let beat_home = home.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(BEAT);
        tick.tick().await;
        loop {
            tick.tick().await;
            write_status(&beat_home, local, &tailnet_dns);
        }
    });
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("fno-agent remote: accept: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let home = home.clone();
        let daemon = daemon.clone();
        tokio::spawn(async move {
            let line = session(stream, &home, &daemon).await;
            eprintln!("fno-agent remote: {peer}: {line}");
        });
    }
}

/// Run one device session to its end and return the line the log keeps.
pub async fn session<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    home: &AgentsHome,
    daemon: &Keypair,
) -> String {
    let authorize =
        |key: &[u8; channel::KEY_LEN], claim: &[u8]| match pairing::with_store(home, |s| {
            pairing::authorize(s, key, claim, now_unix())
        }) {
            Ok(verdict) => verdict.map_err(|r| r.to_string()),
            Err(e) => Err(format!("device store: {e}")),
        };
    let (mut ch, device) =
        match tokio::time::timeout(HANDSHAKE_BOUND, channel::respond(stream, daemon, authorize))
            .await
        {
            Err(_) => return "refused: handshake timed out".into(),
            Ok(Err(e)) => return format!("handshake: {e}"),
            Ok(Ok(open)) => open,
        };
    let mut answered = 0u64;
    loop {
        let body = match tokio::time::timeout(SESSION_IDLE, ch.recv()).await {
            Err(_) => {
                return format!(
                    "device {}: idle close after {answered} request(s)",
                    device.id
                )
            }
            Ok(Err(e)) if e.is_eof() => {
                return format!("device {}: closed after {answered} request(s)", device.id)
            }
            Ok(Err(e)) => return format!("device {}: session ended: {e}", device.id),
            Ok(Ok(body)) => body,
        };
        let resp = answer(home, &body).await;
        let bytes = serde_json::to_vec(&resp).expect("response serializes");
        if let Err(e) = ch.send(&bytes).await {
            return format!("device {}: reply failed: {e}", device.id);
        }
        answered += 1;
    }
}

/// Answer one opened request: refuse off-list methods here, forward the
/// rest to a running daemon unchanged.
pub async fn answer(home: &AgentsHome, body: &[u8]) -> Response {
    let req: Request = match serde_json::from_slice(body) {
        Ok(req) => req,
        Err(e) => return Response::err(0, ErrorCode::MalformedFrame, e.to_string()),
    };
    if !allow::allowed(&req.method) {
        return Response::err(
            req.id,
            ErrorCode::UnknownMethod,
            format!(
                "{} is not on the remote allow list ({})",
                req.method,
                allow::ALLOWED.join(", ")
            ),
        );
    }
    match client::call_if_running(home, &req).await {
        Ok(resp) => resp,
        Err(ClientError::DaemonNotRunning) => Response::err(
            req.id,
            ErrorCode::Internal,
            format!(
                "{DAEMON_DOWN}: the fno-agent daemon is not running; the door never starts one"
            ),
        ),
        Err(e) => Response::err(req.id, ErrorCode::Internal, format!("forward failed: {e}")),
    }
}
