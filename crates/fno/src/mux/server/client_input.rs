use super::*;

pub(crate) struct PaneInputRequest {
    pub id: u64,
    pub request_id: u64,
    pub pane: u64,
    pub expected_identity: String,
    pub bytes: Vec<u8>,
    pub agents: Result<Vec<RegistryAgent>, &'static str>,
}

impl Core {
    pub(super) fn watch_input(&mut self, pane: u64, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Some(entry) = self.panes.get(&pane) {
            self.input_echo
                .entry(pane)
                .or_default()
                .sent(Instant::now(), entry.last_output);
        }
    }

    pub(super) fn poll_input_echo(&mut self) {
        let now = Instant::now();
        let panes = &self.panes;
        let agents = &self.agents;
        let portals = &self.portals;
        let session = &self.session_name;
        self.input_echo.retain(|pane, watch| {
            let Some(entry) = panes.get(pane) else {
                return false;
            };
            if watch.poll(now, entry.last_output) {
                let subject = super::input_echo::no_echo_subject(
                    portals,
                    agents,
                    session,
                    *pane,
                    entry.pty.child_pid(),
                );
                crate::mux_log!(warn; "fno mux: input-no-echo pane={pane} {subject}");
            }
            true
        });
    }

    pub(super) fn handle_input(&mut self, id: u64, bytes: Vec<u8>) {
        let focus = self.links.pane(id).or_else(|| {
            self.client_view(id)
                .and_then(|view| self.viewed_tab(view))
                .map(|tab| tab.focus)
        });
        if let Some(focus) = focus {
            // The hold gate for cross-machine sends: a remote viewer typing
            // into a held pane is refused like a local `pane send`, while
            // the person at the machine keeps typing. The refusal rides a
            // notice the far door paints (mux_link), so the sender sees it.
            let remote = self.links.is_remote(id);
            if remote && agent_rows_join::pane_is_dnd(&self.agents, &self.session_name, focus) {
                self.notice(id, composer_guard::HOLD_REFUSAL_TEXT);
                return;
            }
            // The revive key on a parked no-signal seat. The parked screen
            // reads no input, so the key is the server's to take.
            if bytes == b"r" && self.request_seat_revive(id, focus) {
                return;
            }
            let now = Instant::now();
            if let Some(&holder) = self.claims.get(&focus) {
                if pid_alive(holder) {
                    // A live relay holds the pane mid-write: buffer the
                    // burst (bounded) and replay it in order on release
                    // instead of dropping it.
                    if !self.composer.buffer_user(
                        focus,
                        &bytes,
                        composer_guard::BufferReason::ClaimHold,
                        Instant::now(),
                    ) {
                        self.notice(id, "composer buffer full; input dropped");
                    } else {
                        self.notice(
                            id,
                            "busy: relay; keystrokes held for replay when the holder releases",
                        );
                    }
                    return;
                }
                self.claims.remove(&focus);
            }
            // A live delivery (a mail envelope mid-burst, its CR pending)
            // holds user bytes for replay after the window: a typed message
            // must not fuse into the envelope.
            if self.composer.delivery_hold(focus, now) || self.composer.holds_pending(focus) {
                if !self.composer.buffer_user(
                    focus,
                    &bytes,
                    composer_guard::BufferReason::DeliveryHold,
                    now,
                ) {
                    self.notice(id, "composer buffer full; input dropped");
                    return;
                }
                self.input_tail(focus, &bytes);
                return;
            }
            let scrolled = self.panes.get_mut(&focus).is_some_and(|entry| {
                if entry.vt.display_offset() == 0 {
                    false
                } else {
                    entry.vt.scroll_to_bottom();
                    true
                }
            });
            if scrolled {
                self.broadcast_pane(focus);
            }
            // A focus whose spawn is still in flight (a fresh-squad attach's
            // placeholder leaf) buffers the bytes for replay at registration
            // instead of dropping them on the missing pane entry.
            if self.write_user_burst(focus, &bytes, Some(id)) {
                self.composer
                    .note_user_input(focus, now, super::human_input::is_submit(&bytes));
            }
            self.input_tail(focus, &bytes);
        }
    }

    pub(super) fn handle_pane_input(&mut self, request: PaneInputRequest) {
        let result = self.pane_input(
            request.id,
            request.pane,
            &request.expected_identity,
            &request.bytes,
            request.agents,
        );
        let failed = self
            .clients
            .iter()
            .find(|client| client.id == request.id)
            .is_some_and(|client| {
                let failed = client
                    .reliable_tx
                    .try_send(ServerMsg::PaneInputResult(
                        crate::proto::pane_input::PaneInputResult {
                            request_id: request.request_id,
                            pane_id: request.pane,
                            result,
                        },
                    ))
                    .is_err();
                if !failed {
                    client.notify.notify_one();
                }
                failed
            });
        if failed {
            crate::mux_log!(error;
                "fno mux: client {} reliable channel wedged on PaneInputResult; dropping it",
                request.id
            );
            self.clients.retain(|client| client.id != request.id);
            self.push_layout(true);
        }
    }

    /// Whether `client_id`'s current view holds `pane`: the rule both the
    /// app's PaneInput write and its DraftHint pass before they touch a pane.
    pub(super) fn pane_visible_to(&self, client_id: u64, pane: u64) -> bool {
        self.client_view(client_id)
            .and_then(|view| self.viewed_tab(view))
            .is_some_and(|tab| crate::tree::leaves(&tab.root).contains(&pane))
    }

    fn pane_input(
        &mut self,
        client_id: u64,
        pane: u64,
        expected_identity: &str,
        bytes: &[u8],
        agents: Result<Vec<RegistryAgent>, &'static str>,
    ) -> Result<(), String> {
        if !self.pane_visible_to(client_id, pane) {
            return Err("pane is no longer in the current view".into());
        }
        if let Some(&holder) = self.claims.get(&pane) {
            if pid_alive(holder) {
                return Err("busy: relay".into());
            }
            self.claims.remove(&pane);
        }
        // This is direct user input, so it bypasses DND like keyboard input;
        // the fresh identity check and relay claim still gate the write.
        let now = Instant::now();
        if self.composer.delivery_hold(pane, now) || self.composer.holds_pending(pane) {
            // A delivery's settle window is open: the composer's burst
            // buffers and replays after the envelope's CR, so a typed
            // message never fuses into a landing mail turn.
            if !self.composer.buffer_user(
                pane,
                bytes,
                composer_guard::BufferReason::DeliveryHold,
                now,
            ) {
                return Err("composer buffer full; retry the send".into());
            }
            self.input_tail(pane, bytes);
            return Ok(());
        }
        match self.pane_send(pane, bytes, false, Some(expected_identity), agents, true) {
            ServerMsg::Ok => {
                self.composer
                    .note_user_input(pane, now, super::human_input::is_submit(bytes));
                let scrolled = self.panes.get_mut(&pane).is_some_and(|entry| {
                    if entry.vt.display_offset() == 0 {
                        false
                    } else {
                        entry.vt.scroll_to_bottom();
                        true
                    }
                });
                if scrolled {
                    self.broadcast_pane(pane);
                }
                self.input_tail(pane, bytes);
                self.watch_input(pane, bytes);
                Ok(())
            }
            ServerMsg::Err { msg, .. } => Err(msg),
            _ => Err("pane input failed".into()),
        }
    }
}
