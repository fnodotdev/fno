//! The hold front commands: `hold` (alias `dnd`) and `hold-release`.
//!
//! Both read and write the session's hold record in the fno-mail hold
//! store. The receipt and help name do-not-disturb (DND);
//! the command stays `hold` with `dnd` accepted as an alias everywhere the
//! front dispatches.

use crate::mail_header::{render_held_release, HeldMessage, HeldRelease};
use crate::mail_hold::{clock_description, hold_store, live_hold, max_hold, put_hold};
use crate::mail_notify_self::read_bus;
use crate::paths::AgentsHome;
use crate::spawn_context::{resolve_self_identity, OwnedDisposition};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Write as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use fno_mail::hold::SetBy;
use fno_mail::session::SessionId;

/// The identity refusal shared by both commands: hold writes a record for one
/// session, so an unprovable self is a hard stop, not a default.
/// The ambiguous case prints its own detail; the generic line is the
/// missing-identity message only, matching the Python leg's per-case output.
fn self_identity_or_exit() -> Option<(String, String)> {
    let home = AgentsHome::from_env();
    let ident = resolve_self_identity(&|name| std::env::var(name).ok(), None, None, &home);
    if matches!(ident.disposition, OwnedDisposition::Ambiguous) {
        eprintln!("cannot decide which session is 'self': multiple harness markers present (inherited env?)");
        eprintln!("a hold on the wrong session holds another agent's mail");
        return None;
    }
    match (ident.session_id.clone(), ident.harness.clone()) {
        (Some(session_id), Some(harness)) => Some((session_id, harness)),
        _ => {
            eprintln!("no provable harness identity - there is no session to hold mail for");
            None
        }
    }
}

/// The hold command's `mail_hold_stamped` writer value, as the event schema
/// names it.
const HOLD_WRITER: &str = "hold_verb"; // verb-wording: allow (event writer enum value)

const HOLD_USAGE: &str =
    "usage: fno agents mail admin hold (--on | --for <minutes> | --minutes <minutes> | --off | --status)
  --on         do-not-disturb until the hold ceiling (mail.hold.max_minutes)
  --for N      do-not-disturb for N wall-clock minutes
  --minutes N  do-not-disturb until N quiet minutes pass
  --off        lift do-not-disturb and deliver what it held
  --status     say whether do-not-disturb is on";

/// `hold` (alias `dnd`): write this session's hold record. A bare call
/// prints usage and writes nothing: an arm is always a choice the caller
/// names, never a default a stray call falls into. Every hold has an end:
/// `--on` asks for the ceiling, and a longer ask is capped there.
pub fn run_hold(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("{HOLD_USAGE}");
        return 2;
    }
    let mut minutes: Option<i64> = None;
    let mut for_minutes: Option<i64> = None;
    let mut on = false;
    let mut status = false;
    let mut off = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--minutes" => match it.next().and_then(|v| v.parse::<i64>().ok()) {
                Some(v) => minutes = Some(v),
                None => {
                    eprintln!("error: --minutes wants an integer");
                    return 2;
                }
            },
            "--for" => match it.next().and_then(|v| v.parse::<i64>().ok()) {
                Some(v) => for_minutes = Some(v),
                None => {
                    eprintln!("error: --for wants an integer");
                    return 2;
                }
            },
            "-h" | "--help" => {
                println!("{HOLD_USAGE}");
                return 0;
            }
            "--on" => on = true,
            "--status" => status = true,
            "--off" => off = true,
            other => {
                eprintln!("hold: unknown argument {other:?}");
                return 2;
            }
        }
    }
    if [minutes.is_some(), for_minutes.is_some(), on]
        .iter()
        .filter(|x| **x)
        .count()
        > 1
    {
        eprintln!("error: --on, --minutes and --for are mutually exclusive");
        return 2;
    }
    let Some((session_id, harness)) = self_identity_or_exit() else {
        return 3;
    };
    let handle = crate::identity::canonical_handle(&session_id);

    if status {
        println!("{}", status_line(&session_id, &handle, chrono::Utc::now()));
        return 0;
    }

    if off {
        let was_on = live_hold(&session_id, chrono::Utc::now()).is_some();
        let result = release(&session_id, 0);
        if result.get("hold_cleared").and_then(Value::as_bool) != Some(true) {
            eprintln!("hold NOT off: the hold record could not be removed, so {handle} still holds mail. Retry, or check the mail-hold store.");
            return 1;
        }
        let held = result
            .get("held_count")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if held > 0 {
            println!(
                "hold off: delivered {held} held message(s) ({} deduped) - {}",
                result
                    .get("deduped_count")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                result.get("outcome").and_then(Value::as_str).unwrap_or(""),
            );
        } else if was_on {
            println!("hold off: do-not-disturb cleared for {handle}; no mail was waiting");
        } else {
            println!("hold off: do-not-disturb was not on for {handle}; nothing to clear");
        }
        return 0;
    }

    let idle = minutes.is_some();
    let length = match for_minutes.or(minutes) {
        Some(window) if window < 1 => {
            eprintln!(
                "error: {} must be at least 1",
                if idle { "--minutes" } else { "--for" }
            );
            return 2;
        }
        // The clamp keeps a huge ask from overflowing the duration; the
        // record caps it at the ceiling anyway.
        Some(window) => chrono::Duration::minutes(window.min(7 * 24 * 60)),
        None if on => max_hold(),
        None => {
            eprintln!("{HOLD_USAGE}");
            return 2;
        }
    };
    // The gate finds a session by its registry row, so the row exists
    // before the hold does.
    if let Err(exc) = register_existing_session(&harness, &session_id) {
        eprintln!("hold: {exc}");
        return 1;
    }
    let hold = match put_hold(&session_id, length, idle, SetBy::User, HOLD_WRITER) {
        Ok(hold) => hold,
        Err(exc) => {
            eprintln!("hold: {exc}");
            return 1;
        }
    };
    crate::mail_hold::spawn_release_timer(&session_id);
    let minutes = (hold.until() - chrono::Utc::now())
        .num_seconds()
        .div_euclid(60)
        .max(0)
        + 1;
    println!(
        "do-not-disturb on for {handle}: {}, holds until {} ({minutes}m), then delivers itself.",
        clock_description(&hold),
        hold.until().format("%H:%M:%S UTC"),
    );
    0
}

/// The `--status` line. A hold is live with an end, or there is none; a
/// mail-only row says so, because it holds typed text but is not a hold.
pub(crate) fn status_line(
    session_id: &str,
    handle: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    if let Some(hold) = live_hold(session_id, now) {
        if hold.set_by() == SetBy::Conversation {
            return format!(
                "{handle}: holding mail, machine-armed while you talk ({}), lifts about 2 min after your answer",
                clock_description(&hold)
            );
        }
        let ms = (hold.until() - now).num_milliseconds();
        let label = if ms < 60_000 {
            format!("~{}s", ms / 1000)
        } else {
            format!("~{}m", (ms + 59_999) / 60_000)
        };
        return format!(
            "{handle}: holding mail, {}, lifts in {label}",
            clock_description(&hold)
        );
    }
    let mail_only = entry_for(session_id)
        .and_then(|e| e.delivery_policy)
        .as_deref()
        == Some(crate::mail_hold::MAIL_ONLY);
    if mail_only {
        format!("{handle}: no hold - this session takes mail only, never typed text")
    } else {
        format!("{handle}: no hold - mail delivers normally")
    }
}

/// `hold-release`: wait out the hold on `--handle` (a full session id),
/// then drain what it held. A record that is gone, ended, or removed by
/// `--off` all mean the wait is over.
pub fn run_hold_release(args: &[String]) -> i32 {
    let mut handle: Option<String> = None;
    let mut poll_s: f64 = 15.0;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            // A flag-shaped value is a typo'd invocation; a quiet exit
            // would hide the miss.
            "--handle" => match it.next() {
                Some(v) if !v.starts_with("--") => handle = Some(v.clone()),
                _ => {
                    eprintln!("error: --handle is required");
                    return 2;
                }
            },
            "--poll-s" => match it.next().and_then(|v| v.parse::<f64>().ok()) {
                Some(v) => poll_s = v,
                None => {
                    eprintln!("error: --poll-s wants a number");
                    return 2;
                }
            },
            other => {
                eprintln!("hold-release: unknown argument {other:?}");
                return 2;
            }
        }
    }
    let Some(handle) = handle else {
        eprintln!("error: --handle is required");
        return 2;
    };
    let Ok(session) = SessionId::parse(&handle) else {
        eprintln!("error: --handle wants a full session id, got {handle:?}");
        return 2;
    };
    let store = hold_store();
    let started = Instant::now();
    loop {
        let Some(hold) = store.get(&session) else {
            break;
        };
        // Milliseconds, not whole seconds: a 0.4 s remainder read as 0 and
        // released before the deadline.
        let remaining_ms = (hold.until() - chrono::Utc::now()).num_milliseconds();
        if remaining_ms <= 0 {
            break;
        }
        let nap = Duration::from_millis(remaining_ms as u64)
            .min(Duration::from_secs_f64(poll_s.max(1.0)));
        std::thread::sleep(nap);
    }
    let held_for_s = started.elapsed().as_secs() as i64;
    let result = release(session.as_str(), held_for_s);
    println!("{}", serde_json::to_string(&result).unwrap_or_default());
    0
}

/// Drain the held bus for `session_id` and remove its hold record. The
/// clock text is read before the record goes, and the record goes first so
/// the lanes the drain hands over to read no hold.
pub(crate) fn release(session_id: &str, held_for_s: i64) -> Value {
    let session = SessionId::parse(session_id).ok();
    let clock_text = session
        .as_ref()
        .and_then(|s| hold_store().get(s))
        .map(|h| clock_description(&h))
        .unwrap_or_else(|| "no hold".to_string());
    let hold_cleared = match &session {
        Some(s) => hold_store().clear(s).is_ok(),
        None => true,
    };

    let home = AgentsHome::from_env();
    let entry = entry_for(session_id);
    let sid = entry.as_ref().and_then(|e| e.harness_session_id.clone());
    let drain_id = sid.clone().unwrap_or_else(|| session_id.to_string());
    let forms = [crate::spawn_context::session_identity_key(&drain_id)];
    let handle = session_id;

    let bus = crate::mail_paths::state_bus_dir(&home);
    let msgs = read_bus(&bus);
    let mut merged: Vec<Value> = Vec::new();
    for form in &forms {
        for m in crate::mail_control_drain::unread_messages(&bus, &msgs, form) {
            merged.push(m.clone());
        }
    }
    let merged = dedupe_keep_order_by_id(merged);
    let held_count = merged.len();

    let mut miss_reason: Option<String> = None;
    let mut outcome = "empty".to_string();
    if held_count > 0 {
        match (&entry, &sid) {
            (Some(entry), Some(sid)) => {
                let messages: Vec<HeldMessage> = merged
                    .iter()
                    .map(|m| HeldMessage {
                        sender: str_field(m, "from", "?"),
                        sent_at: str_field(m, "ts", ""),
                        id: str_field(m, "id", "?"),
                        body: str_field(m, "body", ""),
                    })
                    .collect();
                let digest = render_held_release(&HeldRelease {
                    held_for_s,
                    harness: entry.harness.clone(),
                    messages,
                });
                match crate::mail_envelope::render_at(
                    &json!({"mode": "held-release", "body": digest}),
                    &AgentsHome::shared_registry_json(),
                ) {
                    Ok(framed) => {
                        let (delivered, reason) = deliver_live(sid, &framed);
                        if delivered {
                            outcome = "delivered".to_string();
                            for form in &forms {
                                for m in &merged {
                                    crate::mail_notify_self::advance_main_cursor(
                                        &bus,
                                        &msgs,
                                        form,
                                        &str_field(m, "id", "?"),
                                    );
                                }
                            }
                            for m in &merged {
                                crate::mail_notify_self::emit_marker(handle, m, "printed");
                            }
                        } else {
                            outcome = "inject-missed".to_string();
                            miss_reason = reason;
                        }
                    }
                    Err(exc) => {
                        outcome = "inject-missed".to_string();
                        miss_reason =
                            Some(truncate_160(format!("deliver-raised:ValueError: {exc}")));
                    }
                }
            }
            _ => {
                outcome = "inject-missed".to_string();
                miss_reason = Some("no-registry-row".to_string());
            }
        }
    }

    emit_release_marker(handle, held_count, &outcome, held_for_s);

    json!({
        "handle": handle,
        "clock": clock_text,
        "held_count": held_count,
        "deduped_count": 0,
        "held_for_s": held_for_s,
        "outcome": outcome,
        "miss_reason": miss_reason,
        "hold_cleared": hold_cleared,
    })
}

/// The release marker, in the hybrid row shape the deleted Python emitter
/// wrote: flat kind-keyed fields beside the envelope keys, committed to the
/// state-root store (`paths.state_dir() / "events.jsonl"`), never the
/// agents-home journal. A failed commit warns to stderr and swallows, as
/// Python's emitter did.
fn emit_release_marker(handle: &str, held_count: usize, outcome: &str, held_for_s: i64) {
    let data = json!({
        "handle": handle,
        "held_count": held_count,
        "outcome": outcome,
        "held_for_s": held_for_s,
    });
    let mut row = data.clone();
    row["ts"] = json!(chrono::Utc::now().to_rfc3339());
    row["kind"] = json!("mail_hold_released");
    row["type"] = json!("mail_hold_released");
    row["source"] = json!("agents");
    row["data"] = data;
    let journal = match crate::agents_config::state_dir(std::path::Path::new(".")) {
        Some(root) => root.join("events.jsonl"),
        None => {
            eprintln!("warning: no state dir; mail_hold_released not committed");
            return;
        }
    };
    if let Err(error) = crate::event_store::append_envelope(&journal, &row.to_string(), None) {
        eprintln!("warning: could not commit mail_hold_released: {error}");
    }
}

fn entry_for(session_id: &str) -> Option<crate::state::RegistryEntry> {
    let registry = crate::state::load_registry(&AgentsHome::shared_registry_json()).ok()?;
    crate::mail_hold::sole_row_for_token(&registry, session_id).map(|i| registry.entries[i].clone())
}

/// The register refusals, checked alone so the hold command can refuse before
/// it arms a clock.
fn registrable(harness: &str, session_id: &str) -> Result<(), String> {
    if std::env::var_os("FNO_AGENT_SELF").is_some_and(|v| !v.is_empty()) {
        return Err(
            "refusing to register from inside a session that has FNO_AGENT_SELF set".to_string(),
        );
    }
    if !crate::provider::KNOWN_HARNESSES.contains(&harness) {
        return Err(format!("unknown harness for registration: {harness}"));
    }
    if session_id.is_empty() {
        return Err("no session id to register".to_string());
    }
    Ok(())
}

/// Register this session's row, minting one when absent. Port of the Python
/// register_existing_session: refusal inside a self-spawn, a known-harness
/// check, idempotent refresh on (harness, harness_session_id).
/// A minted row gets `created_at` now.
pub(crate) fn register_existing_session(harness: &str, session_id: &str) -> Result<(), String> {
    registrable(harness, session_id)?;
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let path = AgentsHome::shared_registry_json();
    // Resolved before the registry lock: the census walk reads the whole
    // process table.
    let ambient_pid = crate::spawn_context::session_identity_ambient(std::process::id()).0;
    crate::state::update_registry(&path, |registry| {
        let existing = registry.entries.iter_mut().find(|e| {
            e.harness.as_deref() == Some(harness)
                && e.harness_session_id.as_deref() == Some(session_id)
        });
        if let Some(row) = existing {
            if row.cwd.is_empty() {
                row.cwd = cwd.clone();
            }
            crate::register::bind_own_pid(row, ambient_pid);
            return;
        }
        let base = crate::identity::canonical_handle(session_id);
        let mut name = base.clone();
        let mut n = 1;
        while registry.entries.iter().any(|e| e.name == name) {
            n += 1;
            name = format!("{base}-{n}");
        }
        registry.entries.push(crate::state::RegistryEntry {
            name,
            harness: Some(harness.to_string()),
            cwd,
            status: crate::AgentStatus::Idle,
            created_at: now,
            ..crate::state::RegistryEntry::new(
                Some(session_id.to_string()),
                crate::state::Lineage::unproven(
                    "mail-hold register: self-registration carries no parent edge",
                ),
            )
        });
    })
    .map_err(|e| format!("registry write failed: {e}"))?;
    Ok(())
}

/// One delivery attempt over the inject path: the current binary re-entered
/// as `mail-inject` with the framed payload on stdin. Success is exit 0; a
/// clean failure surfaces the inject outcome's own reason when it prints one.
fn deliver_live(session_id: &str, framed: &str) -> (bool, Option<String>) {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            return (
                false,
                Some(truncate_160(format!("deliver-raised:OSError: {e}"))),
            )
        }
    };
    let mut child = match Command::new(exe)
        .args(["mail-inject", "--session", session_id])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                false,
                Some(truncate_160(format!("deliver-raised:OSError: {e}"))),
            )
        }
    };
    {
        let mut stdin = child.stdin.take().expect("piped stdin");
        let _ = stdin.write_all(framed.as_bytes());
    }
    match child.wait_with_output() {
        Ok(out) if out.status.success() => (true, None),
        Ok(out) => {
            let reason = std::str::from_utf8(&out.stdout)
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .and_then(|v| {
                    v.get("outcome")
                        .and_then(|o| o.get("reason"))
                        .and_then(Value::as_str)
                        .or_else(|| v.get("reason").and_then(Value::as_str))
                        .map(str::to_string)
                });
            (false, reason)
        }
        Err(e) => (
            false,
            Some(truncate_160(format!("deliver-raised:OSError: {e}"))),
        ),
    }
}

fn str_field(m: &Value, key: &str, blank: &str) -> String {
    m.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(blank)
        .to_string()
}

fn dedupe_keep_order_by_id(messages: Vec<Value>) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for m in messages {
        if seen.insert(str_field(&m, "id", "?")) {
            out.push(m);
        }
    }
    out
}

fn truncate_160(detail: String) -> String {
    detail.chars().take(160).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC3-HP: the join door's refresh binds the live ambient pid onto a row
    /// carrying a dead pre-restart pid, through the same helper the register
    /// command uses. A synthetic census makes the test process itself the
    /// live harness ancestor, so the ambient walk answers a real, live pid.
    #[test]
    fn a_join_refresh_binds_the_live_ambient_pid() {
        let _env = crate::claims::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let agents = tmp.path().join("agents-home");
        std::fs::create_dir_all(&agents).unwrap();
        let saved_agents = std::env::var_os(crate::paths::HOME_ENV);
        let saved_stamp = std::env::var_os("FNO_SESSION_PID");
        std::env::set_var(crate::paths::HOME_ENV, &agents);
        std::env::remove_var("FNO_SESSION_PID");

        let dead = crate::row_verdict::reaped_pid();
        crate::state::update_registry(&AgentsHome::shared_registry_json(), |registry| {
            let mut row = crate::state::RegistryEntry::new(
                Some("sid-join".to_string()),
                crate::state::Lineage::unproven("hold register test"),
            );
            row.name = "joiner".to_string();
            row.harness = Some("codex".to_string());
            row.host_mode = Some("interactive".to_string());
            row.pid = Some(dead);
            registry.entries.push(row);
        })
        .unwrap();

        let me = std::process::id();
        let mut table = std::collections::BTreeMap::new();
        table.insert(
            me,
            crate::census::ProcRow {
                pid: me,
                ppid: 1,
                state: 'S',
                elapsed_s: 1,
                cpu_pct: 0.0,
                rss_kb: 0,
                command: "codex exec --json".to_string(),
            },
        );
        crate::spawn_context::set_test_ancestry_table(Some(table));

        let outcome = register_existing_session("codex", "sid-join");
        assert!(outcome.is_ok(), "{outcome:?}");

        let registry = crate::state::load_registry(&AgentsHome::shared_registry_json()).unwrap();
        let row = registry
            .entries
            .iter()
            .find(|e| e.harness_session_id.as_deref() == Some("sid-join"))
            .expect("row still present");
        assert_eq!(row.pid, Some(me));
        assert_eq!(
            row.pid_start_time,
            crate::process_probe::process_start_time(me)
        );

        crate::spawn_context::set_test_ancestry_table(None);
        match saved_stamp {
            Some(v) => std::env::set_var("FNO_SESSION_PID", v),
            None => std::env::remove_var("FNO_SESSION_PID"),
        }
        match saved_agents {
            Some(v) => std::env::set_var(crate::paths::HOME_ENV, v),
            None => std::env::remove_var(crate::paths::HOME_ENV),
        }
    }
}
