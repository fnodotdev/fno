//! `fno agent mail sent`, answered in process. The view is `mail_sent_core`,
//! the copy of the module fno-agent's own leaf and nag share, so the two
//! cannot disagree. This file supplies what the front door resolves for it:
//! the handle, the config ttls, the numbers-store hash lookup and a landed
//! recorder that appends the bus row. The chats delivery mirror needs the
//! chats store only fno-agent owns, so a landing proven here lands as a bus
//! row alone.

use std::path::{Path, PathBuf};

use crate::config_defaults::{global_config_path, lookup_value, project_config_path};
use crate::config_doctor::{agents_registry_path, bus_dir, graph_json, Settings};
use crate::mail_sent_core::{
    canonical_handle, landed_envelope, now_secs, run_sent, SentEnv, DEFAULT_LANDED_ABANDON_TTL,
    DEFAULT_UNCLAIMED_TTL,
};
use crate::self_identity::resolve_self_identity;
use serde_json::Value;

const CODEX_SESSIONS_DIR_ENV: &str = "FNO_CODEX_SESSIONS_DIR";

/// Run the leaf and return its exit code.
pub fn run(args: &[String]) -> i32 {
    let settings = Settings::load();
    let bus = bus_dir(&settings);
    let database = graph_json(&settings).with_extension("db");
    let hash = move |token: &str| mail_hash(&database, token);
    let env = SentEnv {
        bus: &bus,
        claude_projects: crate::transcript_tail::claude_projects_dir(),
        codex_sessions: codex_sessions_dir(),
        unclaimed_ttl: inbox_ttl("inbox.unclaimed_ttl", DEFAULT_UNCLAIMED_TTL),
        abandon_ttl: inbox_ttl("inbox.landed_abandon_ttl", DEFAULT_LANDED_ABANDON_TTL),
        hash: &hash,
        record_landed: &record_landed,
    };
    let registry = agents_registry_path(&settings);
    let handle = || {
        let ident = resolve_self_identity(&|name| std::env::var(name).ok(), None, None, &registry);
        match (ident.session_id, ident.harness) {
            (Some(session_id), Some(_)) => canonical_handle(&session_id),
            _ => "fno".to_string(),
        }
    };
    run_sent(args, &env, &handle, now_secs())
}

fn inbox_ttl(key: &str, default: i64) -> i64 {
    lookup_value(
        Some(&global_config_path()),
        Some(&project_config_path()),
        key,
    )
    .and_then(|v| v.as_integer())
    .unwrap_or(default)
}

fn codex_sessions_dir() -> PathBuf {
    if let Some(v) = std::env::var_os(CODEX_SESSIONS_DIR_ENV) {
        return PathBuf::from(v);
    }
    if let Some(v) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(v).join("sessions");
    }
    home().join(".codex").join("sessions")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The hash id a delivered header token names: `fmail-87` reads the numbers
/// table in the graph store, anything else passes through as typed. The
/// store is read on this machine only; fno-agent also pulls from a
/// configured primary, which a token no local row holds would need.
fn mail_hash(database: &Path, token: &str) -> String {
    let token = token.trim();
    if !fno_mail::address::is_mail_number(token) {
        return token.to_string();
    }
    let Some(number) = token
        .strip_prefix("fmail-")
        .and_then(|n| n.parse::<i64>().ok())
    else {
        return token.to_string();
    };
    crate::store_conn::with_read(database, |conn| {
        conn.query_row(
            "SELECT key FROM fno_numbers WHERE kind = 'm' AND num = ?1 \
             ORDER BY store = 'local' DESC, created_at DESC LIMIT 1",
            [number],
            |row| row.get::<_, String>(0),
        )
        .ok()
    })
    .ok()
    .flatten()
    .unwrap_or_else(|| token.to_string())
}

/// Persist a landing as a bus row so later scans skip the transcript read.
fn record_landed(bus: &Path, m: &Value) {
    let id = landed_id();
    let env = landed_envelope(&id, &now_isoformat(), m);
    if let Err(e) = fno_mail::log::append_raw(&bus.join("messages.jsonl"), &env) {
        eprintln!("landed record failed: {e}");
    }
}

/// `fmail-` plus 12 hex, from the pid, the clock and a counter: one
/// collision costs one duplicate id.
fn landed_id() -> String {
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(nanos.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(SEQ.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    let digest = hasher.finalize();
    let hex: String = digest[..6].iter().map(|b| format!("{b:02x}")).collect();
    format!("fmail-{hex}")
}

/// Python `datetime.now(timezone.utc).isoformat()`: seconds, plus
/// microseconds only when nonzero, `+00:00` suffix.
fn now_isoformat() -> String {
    let now = chrono::Utc::now();
    if now.timestamp_subsec_micros() == 0 {
        now.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
    } else {
        now.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
    }
}
