//! The keeper adapter: a keeper-hosted thread takes `Input` frames on its keeper socket. A new line rides as LF, the non-submitting key, and the submit is a separate CR.

use super::*;

/// The keeper lane's raw-byte transport: one `Input` frame per write, through
/// the keeper binary's own frame codec. `send_line` is VERBATIM by the
/// ControlTransport contract, so `inject_with_submit`'s one-line + separate
/// wire CR sequence rides unchanged - the same keystroke discipline the pane lane
/// types, just framed for a keeper instead of a mux pane.
struct KeeperTransport {
    stream: std::os::unix::net::UnixStream,
}

impl crate::claude_attach::ControlTransport for KeeperTransport {
    fn send_line(&mut self, line: &str) -> io::Result<()> {
        use std::io::Write;
        self.stream.write_all(&crate::pane_keeper::encode(
            &crate::pane_keeper::Frame::Input(line.as_bytes().to_vec()),
        ))?;
        self.stream.flush()
    }
    fn recv_line(&mut self) -> io::Result<Option<String>> {
        // The keeper never sends the inject frames a line-oriented reply; the
        // confirm loop polls the transcript, not the socket.
        Ok(None)
    }
}

/// What the keeper lane resolved for one recipient: the keeper socket off the
/// registry row, the hosted harness (whose row owns the settle delay and the
/// confirm target), and the row's cwd (pi's session store is cwd-scoped).
struct KeeperTarget {
    sock: PathBuf,
    hosted_harness: String,
    cwd: PathBuf,
}

/// Resolve a keeper-hosted lane-B thread by its harness session id: the
/// registry row carrying that id AND a keeper socket (`messaging_socket_path`
/// under `mux/threads/`). The row is the only thing that binds a session id to
/// its keeper - the id alone addresses nothing on this lane. The SINGLE
/// resolution path for both the send and any future probe, mirroring
/// `resolve_target`'s one-implementation discipline on the claude lane.
fn resolve_keeper_target_in(
    home: &crate::paths::AgentsHome,
    session: &str,
) -> Result<KeeperTarget, &'static str> {
    let registry =
        crate::state::load_registry(&home.registry_json()).map_err(|_| "registry-unreadable")?;
    let entry = registry
        .entries
        .iter()
        .find(|e| {
            e.harness_session_id.as_deref() == Some(session)
                && e.messaging_socket_path
                    .as_deref()
                    .is_some_and(|p| p.contains("mux/threads/"))
        })
        .ok_or(NOT_INJECTABLE)?;
    Ok(KeeperTarget {
        sock: PathBuf::from(entry.messaging_socket_path.clone().expect("checked above")),
        hosted_harness: entry.harness_name().to_string(),
        cwd: PathBuf::from(&entry.cwd),
    })
}

/// Deliver `text` to a keeper-hosted lane-B thread: resolve the row,
/// connect to its keeper socket, type the envelope (LF line breaks) in
/// an `Input` frame, settle the hosted harness's own delay, then
/// send the wire-level CR - and confirm by CONTENT in the hosted harness's
/// accepted-turn records, withdrawing on total silence like the claude lane
/// (both loops are the SHARED `inject_with_submit` / `confirm_or_withdraw`
/// pair; only the transport and the confirm target differ). A hosted harness
/// with no local record (cursor-agent) types and stays unconfirmed:
/// pty paint is typing progress, never delivery.
pub fn deliver_via_keeper_socket(
    session: &str,
    text: &str,
    attempts: u32,
    interval_ms: u64,
    enter_delay_ms: u64,
) -> Result<(), &'static str> {
    legacy_outcome(deliver_via_keeper_socket_in(
        &crate::paths::AgentsHome::from_env(),
        None,
        &crate::grok_store::grok_sessions_root(),
        session,
        text,
        attempts,
        interval_ms,
        enter_delay_ms,
    ))
}

/// Deliver `text` to a keeper-hosted lane-B thread against an explicit
/// agents home, pi session store, and grok sessions root: the seam the
/// keeper journey test drives, so the fixtures resolve rows and confirm
/// targets exactly as the command does. `pi_store` of `None` resolves the
/// store for the row's cwd exactly as production does, honoring
/// `PI_CODING_AGENT_DIR` and the session-dir settings; a test injects a
/// scratch store through `Some`.
pub fn deliver_via_keeper_socket_in(
    home: &crate::paths::AgentsHome,
    pi_store: Option<&crate::pi::PiStore>,
    grok_root: &Path,
    session: &str,
    text: &str,
    attempts: u32,
    interval_ms: u64,
    enter_delay_ms: u64,
) -> Result<DeliveryState, &'static str> {
    let target = resolve_keeper_target_in(home, session)?;
    let stream =
        std::os::unix::net::UnixStream::connect(&target.sock).map_err(|_| "no-keeper-listener")?;
    let store_fallback;
    let store: Option<&crate::pi::PiStore> = match pi_store {
        Some(s) => Some(s),
        None => match crate::pi::pi_store(&target.cwd) {
            Ok(s) => {
                store_fallback = s;
                Some(&store_fallback)
            }
            // The store cannot be resolved for this cwd, so no confirm is
            // possible; the typed refusal names it and nothing gets typed.
            Err(_) => None,
        },
    };
    let confirm = resolve_confirm(
        &target.hosted_harness,
        session,
        &target.cwd,
        store,
        grok_root,
    );
    if let LaneConfirm::Refused(reason) = confirm {
        // Connected but never typed into: closing without a keystroke is the
        // honest outcome, and the reason names why nothing was pasted.
        return Err(reason);
    }
    let mut transport = KeeperTransport { stream };
    let typed_since = now_ms();
    // The typed turn's opening line (unchanged by the one-line flattening)
    // is the content marker the confirm greps for: recorded verbatim once
    // the turn is accepted, and matched as the FULL line - never a truncated
    // prefix, which sibling messages can share.
    let marker = typed_marker(text);
    inject_with_submit(&mut transport, text, Duration::from_millis(enter_delay_ms)).map_err(
        |e| match e {
            DriveError::UnsafeText => "unsafe-text",
            _ => "io-error",
        },
    )?;
    // The subscriber seat must keep draining: the keeper's pty-reader thread
    // relays every Output chunk to this connection with a BLOCKING write
    // under the client lock, so an unread socket backpressures the keeper
    // into the hosted TUI and freezes it for the rest of the budget. Every
    // poll drains and DISCARDS - paint is never read as evidence;
    // the only reader here is the buffer's, not the matcher's.
    let confirm_stream = transport.stream.try_clone().ok();
    if let Some(cs) = confirm_stream.as_ref() {
        let _ = cs.set_read_timeout(Some(Duration::from_millis(50)));
    }
    // The keeper `Unconfirmable` arm (cursor-agent) keeps today's shape:
    // it types and never withdraws, because no landing can be seen there.
    let withdraw_on_silence = !matches!(confirm, LaneConfirm::Unconfirmable);
    let running = running_turn_of(&confirm);
    let land = move |path: &Path, baseline: u64| {
        landing_after(path, &marker, baseline, running.as_deref()).unwrap_or(None)
    };
    let confirmed = move || -> Option<DeliveryState> {
        if let Some(stream) = confirm_stream.as_ref() {
            let mut sink = [0u8; 8192];
            let mut reader = stream;
            loop {
                match std::io::Read::read(&mut reader, &mut sink) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        }
        match &confirm {
            LaneConfirm::Transcript { path, baseline } => land(path, *baseline),
            LaneConfirm::PendingStore { harness } => match harness.as_str() {
                "pi" => match store {
                    Some(s) => match crate::pi::lookup_sessions_in(s, &target.cwd, session) {
                        crate::pi::SessionLookup::One { file } => land(&file, 0),
                        _ => None,
                    },
                    None => None,
                },
                "claude" => match crate::claude_drive::find_transcript_in(
                    &crate::claude_drive::claude_projects_dir(),
                    session,
                ) {
                    Some(file) => land(&file, 0),
                    None => None,
                },
                "codex" => match crate::codex_store::codex_rollout_path(None, session) {
                    Some(file) => land(&file, 0),
                    None => None,
                },
                "agy" => {
                    let (_, history, _) = crate::agy_transcript::agy_stores();
                    if history.is_file() {
                        land(&history, 0)
                    } else {
                        None
                    }
                }
                "footnote" => match crate::footnote_harness::transcript::find_session_dir(
                    &crate::footnote_harness::transcript::sessions_root(),
                    session,
                ) {
                    Some(dir) => land(
                        &crate::footnote_harness::transcript::transcript_file(&dir, session),
                        0,
                    ),
                    None => None,
                },
                _ => match crate::grok_store::lookup_session(grok_root, session) {
                    crate::pi::SessionLookup::One { file } => land(&file, 0),
                    _ => None,
                },
            },
            // No local accepted-turn record exists to grep, so nothing on
            // this lane ever confirms. The budget still runs its
            // full course: the CR resubmits inside it are send retries for a
            // busy recipient, not confirm polls, and the honest outcome is
            // `injected`: the bytes sit at the prompt and no record says more.
            LaneConfirm::Unconfirmable => None,
            LaneConfirm::Refused(_) => None,
        }
    };
    let journal = AgentsHome::from_env().events_jsonl();
    Ok(confirm_or_withdraw(
        &mut transport,
        attempts,
        Duration::from_millis(interval_ms),
        one_line(text).chars().count(),
        typed_since,
        &journal,
        session,
        withdraw_on_silence,
        confirmed,
    ))
}
