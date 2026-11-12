//! The claude control.sock adapter: the one place fno opens a claude session's control socket, attaches, and types into it. `scripts/ci/check-control-sock-writers.sh` fails a change that opens the socket anywhere else.
//!
//! Post-attach the socket is a raw keystroke pipe, so a turn is typed as keystrokes and submitted with a separate wire-level CR. Any new line parks the turn in the harness queue until a turn boundary, so the mail lane types a paneless session's turn as one line (`NewlineRule::OneLine`).

use super::*;
use crate::claude_attach::{perform_attach, AttachRequest, UnixControlTransport};
use crate::claude_roster::{read_control_key_in, ClaudeRoster};

/// Resolve everything an inject needs before it can write a byte: the recipient's
/// roster entry, its `control.sock`, and its transcript (the confirm target).
/// Returns `(control_sock, short_id, transcript)`.
///
/// The SINGLE resolution path. Both [`deliver_via_control_sock`] and `--probe`
/// call it, so a probe cannot disagree with the send it predicts -- a second
/// implementation of these four steps would drift the moment resolution changes,
/// and a probe that says yes where the send says no is worse than no probe.
fn resolve_target_in(
    session: &str,
    daemon_dir: &Path,
    projects_base: &Path,
) -> Result<(PathBuf, String, PathBuf), &'static str> {
    let roster = ClaudeRoster::load(&daemon_dir.join("roster.json")).map_err(|_| NOT_INJECTABLE)?;
    let worker = roster.find(session).ok_or(NOT_INJECTABLE)?;
    let sock = worker.resolve_control_sock().ok_or(NOT_INJECTABLE)?;
    // No transcript yet == we cannot confirm landing, so there is no usable path
    // even though the socket resolved.
    let transcript =
        find_transcript_in(projects_base, &worker.session_id).ok_or("no-transcript")?;
    Ok((sock, worker.short_id().to_string(), transcript))
}

pub(crate) fn resolve_target(session: &str) -> Result<(PathBuf, String, PathBuf), &'static str> {
    resolve_target_in(
        session,
        &crate::claude_roster::daemon_dir(),
        &crate::claude_drive::claude_projects_dir(),
    )
}

/// Deliver `text` to `session` over the daemon `control.sock`: resolve the
/// recipient on the roster, attach, type the envelope (LF line breaks) + wire-level CR submit, and
/// confirm by CONTENT that the injected turn landed in the recipient transcript.
/// `Ok(())` == delivered (the `<fno_mail>` marker appeared after the inject);
/// `Err(reason)` is a clean not-delivered signal whose value IS the `mail-inject`
/// JSON `reason` token.
///
/// The SINGLE control.sock wire implementation (Locked Decision 1, node
///): both the `mail-inject` command (`fno agents mail send`) and the Rust ask-lane
/// fallback (`claude_ask::ask_followup`) deliver through here, so the wire
/// contract lives in one place and can never drift. `text` is injected verbatim
/// -- a dumb transport; callers wrap it in the `<fno_mail>` /
/// `<cross-session-message>` envelope first.
pub fn deliver_via_control_sock(
    session: &str,
    text: &str,
    attempts: u32,
    interval_ms: u64,
    enter_delay_ms: u64,
) -> Result<(), &'static str> {
    legacy_outcome(deliver_via_control_sock_state(
        &crate::claude_roster::daemon_dir(),
        &crate::claude_drive::claude_projects_dir(),
        session,
        text,
        attempts,
        interval_ms,
        enter_delay_ms,
        false,
    ))
}

/// Deliver through an explicitly selected Claude account root. The roster,
/// control key and transcript all come from that same root, so an isolated
/// account can never attach to or confirm against the ambient account.
pub fn deliver_via_control_sock_in(
    daemon_dir: &Path,
    projects_base: &Path,
    session: &str,
    text: &str,
    attempts: u32,
    interval_ms: u64,
    enter_delay_ms: u64,
) -> Result<(), &'static str> {
    legacy_outcome(deliver_via_control_sock_state(
        daemon_dir,
        projects_base,
        session,
        text,
        attempts,
        interval_ms,
        enter_delay_ms,
        false,
    ))
}

/// Type one permission-response key over the attach transport: the wire form
/// of `claude attach` + pressing the picker key. Post-attach the
/// `control.sock` is a raw keystroke pipe, so the key plus a separate CR
/// answers a showing prompt without a TTY. No transcript confirm: the picker
/// closes on the key and prints nothing a message confirm could grep for.
pub fn answer_permission_via_control_sock_in(
    daemon_dir: &Path,
    session: &str,
    key: &str,
    enter_delay_ms: u64,
) -> Result<(), &'static str> {
    if key.is_empty() || key.chars().any(|c| c.is_control()) {
        return Err("unsafe-text");
    }
    let roster = ClaudeRoster::load(&daemon_dir.join("roster.json")).map_err(|_| NOT_INJECTABLE)?;
    let worker = roster.find(session).ok_or(NOT_INJECTABLE)?;
    let sock = worker.resolve_control_sock().ok_or(NOT_INJECTABLE)?;
    let auth = read_control_key_in(daemon_dir);
    let mut transport = UnixControlTransport::connect(&sock).map_err(|_| "io-error")?;
    if perform_attach(
        &mut transport,
        &AttachRequest::for_frame_stream(worker.short_id().to_string(), auth.clone()),
    )
    .is_err()
    {
        return Err("attach-failed");
    }
    transport.send_line(key).map_err(|_| "io-error")?;
    std::thread::sleep(Duration::from_millis(enter_delay_ms));
    transport.send_line("\r").map_err(|_| "io-error")
}

/// The control.sock lane with the state it reached. `Err` is a refusal
/// before any byte was typed. `steer` types the answer as a TRUE
/// single line, which a mid-turn claude absorbs within seconds; the default
/// keeps the LF-normalized multi-line form, which the harness queue parks
/// until a turn boundary.
pub fn deliver_via_control_sock_state(
    daemon_dir: &Path,
    projects_base: &Path,
    session: &str,
    text: &str,
    attempts: u32,
    interval_ms: u64,
    enter_delay_ms: u64,
    steer: bool,
) -> Result<DeliveryState, &'static str> {
    let (sock, short, transcript) = resolve_target_in(session, daemon_dir, projects_base)?;
    let auth = read_control_key_in(daemon_dir);

    let mut transport = UnixControlTransport::connect(&sock).map_err(|_| "io-error")?;
    if perform_attach(
        &mut transport,
        &AttachRequest::for_frame_stream(short.clone(), auth.clone()),
    )
    .is_err()
    {
        return Err("attach-failed");
    }
    let typed_text = if steer {
        steer_line(text)
    } else {
        one_line(text)
    };
    // Baseline the transcript byte-length AFTER attach, immediately before inject,
    // so attach side-effects cannot be mistaken for our turn landing (codex peer
    // P2); the content confirm scans only lines appended past this offset.
    let baseline = transcript_len(&transcript);
    // The withdraw guard's clock: typing the operator does AFTER this instant
    // is theirs, so the line is never deleted out from under them.
    let typed_since = now_ms();
    // The typed turn's opening line -- its `<fno_mail>` open tag, unchanged by
    // the one-line flattening -- is the content marker the confirm greps for;
    // it is recorded verbatim once the turn submits.
    let marker = typed_marker(&typed_text);
    inject_with_submit(
        &mut transport,
        &typed_text,
        Duration::from_millis(enter_delay_ms),
    )
    .map_err(|e| match e {
        DriveError::UnsafeText => "unsafe-text",
        _ => "io-error",
    })?;

    let journal = AgentsHome::from_env().events_jsonl();
    Ok(confirm_or_withdraw(
        &mut transport,
        attempts,
        Duration::from_millis(interval_ms),
        typed_text.chars().count(),
        typed_since,
        &journal,
        session,
        true,
        || landing_after(&transcript, &marker, baseline, None).unwrap_or(None),
    ))
}

/// How long one screen read waits for the redraw to settle.
const READ_WINDOW: Duration = Duration::from_millis(1500);
/// A redraw is a few KB; this bounds a chatty worker.
const MAX_BYTES: usize = 256 * 1024;

/// Attach, feed the redraw through the terminal grid, return the visible text.
pub(crate) fn read_screen(
    socket: &Path,
    short: &str,
    auth: Option<String>,
) -> Result<String, String> {
    let stream =
        std::os::unix::net::UnixStream::connect(socket).map_err(|e| format!("connect: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(400)))
        .map_err(|e| e.to_string())?;
    let writer = stream.try_clone().map_err(|e| e.to_string())?;
    let (_, mut frames) = crate::claude_attach::attach_for_frames(
        writer,
        stream,
        &AttachRequest::for_frame_stream(short, auth),
    )
    .map_err(|e| e.to_string())?;
    let mut grid = crate::screen::TerminalGrid::with_default_size();
    let (start, mut total) = (std::time::Instant::now(), 0usize);
    let mut buf = [0u8; 8192];
    while start.elapsed() < READ_WINDOW && total < MAX_BYTES {
        match frames.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                grid.feed(buf.get(..n).unwrap_or_default());
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if total > 0 {
                    break;
                }
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(grid.snapshot().text)
}

/// Whether the worker behind `endpoint` answers the attach handshake: the
/// health read types nothing.
pub(crate) fn attach_answers(endpoint: &Path, short_id: &str) -> bool {
    let Ok(mut transport) = UnixControlTransport::connect(endpoint) else {
        return false;
    };
    perform_attach(
        &mut transport,
        &AttachRequest::for_frame_stream(short_id, crate::claude_roster::read_control_key()),
    )
    .is_ok()
}
