//! What one stdin chunk does: the modal pointer pre-pass, the prefix scanner's
//! commands, and the Connections keys. Moved out of client.rs (file budget).

use super::*;

pub(crate) enum StdinFlow {
    Continue,
    Detach,
}

pub(super) fn consume_modal_close_gesture(view: &mut View, kind: MouseKind) -> bool {
    if view.modal_release_swallow {
        if matches!(kind, MouseKind::Release(MouseButton::Left)) {
            view.modal_release_swallow = false;
        }
        return true;
    }
    false
}

/// Family-B name and confirmation overlays own every pointer event while open.
/// The pre-pass's esc-chip tap cancels them; outside clicks are swallowed so
/// they cannot dismiss the modal or reach a pane underneath it. The
/// Connections modal joins them. The centered peek is click-through; the dock owns its box.
pub(super) fn modal_mouse(view: &mut View, rep: crate::mouse::MouseReport) -> bool {
    let peek_open = view.peek.is_some();
    if !peek_open && consume_modal_close_gesture(view, rep.kind) {
        return true;
    }
    view.active_overlay_layout()
        .is_some_and(|l| !peek_open || view.dock_holds(&l, rep))
}

/// Route one stdin chunk: the selector consumes keys while open (AC6-FR
/// validates against the CURRENT layout before sending); otherwise the
/// prefix scanner splits it into forwards and commands.
pub(super) async fn handle_stdin(
    view: &mut View,
    scanner: &mut Scanner,
    mouse_carry: &mut Vec<u8>,
    bytes: &[u8],
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<StdinFlow, String> {
    // Mouse pre-pass (US1/US2/US3): pull SGR reports out first, then feed the
    // remaining bytes to the key scanner.
    let (reports, passthrough) = crate::mouse::extract_mouse(mouse_carry, bytes);
    // Reports route first; a left press against a visible region moves the
    // client-local input owner (region_focus). Then the remaining bytes
    // reach the key scanner below.
    if let StdinFlow::Detach = region_focus::mouse_pre_pass(view, scanner, reports, sock_w).await? {
        return Ok(StdinFlow::Detach);
    }
    if passthrough.is_empty() {
        return Ok(StdinFlow::Continue);
    }
    // (AC6-FR): a bare Esc during a seam drag reverts it. The revert is
    // an explicit final command to the drag-start ratio, not a client-side
    // rollback - the server owns the layout, so putting the seam back has to
    // travel the same path that moved it. Matched on a lone 0x1b so an arrow
    // key's escape sequence never reads as a cancel.
    if view.seam_drag.is_some() && passthrough == [0x1b] {
        if let Some(cmd) = view.revert_seam_drag() {
            write_msg(sock_w, &ClientMsg::Command(cmd))
                .await
                .map_err(|e| format!("seam revert send failed: {e}"))?;
        }
        return Ok(StdinFlow::Continue);
    }
    // (AC5-EDGE): a bare Esc during a relocation drag cancels it. No
    // command travels, unlike the seam revert above - nothing was ever applied,
    // so there is nothing to put back. Same lone-0x1b match so an arrow key's
    // escape sequence cannot read as a cancel.
    if view.pane_drag.is_some() && passthrough == [0x1b] {
        view.cancel_pane_drag();
        return Ok(StdinFlow::Continue);
    }
    // a bare Esc during a sideline-border drag reverts the width to where
    // the drag began, mirroring the seam revert. Client-local, so only a Resize
    // travels (and only if the width actually changed) - the layout is the
    // client's own, not the server's.
    if view.sideline_drag.is_some() && passthrough == [0x1b] {
        if view.revert_sideline_drag() {
            let (rows, cols) = view.content_dims();
            write_msg(sock_w, &ClientMsg::Resize { rows, cols })
                .await
                .map_err(|e| format!("sideline revert resize send failed: {e}"))?;
        }
        return Ok(StdinFlow::Continue);
    }
    if passthrough == [0x1b] && view.revert_edge_drag() {
        return Ok(StdinFlow::Continue);
    }
    // A bare Esc during a feed-border drag reverts the width to where
    // the drag began, the sideline revert's mirror.
    if view.feed_drag.is_some()
        && passthrough == [0x1b]
        && feed_view::esc_revert(view, sock_w).await?
    {
        return Ok(StdinFlow::Continue);
    }
    // (AC1-FR) A bare Esc cancels a tab-cell or sideline-row drag too. No
    // command travels; the same lone-0x1b match so an arrow key cannot read as a
    // cancel.
    if view.tab_drag.is_some() && passthrough == [0x1b] {
        view.cancel_tab_drag();
        return Ok(StdinFlow::Continue);
    }
    if view.row_drag.is_some() && passthrough == [0x1b] {
        view.cancel_row_drag();
        return Ok(StdinFlow::Continue);
    }
    // A chunk the mouse pre-pass consumed whole carries no key bytes: the
    // folds must not see an empty read here, or a lone ESC in the owning
    // overlay's carry would flush on the mouse's clock instead of the
    // quiet window's. The deadline's flush enters through flush_lone_esc.
    if passthrough.is_empty() {
        return Ok(StdinFlow::Continue);
    }
    // The overlay chain lives in overlay_keys::route: the one precedence
    // list, plus the quiet-window flush entry (`flush_lone_esc`). `None`
    // here means no overlay owns the keyboard.
    if let Some(flow) = overlay_keys::route(view, scanner, &passthrough, sock_w).await {
        return flow;
    }
    for event in scanner.scan(&passthrough, Instant::now()) {
        match dispatch_event(view, event, sock_w).await? {
            DispatchFlow::Continue => {}
            DispatchFlow::Break => break,
            DispatchFlow::Detach => return Ok(StdinFlow::Detach),
        }
    }
    Ok(StdinFlow::Continue)
}

/// One of three control-flow outcomes of dispatching a prefix event: fall
/// through to the next event, stop consuming this chunk (a chord that opens a
/// typing mode must not leak the chunk's trailing bytes into a pane), or detach.
pub(super) enum DispatchFlow {
    Continue,
    Break,
    Detach,
}

/// Dispatch one resolved prefix [`Event`] to the wire / view state - the single
/// executor the key-scan loop and the which-key modal both call (Locked
/// 3), so a modal-executed chord runs the IDENTICAL path as a directly-typed one
/// (no parallel keymap to drift).
pub(super) async fn dispatch_event(
    view: &mut View,
    event: Event,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<DispatchFlow, String> {
    match event {
        Event::Report => {
            report::open(view);
            return Ok(DispatchFlow::Break);
        }
        Event::Forward(chunk) => {
            // Reliable channel: awaited send, input is NEVER dropped.
            write_msg(sock_w, &ClientMsg::Input(chunk))
                .await
                .map_err(|e| format!("input send failed: {e}"))?;
            view.health.input(Instant::now());
        }
        Event::ShowPaneIds => {
            view.reveal_pane_ids_at(Instant::now());
        }
        Event::Cmd(cmd) => {
            if let Command::SplitDir(_) = cmd {
                if !server_has_splitdir(view.server_proto) {
                    server_skew::offer_server_restart(view, "split panes in a direction");
                    return Ok(DispatchFlow::Continue);
                }
            }
            if matches!(cmd, Command::ResizeDir(d) if view.nudge_edge(d)) {
                return Ok(DispatchFlow::Continue);
            }
            view.note_command_sent(&cmd);
            write_msg(sock_w, &ClientMsg::Command(cmd))
                .await
                .map_err(|e| format!("command send failed: {e}"))?;
        }
        Event::SelectTabIdx(ordinal) => {
            // The event carries the 1-BASED ordinal the operator
            // reads off the tab strip (three-identifier-spaces trap:
            // the ordinal is the SHIFTING space and is deliberately what a
            // number gesture addresses; it is resolved to the stable TabId
            // HERE, once, so a layout push mid-gesture cannot move the
            // target). An out-of-range number answers with a notice naming
            // the miss - a silent BEL reads as a dead keybind - plus the BEL,
            // and never a wire message the server would refuse anyway.
            let squad = view
                .layout
                .squads
                .iter()
                .find(|s| s.id == view.layout.active_squad);
            let id = squad
                .and_then(|s| ordinal.checked_sub(1).and_then(|idx| s.tabs.get(idx)))
                .map(|t| t.id);
            match id {
                Some(id) => {
                    write_msg(sock_w, &ClientMsg::Command(Command::SelectTab(id)))
                        .await
                        .map_err(|e| format!("command send failed: {e}"))?;
                }
                None => {
                    let _ = raw_out(b"\x07");
                    let count = squad.map(|s| s.tabs.len()).unwrap_or(0);
                    view.set_notice(format!("no tab {ordinal} ({count} open)"));
                }
            }
        }
        Event::Detach => return Ok(DispatchFlow::Detach),
        Event::OpenSelector => {
            // The unified rows are never empty - the `+ new workspace`
            // footer is always present - so an empty session opens on it
            // (AC3-EDGE) instead of a BEL. Only the width gate stays.
            // Gate on the CURRENT density's width authority, not the regular
            // width: Slim renders down to a narrower terminal than Regular, and
            // gating on PANEL_W left that rail mouse-clickable but refusing the
            // keyboard - the exact mouse-only trap this feature forbids.
            view.panel_on = true;
            if view.panel_w() == 0 {
                view.panel_on = false;
                let _ = raw_out(b"\x07");
            } else {
                // Row 0 is NOT always actionable: Extended opens on the inert
                // column header, where the cursor would paint nothing and Enter
                // would only ring. Seed from the FOCUSED pane's row (:
                // opening must land where you already are, not row one - the
                // confirmed `ctrl+w goes to the top each time` defect); a pane
                // with no visible row (a scratch pane, a folded section) falls
                // back to the old row-zero anchor, never an invalid index.
                // `selector_anchor` still steps off inert rows in both
                // directions from whatever seed it gets.
                let seed = view
                    .agent_row_index_for_pane(view.layout.focus)
                    .unwrap_or(0);
                view.selector = view.selector_anchor(seed);
                view.sel_esc.clear();
                // Open at the top: a stale offset from a prior session must
                // not hide row 0. Then re-follow the SEEDED cursor -
                // offset 0 can leave it scrolled off-screen, and Enter must
                // act on a visible row.
                view.set_sideline_offset(0);
                view.clamp_sideline_scroll();
            }
        }
        Event::OpenAnswers => {
            // open the needs-me queue. Always opens (even with an
            // empty live leg) so the async event-fold leg can populate it;
            // an ultimately-empty union renders "nothing needs you". The
            // fold merges in when it lands - the overlay never blocks on it.
            view.answers = Some(0);
            view.ans_esc.clear();
            view.needs_gen = view.needs_gen.wrapping_add(1);
            let fresh = view
                .needs_fold_at
                .is_some_and(|t| t.elapsed() < NEEDS_CACHE_TTL);
            if fresh {
                // Re-open within the cache TTL: reuse the last fold instantly
                // (Perspective B - mashing prefix+a never re-shells). MINE and
                // questions share the same cache window - all three legs fold
                // together.
                view.needs_degraded = false;
                view.mine_degraded = false;
                view.questions_degraded = false;
                view.questions_degraded_reason = None;
            } else {
                // Stale/reset: live-only until the refresh lands; the reason
                // clears with the fold, never outlives its fold.
                view.needs_fold = None;
                view.needs_degraded = false;
                view.mine_fold = None;
                view.mine_degraded = false;
                view.questions_fold = None;
                view.questions_degraded = false;
                view.questions_degraded_reason = None;
                view.needs_want = true;
            }
        }
        Event::OpenFeed => feed_view::toggle(view, sock_w).await?,
        Event::OpenReview => backlog_board::pr_review::toggle_split(view, sock_w).await?,
        Event::FocusFeed => feed_view::focus(view, sock_w).await?,
        Event::OpenTeam => view.open_org_modal(),
        Event::ToggleBell => bell::toggle(view),
        Event::OpenMessages => messages_view::open_from_chord(view),
        Event::TogglePanel => {
            view.panel_on = !view.panel_on;
            // Hiding the sideline never strands an open composer (it would
            // eat keys while painting nowhere), and full-screen mode is
            // painted sideline, so it leaves with it.
            if !view.panel_on {
                view.sideline_full = false;
                if view.launcher.is_some() {
                    agent_launcher::close(view);
                }
            }
            // Chrome changed size: report the new content area so rects
            // fill it (the reply Layout redraws everything).
            let (r, c) = view.content_dims();
            write_msg(sock_w, &ClientMsg::Resize { rows: r, cols: c })
                .await
                .map_err(|e| format!("resize send failed: {e}"))?;
        }
        Event::ToggleComposer => {
            sideline::toggle_composer(view, sock_w).await?;
        }
        Event::ToggleFullSideline => {
            let was_full = view.sideline_full;
            sideline::toggle_full(view);
            if was_full {
                // Leaving restores the painted sideline; if entering had
                // shown a hidden one, the server still holds panel-free
                // pane rects, so the content area re-syncs here (a no-op
                // re-layout when nothing changed).
                let (r, c) = view.content_dims();
                write_msg(sock_w, &ClientMsg::Resize { rows: r, cols: c })
                    .await
                    .map_err(|e| format!("resize send failed: {e}"))?;
            }
        }
        Event::CycleDensity => {
            view.cycle_density();
            // The panel width changed with the density, so the content area
            // did too - same accounting as TogglePanel above.
            let (r, c) = view.content_dims();
            write_msg(sock_w, &ClientMsg::Resize { rows: r, cols: c })
                .await
                .map_err(|e| format!("resize send failed: {e}"))?;
        }
        Event::ToggleAgentSort => {
            // Pure local state: re-ordering rows changes no geometry, so unlike
            // the density cycle this needs no resize round trip.
            view.toggle_agent_sort();
        }
        Event::CycleAgentGroup => {
            // Pure local state, like the sort cycle: no geometry, no wire.
            view.agent_group = view.agent_group.next();
            view_store::save_agent_group(view.agent_group);
            view.set_notice(format!("grouping by {}", view.agent_group.word()));
        }
        Event::ToggleGroupByLead => view.toggle_group_by_lead(),
        Event::ToggleCardHead => {
            // Pure local state: the head row is one display row, and the
            // scroll math reads painted_rows() every pass.
            view.show_card_head = !view.show_card_head;
            view_store::save_card_head(view.show_card_head);
        }
        Event::ToggleStatus => {
            view.status_on = !view.status_on;
            // Same accounting as the sideline: the content area grew or
            // shrank by one row.
            let (r, c) = view.content_dims();
            write_msg(sock_w, &ClientMsg::Resize { rows: r, cols: c })
                .await
                .map_err(|e| format!("resize send failed: {e}"))?;
            // Persist here too: prefix+s and the settings toggle write the
            // same key, or the two entry points disagree about what the
            // operator asked for.
            config_set::persist_in_background("mux.status_row", view.status_on);
        }
        Event::CycleSection => {
            // Pure local state, no I/O - usable even when the socket write path
            // is failing (same posture as the click path).
            if let Some(key) = squad_key(&view.layout, view.layout.active_squad) {
                view.cycle_section(key);
            }
        }
        Event::ShowKeys => key_grid::toggle(view),
        Event::OpenBacklogBoard => View::open(view),
        Event::ShareFocusedPane => share_flow::chord(view),
        Event::CycleSidelineView => backlog_board::cycle_sideline_view(view),
        Event::OpenOrgView => backlog_board::toggle_org_view(view),
        Event::OpenSettings => {
            execute_aux_action(view, AuxAction::OpenSettings, sock_w).await?;
        }
        Event::OpenConnections => {
            execute_aux_action(view, AuxAction::OpenConnections, sock_w).await?;
        }
        Event::OpenDecisions => decisions_view::open(view),
        Event::OpenSweepThreads => {
            execute_aux_action(view, AuxAction::OpenSweep, sock_w).await?;
        }
        Event::LiftHold => hold::lift(view).await,
        Event::OpenSidelineMenu => view.open_sideline_menu(Anchor::Center), // the footer `menu` popup
        Event::OpenCreate => view.open_create(), // the `+ new workspace` name input
        Event::OpenAddHost => remote_hosts::open_add(view),
        Event::BlockJump(dir) => {
            write_msg(
                sock_w,
                &ClientMsg::BlockJump {
                    pane: view.layout.focus,
                    dir,
                },
            )
            .await
            .map_err(|e| format!("block-jump send failed: {e}"))?;
        }
        Event::BlockSelect(dir) => {
            write_msg(
                sock_w,
                &ClientMsg::BlockSelect {
                    pane: view.layout.focus,
                    dir,
                },
            )
            .await
            .map_err(|e| format!("block-select send failed: {e}"))?;
        }
        Event::BlockRerun => {
            write_msg(
                sock_w,
                &ClientMsg::BlockRerun {
                    pane: view.layout.focus,
                },
            )
            .await
            .map_err(|e| format!("block-rerun send failed: {e}"))?;
        }
        Event::DispatchNext => {
            write_msg(
                sock_w,
                &ClientMsg::DispatchNext {
                    account: view.active_account.clone(),
                },
            )
            .await
            .map_err(|e| format!("dispatch-next send failed: {e}"))?;
        }
        Event::SearchOpen => {
            // Enter client-local typing mode over the focused pane; keystrokes
            // divert to search_keys on the next read (no message sent yet, no
            // Resize - the input line overlays the bottom chrome). Break so no
            // same-chunk bytes after the chord leak to the pane.
            view.search = Some(SearchView {
                pane: view.layout.focus,
                query: String::new(),
                submitted: false,
                result: None,
            });
            view.search_esc.clear();
            return Ok(DispatchFlow::Break);
        }
        Event::ScrollModeOpen => {
            // Enter keyboard scroll mode on the focused pane. Keystrokes
            // divert to scroll_mode_keys on the next read; the moves ride the
            // shared server-side scroll offset the wheel drives. A server that
            // predates the v118 commands gets the restart popup, never a dead mode.
            if !server_has_pane_scroll(view.server_proto) {
                server_skew::offer_server_restart(view, "scroll a pane from the keyboard");
                return Ok(DispatchFlow::Continue);
            }
            view.scroll_mode = true;
            view.scroll_esc.clear();
            return Ok(DispatchFlow::Break);
        }
        Event::OpenNav => {
            // Client-local overlay: opening sends nothing and
            // reserves no row (it draws over the content top-left like the
            // answer overlay, not the bottom chrome). Break so same-chunk
            // bytes after the chord can't leak to the pane (like SearchOpen).
            // No width gate: draw_lines_overlay clips a tiny terminal, and a
            // zero-squad session shows an explicit `no matches` (AC1-EDGE).
            // Seed from the focused pane so the navigator opens on
            // the row you are already at; the filter is empty at open, so the
            // filtered list IS nav_rows() here. A focused pane with no row (a
            // scratch pane) opens at row zero (AC7-EDGE).
            let seed = view
                .nav_rows()
                .iter()
                .position(|r| overlay_keys::nav_row_targets_pane(r, view.layout.focus))
                .unwrap_or(0);
            view.nav = Some(NavView {
                query: String::new(),
                state_filter: None,
                cursor: seed,
            });
            view.nav_esc.clear();
            return Ok(DispatchFlow::Break);
        }
        Event::FocusNextBlocked => {
            // Client-local jump (prefix+Z): the walk lives with nav_goto in
            // overlay_keys; Continue, since no typing mode opens.
            overlay_keys::focus_next_blocked(view, sock_w)
                .await
                .map_err(|e| format!("jump-to-waiting send failed: {e}"))?;
        }
        Event::OpenRename => {
            // Rename targets the ACTIVE tab, resolved to its stable id at
            // open time so a tab switch mid-edit cannot retarget the send
            // (the server refuses a stale id fail-closed - AC1-FR).
            let tab = view
                .layout
                .squads
                .iter()
                .find(|s| s.id == view.layout.active_squad)
                .and_then(|s| s.tabs.get(s.active_tab))
                .map(|t| t.id);
            match tab {
                Some(id) => {
                    view.open_rename(RenameTarget::Tab(id));
                    // Swallow same-chunk bytes after the chord, like
                    // SearchOpen: nothing may leak into the pane.
                    return Ok(DispatchFlow::Break);
                }
                None => {
                    let _ = raw_out(b"\x07");
                }
            }
        }
        Event::OpenMoveTo => {
            // Move-to targets the ACTIVE tab, resolved to its stable
            // id at open so a tab switch mid-edit cannot retarget the send -
            // the OpenRename discipline. Same-chunk bytes after the chord are
            // swallowed, like every prompt-opening chord.
            let tab = view
                .layout
                .squads
                .iter()
                .find(|s| s.id == view.layout.active_squad)
                .and_then(|s| s.tabs.get(s.active_tab))
                .map(|t| t.id);
            match tab {
                Some(id) => {
                    view.open_move_to(id);
                    return Ok(DispatchFlow::Break);
                }
                None => {
                    let _ = raw_out(b"\x07");
                }
            }
        }
        Event::ReorderTab(delta) => {
            let target = view
                .layout
                .squads
                .iter()
                .find(|s| s.id == view.layout.active_squad)
                .and_then(|s| s.tabs.get(s.active_tab).map(|tab| (s.id, tab.id)));
            match target {
                Some((squad, tab)) => {
                    write_msg(
                        sock_w,
                        &ClientMsg::Command(Command::ReorderTab { squad, tab, delta }),
                    )
                    .await
                    .map_err(|e| format!("command send failed: {e}"))?;
                }
                None => {
                    let _ = raw_out(b"\x07");
                }
            }
            return Ok(DispatchFlow::Break);
        }
        Event::Bell => {
            let _ = raw_out(b"\x07");
        }
    }
    Ok(DispatchFlow::Continue)
}

/// Open (or move) the peek overlay to `cursor` and fetch its transcript: bumps
/// the seq, resets the body to loading, and sends the matching `PeekAgent`. The
/// caller guarantees `cursor` is a `DisplayRow::Agent`. Shared by Space-open,
/// j/k, and the layout re-anchor so the seq/loading discipline is identical on
/// every path.
pub(super) async fn fetch_peek(
    view: &mut View,
    cursor: usize,
    name: String,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    let seq = view.open_peek(cursor, name.clone());
    write_msg(
        sock_w,
        &ClientMsg::Command(Command::PeekAgent { name, seq }),
    )
    .await
    .map_err(|e| format!("peek send failed: {e}"))
}

/// Route keys to the Connections modal. Pure state changes run through
/// the modal's own reducer ([`ConnectionsView::on_key`]); the intents that touch
/// the world (close, refresh) are executed here. The run loop redraws after this
/// returns `Continue`, so a `Redraw` intent needs no explicit paint.
pub(super) async fn connections_keys(
    view: &mut View,
    bytes: &[u8],
    _sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<StdinFlow, String> {
    use crate::connections_view::ConnIntent;
    let mut esc = std::mem::take(&mut view.conn_esc);
    let text = view
        .connections
        .as_ref()
        .is_some_and(|cv| cv.captures_text());
    let keys = fold_overlay_keys(&mut esc, bytes, text);
    view.conn_esc = esc;
    for &k in &keys {
        // Closed mid-chunk: swallow the rest, never forward to a pane.
        if view.connections.is_none() {
            break;
        }
        let intent = view
            .connections
            .as_mut()
            .map(|cv| cv.on_key(k))
            .unwrap_or(ConnIntent::Bell);
        match intent {
            ConnIntent::Redraw => {}
            ConnIntent::Bell => {
                let _ = raw_out(b"\x07");
            }
            ConnIntent::Close => view.close_connections(),
            ConnIntent::Refresh => view.refresh_connections(),
            ConnIntent::Run(argv) => {
                // The reducer already armed `acting`; stash the argv for the run
                // loop to spawn at loop top (single-flight, sender in scope there).
                view.conn_action = Some((argv, Vec::new(), false));
            }
            ConnIntent::RunEnv { argv, env } => {
                view.conn_action = Some((argv, env, false));
            }
            ConnIntent::SpawnLogin(argv) => {
                // Stash for the run loop to spawn on the wire; the landing
                // focuses the pane.
                view.conn_action = Some((argv, Vec::new(), true));
            }
            ConnIntent::SetActiveAccount(account) => {
                // Mirror the modal's post-toggle value into the client's
                // authoritative session-local active account. Shells nothing and
                // touches no credential (Locked Decisions 1-2); later spawns read
                // it. The modal already repainted its own marker.
                view.active_account = account;
            }
        }
    }
    Ok(StdinFlow::Continue)
}
