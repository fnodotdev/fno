//! Control-mail landing at the tool boundary: the `mail-inject
//! --control-drain` mode.
//!
//! The recipient-side half of the control lane. A control body that demoted
//! durable waits on `notify-self`, which fires only at a prompt boundary; a
//! worker holding one long turn never reaches one. The PreToolUse hook runs
//! this command at every tool call instead, gated on the sender-stamped pending
//! flags so a clean tool call pays three stat calls.
//!
//! The scan ports the Python bus read semantics it replaces
//! (`fno.bus.log.is_deliverable`, `withdrawn_ids`, the `scan_unread` cursor
//! position, the transcript-presence dedup, and the defang): a control row
//! (withdraw tombstone, landed receipt) is never inbox content; a hosted or
//! typed row is bytes already delivered and drains as a skip; a tombstone
//! retracts its target only when both came from the same sender to the same
//! address; a transcript read failure prints everything rather than risking
//! a drop. Ordinary mail is untouched: the lane positions at its own
//! `control:<form>` cursors and advances nothing the prompt boundary reads.

use serde_json::{json, Value};
use std::io::{BufRead as _, Read as _};
use std::path::{Path, PathBuf};

pub(crate) use crate::bus_read::{
    deliverable, landed_ids, read_cursor_file, withdrawn_ids, WITHDRAW_KIND,
};

pub struct DrainArgs {
    pub bus_dir: PathBuf,
    pub session: String,
}

pub fn parse_args(rest: &[String]) -> Result<DrainArgs, String> {
    let mut bus_dir: Option<PathBuf> = None;
    let mut session: Option<String> = None;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bus-dir" => {
                bus_dir = Some(PathBuf::from(
                    it.next().ok_or("--bus-dir needs a path")?.as_str(),
                ))
            }
            "--session" => {
                session = Some(it.next().ok_or("--session needs an id")?.clone());
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(DrainArgs {
        bus_dir: bus_dir.ok_or("--bus-dir is required")?,
        session: session.ok_or("--session is required")?,
    })
}

/// The mailbox form one session answers to: its full id. A short head two
/// sessions can share never drains here.
fn address_forms(session: &str) -> Vec<String> {
    vec![crate::spawn_context::session_identity_key(session)]
}

fn flag_path(bus_dir: &Path, form: &str) -> PathBuf {
    bus_dir.join("control-pending").join(format!("{form}.flag"))
}

fn cursor_path(bus_dir: &Path, form: &str) -> PathBuf {
    bus_dir.join("cursors").join(format!("control:{form}.json"))
}

fn read_cursor(bus_dir: &Path, form: &str) -> Option<String> {
    let text = std::fs::read_to_string(cursor_path(bus_dir, form)).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("last_seen_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Unread, deliverable mail addressed to `name`, oldest first, positioned
/// after `name`'s main cursor (`bus.cursor.scan_unread`; the cursor filename
/// IS the address). An absent, corrupt or rotated-out cursor rescans the
/// whole retained log.
pub(crate) fn unread_messages<'a>(bus_dir: &Path, msgs: &'a [Value], name: &str) -> Vec<&'a Value> {
    unread_messages_excluding(bus_dir, msgs, name, &[])
}

/// The same scan with `scan_unread`'s sender exclusion: a `to_kind=project`
/// broadcast read drops rows whose `from` name or `from_session` matches, so
/// a project member never drains its own broadcast back.
pub(crate) fn unread_messages_excluding<'a>(
    bus_dir: &Path,
    msgs: &'a [Value],
    name: &str,
    exclude_from: &[String],
) -> Vec<&'a Value> {
    let cursor = std::fs::read_to_string(bus_dir.join("cursors").join(format!("{name}.json")))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|v| {
            v.get("last_seen_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    // Hosted-send parity with `bus.cursor.scan_unread`: a hosted receipt
    // supersedes the staged twin sharing its id (the receipt proves the
    // inject confirmed, so the transcript already carries the body), and
    // rows dedupe one per id, first in log order wins, before the cursor
    // walk. A typed receipt never supersedes: pane bytes are not proven
    // consumed, so the staged row stays the recovery path.
    let superseded: std::collections::HashSet<&str> = msgs
        .iter()
        .filter(|m| m.get("delivery").and_then(Value::as_str) == Some("hosted"))
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .collect();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let deduped: Vec<&Value> = msgs
        .iter()
        .filter(|m| {
            let id = m.get("id").and_then(Value::as_str).unwrap_or("");
            seen.insert(id)
        })
        .collect();
    let after = cursor.and_then(|id| {
        deduped
            .iter()
            .position(|m| m.get("id").and_then(Value::as_str) == Some(id.as_str()))
    });
    let withdrawn = withdrawn_ids(msgs);
    deduped
        .iter()
        .enumerate()
        .filter(|(i, m)| {
            after.is_none_or(|pos| *i > pos)
                && m.get("to").and_then(Value::as_str) == Some(name)
                && deliverable(m)
                && m.get("id").and_then(Value::as_str).is_some_and(|id| {
                    !id.is_empty() && !withdrawn.contains(id) && !superseded.contains(id)
                })
                && !excluded_sender(m, exclude_from)
        })
        .map(|(_, m)| *m)
        .collect()
}

/// The `scan_unread` exclusion: a row is dropped when its `from` name or its
/// `from_session` matches the set (a direct address is never a self-echo, so
/// by-name reads pass an empty set).
fn excluded_sender(m: &Value, exclude_from: &[String]) -> bool {
    if exclude_from.is_empty() {
        return false;
    }
    let from = m.get("from").and_then(Value::as_str);
    let session = m.get("from_session").and_then(Value::as_str);
    exclude_from
        .iter()
        .any(|x| Some(x.as_str()) == from || Some(x.as_str()) == session)
}

pub(crate) fn unread_count(bus_dir: &Path, msgs: &[Value], name: &str) -> usize {
    unread_messages(bus_dir, msgs, name).len()
}

fn write_cursor(bus_dir: &Path, form: &str, msg_id: &str) {
    let path = cursor_path(bus_dir, form);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let payload = json!({"last_seen_id": msg_id, "ts": crate::graph_store::now_isoformat()});
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, format!("{payload}\n")).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

pub(crate) fn is_control(body: &str) -> bool {
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .is_some_and(|first| first.to_lowercase().starts_with("control:"))
}

/// Every delivered-mail id in the session's own transcript - the middle
/// token of a header line, or the `id` of an old `<fno_mail id="...">` tag -
/// or None when the transcript cannot be resolved or read (print-everything
/// posture).
pub(crate) fn present_mail_ids(
    projects_base: &Path,
    session: &str,
) -> Option<std::collections::HashSet<String>> {
    let transcript = crate::claude_drive::find_transcript_in(projects_base, session)?;
    let file = std::fs::File::open(transcript).ok()?;
    let mut out = std::collections::HashSet::new();
    // One JSONL record per physical line; the reader streams line by line so
    // a long-lived session's multi-hundred-MB transcript never materializes
    // whole. The prompt-boundary leg runs under a 4s budget, and a full
    // read plus its escaped-quote copy blew it on the largest transcript:
    // every line without a marker prefix is skipped after one byte scan.
    let mut reader = std::io::BufReader::new(file);
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        let read = reader.read_until(b'\n', &mut bytes).ok()?;
        if read == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&bytes);
        let tag_line = line.contains("<fno_mail");
        // Three delivered shapes: the rendered `--- from ... id: ... ---`
        // header, the legacy open tag, and the typed law header
        // `` `@sender · id` `` a live lane lays down with the body.
        if !tag_line && !line.contains("--- from ") && !line.contains(" · ") {
            continue;
        }
        // A JSONL record holds delivered text inside one JSON string, its
        // newlines escaped, so a header never starts a physical line: split
        // on string quotes and escaped newlines to read each logical line.
        for seg in line.split(['\n', '"']).flat_map(|s| s.split("\\n")) {
            if let Some(id) = crate::mail_header::delivered_msg_id(seg).or_else(|| rendered_id(seg))
            {
                out.insert(id);
            }
        }
        if !tag_line {
            continue;
        }
        let unescaped = line.replace("\\\"", "\"");
        let mut rest = unescaped.as_str();
        while let Some(pos) = rest.find("<fno_mail") {
            rest = &rest[pos..];
            let end = rest.find('>').map(|e| e.min(400)).unwrap_or(400);
            let tag = &rest[..end.min(rest.len())];
            if let Some(id_pos) = tag.find("id=\"") {
                let id = &tag[id_pos + 4..];
                if let Some(close) = id.find('"') {
                    out.insert(id[..close].to_string());
                }
            }
            rest = &rest[end.min(rest.len())..];
        }
    }
    Some(out)
}

/// The id of a `--- from <sender> (<ts>)  id:<id> ---` line, the header
/// both drains render above each body.
fn rendered_id(seg: &str) -> Option<String> {
    let inner = seg.trim().strip_prefix("--- from ")?.strip_suffix(" ---")?;
    let (_, id) = inner.rsplit_once("  id:")?;
    (!id.is_empty() && !id.contains(char::is_whitespace)).then(|| id.to_string())
}

/// The defang `mail.landed._defang_reminder` applies: a body cannot close
/// the reminder wrapper early. Ports `<\s*(/?)\s*system-reminder\s*>`
/// (case-folded) to a `[`-bracketed literal.
pub(crate) fn defang(s: &str) -> String {
    const TAG: &str = "system-reminder";
    let lower = s.to_lowercase();
    let skip_ws = |j: usize| -> usize {
        let mut k = j;
        for (off, c) in lower[k..].char_indices() {
            if c.is_whitespace() {
                k += off + c.len_utf8();
            } else {
                break;
            }
        }
        k
    };
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(lt) = lower[i..].find('<') {
        let start = i + lt;
        let mut j = skip_ws(start + 1);
        let mut slash = false;
        if lower[j..].starts_with('/') {
            slash = true;
            j = skip_ws(j + 1);
        }
        if lower[j..].starts_with(TAG) {
            let k = skip_ws(j + TAG.len());
            if lower[k..].starts_with('>') {
                out.push_str(&s[i..start]);
                out.push('[');
                if slash {
                    out.push('/');
                }
                out.push_str(TAG);
                out.push(']');
                i = k + 1;
                continue;
            }
        }
        // No match: emit through the '<' and keep scanning after it.
        out.push_str(&s[i..=start]);
        i = start + 1;
    }
    out.push_str(&s[i..]);
    out
}

pub fn run(args: &[String]) -> i32 {
    let parsed = match parse_args(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("mail-inject --control-drain: {e}");
            return 2;
        }
    };
    let ordinary = std::env::current_dir().is_ok_and(|cwd| {
        crate::agents_config::config_value_deep(&cwd, &["inbox", "tool_boundary_ordinary"])
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    });
    tool_boundary(
        &parsed.bus_dir,
        &parsed.session,
        ordinary,
        &crate::claude_drive::claude_projects_dir(),
        &mut std::io::stdout(),
    );
    0
}

/// The tool-boundary pass as `hook gate` runs it in process: the stat gate
/// the shell hook ran (a pending control flag, or a bus log newer than this
/// session's stamp), then one pass. Answers the PreToolUse payload, if any.
pub(crate) fn tool_boundary_hook(session: &str, cwd: &Path) -> Option<String> {
    let valid = session.len() >= 16 && session.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    if !valid {
        return None;
    }
    let Some(bus_dir) = crate::mail_paths::state_bus_dir_opt() else {
        // A door miss is a no-answer, never a guessed path: one row says so.
        let events = crate::state_path::resolve("events", cwd)
            .unwrap_or_else(|| crate::paths::events_path(cwd));
        let row = json!({
            "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            "type": "mail_notify_self_missed",
            "data": {"rc": 1, "stderr_tail": "bus dir unresolved"},
            "source": "hook"
        });
        if let Err(e) =
            crate::claims::append_event_line(&events, &row, std::time::Duration::from_secs(2))
        {
            eprintln!("tool-boundary drain: miss row not written: {e}");
        }
        return None;
    };
    if !tool_boundary_due(&bus_dir, session) {
        return None;
    }
    let ordinary =
        crate::agents_config::config_value_deep(cwd, &["inbox", "tool_boundary_ordinary"])
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    let mut out = Vec::new();
    tool_boundary(
        &bus_dir,
        session,
        ordinary,
        &crate::claude_drive::claude_projects_dir(),
        &mut out,
    );
    let text = String::from_utf8_lossy(&out).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// The cheap gate in front of a pass: a pending control flag for any
/// address form, or a bus log newer than this session's stamp.
fn tool_boundary_due(bus_dir: &Path, session: &str) -> bool {
    let flagged = address_forms(session)
        .iter()
        .any(|f| flag_path(bus_dir, f).exists());
    let modified = |p: PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let bus_newer = match (
        modified(bus_dir.join("messages.jsonl")),
        modified(stamp_path(bus_dir, session)),
    ) {
        (Some(bus), Some(stamp)) => bus > stamp,
        (Some(_), None) => true,
        _ => false,
    };
    flagged || bus_newer
}

/// How far ahead the stamp is set when the ordinary lane did not read the
/// bus (key off, or a live hold): the hook's `-nt` gate then skips the command
/// until this passes, so an off session pays one run per window, not one
/// per bus append, and still sees a later key change.
const RECHECK: std::time::Duration = std::time::Duration::from_secs(300);

/// The per-session file the hook's `[[ messages.jsonl -nt stamp ]]` gate
/// reads.
pub(crate) fn stamp_path(bus_dir: &Path, session: &str) -> PathBuf {
    bus_dir
        .join("tool-boundary")
        .join(format!("{session}.stamp"))
}

fn set_stamp(path: &Path, at: std::time::SystemTime) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(f) = std::fs::File::create(path) {
        let _ = f.set_modified(at);
    }
}

/// One tool-boundary pass: control rows behind their pending flags, then,
/// with `inbox.tool_boundary_ordinary` on and no live hold, unread ordinary
/// mail through the prompt-boundary renderer. Writes at most one PreToolUse
/// payload; ordinary rows are acknowledged only after it is flushed.
pub(crate) fn tool_boundary(
    bus_dir: &Path,
    session: &str,
    ordinary: bool,
    projects_base: &Path,
    out: &mut impl std::io::Write,
) {
    use crate::mail_notify_self as notify;
    // The mtime is read BEFORE the bus: a row appended during the read
    // leaves the bus newer than the stamp, so the next call drains it.
    let seen = std::fs::metadata(bus_dir.join("messages.jsonl"))
        .and_then(|m| m.modified())
        .ok();
    let control = drain(bus_dir, session);
    let live = ordinary && !notify::held(session);
    let msgs = if live {
        notify::read_drain_bus(
            bus_dir,
            &crate::spawn_context::session_identity_key(session),
        )
    } else {
        Vec::new()
    };
    // A control row this pass already landed is not rendered twice.
    let landed: Vec<String> = control.iter().flat_map(|c| c.ids.clone()).collect();
    let pending = live.then(|| notify::pending(bus_dir, &msgs, session, projects_base, &landed));
    let mut parts: Vec<String> = control.into_iter().map(|c| c.context).collect();
    let mut system = None;
    if let Some(p) = pending.as_ref().filter(|p| !p.lines.is_empty()) {
        parts.push(notify::context(&p.lines));
        system = Some(p.summary());
    }
    if !parts.is_empty() {
        let payload = notify::envelope("PreToolUse", &parts.join("\n"), system);
        if notify::write_flushed(out, &payload).is_err() {
            return;
        }
    }
    if let Some(p) = &pending {
        notify::acknowledge(bus_dir, &msgs, p);
    }
    let until = if live {
        seen
    } else {
        Some(std::time::SystemTime::now() + RECHECK)
    };
    if let Some(at) = until {
        set_stamp(&stamp_path(bus_dir, session), at);
    }
}

/// What one control pass landed: the `<system-reminder>` text and its ids.
#[derive(Debug, PartialEq)]
pub struct Landed {
    pub context: String,
    pub ids: Vec<String>,
}

/// One control pass over the bus for one session. `None` is a clean no-op.
pub fn drain(bus_dir: &Path, session: &str) -> Option<Landed> {
    let forms = address_forms(session);
    // Cheap gate: no pending flag, nothing to land; today's prompt-boundary
    // delivery still owns the body.
    if !forms.iter().any(|f| flag_path(bus_dir, f).exists()) {
        return None;
    }
    // One read of the whole bus log; the filters mirror scan_unread.
    let msgs = crate::mail_notify_self::read_bus(bus_dir);
    let retracted = withdrawn_ids(&msgs);
    let present = present_mail_ids(&crate::claude_drive::claude_projects_dir(), session);

    let mut rendered: Vec<String> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    for form in &forms {
        let cursor = read_cursor(bus_dir, form);
        let mut controls: Vec<&Value> = msgs
            .iter()
            .filter(|m| {
                if m.get("to").and_then(Value::as_str) != Some(form.as_str()) {
                    return false;
                }
                if !deliverable(m) {
                    return false;
                }
                let id = m.get("id").and_then(Value::as_str).unwrap_or("");
                if id.is_empty() || retracted.contains(id) {
                    return false;
                }
                m.get("body")
                    .and_then(Value::as_str)
                    .is_some_and(is_control)
            })
            .collect();
        if let Some(last_seen) = &cursor {
            if let Some(pos) = msgs
                .iter()
                .position(|m| m.get("id").and_then(Value::as_str) == Some(last_seen.as_str()))
            {
                controls.retain(|m| {
                    let idx = msgs
                        .iter()
                        .position(|x| std::ptr::eq(x, *m))
                        .unwrap_or(usize::MAX);
                    idx > pos
                });
            }
        }
        if controls.is_empty() {
            continue;
        }
        for m in &controls {
            let id = m.get("id").and_then(Value::as_str).unwrap_or("");
            if present.as_ref().is_some_and(|p| p.contains(id)) {
                continue;
            }
            let from = m.get("from").and_then(Value::as_str).unwrap_or("");
            let ts = m.get("ts").and_then(Value::as_str).unwrap_or("");
            let body = m.get("body").and_then(Value::as_str).unwrap_or("");
            rendered.push(format!(
                "\n--- from {from} ({ts})  id:{id} ---\n{}",
                body.trim_end_matches('\n')
            ));
            ids.push(id.to_string());
        }
        let last_id = controls
            .last()
            .and_then(|m| m.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !last_id.is_empty() {
            write_cursor(bus_dir, form, &last_id);
        }
    }
    for form in &forms {
        let _ = std::fs::remove_file(flag_path(bus_dir, form));
    }
    if ids.is_empty() {
        return None;
    }
    let joined = rendered.join("\n");
    let defanged = defang(&joined);
    let context = format!(
        "<system-reminder>\n[fno agent mail] CONTROL delivery (tool boundary):{defanged}\n</system-reminder>"
    );
    Some(Landed { context, ids })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bus_fixture(tag: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let bus = dir.path().join(tag).join("bus");
        std::fs::create_dir_all(&bus).unwrap();
        (dir, bus)
    }

    fn fnv(a: &str, b: &str, c: &str) -> String {
        let mut h: u64 = 14695981039346656037;
        for byte in a.as_bytes().iter().chain(b.as_bytes()).chain(c.as_bytes()) {
            h ^= *byte as u64;
            h = h.wrapping_mul(1099511628211);
        }
        format!("{h:x}")
    }

    fn append(bus: &Path, from: &str, to: &str, body: &str) {
        let id = format!("msg-{}", fnv(from, to, body));
        let line = json!({
            "id": id,
            "thread": "t",
            "from": from,
            "to": to,
            "kind": "send",
            "body": body,
            "ts": "2026-09-28T12:00:00Z",
        });
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(bus.join("messages.jsonl"))
            .unwrap();
        writeln!(f, "{line}").unwrap();
    }
    fn flag(bus: &Path, form: &str) {
        let p = flag_path(bus, form);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "").unwrap();
    }

    fn append_raw(bus: &Path, line: Value) {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(bus.join("messages.jsonl"))
            .unwrap();
        writeln!(f, "{line}").unwrap();
    }

    fn msg_id(from: &str, to: &str, body: &str) -> String {
        format!("msg-{}", fnv(from, to, body))
    }

    #[test]
    fn drain_contract() {
        let (_d, bus) = bus_fixture("gate");
        append(&bus, "lead", "ffffabcd1234", "control: hold");
        assert_eq!(drain(&bus, "ffffabcd1234"), None);
        control_lands_contract();
        cursor_position_contract();
        tombstone_contract();
        defang_contract();
        let (_d, bus) = bus_fixture("ordinary");
        let name = "t-x-1-glm";
        let msgs = vec![
            json!({"id":"one", "from":"lead", "to":name}),
            json!({"id":"two", "from":"lead", "to":name}),
            json!({"id":"three", "from":"lead", "to":name}),
            json!({"id":"withdraw", "kind":"withdraw", "from":"lead", "to":name, "meta":{"withdraws":"three"}}),
            json!({"id":"hosted", "from":"lead", "to":name, "delivery":"hosted"}),
            json!({"id":"other", "from":"lead", "to":"other"}),
        ];
        assert_eq!(unread_count(&bus, &msgs, name), 2);
        std::fs::create_dir_all(bus.join("cursors")).unwrap();
        std::fs::write(
            bus.join("cursors").join(format!("{name}.json")),
            r#"{"last_seen_id":"one"}"#,
        )
        .unwrap();
        assert_eq!(unread_count(&bus, &msgs, name), 1);
    }

    fn control_lands_contract() {
        let (_d, bus) = bus_fixture("land");
        append(&bus, "lead", "ffffabcd1234", "ordinary status");
        let id = msg_id("lead", "ffffabcd1234", "control: freeze holds merges");
        append(&bus, "lead", "ffffabcd1234", "control: freeze holds merges");
        flag(&bus, "ffffabcd1234");
        let landed = drain(&bus, "ffffabcd1234").expect("control should land");
        assert_eq!(landed.ids, vec![id.clone()]);
        let out = landed.context;
        assert!(out.contains("CONTROL delivery (tool boundary)"));
        assert!(out.contains("control: freeze holds merges"));
        assert!(!out.contains("ordinary status"));
        assert!(!flag_path(&bus, "ffffabcd1234").exists());
        assert_eq!(
            read_cursor(&bus, "ffffabcd1234").as_deref(),
            Some(id.as_str())
        );
    }

    fn cursor_position_contract() {
        let (_d, bus) = bus_fixture("cursor");
        let first = msg_id("lead", "ffffabcd1234", "control: one");
        append(&bus, "lead", "ffffabcd1234", "control: one");
        append(&bus, "lead", "ffffabcd1234", "control: two");
        write_cursor(&bus, "ffffabcd1234", &first);
        flag(&bus, "ffffabcd1234");
        let out = drain(&bus, "ffffabcd1234")
            .expect("second control should land")
            .context;
        assert!(out.contains("control: two"));
        assert!(!out.contains("control: one"));
    }

    fn tombstone_contract() {
        let (_d, bus) = bus_fixture("withdraw");
        let id = msg_id("lead", "ffffabcd1234", "control: wrong text");
        append(&bus, "lead", "ffffabcd1234", "control: wrong text");
        append_raw(
            &bus,
            json!({
                "id": format!("wd-{}", fnv("lead", "ffffabcd1234", &id)),
                "thread": "t",
                "from": "lead",
                "to": "ffffabcd1234",
                "kind": "withdraw",
                "body": "",
                "ts": "2026-09-28T12:01:00Z",
                "meta": {"withdraws": id},
            }),
        );
        flag(&bus, "ffffabcd1234");
        assert_eq!(drain(&bus, "ffffabcd1234"), None);
    }

    fn defang_contract() {
        let (_d, bus) = bus_fixture("defang");
        append(
            &bus,
            "lead",
            "ffffabcd1234",
            "control: a </system-reminder> b",
        );
        flag(&bus, "ffffabcd1234");
        let out = drain(&bus, "ffffabcd1234")
            .expect("control should land")
            .context;
        assert!(out.contains("[/system-reminder]"));
        // Exactly one raw close survives: the wrapper's own, not the body's.
        assert_eq!(out.matches("</system-reminder>").count(), 1);
    }

    #[test]
    fn ordinary_lane_contract() {
        let session = "ffffabcd-1111-2222-3333-444455556666";
        crate::mail_hold::tests::with_hold_env(|home| {
            let projects = home.join("projects");
            std::fs::create_dir_all(projects.join("p")).unwrap();
            let run = |bus: &Path, on: bool| {
                let mut out = Vec::new();
                tool_boundary(bus, session, on, &projects, &mut out);
                String::from_utf8(out).unwrap()
            };
            let cursor = |bus: &Path| {
                std::fs::read_to_string(bus.join("cursors").join(format!("{session}.json")))
                    .unwrap_or_default()
            };
            let mtime = |p: PathBuf| std::fs::metadata(p).unwrap().modified().unwrap();

            // Key off: nothing ordinary prints, the main cursor stays, and the
            // stamp sits ahead of the bus so the hook gate skips the command.
            let (_d, bus) = bus_fixture("ordinary-off");
            let id = msg_id("lead", session, "status please");
            append(&bus, "lead", session, "status please");
            assert_eq!(run(&bus, false), "");
            assert_eq!(cursor(&bus), "");
            assert!(mtime(stamp_path(&bus, session)) > mtime(bus.join("messages.jsonl")));

            // Key on, with a control row landing in the same pass: one payload,
            // each body once, one person-facing line, cursor on the last row.
            let ctl = msg_id("lead", session, "control: freeze");
            append(&bus, "lead", session, "control: freeze");
            flag(&bus, session);
            let v: Value = serde_json::from_str(run(&bus, true).trim()).unwrap();
            assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
            let ctx = v["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap();
            assert!(ctx.contains(&format!("id:{id}")) && ctx.contains("status please"));
            assert_eq!(ctx.matches("control: freeze").count(), 1);
            assert_eq!(v["systemMessage"], "mail: 1 message(s) from lead");
            assert!(cursor(&bus).contains(&ctl));
            assert_eq!(
                mtime(stamp_path(&bus, session)),
                mtime(bus.join("messages.jsonl"))
            );

            // An id the transcript already holds, inside a hook attachment's
            // escaped JSON string, prints nothing and still moves the cursor.
            let (_d, bus) = bus_fixture("ordinary-dup");
            let dup = msg_id("lead", session, "seen already");
            append(&bus, "lead", session, "seen already");
            let record = json!({"attachment": {"type": "hook_additional_context",
                "content": [format!("<system-reminder>\n--- from lead (t)  id:{dup} ---\nseen already")]}});
            std::fs::write(
                projects.join("p").join(format!("{session}.jsonl")),
                format!("{record}\n"),
            )
            .unwrap();
            assert_eq!(run(&bus, true), "");
            assert!(cursor(&bus).contains(&dup));
        });
    }

    /// The legacy `<fno_mail id="...">` receipt tags still dedup, raw or
    /// JSON-escaped; a bare id in prose is no marker and never dedups.
    #[test]
    fn legacy_tag_ids_still_dedup() {
        let session = "ffffabcd-1111-2222-3333-444455556666";
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        std::fs::create_dir_all(projects.join("p")).unwrap();
        let raw = "<fno_mail id=\"tag-raw\" from=\"lead\">body</fno_mail>";
        let escaped = "{\"content\":\"<fno_mail id=\\\"tag-escaped\\\" from=\\\"lead\\\">\"}";
        std::fs::write(
            projects.join("p").join(format!("{session}.jsonl")),
            format!("{raw}\n{escaped}\n"),
        )
        .unwrap();
        let present = present_mail_ids(&projects, session).expect("transcript readable");
        assert!(present.contains("tag-raw"), "raw tag missed");
        assert!(present.contains("tag-escaped"), "escaped tag missed");
        // A transcript that cannot be resolved claims nothing: print-everything.
        assert_eq!(
            present_mail_ids(&projects, "0000ffff-1111-2222-3333-444455556666"),
            None
        );
    }

    /// A typed law header (`` `@sender · id` ``) inside a JSON-escaped
    /// transcript record dedups its id: the live lane laid the body down,
    /// and a staged row without its receipt must not re-render.
    #[test]
    fn typed_header_ids_still_dedup() {
        let session = "ffffabcd-1111-2222-3333-444455556666";
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        std::fs::create_dir_all(projects.join("p")).unwrap();
        let record = "{\"prompt\":\"`@lead · msg-typed-header`\\nbody the lane typed\"}";
        std::fs::write(
            projects.join("p").join(format!("{session}.jsonl")),
            format!("{record}\n"),
        )
        .unwrap();
        let present = present_mail_ids(&projects, session).expect("transcript readable");
        assert!(present.contains("msg-typed-header"), "typed header missed");
    }
}
