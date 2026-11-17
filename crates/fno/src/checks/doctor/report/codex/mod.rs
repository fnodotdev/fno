//! The codex collectors: app-server probe, hooks inspection and
//! context-window arithmetic. The plugin freshness verdict reads from
//! `fno-agent plugin-install codex --status` (one owner).

mod context_window;
mod hooks;

pub(crate) use context_window::*;
pub(crate) use hooks::*;

use serde_json::{json, Value};
use std::path::Path;

use super::codex_home;

/// The codex plugin verdict from `fno-agent plugin-install codex --status`,
/// the engine that converges the plugin, so doctor never reads the state a
/// second way.
pub(crate) fn codex_plugin_report() -> Value {
    plugin_verdict(super::call_binary_json(
        "plugin-install",
        &["--status", "codex"],
        120,
    ))
}

fn plugin_verdict((err, verdict): (Option<String>, Value)) -> Value {
    if err.is_none() && verdict.is_object() {
        return verdict;
    }
    json!({
        "status": "unknown",
        "issue": "state-unreadable",
        "detail": err.unwrap_or_else(|| "plugin-install codex --status: not a JSON object".into()),
        // The plugin install runs through the same binary, so it cannot be
        // the repair for a binary that did not answer.
        "remedy": "fno doctor update --rust",
    })
}

/// Whether a codex app-server daemon is listening on its control socket
/// (doctor.py:3190-3210). The socket file survives its daemon, so liveness is
/// a bounded connect probe, never the file's existence.
pub(crate) fn codex_app_server_report() -> Value {
    let socket_path = codex_home().join("app-server-control/app-server-control.sock");
    // Bounded connect probe on a worker thread: recv_timeout supplies the one
    // second the socket-level connect timeout used to, and a hung connect
    // strands at most this probe's thread until the process exits.
    let (tx, rx) = std::sync::mpsc::channel();
    let probe_path = socket_path.clone();
    std::thread::spawn(move || {
        let _ = tx.send(std::os::unix::net::UnixStream::connect(probe_path).is_ok());
    });
    let present = rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap_or(false);
    let mut row = json!({
        "present": present,
        "socket_path": socket_path.to_string_lossy(),
    });
    if present {
        if let Some(identity) =
            daemon_identity_at(&codex_home().join("app-server-daemon/daemon.pid"))
        {
            row["daemon_pid"] = json!(identity.pid);
            row["daemon_age_secs"] = json!(now_secs().saturating_sub(identity.started_epoch));
            row["daemon_started_epoch"] = json!(identity.started_epoch);
            row["credential_remedy"] = json!(
                "restart the daemon from a GUI login (`fno agent restart`, session-preserving); the boot env then carries GH_TOKEN, and threads stop losing keychain reads"
            );
        }
    }
    row
}

/// The daemon's process identity from its provider pid file, read tolerantly
/// (codex owns the shape).
#[derive(Debug, serde::Deserialize)]
struct DaemonPidFile {
    pid: Option<u64>,
    #[serde(default)]
    process_identity: Option<DaemonProcessIdentity>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DaemonProcessIdentity {
    start_seconds: Option<u64>,
}

struct DaemonIdentity {
    pid: u64,
    started_epoch: u64,
}

fn daemon_identity_at(pid_file: &Path) -> Option<DaemonIdentity> {
    let raw = std::fs::read_to_string(pid_file).ok()?;
    let parsed: DaemonPidFile = serde_json::from_str(raw.trim()).ok()?;
    let pid = parsed.pid?;
    let started = parsed
        .process_identity
        .and_then(|identity| identity.start_seconds)
        .filter(|secs| *secs > 0)?;
    Some(DaemonIdentity {
        pid,
        started_epoch: started,
    })
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The surface-report view of the hooks inspection (doctor.py:3812-3825):
/// dual layers plus any foreign hooks in `~/.codex/hooks.json`.
pub(crate) fn codex_hooks_dual_and_foreign() -> (bool, Value) {
    let codex = codex_hooks_report();
    let duplicate = codex.get("duplicate_layers") == Some(&Value::Bool(true));
    let footnote = codex
        .get("footnote_json_hooks")
        .and_then(Value::as_array)
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    let foreign = codex
        .get("foreign_json_hooks")
        .cloned()
        .unwrap_or_else(|| Value::Array(vec![]));
    (duplicate && footnote, foreign)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_engine_verdict_is_the_doctor_row() {
        let fresh = json!({"status": "fresh", "channel": "dev"});
        assert_eq!(plugin_verdict((None, fresh.clone())), fresh);
    }

    #[test]
    fn the_daemon_identity_reads_the_pid_file_codex_writes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("daemon.pid");
        std::fs::write(
            &file,
            r#"{"pid":2889,"processStartTime":"Sat Nov  7 00:17:30 2026","processIdentity":{"bootId":"67EE3BB2-4CD9-4C81-85A5-70F9B5044703","uniqueId":13389096,"startSeconds":1794039450,"startMicroseconds":749106}}"#,
        )
        .expect("write pid file");
        let identity = daemon_identity_at(&file).expect("identity reads");
        assert_eq!(identity.pid, 2889);
        assert_eq!(identity.started_epoch, 1794039450);
    }

    #[test]
    fn a_pid_file_without_start_seconds_yields_no_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("daemon.pid");
        std::fs::write(&file, r#"{"pid":7}"#).expect("write pid file");
        assert!(daemon_identity_at(&file).is_none());
    }
}
