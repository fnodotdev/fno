//! Holds on another session's live mail, and the delivery gate that reads
//! them. The harness hook entries (`hook prompt`, `hook stop`), `lead
//! cancel`, and the sideline menu's Release hold entry reach it through the
//! binary path like the other early dispatches.
//!
//! A hold is one record per session in the fno-mail hold store
//! ([`fno_mail::hold`]), always with an end. The conversation rules (C2-C4,
//! C6) arm and rewrite their own record: a real user message whose Enter the
//! mux witnessed arms it, the session's Stop fire shortens it to the grace,
//! and the release timer drains what it held. A hold the user set is never
//! touched by them.
//!
//! The registry row's `delivery_policy = "bus-only"` is a different fact: a
//! lane capability the register command sets for a session that takes mail
//! only, never typed text. It never ends and is not a hold.
//!
//! The delivery gate (C15, C16) lives here too: it answers whether mail to a
//! session delivers live now. Own sends and `control:` mail pass a hold; the
//! receipt names when it ends.

use crate::pid::Pid;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use fno_mail::hold::{Hold, HoldClock, HoldStore, SetBy};
use fno_mail::session::SessionId;

use crate::paths::AgentsHome;
use crate::state::{load_registry, update_registry};

fn j_field<'a>(v: &'a serde_json::Value, k: &str) -> &'a serde_json::Value {
    v.get(k).unwrap_or(&serde_json::Value::Null)
}

/// The turn-end grace (C3), in seconds: a Stop fire shortens a live
/// conversation clock to `now + GRACE_S`, so held mail flows about two
/// minutes after the answer ends. The user may change this value; the
/// change is this one line.
pub(crate) const GRACE_S: i64 = 120;

/// The answering-phase backstop default, in minutes: a conversation hold
/// armed on the user's last real message ends this long after it,
/// whichever comes first with the Stop grace (C3). Overridable through
/// `mail.hold.answer_minutes` in the global config, or
/// `$FNO_AGENTS_ANSWER_MINUTES`. A value below 1 minute coerces to this
/// default; one above a day clamps to a day, so a typo can never arm a
/// hold the reader cannot outlive.
pub(crate) const DEFAULT_ANSWER_MINUTES: i64 = 10;

/// The clamp ceiling for the answer minutes: one day.
const MAX_ANSWER_MINUTES: i64 = 24 * 60;

/// The answering-phase backstop (C2), in seconds: the answering clock is
/// written this far out, so a crash or an interrupt (Esc fires no Stop)
/// cannot hold mail forever. Not the C5 cap.
pub(crate) fn answer_backstop_s() -> i64 {
    answer_minutes().saturating_mul(60)
}

/// The minutes behind [`answer_backstop_s`]: `$FNO_AGENTS_ANSWER_MINUTES`
/// wins, then the global config's `mail.hold.answer_minutes`, then the
/// default. An unparseable or non-positive value falls through like an
/// absence, and the result clamps to [`MAX_ANSWER_MINUTES`].
fn answer_minutes() -> i64 {
    let env = std::env::var("FNO_AGENTS_ANSWER_MINUTES")
        .ok()
        .and_then(|raw| raw.trim().parse::<i64>().ok());
    let configured = env.or_else(|| {
        crate::agents_config::config_lookup_global(&["mail", "hold", "answer_minutes"])
            .and_then(|v| v.as_integer())
    });
    configured
        .filter(|minutes| *minutes > 0)
        .unwrap_or(DEFAULT_ANSWER_MINUTES)
        .min(MAX_ANSWER_MINUTES)
}

/// The full normalized session id: the collision-free clock key. Mirrors
/// the deleted Python `session_identity_key`, so the two legs
/// agree on the file both write. UUID-family ids compare case-insensitively;
/// opencode's `ses_` ids do not. The retired first-eight key stays readable
/// through hold.py's `addresses()` sweep.
pub(crate) fn identity_key(session_id: &str) -> String {
    if session_id.starts_with("ses_") {
        session_id.to_string()
    } else {
        session_id.to_lowercase()
    }
}

/// The state root the sidecar clock lives under: `$FNO_HOME`, else
/// `paths.state_dir` from the global config's `[paths]` table (`~` expands),
/// else `$HOME/.fno`. The Python resolver reads `FNO_STATE_DIR` and its full
/// settings stack instead of `FNO_HOME`, so the two agree wherever the
/// global config (or the default) carries the root, and a test env must pin
/// both variables to aim both legs at one directory.
pub(crate) fn state_root() -> PathBuf {
    if let Some(home) = std::env::var_os("FNO_HOME") {
        return PathBuf::from(home);
    }
    let ambient = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".fno"))
        .unwrap_or_else(|| PathBuf::from(".fno"));
    let global = ambient.join("config.toml");
    let Ok(text) = std::fs::read_to_string(&global) else {
        return ambient;
    };
    let Ok(doc) = text.parse::<toml::Table>() else {
        return ambient;
    };
    let Some(configured) = doc
        .get("paths")
        .and_then(|p| p.get("state_dir"))
        .and_then(|v| v.as_str())
    else {
        return ambient;
    };
    let expanded = configured
        .strip_prefix("~/")
        .map(|rest| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(rest))
                .unwrap_or_else(|| PathBuf::from(configured))
        })
        .unwrap_or_else(|| PathBuf::from(configured));
    if expanded.is_absolute() {
        expanded
    } else {
        ambient
    }
}

/// The one token-to-row rule, `hold.addresses` mirrored: a token addresses a
/// row when it names its full session id (case-normalized), the row's
/// canonical first-eight handle, its daemon short_id, or its name. The
/// check-in resolves by full session id, the status command by the short
/// handle, senders by any of the four; one matcher so every reader of a hold
/// answers the same question instead of the gate resolving fewer addresses
/// than the writers stamp.
pub(crate) fn row_matches_token(entry: &crate::state::RegistryEntry, token: &str) -> bool {
    let wanted = identity_key(token);
    entry.harness_session_id.as_deref().map_or(false, |sid| {
        let key = identity_key(sid);
        key == wanted || key.get(..8) == Some(wanted.as_str())
    }) || entry.short_id == token
        || entry.name == token
}

/// The single row `token` addresses, or None when zero or TWO OR MORE rows
/// answer it. The short-form arms of [`row_matches_token`] (the first-eight
/// handle and the legacy last-eight) can name two same-window codex sessions;
/// a winner there stamps, gates or releases the wrong worker's hold. A
/// multi-match refuses and names the candidates on stderr.
pub(crate) fn sole_row_for_token(registry: &crate::state::Registry, token: &str) -> Option<usize> {
    let matches: Vec<usize> = registry
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| row_matches_token(e, token))
        .map(|(i, _)| i)
        .collect();
    if matches.len() <= 1 {
        return matches.first().copied();
    }
    let named: Vec<&str> = matches
        .iter()
        .filter_map(|&i| registry.entries[i].harness_session_id.as_deref())
        .collect();
    eprintln!(
        "refusing ambiguous session token {token:?}: {} rows answer to it ({}); send the full session id",
        matches.len(),
        named.join(", ")
    );
    None
}

/// Find the registry row `session_id` addresses, returning its index.
fn row_for_session(registry: &crate::state::Registry, session_id: &str) -> Option<usize> {
    sole_row_for_token(registry, session_id)
}

/// Stamp the row `bus-only` (or clear the stamp for `--off`) under the
/// cross-language registry lock. Returns the matched row's session id, or
/// None when no row carries it (fail-closed: no row, no clock).
fn set_policy(session_id: &str, policy: Option<&str>) -> Option<String> {
    let path = AgentsHome::shared_registry_json();
    let matched = update_registry(&path, |registry| {
        let i = row_for_session(registry, session_id)?;
        registry.entries[i].delivery_policy = policy.map(str::to_string);
        registry.entries[i].harness_session_id.clone()
    })
    .ok()?;
    matched
}

/// One `mail_hold_stamped` row naming who stamped `session_id` bus-only:
/// the writer, this process's argv, and its parent's command line. Neither
/// writer left a trace before, so a stamp nobody armed could not be traced
/// to its caller. Best effort: an emit failure never blocks the stamp.
pub(crate) fn record_stamp(writer: &str, session_id: &str) {
    let clip = |s: &str| s.chars().take(300).collect::<String>();
    let argv: Vec<String> = std::env::args().take(16).map(|a| clip(&a)).collect();
    let ppid = std::os::unix::process::parent_id();
    let parent = Command::new("ps")
        .args(["-o", "command=", "-p", &ppid.to_string()])
        .output()
        .ok()
        .map(|o| clip(String::from_utf8_lossy(&o.stdout).trim()))
        .unwrap_or_default();
    let _ = crate::events::EventEmitter::new(AgentsHome::from_env().events_jsonl(), "rust").emit(
        "mail_hold_stamped",
        &serde_json::json!({
            "session_id": session_id,
            "writer": writer,
            "pid": std::process::id(),
            "ppid": ppid,
            "argv": argv,
            "parent": parent,
        }),
    );
}

/// The matched row's (session id, delivery policy), read-only. The guard
/// needs the policy to tell a hold the user set on purpose from a
/// conversation clock.
fn lookup_row(session_id: &str) -> Option<(String, Option<String>)> {
    let registry = load_registry(&AgentsHome::shared_registry_json()).ok()?;
    let i = row_for_session(&registry, session_id)?;
    let entry = &registry.entries[i];
    Some((
        entry.harness_session_id.clone()?,
        entry.delivery_policy.clone(),
    ))
}

/// The hold store under the state root: one record per session.
pub(crate) fn hold_store() -> HoldStore {
    let root = state_root();
    HoldStore::new(
        crate::state_layout::place_dir(&root, "mail-hold")
            .unwrap_or_else(|| root.join("mail-hold")),
    )
}

/// The default hold ceiling in minutes: no hold, asked for or not, ends
/// later than this. Overridable through `mail.hold.max_minutes` in the
/// global config; a value below one minute falls back to this default, and
/// one above a week clamps to a week.
pub(crate) const DEFAULT_MAX_HOLD_MINUTES: i64 = 8 * 60;

const MAX_HOLD_CEILING_MINUTES: i64 = 7 * 24 * 60;

/// The configured hold ceiling.
pub(crate) fn max_hold() -> chrono::Duration {
    let minutes = crate::agents_config::config_lookup_global(&["mail", "hold", "max_minutes"])
        .and_then(|v| v.as_integer())
        .filter(|m| *m > 0)
        .unwrap_or(DEFAULT_MAX_HOLD_MINUTES)
        .min(MAX_HOLD_CEILING_MINUTES);
    chrono::Duration::minutes(minutes)
}

/// The live hold for `session_id`, or None. An ended record is removed by
/// the read.
pub(crate) fn live_hold(session_id: &str, now: chrono::DateTime<chrono::Utc>) -> Option<Hold> {
    let session = SessionId::parse(session_id).ok()?;
    hold_store().live(&session, now)
}

/// Why a hold write or removal did not happen.
#[derive(Debug)]
pub(crate) enum HoldWriteError {
    /// The id is not a full session id.
    Session(fno_mail::session::SessionIdError),
    /// The store refused the write or the removal.
    Store(std::io::Error),
}

impl std::fmt::Display for HoldWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HoldWriteError::Session(e) => write!(f, "{e}"),
            HoldWriteError::Store(e) => write!(f, "the hold store refused the change: {e}"),
        }
    }
}

/// Write one hold for `session_id` and journal who set it.
pub(crate) fn put_hold(
    session_id: &str,
    length: chrono::Duration,
    idle: bool,
    set_by: SetBy,
    reason: &str,
) -> Result<Hold, HoldWriteError> {
    let session = SessionId::parse(session_id).map_err(HoldWriteError::Session)?;
    let hold = Hold::new(
        session,
        length,
        idle,
        chrono::Utc::now(),
        max_hold(),
        set_by,
        reason,
    );
    hold_store().put(&hold).map_err(HoldWriteError::Store)?;
    let writer = match set_by {
        SetBy::User => reason,
        SetBy::Conversation => "conversation_arm",
    };
    record_stamp(writer, session_id);
    Ok(hold)
}

/// Remove the hold for `session_id`. True when a record was there.
pub(crate) fn clear_hold(session_id: &str) -> Result<bool, HoldWriteError> {
    let session = SessionId::parse(session_id).map_err(HoldWriteError::Session)?;
    hold_store().clear(&session).map_err(HoldWriteError::Store)
}

/// The registry row's `bus-only` value: the lane capability the register
/// command sets for a session that takes mail only, never typed text. It is
/// not a hold and has no end.
pub(crate) const MAIL_ONLY: &str = "bus-only";

/// Read the current session's hold and lane capability together for the
/// check-in.
pub(crate) fn self_status(session_id: &str) -> Result<serde_json::Value, String> {
    let registry = load_registry(&AgentsHome::shared_registry_json())
        .map_err(|error| format!("agent registry unreadable: {error}"))?;
    let index = row_for_session(&registry, session_id)
        .ok_or_else(|| format!("no registry row carries session {session_id}"))?;
    let entry = &registry.entries[index];
    let matched = entry
        .harness_session_id
        .as_deref()
        .ok_or_else(|| format!("registry row for session {session_id} has no session id"))?;
    let hold = live_hold(matched, chrono::Utc::now());
    Ok(serde_json::json!({
        "clock_live": hold.is_some(),
        "clock_until": hold.as_ref().map(|h| h.until().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        "conversation": hold.as_ref().is_some_and(|h| h.set_by() == SetBy::Conversation),
        "delivery_policy": entry.delivery_policy.clone(),
    }))
}

/// Re-arm a live idle hold (the prompt-boundary activity rule). Answers
/// whether a hold is live after the call.
pub(crate) fn extend_hold(session_id: &str) -> bool {
    let Ok(session) = SessionId::parse(session_id) else {
        return false;
    };
    let store = hold_store();
    let now = chrono::Utc::now();
    let Some(hold) = store.live(&session, now) else {
        return false;
    };
    let next = hold.extended(now);
    if next != hold {
        let _ = store.put(&next);
    }
    true
}

/// The operator-facing clock prose the status line and the receipts share.
pub(crate) fn clock_description(hold: &Hold) -> String {
    let stamp = |d: chrono::DateTime<chrono::Utc>| d.format("%H:%M:%S UTC").to_string();
    match hold.clock() {
        HoldClock::Wall => format!("wall clock, fixed deadline {}", stamp(hold.until())),
        HoldClock::Idle { ceiling, .. } => {
            format!("quiet minutes idle clock, ceiling {}", stamp(ceiling))
        }
    }
}

/// Spawn the detached release timer (stdio null, own process group): it
/// waits out the hold, then drains what it kept on the bus with no further
/// input. The timer re-enters the running binary's `mail hold-release` and
/// re-reads the record every poll, so a re-arm keeps it sleeping. The
/// re-entry binary resolves through `FNO_AGENTS_TIMER_EXE` when set;
/// production leaves it unset and the tests point it at an argv-logging stub.
pub(crate) fn spawn_release_timer(session_id: &str) {
    let seam = std::env::var_os("FNO_AGENTS_TIMER_EXE").map(std::path::PathBuf::from);
    let Some(exe) = seam.or_else(|| std::env::current_exe().ok()) else {
        eprintln!("mail-hold: release timer did not start: cannot resolve the running binary");
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.args(["mail", "hold-release", "--handle", session_id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    match cmd.spawn() {
        // The timer child runs unwaited; the daemon drain reaps it by pid.
        Ok(child) => crate::orphan_reap::release_child(child.id()),
        Err(exc) => {
            // The held mail still drains at the next prompt boundary.
            eprintln!("mail-hold: release timer did not start: {exc}");
        }
    }
}

/// A real user message (C2): the provenance classifier passes it, and it is
/// neither a `!` shell line nor a slash/dollar command token. Everything else
/// is machinery, and machinery never arms the conversation hold.
pub(crate) fn is_real_message(prompt: &str) -> bool {
    let Ok(cleaned) = crate::provenance::classify(prompt) else {
        return false;
    };
    if cleaned.starts_with('!') {
        return false;
    }
    cleaned
        .split_whitespace()
        .next()
        .map(|tok| crate::provider::parse_command_token(tok).is_none())
        .unwrap_or(false)
}

/// What [`arm_conversation`] did.
pub(crate) enum ArmOutcome {
    /// The conversation hold is written.
    Armed,
    /// The session carries a hold the user set, or takes mail only; nothing
    /// changed.
    StoodAside,
    /// No registry row carries the session.
    NoRow,
    /// The hold write failed.
    WriteFailed,
}

impl ArmOutcome {
    /// The `mail-hold` command's exit code for the outcome.
    pub(crate) fn code(self) -> i32 {
        match self {
            ArmOutcome::Armed | ArmOutcome::StoodAside => 0,
            ArmOutcome::NoRow => 3,
            ArmOutcome::WriteFailed => 1,
        }
    }
}

/// The conversation arm (C2): write a wall hold the answer backstop out,
/// set by the conversation. A live hold the user set is never overwritten,
/// and a mail-only row needs no hold. Silent: no stdout, no stderr, so the
/// hook entries can call it mid-render.
pub(crate) fn arm_conversation(session_id: &str) -> (ArmOutcome, Option<String>) {
    match lookup_row(session_id) {
        Some((matched, policy)) => arm_row(&matched, policy),
        None => (ArmOutcome::NoRow, None),
    }
}

/// The conversation arm over an already-resolved registry row, so a caller
/// that read the row for its own decision (the prompt hook's since_ms) arms
/// without a second registry read.
fn arm_row(matched: &str, policy: Option<String>) -> (ArmOutcome, Option<String>) {
    let handle = identity_key(matched);
    if policy.as_deref() == Some(MAIL_ONLY) {
        return (ArmOutcome::StoodAside, Some(handle));
    }
    let prior = live_hold(matched, chrono::Utc::now());
    if prior.as_ref().is_some_and(|h| h.set_by() == SetBy::User) {
        return (ArmOutcome::StoodAside, Some(handle));
    }
    let backstop = chrono::Duration::seconds(answer_backstop_s());
    if put_hold(
        matched,
        backstop,
        false,
        SetBy::Conversation,
        "conversation",
    )
    .is_err()
    {
        return (ArmOutcome::WriteFailed, Some(handle));
    }
    if prior.is_none() {
        // One release timer per conversation: the timer re-reads the record
        // every poll, so a re-arm keeps the first timer sleeping.
        spawn_release_timer(matched);
    }
    (ArmOutcome::Armed, Some(handle))
}

/// The UserPromptSubmit side (C2, C4): a real message whose Enter the mux
/// witnessed inside the window arms the conversation hold. The window is
/// the 30 s submit look-back, widened to the answer backstop while a
/// conversation hold is live: a follow-up the harness queues during a long
/// answer fires UserPromptSubmit only after the turn ends, often minutes
/// after its Enter, and without the wide look-back it would not restart the
/// grace. An ended conversation hold whose record is still on disk widens
/// the window to its own arm instant: the follow-up behind it was witnessed
/// inside the window it covered. Silent on every path, never panics.
pub(crate) fn conversation_prompt(session_id: &str, prompt: &str) {
    if !is_real_message(prompt) {
        return;
    }
    let Some((matched, policy)) = lookup_row(session_id) else {
        return;
    };
    let now = chrono::Utc::now();
    let now_ms = now.timestamp_millis();
    let stored = SessionId::parse(&matched)
        .ok()
        .and_then(|s| hold_store().get(&s));
    let live = stored.as_ref().is_some_and(|h| h.live(now));
    let since_ms = if live {
        now_ms - answer_backstop_s() * 1000
    } else {
        lapsed_arm_ms(&stored, now)
            .unwrap_or(now_ms - crate::operator_witness::SUBMIT_WINDOW_AFTER_MS)
    };
    let journal = crate::paths::AgentsHome::from_env().events_jsonl();
    if crate::operator_witness::submitted_since(&journal, session_id, since_ms, now_ms) {
        arm_row(&matched, policy);
    }
}

/// The look-back floor for an ended conversation hold: its arm instant (the
/// end minus the answer backstop), so a follow-up witnessed inside that
/// window still re-arms after the hold ends. None for no record, a live
/// one, or one the user set, which keeps the plain 30 s look-back.
fn lapsed_arm_ms(hold: &Option<Hold>, now: chrono::DateTime<chrono::Utc>) -> Option<i64> {
    let hold = hold.as_ref()?;
    if hold.live(now) || hold.set_by() != SetBy::Conversation {
        return None;
    }
    Some(
        hold.until()
            .timestamp_millis()
            .saturating_sub(answer_backstop_s().saturating_mul(1000)),
    )
}

/// The Stop side (C3, C6): a live conversation hold is shortened to
/// `now + GRACE_S`, never extended, so repeated Stop fires in a loop cannot
/// keep one alive and the release timer lifts the hold about two minutes
/// after the answer ends. A hold the user set, or none, is left alone.
/// Silent, never panics.
pub(crate) fn conversation_turn_end(session_id: &str) {
    let Some((matched, _)) = lookup_row(session_id) else {
        return;
    };
    let now = chrono::Utc::now();
    let Some(hold) = live_hold(&matched, now) else {
        return;
    };
    if hold.set_by() != SetBy::Conversation {
        return;
    }
    let _ = hold_store().put(&hold.shortened_to(now + chrono::Duration::seconds(GRACE_S)));
}

/// The delivery gate's answer for one send (C15, C16).
pub(crate) struct GateVerdict {
    /// True when the mail may deliver live now.
    pub deliver: bool,
    /// Why a held send delivered anyway: the caller IS the recipient
    /// (`own`), or the body opens with a `control:` directive line
    /// (`control`).
    pub pass: Option<&'static str>,
    /// The C16 receipt naming when the hold ends. `None` for a mail-only
    /// row, which has no end, and when delivering.
    pub receipt: Option<String>,
    /// The live hold's `until`. `None` for a mail-only row or when
    /// delivering.
    pub until: Option<chrono::DateTime<chrono::Utc>>,
}

/// The registry row the gate's token addresses, as (harness session id,
/// name, delivery policy), under the shared [`row_matches_token`] rule.
fn lookup_gate_row(token: &str) -> Option<(String, String, Option<String>)> {
    let registry = load_registry(&AgentsHome::shared_registry_json()).ok()?;
    let entry = registry
        .entries
        .iter()
        .find(|e| row_matches_token(e, token))?;
    Some((
        entry.harness_session_id.clone()?,
        entry.name.clone(),
        entry.delivery_policy.clone(),
    ))
}

/// True when the caller resolving its own identity IS the held row (C15:
/// a session's own sends pass its own hold). The resolver is the authority;
/// the direct canonical-stamp read is its deterministic half, so a caller
/// whose stamp is complete but unproven still passes its own hold.
fn caller_is_recipient(sid: &str) -> bool {
    let home = AgentsHome::from_env();
    let owned =
        crate::spawn_context::resolve_self_identity(&|k| std::env::var(k).ok(), None, None, &home);
    if owned
        .session_id
        .as_deref()
        .map(|s| identity_key(s) == identity_key(sid))
        .unwrap_or(false)
    {
        return true;
    }
    // The resolver's first input, read directly: a complete canonical stamp
    // (FNO_HARNESS_NAME + FNO_HARNESS_SESSION_ID) names the caller.
    let name = std::env::var("FNO_HARNESS_NAME").unwrap_or_default();
    let session = std::env::var("FNO_HARNESS_SESSION_ID").unwrap_or_default();
    !name.trim().is_empty()
        && !session.trim().is_empty()
        && crate::claims::same_session_id(&session, sid)
}

/// The C15 control pass: the body's first non-blank line -- after one
/// leading `<fno_mail ...>` open tag is stripped (the wrapped lane puts the
/// directive after the tag), or after the delivered-mail header line is
/// stripped (header framing puts it first; the directive rides the body
/// under it) -- starts with `control:` (case-insensitive, the budget rule).
/// A header counts only when the store backs its sender
/// ([`crate::mail_header::verified_sender`]); an unbacked header line is
/// prose, never a directive.
pub(crate) fn body_is_control(body: &str) -> bool {
    let mut lines = body.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(first) = lines.next() else {
        return false;
    };
    // `get`, not a slice: a stamp like "`@Hazel · " puts a multibyte char
    // across byte 9, and slicing there panics every send.
    let rest = if first
        .get(..9)
        .is_some_and(|p| p.eq_ignore_ascii_case("<fno_mail"))
    {
        match first.find('>') {
            Some(gt) => &first[gt + 1..],
            None => first,
        }
    } else if crate::mail_header::is_header_line(first)
        && crate::mail_header::verified_sender(body, crate::chats::message_sender).is_some()
    {
        lines.next().unwrap_or("")
    } else {
        first
    };
    rest.trim_start().to_lowercase().starts_with("control:")
}

/// The C16 receipt for a live hold, in the row's local time. A conversation
/// hold with more than the grace left names the answer end (its `until` is
/// only the crash backstop, so a clock time there would be false); one
/// inside its grace reads as the wall clock it now is. An idle hold says
/// "or later", because activity restarts it.
fn hold_receipt(hold: &Hold, name: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    let until = hold.until();
    let hhmm = until
        .with_timezone(&chrono::Local)
        .format("%H:%M")
        .to_string();
    if hold.set_by() == SetBy::Conversation && until - now > chrono::Duration::seconds(GRACE_S) {
        return format!(
            "held: {name} is in a conversation with the user; delivers about 2 minutes after the answer ends"
        );
    }
    if matches!(hold.clock(), HoldClock::Idle { .. }) {
        return format!(
            "held until about {hhmm} or later: {name} is in do-not-disturb and the quiet-minutes clock restarts on activity; delivers itself then"
        );
    }
    format!(
        "held until about {hhmm}: {name} is in do-not-disturb (wall clock); delivers itself then"
    )
}

/// The delivery gate (C15, C16): does mail to `token` deliver live now?
/// A mail-only row never takes typed text and carries no receipt; a live
/// hold record holds with one. An ended record is removed here and the mail
/// delivers. Own sends and `control:` mail pass either. The gate never
/// writes the registry, so it runs inside callers that hold its lock.
pub(crate) fn gate(
    token: &str,
    body: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> GateVerdict {
    let deliver = |pass| GateVerdict {
        deliver: true,
        pass,
        receipt: None,
        until: None,
    };
    let Some((sid, name, policy)) = lookup_gate_row(token) else {
        return deliver(None);
    };
    let hold = if policy.as_deref() == Some(MAIL_ONLY) {
        None
    } else {
        match live_hold(&sid, now) {
            Some(hold) => Some(hold),
            None => return deliver(None),
        }
    };
    if caller_is_recipient(&sid) {
        return deliver(Some("own"));
    }
    if body.is_some_and(body_is_control) {
        return deliver(Some("control"));
    }
    GateVerdict {
        deliver: false,
        pass: None,
        receipt: hold.as_ref().map(|h| hold_receipt(h, &name, now)),
        until: hold.as_ref().map(Hold::until),
    }
}

/// The `--gate --park-on-hold` answer for a held body (C15): park the
/// payload and answer the `parked` JSON line with the park receipt. The
/// session's own sends type now (the pane's hold-pass stands down for them),
/// and so does a genuinely deliverable body; a `control:`-prefixed body on
/// the raw door parks too, because typed text carries no control envelope
/// for anything downstream to honor. An empty body or a failed park answers
/// None and the caller prints the plain verdict instead.
pub(crate) fn gate_parked_line(
    verdict: &GateVerdict,
    session_id: &str,
    body: &str,
) -> Option<String> {
    let typable =
        verdict.pass.as_deref() == Some("own") || (verdict.deliver && verdict.pass.is_none());
    if body.trim().is_empty() || typable {
        return None;
    }
    // The receipt promises a run-when, so a held shape with no deadline never
    // parks: a mail-only row never takes typed text at all, the runner
    // would sit on the payload to its 24 h bound, and the raw lane's
    // documented answer for that case is the non-zero refusal. A
    // control-prefixed body carries a deliver verdict with no until and
    // parks as typed text, so only the held-and-undated shape refuses.
    if !verdict.deliver && verdict.until.is_none() {
        return None;
    }
    let receipt = park_payload_inner(session_id, body).ok()?;
    Some(
        serde_json::json!({
            "verdict": "parked",
            "pass": serde_json::Value::Null,
            "receipt": receipt,
            "until": verdict.until.map(|u| u.to_rfc3339()),
        })
        .to_string(),
    )
}

/// The parked raw payloads for one held session, beside its clock in the one
/// hold store.
fn parked_dir(handle: &str) -> PathBuf {
    crate::state_layout::place_in(&state_root(), "mail-hold", &format!("{handle}.parked"))
}

fn process_alive(pid: i32) -> bool {
    Pid::try_from(i64::from(pid)).is_ok_and(|p| p.probe())
}

/// The park receipt, on the gate's time rule: a wall or grace clock names
/// its local hour, a conversation hold just says held.
fn park_receipt(session_id: &str, payload_first_line: &str, name: &str) -> String {
    let now = chrono::Utc::now();
    let verdict = gate(session_id, None, now);
    let time_part = match (&verdict.receipt, verdict.until) {
        (Some(r), _) if r.starts_with("held until about ") => {
            let hhmm = r
                .trim_start_matches("held until about ")
                .split(':')
                .take(2)
                .collect::<Vec<_>>()
                .join(":");
            // The receipt's clock is already local and prefixed; reuse it.
            format!("held until about {hhmm}")
        }
        _ => "held".to_string(),
    };
    format!("{time_part}: {payload_first_line} runs on {name} when the hold ends")
}

/// Park one raw payload for a held session (C15): it lands beside the hold
/// record, one detached runner is made sure of, and the answer is the
/// receipt naming when it will run, or the exit code on failure. The runner
/// inherits this process's environment, so the replayed send is stamped
/// with the original sender.
fn park_payload_inner(session_id: &str, payload: &str) -> Result<String, i32> {
    let Some((matched, name, _)) = lookup_gate_row(session_id) else {
        eprintln!("mail-hold: no registry row carries session {session_id}");
        return Err(3);
    };
    let handle = identity_key(&matched);
    let dir = parked_dir(&handle);
    if std::fs::create_dir_all(&dir).is_err() {
        eprintln!("mail-hold: could not create the park directory under the hold store");
        return Err(1);
    }
    let file = dir.join(format!(
        "{}-{}.txt",
        chrono::Utc::now().timestamp_millis(),
        std::process::id()
    ));
    if std::fs::write(&file, payload).is_err() {
        eprintln!("mail-hold: could not write the parked payload");
        return Err(1);
    }
    // One runner per held session: a live pid in `runner.pid` stands.
    let pid_file = dir.join("runner.pid");
    let spawn = match std::fs::read_to_string(&pid_file) {
        Ok(text) => text
            .trim()
            .parse::<i32>()
            .map(|pid| !process_alive(pid))
            .unwrap_or(true),
        Err(_) => true,
    };
    if spawn {
        let mut cmd = Command::new(runner_bin());
        cmd.args(["mail-hold", "--run-parked", "--session", session_id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        match cmd.spawn() {
            Ok(child) => {
                let _ = std::fs::write(&pid_file, child.id().to_string());
            }
            Err(exc) => {
                eprintln!("mail-hold: parked the payload but the runner did not start: {exc}");
                return Err(1);
            }
        }
    }
    let first_line = payload.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    Ok(park_receipt(session_id, first_line, &name))
}

/// The binary a spawned runner execs: `FNO_AGENTS_RUNNER_BIN` when declared,
/// else this process's own executable. The override is a test seam: a suite
/// that parks through the real gate pins a stub here so no detached runner
/// escapes the sandbox.
fn runner_bin() -> std::ffi::OsString {
    if let Some(pin) = std::env::var_os("FNO_AGENTS_RUNNER_BIN").filter(|v| !v.is_empty()) {
        return pin;
    }
    std::env::current_exe()
        .map(|exe| exe.into_os_string())
        .unwrap_or_else(|_| std::ffi::OsString::from("fno-agents"))
}

/// What one poll of the parked store did.
enum DrainPoll {
    /// The gate delivered and every payload went out.
    Sent,
    /// The hold is still live (or the store momentarily unreadable): sleep.
    Waiting,
    /// The gate said deliver but a send failed or a payload is unreadable,
    /// so the same files will fail again until something changes.
    Stalled,
}

/// Seconds between the runner's gate polls.
const RUN_PARKED_POLL_S: u64 = 5;

/// The ceiling on a stalled poll's backoff: the sender was told the payload
/// runs when the hold ends, so a failing transport never strands it - the
/// runner retries at doubling intervals up to this cap until its bound.
const RUN_PARKED_STALL_BACKOFF_CAP_S: u64 = 600;

/// The runner's absolute ceiling: a hold can legitimately last hours (up to
/// the hold ceiling), so the runner keeps polling until its
/// files drain, its session's row vanishes, or this bound fires.
const RUN_PARKED_BOUND_S: u64 = 24 * 3600;

/// One runner poll (C15): with the hold still live it answers
/// [`DrainPoll::Waiting`]; when the gate delivers, every parked file is sent
/// oldest first through the normal raw door and deleted on exit 0
/// ([`DrainPoll::Sent`]). A failed send keeps its file, logs to stderr and
/// answers [`DrainPoll::Stalled`], so the runner bounds the retries instead
/// of spinning.
fn drain_parked_once(session_id: &str) -> DrainPoll {
    let Some((matched, _, _)) = lookup_gate_row(session_id) else {
        return DrainPoll::Waiting;
    };
    let handle = identity_key(&matched);
    let dir = parked_dir(&handle);
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return DrainPoll::Waiting;
    };
    let mut files: Vec<PathBuf> = read_dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "txt"))
        .collect();
    files.sort();
    if files.is_empty() {
        return DrainPoll::Waiting;
    }
    let now = chrono::Utc::now();
    if !gate(session_id, None, now).deliver {
        return DrainPoll::Waiting;
    }
    for file in files {
        let Ok(payload) = std::fs::read_to_string(&file) else {
            eprintln!(
                "mail-hold: parked payload {} unreadable; leaving it parked",
                file.display()
            );
            return DrainPoll::Stalled;
        };
        let mut cmd = Command::new(crate::scrape::fno_bin());
        cmd.args(["agents", "mail", "send", session_id, &payload, "--raw"]);
        match cmd.status() {
            Ok(status) if status.success() => {
                let _ = std::fs::remove_file(&file);
            }
            other => {
                eprintln!(
                    "mail-hold: parked payload {} did not send ({other:?}); it stays parked",
                    file.display()
                );
                return DrainPoll::Stalled;
            }
        }
    }
    DrainPoll::Sent
}

/// The poll loop behind [`run_parked`], with the constants as parameters so
/// the backoff is testable in milliseconds.
fn run_parked_loop(
    session_id: &str,
    dir: &std::path::Path,
    poll: std::time::Duration,
    bound: std::time::Duration,
) -> i32 {
    let start = std::time::Instant::now();
    let mut stalled = 0u32;
    while start.elapsed() < bound {
        // A row that vanishes mid-run (the session ended, or a test's home
        // was torn down) leaves every parked file undeliverable: exit and
        // leave the files for the next park to re-arm over.
        if lookup_gate_row(session_id).is_none() {
            break;
        }
        let files = std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "txt"))
                    .count()
            })
            .unwrap_or(0);
        if files == 0 {
            break;
        }
        match drain_parked_once(session_id) {
            DrainPoll::Sent => {
                stalled = 0;
                continue;
            }
            // Held (or the store momentarily unreadable): the normal poll
            // interval, and the stall ladder resets.
            DrainPoll::Waiting => {
                stalled = 0;
                std::thread::sleep(poll);
            }
            // The gate said deliver but the send failed: back off along the
            // ladder instead of spinning, and keep trying until the bound.
            DrainPoll::Stalled => {
                stalled += 1;
                let ladder = poll.saturating_mul(1u32 << stalled.min(8));
                std::thread::sleep(ladder.min(std::time::Duration::from_secs(
                    RUN_PARKED_STALL_BACKOFF_CAP_S,
                )));
            }
        }
    }
    0
}

/// `--run-parked`: the detached runner body. Polls the gate every 5 s,
/// drains parked files when the hold ends, and exits when the directory is
/// empty, when the session's row is gone or unknown, or after 24 hours -
/// always leaving undelivered files for the next park to pick up.
fn run_parked(session_id: &str) -> i32 {
    let Some((matched, _, _)) = lookup_gate_row(session_id) else {
        return 3;
    };
    let dir = parked_dir(&identity_key(&matched));
    let pid_file = dir.join("runner.pid");
    let _ = std::fs::write(&pid_file, std::process::id().to_string());
    let code = run_parked_loop(
        session_id,
        &dir,
        std::time::Duration::from_secs(RUN_PARKED_POLL_S),
        std::time::Duration::from_secs(RUN_PARKED_BOUND_S),
    );
    let _ = std::fs::remove_file(&pid_file);
    code
}

/// How far past its end a hold must sit before the sweep considers its
/// release timer dead: the live timer polls every 15 s, so 60 s of silence
/// means it is gone.
const LAPSED_TIDY_MARGIN_S: i64 = 60;

/// The sessions whose hold record ended more than [`LAPSED_TIDY_MARGIN_S`]
/// ago: the sweep re-arms a release timer for each (C16), which drains what
/// the hold kept. A live record is not picked; its own timer is winning.
pub(crate) fn lapsed_hold_sessions(
    store: &HoldStore,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<String> {
    let margin = now - chrono::Duration::seconds(LAPSED_TIDY_MARGIN_S);
    store
        .list()
        .into_iter()
        .filter(|h| h.until() < margin)
        .map(|h| h.session().as_str().to_string())
        .collect()
}

/// Fold one clock file the retired sidecar writer left: a live timed one
/// becomes a hold record with the same end; anything else (no end, ended,
/// keyed by a head, unreadable) is removed. A file that already reads as a
/// hold record is left alone.
fn fold_legacy_clock(
    store: &HoldStore,
    path: &std::path::Path,
    now: chrono::DateTime<chrono::Utc>,
) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    if serde_json::from_str::<Hold>(&text).is_ok() {
        return;
    }
    let session = path
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| SessionId::parse(s).ok());
    let legacy: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    let until = j_field(&legacy, "until")
        .as_str()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|d| d.with_timezone(&chrono::Utc));
    if let (Some(session), Some(until)) = (session, until.filter(|u| *u > now)) {
        let set_by = if j_field(&legacy, "source").as_str() == Some("conversation") {
            SetBy::Conversation
        } else {
            SetBy::User
        };
        let hold = Hold::new(
            session,
            until - now,
            false,
            now,
            max_hold(),
            set_by,
            "folded",
        );
        if store.put(&hold).is_ok() {
            return;
        }
    }
    let _ = std::fs::remove_file(path);
}

/// The C16 sweep arm: fold any retired clock file, re-arm a release timer
/// for every ended hold, and clear the old do-not-disturb stamps. After the
/// move to the hold record, the only legitimate `bus-only` value is the
/// register command's mail-only capability, which writes `origin =
/// "operator"`; any other row stamped `bus-only` past the answer backstop is
/// a stamp the retired hold writers left.
pub(crate) fn tidy_lapsed_holds(home: &AgentsHome, now: chrono::DateTime<chrono::Utc>) {
    let store = hold_store();
    if let Ok(entries) = std::fs::read_dir(store.dir()) {
        for path in entries.flatten().map(|e| e.path()) {
            if path.extension().is_some_and(|x| x == "json") {
                fold_legacy_clock(&store, &path, now);
            }
        }
    }
    for session in lapsed_hold_sessions(&store, now) {
        spawn_release_timer(&session);
    }
    let Ok(registry) = load_registry(&home.registry_json()) else {
        return;
    };
    for (name, sid) in orphan_stamp_rows(&registry, now) {
        if set_policy(&sid, None).is_some() {
            eprintln!("mail-hold: cleared retired bus-only stamp on {name} ({sid})");
        }
    }
}

/// How old a row must be before a non-operator `bus-only` stamp on it is
/// cleared: the conversation backstop, so a stamp an old binary wrote while
/// it still armed holds this way runs its course first.
fn orphan_stamp_age_s() -> i64 {
    answer_backstop_s()
}

/// The (name, session id) of every non-operator row stamped `bus-only`,
/// older than [`orphan_stamp_age_s`]. A row with no readable `created_at`
/// counts as old. Pure so the pick is testable without a registry write.
pub(crate) fn orphan_stamp_rows(
    registry: &crate::state::Registry,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<(String, String)> {
    registry
        .entries
        .iter()
        .filter(|e| e.delivery_policy.as_deref() == Some(MAIL_ONLY))
        .filter(|e| e.origin.as_deref() != Some("operator"))
        .filter_map(|e| {
            let sid = e.harness_session_id.as_deref()?;
            let old = chrono::DateTime::parse_from_rfc3339(&e.created_at).map_or(true, |born| {
                now - born.with_timezone(&chrono::Utc)
                    > chrono::Duration::seconds(orphan_stamp_age_s())
            });
            old.then(|| (e.name.clone(), sid.to_string()))
        })
        .collect()
}

/// `fno-agents mail-hold --session <id> [--off | --release | --run-parked]`
///
/// Arm (default): run the conversation arm and report its outcome.
/// `--off`: remove the session's hold record. No row for the session: exit
/// 3, nothing written.
/// `--release`: `--off` plus the release leg - the sideline menu's Release
/// hold. The detached `hold-release` timer finds no live hold, so it drains
/// what the hold kept on the bus at once.
/// `--run-parked` (C15): the detached runner that replays raw sends parked
/// on a held session when the hold ends.
pub fn run_mail_hold(args: &[String]) -> i32 {
    let mut session: Option<&String> = None;
    let mut off = false;
    let mut release = false;
    let mut run_parked_mode = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--session" => session = iter.next(),
            "--off" => off = true,
            "--release" => release = true,
            "--run-parked" => run_parked_mode = true,
            other => {
                eprintln!("mail-hold: unknown argument {other:?}");
                return 2;
            }
        }
    }
    let Some(session_id) = session else {
        eprintln!("mail-hold: --session <session-id> is required");
        return 2;
    };
    if run_parked_mode {
        return run_parked(session_id);
    }
    if off || release {
        let Some((matched, _)) = lookup_row(session_id) else {
            eprintln!("mail-hold: no registry row carries session {session_id}");
            return 3;
        };
        if let Err(e) = clear_hold(&matched) {
            eprintln!("mail-hold: {e}");
            return 1;
        }
        if release {
            spawn_release_timer(&matched);
        }
        return 0;
    }
    let (outcome, matched) = arm_conversation(session_id);
    match outcome {
        ArmOutcome::Armed => println!(
            "mail-hold: conversation hold armed for {}",
            matched.as_deref().unwrap_or(session_id)
        ),
        ArmOutcome::StoodAside => println!(
            "mail-hold: hold for {} stands (the conversation rules never overwrite a manual hold)",
            matched.as_deref().unwrap_or(session_id)
        ),
        ArmOutcome::NoRow => {
            eprintln!("mail-hold: no registry row carries session {session_id}")
        }
        ArmOutcome::WriteFailed => {
            eprintln!("mail-hold: could not write the conversation hold for {session_id}")
        }
    }
    outcome.code()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Pin FNO_AGENTS_HOME (registry + journal), FNO_HOME (hold records)
    /// and FNO_STATE_DIR (chats store) to one tempdir for `f`, under the
    /// crate-wide env lock (claim_commands idiom). FNO_BIN points at `true` so
    /// a parked-payload drain spawn is a no-op the test never waits on.
    /// The global config read is pinned away too: FNO_GLOBAL_SETTINGS_PATH
    /// names a file that does not exist, so `mail.hold.*` on the developer's
    /// machine cannot reach a hold test, and FNO_AGENTS_ANSWER_MINUTES is
    /// cleared so the override cannot either. FNO_AGENTS_TIMER_EXE points at
    /// `true` so no release timer escapes the sandbox. Prior values are
    /// restored, so an ambient FNO_HOME survives the test.
    pub(crate) fn with_hold_env(f: impl FnOnce(&std::path::Path)) {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior: Vec<(String, Option<std::ffi::OsString>)> = [
            "FNO_AGENTS_HOME",
            "FNO_HOME",
            "FNO_BIN",
            "FNO_STATE_DIR",
            "FNO_GLOBAL_SETTINGS_PATH",
            "FNO_AGENTS_ANSWER_MINUTES",
            "FNO_AGENTS_TIMER_EXE",
        ]
        .iter()
        .map(|k| (k.to_string(), std::env::var_os(k)))
        .collect();
        let td = tempfile::TempDir::new().unwrap();
        std::env::set_var("FNO_AGENTS_HOME", td.path());
        std::env::set_var("FNO_HOME", td.path());
        std::env::set_var("FNO_BIN", "true");
        std::env::set_var("FNO_STATE_DIR", td.path());
        std::env::set_var(
            "FNO_GLOBAL_SETTINGS_PATH",
            td.path().join("no-global-settings.toml"),
        );
        std::env::set_var("FNO_AGENTS_TIMER_EXE", "true");
        std::env::remove_var("FNO_AGENTS_ANSWER_MINUTES");
        f(td.path());
        for (key, value) in prior {
            match value {
                Some(v) => std::env::set_var(&key, v),
                None => std::env::remove_var(&key),
            }
        }
    }

    /// One stored chats message row under `<dir>/chats/`, so the trust
    /// lookup ([`crate::chats::message_sender`]) finds `id` naming `sender`.
    /// The index is never built; the scan fallback reads the file.
    pub(crate) fn write_chats_message(dir: &std::path::Path, id: &str, sender: &str) {
        let chat = crate::state_layout::place_dir(dir, "chats")
            .unwrap()
            .join(format!("pair-{sender}-worker"));
        std::fs::create_dir_all(&chat).unwrap();
        let line = serde_json::json!({
            "type": "message", "id": id, "from": sender, "body": "note",
        });
        let path = chat.join("messages.jsonl");
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        text.push_str(&line.to_string());
        text.push('\n');
        std::fs::write(path, text).unwrap();
    }

    pub(crate) fn registry_row(name: &str, session: &str) -> serde_json::Value {
        serde_json::json!({
            "name": name, "status": "live", "cwd": "/repo", "harness": "claude",
            "harness_session_id": session,
            "created_at": "2026-09-26T00:00:00Z",
        })
    }

    pub(crate) fn write_registry(dir: &std::path::Path, rows: serde_json::Value) {
        let doc = serde_json::json!({
            "schema_version": crate::state::REGISTRY_SCHEMA_VERSION,
            "agents": rows,
        });
        crate::registry_store::seed_raw(&dir.join("registry.json"), doc.to_string());
    }

    /// Append one `operator_submit` witness row to the temp journal
    /// (`$FNO_AGENTS_HOME/events.jsonl`, the file `conversation_prompt`
    /// reads).
    pub(crate) fn witness_row(session: &str, submit_ms: i64) {
        use std::io::Write as _;
        let path = std::env::var_os("FNO_AGENTS_HOME")
            .map(std::path::PathBuf::from)
            .unwrap()
            .join("events.jsonl");
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        let row = serde_json::json!({
            "ts": "2026-09-27T00:00:00Z",
            "type": "operator_submit",
            "source": "daemon",
            "data": {
                "mux_session": "main",
                "pane": 7,
                "via": "pane",
                "submit_ms": submit_ms,
                "resolution": "ok",
                "harness_session": session,
            }
        });
        writeln!(f, "{row}").unwrap();
    }

    fn now_ms() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    const SID: &str = "cccccccc-1111-2222-3333-444455556666";
    const OTHER: &str = "dddddddd-1111-2222-3333-444455556666";

    fn sid(raw: &str) -> SessionId {
        SessionId::parse(raw).unwrap()
    }

    /// Write a hold that started `ago_s` seconds ago and runs `len_s`.
    fn plant(session: &str, ago_s: i64, len_s: i64, set_by: SetBy) {
        let start = chrono::Utc::now() - chrono::Duration::seconds(ago_s);
        let hold = Hold::new(
            sid(session),
            chrono::Duration::seconds(len_s),
            false,
            start,
            chrono::Duration::hours(8),
            set_by,
            "test",
        );
        hold_store().put(&hold).unwrap();
    }

    fn stored(session: &str) -> Option<Hold> {
        hold_store().get(&sid(session))
    }

    /// A mail-only row: the register command's lane capability.
    fn mail_only_row(name: &str, session: &str) -> serde_json::Value {
        let mut row = registry_row(name, session);
        if let Some(fields) = row.as_object_mut() {
            fields.insert(
                "delivery_policy".into(),
                serde_json::Value::String(MAIL_ONLY.into()),
            );
        }
        if let Some(fields) = row.as_object_mut() {
            fields.insert(
                "origin".into(),
                serde_json::Value::String("operator".into()),
            );
        }
        row
    }

    /// Two same-window codex rows share a first-eight head: a short token
    /// answers to both and resolves to nothing, while the full id still names
    /// one row.
    #[test]
    fn a_short_token_two_rows_answer_to_resolves_to_nothing() {
        let mut registry = crate::state::Registry::default();
        for (name, sid) in [
            (
                "t-parked-prompt-blocked",
                "01a1ad25-f04f-7220-8b8e-6c41a3bad0dd",
            ),
            ("t-composer-flags", "01a1ad25-7752-7020-a858-4253ed259833"),
        ] {
            let mut row = crate::state::RegistryEntry::default();
            row.name = name.to_string();
            row.harness = Some("codex".to_string());
            row.harness_session_id = Some(sid.to_string());
            registry.entries.push(row);
        }
        assert_eq!(sole_row_for_token(&registry, "01a1ad25"), None);
        assert_eq!(
            sole_row_for_token(&registry, "01a1ad25-f04f-7220-8b8e-6c41a3bad0dd"),
            Some(0)
        );
        assert_eq!(
            sole_row_for_token(&registry, "01a1ad25-7752-7020-a858-4253ed259833"),
            Some(1)
        );
    }

    #[test]
    fn arming_a_registered_session_writes_a_conversation_hold_and_no_stamp() {
        with_hold_env(|dir| {
            write_registry(dir, serde_json::json!([registry_row("worker", SID)]));
            let (outcome, _) = arm_conversation(SID);
            assert!(matches!(outcome, ArmOutcome::Armed));
            let registry = crate::state::load_registry(&dir.join("registry.json")).unwrap();
            assert_eq!(registry.entries.len(), 1);
            assert_eq!(
                registry
                    .entries
                    .first()
                    .and_then(|e| e.delivery_policy.clone()),
                None
            );
            let hold = stored(SID).expect("the arm writes a hold record");
            assert_eq!(hold.set_by(), SetBy::Conversation);
            assert_eq!(hold.clock(), HoldClock::Wall);
            let left = (hold.until() - chrono::Utc::now()).num_seconds();
            assert!(
                (answer_backstop_s() - 50..=answer_backstop_s()).contains(&left),
                "the conversation hold sits about the answer backstop out, got {left}s"
            );
            let status = self_status(SID).unwrap();
            assert_eq!(status["clock_live"], true);
            assert_eq!(status["conversation"], true);
            assert!(j_field(&status, "delivery_policy").is_null());
            // The write names its writer, so a phantom hold traces to a caller.
            let journal =
                crate::event_store::journal_text(&dir.join("events.jsonl"), &["mail_hold_stamped"]);
            let stamp: serde_json::Value = journal
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .find(|v: &serde_json::Value| v["type"] == "mail_hold_stamped")
                .expect("the arm journals a mail_hold_stamped row");
            assert_eq!(stamp["data"]["writer"], "conversation_arm");
            assert_eq!(stamp["data"]["session_id"], SID);
            assert_eq!(
                crate::hold_label::hold_attention(&status).unwrap(),
                "mail held while the user talks to you (machine-armed; lifts about 2 min after your answer)"
            );
            // The status command addresses the same hold by the canonical
            // short handle; the gate must hold on it, not deliver.
            let first8 = identity_key(SID).get(..8).unwrap().to_string();
            assert!(!gate(&first8, None, chrono::Utc::now()).deliver);
        });
    }

    /// The `$FNO_AGENTS_ANSWER_MINUTES` override and the global config key
    /// move the hold the arm writes; a non-positive value falls through to
    /// the default the way an absence does.
    #[test]
    fn answer_backstop_honors_the_env_override_and_the_config_key() {
        with_hold_env(|dir| {
            write_registry(dir, serde_json::json!([registry_row("worker", SID)]));
            let length = || {
                let hold = stored(SID).unwrap();
                (hold.until() - chrono::Utc::now()).num_seconds() + 1
            };
            std::env::set_var("FNO_AGENTS_ANSWER_MINUTES", "2");
            arm_conversation(SID);
            assert!((110..=120).contains(&length()));
            std::env::set_var("FNO_AGENTS_ANSWER_MINUTES", "0");
            assert_eq!(answer_backstop_s(), 600);
            std::env::remove_var("FNO_AGENTS_ANSWER_MINUTES");
            let global = dir.join("global");
            std::fs::create_dir_all(&global).unwrap();
            std::fs::write(
                global.join("config.toml"),
                "[mail.hold]\nanswer_minutes = 3\nmax_minutes = 60\n",
            )
            .unwrap();
            std::env::set_var("FNO_GLOBAL_SETTINGS_PATH", global.join("settings.toml"));
            assert_eq!(answer_backstop_s(), 180);
            assert_eq!(max_hold(), chrono::Duration::minutes(60));
        });
    }

    /// A follow-up queued during a long answer carries a witness inside the
    /// answering window the ended hold covered, so it re-arms even though
    /// the prompt fires well past the end. With no record the plain 30 s
    /// look-back stands and the same old witness arms nothing.
    #[test]
    fn a_followup_submitted_after_the_hold_ends_still_re_arms() {
        with_hold_env(|dir| {
            write_registry(
                dir,
                serde_json::json!([registry_row("worker", SID), registry_row("idle", OTHER)]),
            );
            // The previous answering window: armed 700 s ago, ended 100 s ago.
            plant(SID, 700, 600, SetBy::Conversation);
            let old_witness_ms = now_ms() - 690_000;
            witness_row(SID, old_witness_ms);
            witness_row(OTHER, old_witness_ms);
            conversation_prompt(SID, "follow up");
            let hold = stored(SID).unwrap();
            assert!(
                hold.live(chrono::Utc::now()),
                "re-armed live for the next answer"
            );
            conversation_prompt(OTHER, "follow up");
            assert!(
                stored(OTHER).is_none(),
                "an old witness with no record never arms"
            );
        });
    }

    #[test]
    fn arming_an_unregistered_session_refuses_and_writes_nothing() {
        with_hold_env(|dir| {
            write_registry(dir, serde_json::json!([registry_row("worker", SID)]));
            let (outcome, _) = arm_conversation(OTHER);
            assert!(matches!(outcome, ArmOutcome::NoRow));
            assert!(stored(OTHER).is_none());
        });
    }

    #[test]
    fn release_removes_the_hold_and_arms_the_release_leg() {
        with_hold_env(|dir| {
            let _g = env_guard(&["PARK_LOG"]);
            write_registry(dir, serde_json::json!([registry_row("worker", SID)]));
            plant(SID, 0, 600, SetBy::Conversation);
            let stub = write_stub_py(dir);
            let log = dir.join("release.log");
            std::env::set_var("FNO_AGENTS_TIMER_EXE", stub.to_string_lossy().to_string());
            std::env::set_var("PARK_LOG", log.to_string_lossy().to_string());
            assert_eq!(
                run_mail_hold(&["--session".into(), SID.into(), "--release".into()]),
                0
            );
            assert!(stored(SID).is_none(), "the record is gone");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let logged = loop {
                let text = std::fs::read_to_string(&log).unwrap_or_default();
                if text.contains("hold-release") || std::time::Instant::now() > deadline {
                    break text;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            };
            assert!(
                logged.contains(&format!("hold-release --handle {SID}")),
                "the release leg spawned for the full id: {logged:?}"
            );
        });
    }

    #[test]
    fn conversation_prompt_needs_a_fresh_witness_for_this_session() {
        with_hold_env(|dir| {
            write_registry(dir, serde_json::json!([registry_row("worker", SID)]));
            conversation_prompt(SID, "hello");
            assert!(stored(SID).is_none(), "no witness, no arm");
            witness_row(SID, now_ms() - 31_000);
            conversation_prompt(SID, "hello");
            assert!(
                stored(SID).is_none(),
                "a 31 s old witness is outside the window"
            );
            witness_row(OTHER, now_ms() - 1_000);
            conversation_prompt(SID, "hello");
            assert!(
                stored(SID).is_none(),
                "another session's witness arms nothing"
            );
        });
    }

    /// A hold the user set is never overwritten by the conversation rules,
    /// and a mail-only row gets no hold at all.
    #[test]
    fn a_user_hold_and_a_mail_only_row_are_never_overwritten() {
        with_hold_env(|dir| {
            write_registry(
                dir,
                serde_json::json!([registry_row("worker", SID), mail_only_row("desk", OTHER)]),
            );
            plant(SID, 0, 3600, SetBy::User);
            let status = self_status(SID).unwrap();
            assert_eq!(status["conversation"], false);
            assert_eq!(
                crate::hold_label::hold_attention(&status).unwrap(),
                "DND on"
            );
            let before = stored(SID).unwrap();
            witness_row(SID, now_ms() - 1_000);
            conversation_prompt(SID, "hello");
            conversation_turn_end(SID);
            assert_eq!(stored(SID).unwrap(), before, "a user hold is unchanged");
            witness_row(OTHER, now_ms() - 1_000);
            conversation_prompt(OTHER, "hello");
            assert!(stored(OTHER).is_none(), "a mail-only row gets no hold");
        });
    }

    #[test]
    fn turn_end_moves_a_conversation_hold_to_the_grace() {
        with_hold_env(|dir| {
            write_registry(dir, serde_json::json!([registry_row("worker", SID)]));
            arm_conversation(SID);
            conversation_turn_end(SID);
            let until = stored(SID).unwrap().until();
            let left = (until - chrono::Utc::now()).num_seconds();
            assert!(
                (1..=GRACE_S).contains(&left),
                "at most one grace out, got {left}s"
            );
            std::thread::sleep(std::time::Duration::from_millis(1_100));
            conversation_turn_end(SID);
            assert!(
                stored(SID).unwrap().until() <= until,
                "a second Stop never extends"
            );
        });
    }

    /// Restores snapshotted env vars on drop, so a panicking assert cannot
    /// leak a stamp into the next test (the crate env lock serializes them).
    struct EnvGuard(Vec<(String, Option<std::ffi::OsString>)>);
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.0.drain(..) {
                match value {
                    Some(v) => std::env::set_var(&key, v),
                    None => std::env::remove_var(&key),
                }
            }
        }
    }

    fn env_guard(keys: &[&str]) -> EnvGuard {
        EnvGuard(
            keys.iter()
                .map(|k| (k.to_string(), std::env::var_os(k)))
                .collect(),
        )
    }

    /// AC7: a hold whose end has passed delivers, and the gate's read
    /// removes the record; a live one holds with a receipt.
    #[test]
    fn gate_delivers_an_ended_hold_and_the_record_is_gone() {
        with_hold_env(|dir| {
            write_registry(dir, serde_json::json!([registry_row("worker", SID)]));
            let now = chrono::Utc::now();
            plant(SID, 660, 600, SetBy::User);
            assert!(gate(SID, None, now).deliver, "an ended hold delivers");
            assert!(stored(SID).is_none(), "the ended record is gone");
            plant(SID, 0, 600, SetBy::User);
            let v = gate(SID, None, now);
            assert!(!v.deliver);
            assert!(v.receipt.is_some() && v.until.is_some());
        });
    }

    /// A mail-only row holds with no receipt and no end: the turn-boundary
    /// surface it promises is real, and it is not a hold.
    #[test]
    fn gate_holds_a_mail_only_row_with_no_receipt() {
        with_hold_env(|dir| {
            write_registry(dir, serde_json::json!([mail_only_row("desk", SID)]));
            let v = gate(SID, None, chrono::Utc::now());
            assert!(!v.deliver && v.receipt.is_none() && v.until.is_none());
        });
    }

    #[test]
    fn gate_passes_control_mail_and_holds_a_mid_line_mention() {
        with_hold_env(|dir| {
            write_registry(dir, serde_json::json!([registry_row("worker", SID)]));
            let now = chrono::Utc::now();
            plant(SID, 0, 600, SetBy::User);
            let v = gate(SID, Some("<fno_mail from=\"k\">control: stop"), now);
            assert!(v.deliver && v.pass == Some("control"));
            let v = gate(SID, Some("CONTROL: stop the budget clock"), now);
            assert!(v.deliver && v.pass == Some("control"));
            let v = gate(SID, Some("the control: word mid-line"), now);
            assert!(!v.deliver, "a mention is prose, not the directive");
            let v = gate(
                SID,
                Some("<fno_mail from=\"k\">\nstop, control: said late"),
                now,
            );
            assert!(!v.deliver);
            write_chats_message(dir, "msg-7", "k");
            write_chats_message(dir, "msg-8", "k");
            let v = gate(SID, Some("`@k · msg-7 · control note`\ncontrol: stop"), now);
            assert!(v.deliver && v.pass == Some("control"));
            let v = gate(SID, Some("`@k · msg-9 · control note`\ncontrol: stop"), now);
            assert!(!v.deliver, "an unbacked header is not a control pass");
            let v = gate(SID, Some("`@j · msg-7 · control note`\ncontrol: stop"), now);
            assert!(!v.deliver, "a mismatched sender is not a control pass");
            let v = gate(SID, Some("`@Hazel · msg-7 · note`\nhello"), now);
            assert!(
                !v.deliver,
                "a multibyte char at byte 9 is prose, not a panic"
            );
            let v = gate(
                SID,
                Some("`@k · msg-8 · hello`\nbody prose\ncontrol: stop"),
                now,
            );
            assert!(!v.deliver);
        });
    }

    /// The argv-logging stub: appends its argv to `$PARK_LOG` and exits 1
    /// when `$PARK_FAIL` is set, else 0. Backs the parked-payload drain
    /// (as the `FNO_BIN` runner) and the release-timer seam (as
    /// `FNO_AGENTS_TIMER_EXE`).
    fn write_stub_py(dir: &std::path::Path) -> std::path::PathBuf {
        crate::write_exec_stub(
            dir,
            "stub-fno.sh",
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$PARK_LOG\"\n[ -n \"$PARK_FAIL\" ] && exit 1\nexit 0\n",
        )
    }

    fn parked_dir_for(dir: &std::path::Path) -> std::path::PathBuf {
        crate::state_layout::place_in(dir, "mail-hold", &format!("{}.parked", identity_key(SID)))
    }

    #[test]
    fn gate_park_on_hold_parks_the_body_and_answers_parked() {
        with_hold_env(|dir| {
            write_registry(
                dir,
                serde_json::json!([registry_row("worker", SID), mail_only_row("desk", OTHER)]),
            );
            plant(SID, 0, 600, SetBy::User);
            // A live runner is on duty, so the park spawns nothing.
            let pdir = parked_dir_for(dir);
            std::fs::create_dir_all(&pdir).unwrap();
            std::fs::write(pdir.join("runner.pid"), std::process::id().to_string()).unwrap();

            let verdict = gate(SID, Some("/compact"), chrono::Utc::now());
            let line = gate_parked_line(&verdict, SID, "/compact").expect("held body parks");
            let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(parsed["verdict"], "parked");
            let receipt = parsed["receipt"].as_str().unwrap();
            assert!(
                receipt.contains("runs on worker when the hold ends"),
                "{receipt}"
            );
            let control = gate(SID, Some("control: stop"), chrono::Utc::now());
            assert!(gate_parked_line(&control, SID, "control: stop").is_some());
            assert!(gate_parked_line(&verdict, SID, "   ").is_none());
            let files: usize = std::fs::read_dir(&pdir)
                .unwrap()
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "txt"))
                .count();
            assert_eq!(files, 2, "the body and the control body parked");
            // A mail-only row has no end, so a raw body never parks there.
            let mail_only = gate(OTHER, Some("/compact"), chrono::Utc::now());
            assert!(mail_only.until.is_none());
            assert!(gate_parked_line(&mail_only, OTHER, "/compact").is_none());
        });
    }

    #[test]
    fn the_runner_sends_nothing_while_held_then_drains_once_and_deletes() {
        with_hold_env(|dir| {
            let _g = env_guard(&["PARK_LOG"]);
            write_registry(dir, serde_json::json!([registry_row("worker", SID)]));
            plant(SID, 0, 600, SetBy::User);
            let stub = write_stub_py(dir);
            let log = dir.join("drain.log");
            std::env::set_var("FNO_BIN", stub.to_string_lossy().to_string());
            std::env::set_var("PARK_LOG", log.to_string_lossy().to_string());
            let pdir = parked_dir_for(dir);
            std::fs::create_dir_all(&pdir).unwrap();
            std::fs::write(pdir.join("1-test.txt"), "/compact").unwrap();

            assert!(matches!(drain_parked_once(SID), DrainPoll::Waiting));
            let logged = std::fs::read_to_string(&log).unwrap_or_default();
            assert!(
                logged.is_empty(),
                "a held poll sends nothing, logged {logged:?}"
            );

            run_mail_hold(&["--session".into(), SID.into(), "--off".into()]);
            assert!(matches!(drain_parked_once(SID), DrainPoll::Sent));
            let logged = std::fs::read_to_string(&log).unwrap_or_default();
            assert!(
                logged.contains("agents mail send")
                    && logged.contains(SID)
                    && logged.contains("/compact"),
                "the replay went through the raw door: {logged:?}"
            );
            assert!(!pdir.join("1-test.txt").exists(), "the sent file is gone");
        });
    }

    /// The sweep picks only holds ended past the margin, folds a live
    /// retired clock file into a record, and drops one with no end.
    #[test]
    fn the_sweep_picks_ended_holds_and_folds_retired_clock_files() {
        with_hold_env(|_dir| {
            let now = chrono::Utc::now();
            let a = "aaaaaaaa-1111-2222-3333-444455556666";
            let b = "bbbbbbbb-1111-2222-3333-444455556666";
            let c = "cccccccc-1111-2222-3333-444455556666";
            plant(a, 690, 600, SetBy::User);
            plant(b, 630, 600, SetBy::User);
            plant(c, 0, 600, SetBy::User);
            let store = hold_store();
            assert_eq!(lapsed_hold_sessions(&store, now), vec![a.to_string()]);

            let until = (now + chrono::Duration::seconds(300)).format("%Y-%m-%dT%H:%M:%SZ");
            let live = store.dir().join(format!("{OTHER}.json"));
            std::fs::write(
                &live,
                format!("{{\"until\": \"{until}\", \"window_s\": 300, \"clock_kind\": \"wall\", \"ceiling\": null, \"source\": \"conversation\"}}\n"),
            )
            .unwrap();
            let forever = store
                .dir()
                .join("eeeeeeee-1111-2222-3333-444455556666.json");
            std::fs::write(
                &forever,
                "{\"until\": null, \"window_s\": 0, \"clock_kind\": \"permanent\", \"ceiling\": null}\n",
            )
            .unwrap();
            fold_legacy_clock(&store, &live, now);
            fold_legacy_clock(&store, &forever, now);
            let folded = stored(OTHER).expect("a live retired clock folds into a record");
            assert_eq!(folded.set_by(), SetBy::Conversation);
            assert_eq!(
                folded.until().timestamp(),
                (now + chrono::Duration::seconds(300)).timestamp()
            );
            assert!(!forever.exists(), "a clock with no end is dropped");
        });
    }

    #[test]
    fn orphan_sweep_picks_old_non_operator_stamps() {
        with_hold_env(|dir| {
            let now = chrono::Utc::now();
            let old = (now - chrono::Duration::seconds(orphan_stamp_age_s() + 60))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string();
            let young = (now - chrono::Duration::seconds(60))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string();
            let sid = |n: u8| format!("{n:08}-1111-2222-3333-444455556666");
            let row = |name: &str, n: u8, origin: Option<&str>, born: &str| {
                let mut r = registry_row(name, &sid(n));
                if let Some(fields) = r.as_object_mut() {
                    fields.insert(
                        "delivery_policy".into(),
                        serde_json::Value::String(MAIL_ONLY.into()),
                    );
                }
                r["created_at"] = serde_json::Value::String(born.into());
                if let Some(o) = origin {
                    r["origin"] = serde_json::Value::String(o.into());
                }
                r
            };
            write_registry(
                dir,
                serde_json::json!([
                    row("orphan", 1, Some("spawn"), &old),
                    row("young", 2, Some("spawn"), &young),
                    row("registered", 3, Some("operator"), &old),
                    row("unmarked", 4, None, &old),
                    row("adopted", 6, Some("adopted"), &old),
                    row("unborn", 8, None, ""),
                ]),
            );
            let registry = crate::state::load_registry(&dir.join("registry.json")).unwrap();
            assert_eq!(
                orphan_stamp_rows(&registry, now),
                vec![
                    ("orphan".to_string(), sid(1)),
                    ("unmarked".to_string(), sid(4)),
                    ("adopted".to_string(), sid(6)),
                    ("unborn".to_string(), sid(8)),
                ],
                "an old or undated stamp is retired unless an operator set it"
            );
        });
    }
}
