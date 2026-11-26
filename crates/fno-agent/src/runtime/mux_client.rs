//! In-process mux pane client: one control round trip to the mux server's socket, no `fno mux pane` subprocess. A CLI caller gets the socket dir from the front door, which exports `FNO_MUX_DIR` to every `fno-agent` it execs.
//!
//! The socket dir comes from `FNO_MUX_DIR` alone. The `fno` front door owns the full resolution (config state root, legacy fallback) and exports its answer where it launches the daemon. With the variable unset the client answers `None` and says so once through a `mux_dir_unset` event, so a missing export reads as a loud gap and never as a silent skip.

use std::io::ErrorKind;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fno_proto::{
    is_retryable, read_msg_sync, write_msg_sync, ClientMsg, ControlCommand, ProtoError, ServerMsg,
    PROTO_VERSION,
};

const DEFAULT_SESSION: &str = "main";
const SERVER_ENV: &str = "FNO_SERVER";
const LEGACY_SERVER_ENV: &str = "FNO_SESSION";
const MUX_DIR_ENV: &str = "FNO_MUX_DIR";
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const REPLY_DEADLINE: Duration = Duration::from_secs(30);

fn nonempty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// The session a call addresses: the explicit name, else `FNO_SERVER`, else the legacy `FNO_SESSION`, else the default.
fn resolve_session(explicit: Option<&str>) -> String {
    explicit
        .map(str::to_string)
        .or_else(|| nonempty_env(SERVER_ENV))
        .or_else(|| nonempty_env(LEGACY_SERVER_ENV))
        .unwrap_or_else(|| DEFAULT_SESSION.to_string())
}

/// The socket path for `session` under `dir`. A name with a separator or NUL is refused so it cannot leave the mux dir.
fn socket_in(dir: &str, session: &str) -> Option<PathBuf> {
    if session.is_empty() || session.contains('/') || session.contains('\0') {
        return None;
    }
    Some(PathBuf::from(dir).join(format!("{session}.sock")))
}

fn warn_mux_dir_unset() {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        let home = crate::paths::AgentsHome::from_env();
        let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
        emitter
            .emit(
                "mux_dir_unset",
                &serde_json::json!({
                    "env": MUX_DIR_ENV,
                    "effect": "in-process pane send and read answer nothing until the launcher exports it",
                }),
            )
            .ok();
    });
}

/// Write one control command and read exactly one reply. Never re-sends: a read that times out keeps polling until the deadline, because the server may already be acting on the command.
fn round_trip(session: Option<&str>, command: ControlCommand) -> Option<ServerMsg> {
    let Some(dir) = nonempty_env(MUX_DIR_ENV) else {
        warn_mux_dir_unset();
        return None;
    };
    let sock = socket_in(&dir, &resolve_session(session))?;
    let stream = UnixStream::connect(sock).ok()?;
    let mut writer = stream.try_clone().ok()?;
    write_msg_sync(
        &mut writer,
        &ClientMsg::Control {
            proto: PROTO_VERSION,
            build: env!("CARGO_PKG_VERSION").to_string(),
            command,
        },
    )
    .ok()?;
    let mut reader = stream;
    reader.set_read_timeout(Some(READ_TIMEOUT)).ok()?;
    let deadline = Instant::now() + REPLY_DEADLINE;
    loop {
        match read_msg_sync::<_, ServerMsg>(&mut reader) {
            Ok(msg) => return Some(msg),
            Err(ProtoError::Io(e)) if is_retryable(&e) && e.kind() != ErrorKind::UnexpectedEof => {
                if Instant::now() >= deadline {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
}

/// Write `bytes` to a pane's PTY, the same unguarded raw write `fno mux pane send --raw` makes. True when the server acknowledged it.
pub(crate) fn pane_send(session: Option<&str>, pane: u64, bytes: &[u8]) -> bool {
    matches!(
        round_trip(
            session,
            ControlCommand::PaneSend {
                pane,
                bytes: bytes.to_vec(),
                guarded: false,
                expected_identity: None,
                hold_pass: false,
            },
        ),
        Some(ServerMsg::Ok)
    )
}

/// A pane's visible grid text, the same read `fno mux pane read` makes. `None` when the dir is unset, the server is down, or the pane is gone.
pub(crate) fn pane_read(session: Option<&str>, pane: u64) -> Option<String> {
    match round_trip(
        session,
        ControlCommand::PaneRead {
            pane,
            lines: None,
            block: None,
        },
    )? {
        ServerMsg::PaneText { text, .. } => Some(text),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_refuses_names_that_escape_the_dir() {
        assert_eq!(socket_in("/m", "main"), Some(PathBuf::from("/m/main.sock")));
        assert_eq!(socket_in("/m", "../x"), None);
        assert_eq!(socket_in("/m", ""), None);
    }
}
