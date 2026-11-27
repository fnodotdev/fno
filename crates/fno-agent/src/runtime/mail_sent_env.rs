//! fno-agent's side of the `mail sent` view: the environment the shared
//! `mail_sent_core` reads (handle, config ttls, the numbers-store hash, the
//! chats-mirrored landed recorder) and the turn-boundary nag built on the
//! same unclaimed predicate. The front door builds its own environment.

use crate::claude_drive::claude_projects_dir;
use crate::mail_sent_core::{
    age_minutes, dedupe_last_wins, landed_envelope, now_secs, read_bus, recipient_transcript_path,
    run_sent as run_core, sent_unclaimed, str_field, SentEnv, DEFAULT_LANDED_ABANDON_TTL,
    DEFAULT_UNCLAIMED_TTL,
};
use crate::paths::AgentsHome;
use crate::spawn_context::resolve_self_identity;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// The default beat window (`FNO_MAIL_MIDLOOP_WINDOW_S`) a recipient
/// transcript must have been written inside for the nag to claim mid-loop.
const DEFAULT_MIDLOOP_WINDOW_SECS: i64 = 1800;
const CODEX_SESSIONS_DIR_ENV: &str = "FNO_CODEX_SESSIONS_DIR";

pub fn run_sent(args: &[String]) -> i32 {
    let home = AgentsHome::from_env();
    let bus = crate::mail_paths::state_bus_dir(&home);
    let env = sent_env(&bus);
    // `stamp_from`, not the precedence-only resolver: this must be the SAME
    // handle the send path stamped into `from`. Unlike hold, an unprovable
    // self is NOT a hard stop: the handle floors to "fno" and lists.
    let handle = || {
        let ident = resolve_self_identity(&|name| std::env::var(name).ok(), None, None, &home);
        match (ident.session_id, ident.harness) {
            (Some(session_id), Some(_)) => crate::identity::canonical_handle(&session_id),
            _ => "fno".to_string(),
        }
    };
    run_core(args, &env, &handle, now_secs())
}

fn sent_env(bus: &Path) -> SentEnv<'_> {
    SentEnv {
        bus,
        claude_projects: claude_projects_dir(),
        codex_sessions: codex_sessions_dir(),
        unclaimed_ttl: inbox_ttl(&["inbox", "unclaimed_ttl"], DEFAULT_UNCLAIMED_TTL),
        abandon_ttl: inbox_ttl(&["inbox", "landed_abandon_ttl"], DEFAULT_LANDED_ABANDON_TTL),
        hash: &crate::numbers::mail_hash,
        record_landed: &record_landed,
    }
}

/// `config.inbox.<key>` from the resolved settings, or the Python default.
fn inbox_ttl(keys: &[&str], default: i64) -> i64 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::agents_config::config_lookup(&cwd, keys)
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
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".codex").join("sessions")
}

/// Persist a landing so later scans skip the transcript read. Uses the
/// appender without the sender guard: the Python recorder stamps the bare
/// fleet name and the Rust sender gate would refuse it.
fn record_landed(bus: &Path, m: &Value) {
    let env = landed_envelope(
        &crate::announce::new_msg_id(),
        &crate::graph_store::now_isoformat(),
        m,
    );
    let live = bus.join("messages.jsonl");
    if let Err(e) = crate::announce::append_line_open(&live, &env, &crate::chats::chats_dir()) {
        eprintln!("landed record failed: {e}");
    }
}

/// The beat window the mid-loop claim must ride: a recipient transcript
/// written inside it reads mid-loop; outside it the receipt says idle or
/// gone. The env override exists so tests read now (`post_send_settle_seconds`
/// idiom).
fn midloop_window_secs() -> i64 {
    std::env::var("FNO_MAIL_MIDLOOP_WINDOW_S")
        .ok()
        .and_then(|raw| raw.parse::<i64>().ok())
        .map(|v| v.max(0))
        .unwrap_or(DEFAULT_MIDLOOP_WINDOW_SECS)
}

/// The recipient state a receipt may claim, liveness read from the
/// transcript mtime: a write inside the beat window is a session mid-loop,
/// a quiet one is idle or gone, and an unreadable transcript is neither.
#[derive(Debug, PartialEq)]
enum RecipientLiveness {
    MidLoop,
    IdleQuiet(i64),
    Unknown,
}

fn recipient_liveness(env: &SentEnv, harness: &str, session: &str, now: i64) -> RecipientLiveness {
    let Some(path) = recipient_transcript_path(env, harness, session) else {
        return RecipientLiveness::Unknown;
    };
    let written = std::fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    match written {
        Some(w) if now.saturating_sub(w) < midloop_window_secs() => RecipientLiveness::MidLoop,
        Some(w) => RecipientLiveness::IdleQuiet(now.saturating_sub(w) / 60),
        None => RecipientLiveness::Unknown,
    }
}

/// The turn-boundary nag: one line naming the mail `handle` sent that no
/// recipient has picked up, or None when the outbox is empty or everything
/// landed. The scan is `sent_unclaimed` at the `inbox.unclaimed_ttl`,
/// exactly the predicate `--unclaimed` prints, so the line counts what the
/// outbox counts. Cheap when nothing is unclaimed: an empty outbox returns
/// before any transcript read, and candidate reads carry the landed budget.
/// The mid-loop claim rides the transcript mtime per recipient: only a
/// write inside the beat window earns it, a quiet transcript reads idle or
/// gone with the wake command, and an unprovable recipient claims neither.
pub(crate) fn nag_line(bus: &Path, handle: &str) -> Option<String> {
    let env = sent_env(bus);
    let now = now_secs();
    let all = dedupe_last_wins(read_bus(bus));
    let unclaimed = sent_unclaimed(&env, &all, handle, env.unclaimed_ttl, now);
    if unclaimed.is_empty() {
        return None;
    }
    let mut seen: Vec<String> = Vec::new();
    let mut lanes: HashMap<String, (String, String)> = HashMap::new();
    for m in &unclaimed {
        let to = str_field(m, "to", "?");
        if !seen.contains(&to) {
            seen.push(to.clone());
        }
        lanes.entry(to).or_insert_with(|| {
            (
                m.pointer("/meta/to_harness")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                m.pointer("/meta/to_session")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            )
        });
    }
    let extra = seen.len().saturating_sub(3);
    let shown: Vec<String> = seen.iter().take(3).cloned().collect();
    let defanged: Vec<String> = shown
        .iter()
        .map(|n| crate::mail_control_drain::defang(n))
        .collect();
    let mut who = defanged.join(", ");
    if extra > 0 {
        who.push_str(&format!(", +{extra} more"));
    }
    let n = unclaimed.len();
    let noun = if n == 1 { "message" } else { "messages" };
    let age_min = unclaimed
        .first()
        .map_or(0, |m| age_minutes(&str_field(m, "ts", ""), now));
    let first = defanged.first().map(String::as_str).unwrap_or("?");
    // An unshown recipient carries no read, so it never earns a liveness claim.
    let states: Vec<RecipientLiveness> = if extra > 0 {
        vec![RecipientLiveness::Unknown]
    } else {
        shown
            .iter()
            .map(|name| match lanes.get(name) {
                Some((h, s)) if !h.is_empty() && !s.is_empty() => {
                    recipient_liveness(&env, h, s, now)
                }
                _ => RecipientLiveness::Unknown,
            })
            .collect()
    };
    let tail = if states.iter().all(|s| s == &RecipientLiveness::MidLoop) {
        "they are mid-loop, ESC to steer".to_string()
    } else if states
        .iter()
        .all(|s| matches!(s, RecipientLiveness::IdleQuiet(_)))
    {
        let quiet = states
            .iter()
            .filter_map(|s| match s {
                RecipientLiveness::IdleQuiet(q) => Some(*q),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        format!("their transcript has been quiet {quiet}m - idle or gone - fno agent resume {first} to wake")
    } else {
        format!("recipient state unclear - fno agent peek {first} to check, fno agent resume {first} to wake")
    };
    Some(format!(
        "{n} {noun} to {who} handed {age_min}m ago, not in transcript - {tail}"
    ))
}
