//! The mux pane adapter: each write rides `fno mux pane send --stdin --raw`. A new line rides as LF (ctrl+j), which keeps the line break and absorbs mid-turn; the submit is a separate CR.

use super::*;

/// The pane lane's transport: each write rides `fno mux pane send --stdin
/// --raw`, the same door the writer-claim holder uses, addressed with
/// `--server` so a foreign session pane is reachable, and `--hold-pass` when
/// the hold gate passed this text so a DND pane accepts the delivery
/// (AC17-EDGE) without standing its guard down for anyone else.
struct PaneTransport {
    server: String,
    address: String,
    fno_id: Option<String>,
    hold_pass: bool,
}

impl crate::claude_attach::ControlTransport for PaneTransport {
    fn send_line(&mut self, line: &str) -> io::Result<()> {
        let mut cmd = std::process::Command::new(crate::scrape::fno_bin());
        cmd.args([
            "mux",
            "pane",
            "send",
            &self.address,
            "--stdin",
            "--raw",
            "--server",
            &self.server,
        ]);
        if let Some(id) = &self.fno_id {
            cmd.args(["--fno-id", id]);
        }
        if self.hold_pass {
            cmd.arg("--hold-pass");
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit());
        let mut child = cmd.spawn()?;
        use std::io::Write as _;
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(line.as_bytes())?;
        let status = child.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other("pane send refused the write"))
        }
    }
    fn recv_line(&mut self) -> io::Result<Option<String>> {
        // The confirm loop polls the hosted harness's transcript, never the
        // subprocess exit stream.
        Ok(None)
    }
}

/// The screen gate for a pane no turn record can confirm. A harness that pins a
/// ready marker takes typed bytes only at a painted composer, idle or busy: a
/// busy opencode queues the line as its next turn. A prompt would consume the
/// submit, and bytes typed before first paint are lost. A harness with no pinned
/// marker (a shell, agy) has nothing to read, so it stays typed and left.
pub(super) fn unconfirmable_pane_refusal(
    harness: &str,
    state: Result<String, String>,
) -> Option<&'static str> {
    let pinned = crate::harness_capabilities::HarnessContract::packaged()
        .ok()
        .and_then(|contract| {
            contract
                .capabilities(harness)
                .ok()
                .map(|c| c.ready_marker.clone())
        })
        .is_some_and(|marker| marker != "unsupported");
    if !pinned {
        return None;
    }
    match state.as_deref() {
        Ok("idle" | "working") => None,
        Ok("blocked") => Some("pane-blocked"),
        _ => Some("pane-not-ready"),
    }
}

/// Deliver `text` to a mux pane (C11, C12, C15, C17): the quiet gate, the
/// one-line flattening and the confirm-or-withdraw cadence of the typed
/// lanes, with the pane transport underneath and the HOSTED harness's own
/// accepted-turn record (claude transcript or codex rollout) as the confirm
/// target -- the landing confirm the codex pane never had.
pub(crate) fn deliver_via_pane_state(
    session: &str,
    address: &str,
    hosted_harness: Option<&str>,
    text: &str,
    attempts: u32,
    interval_ms: u64,
    enter_delay_ms: u64,
) -> Result<DeliveryState, &'static str> {
    let (server, pane_id) = match address.split_once(':') {
        Some(pair) => pair,
        None => return Err(NOT_INJECTABLE),
    };
    let row_hosted = hosted_harness
        .map(str::to_string)
        .unwrap_or_else(|| resolve_row_harness(session).unwrap_or_else(|| "claude".to_string()));
    let confirm_cwd = resolve_row_cwd(session);
    let pi_store = crate::pi::pi_store(&confirm_cwd).ok();
    let confirm_target = match resolve_confirm(
        &row_hosted,
        session,
        &confirm_cwd,
        pi_store.as_ref(),
        &grok_root(),
    ) {
        // A pane can host a harness no lane reads a turn record for
        // (opencode, a shell). It still takes typed bytes: `injected`.
        LaneConfirm::Refused("no-confirm-source") => LaneConfirm::Unconfirmable,
        LaneConfirm::Refused(reason) => return Err(reason),
        confirm => confirm,
    };
    // An unconfirmable pane is typed and left: no record could ever clear
    // a withdraw, so a withdraw would delete every message it is sent.
    let withdraw_on_silence = !matches!(confirm_target, LaneConfirm::Unconfirmable);
    if !withdraw_on_silence {
        // With no record to catch a lost line, the screen is the only check.
        let state = pane_id
            .parse::<u64>()
            .map_err(|e| e.to_string())
            .and_then(|pane| {
                crate::mail_cli::pane_prepare::screen_state(server, pane, &row_hosted)
            });
        if let Some(reason) = unconfirmable_pane_refusal(&row_hosted, state) {
            return Err(reason);
        }
    }
    let mut transport = PaneTransport {
        server: server.to_string(),
        address: pane_id.to_string(),
        fno_id: Some(session.to_string()),
        // The gate's pass rides the pane write: a `control:` directive or the
        // pane's own send lands on a held pane (AC17-EDGE); a plain send
        // carries no pass and the pane's DND refusal stands.
        hold_pass: crate::mail_hold::gate(session, Some(text), chrono::Utc::now())
            .pass
            .is_some(),
    };
    let typed_since = now_ms();
    let marker = typed_marker(text);
    inject_with_submit(&mut transport, text, Duration::from_millis(enter_delay_ms)).map_err(
        |e| match e {
            DriveError::UnsafeText => "unsafe-text",
            _ => "io-error",
        },
    )?;
    let journal = AgentsHome::from_env().events_jsonl();
    let running = running_turn_of(&confirm_target);
    Ok(confirm_or_withdraw(
        &mut transport,
        attempts,
        Duration::from_millis(interval_ms),
        one_line(text).chars().count(),
        typed_since,
        &journal,
        session,
        withdraw_on_silence,
        move || match &confirm_target {
            LaneConfirm::Transcript { path, baseline } => {
                landing_after(path, &marker, *baseline, running.as_deref()).unwrap_or(None)
            }
            LaneConfirm::PendingStore { harness } => {
                let file = match harness.as_str() {
                    "claude" => crate::claude_drive::find_transcript_in(
                        &crate::claude_drive::claude_projects_dir(),
                        session,
                    ),
                    "codex" => crate::codex_store::codex_rollout_path(None, session),
                    "footnote" => crate::footnote_harness::transcript::find_session_dir(
                        &crate::footnote_harness::transcript::sessions_root(),
                        session,
                    )
                    .map(|dir| crate::footnote_harness::transcript::transcript_file(&dir, session)),
                    "pi" => {
                        pi_store.as_ref().and_then(|store| {
                            match crate::pi::lookup_sessions_in(store, &confirm_cwd, session) {
                                crate::pi::SessionLookup::One { file } => Some(file),
                                _ => None,
                            }
                        })
                    }
                    _ => None,
                };
                file.and_then(|f| landing_after(&f, &marker, 0, None).ok().flatten())
            }
            LaneConfirm::Unconfirmable => None,
            LaneConfirm::Refused(_) => None,
        },
    ))
}

/// The pane lane's pre-state answer, for callers that only ask whether it landed.
pub(crate) fn deliver_via_pane(
    session: &str,
    address: &str,
    hosted_harness: Option<&str>,
    text: &str,
    attempts: u32,
    interval_ms: u64,
    enter_delay_ms: u64,
) -> Result<(), &'static str> {
    legacy_outcome(deliver_via_pane_state(
        session,
        address,
        hosted_harness,
        text,
        attempts,
        interval_ms,
        enter_delay_ms,
    ))
}
