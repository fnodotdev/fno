//! The selector's keys: what a key does while the sideline selector is open.
//! Moved out of client.rs (file budget).

use super::*;

/// Selector-mode keys: j/k (and arrows) move over the unified display rows,
/// skipping inert Headers; h (and left) collapse, `l` explicitly expands a
/// squad row, Right toggles a workspace row's idle caret;
/// Enter acts on the row through [`View::row_action`] + [`apply_hit`] - the
/// same resolver a mouse click uses, so squad/tab switch, agent
/// focus/attach, card dispatch-confirm, and workspace-create are all keyboard
/// reachable. The squad-management context keys ride here too: on a
/// squad row `r` renames, `x` removes (behind a confirm), `J`/`K` reorder; on
/// a tab row `m` opens the move-to-squad picker. A refusal (Notice/BEL) keeps
/// the selector open; Esc/q closes. Rows and cursor are re-read per key so a
/// close mid-chunk swallows the remainder instead of resurrecting the selector.
/// Detach is prefix+d from NORMAL mode only (Locked 11): close the selector.
/// The selector's act-on-a-row body, shared by Enter, the `l` reach on
/// an agent row, and the Right-arrow pre-pass on an agent row, so the three
/// doors cannot drift on what "act" means: a refusal keeps the selector open
/// (locked 3), a hit closes it and applies, a row with no action says
/// so on-screen.
pub(super) async fn selector_apply_row_action(
    view: &mut View,
    cur: usize,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    // row_action resolves against the CURRENT catalog (AC6-FR) and returns an
    // OWNED hit, so applying it can mutate the view.
    match view.row_action(cur) {
        Some(ChromeHit::Notice(msg)) => view.set_notice(msg.to_string()),
        Some(hit) => {
            view.selector = None;
            apply_hit(view, hit, sock_w).await?;
        }
        // Out of range / Header: unreachable via j/k, but a stale cursor gets
        // an on-screen notice, never a silent close or a bare beep (US3).
        None => view.set_notice("no action for this row".into()),
    }
    Ok(())
}

pub(super) async fn selector_keys(
    view: &mut View,
    bytes: &[u8],
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<StdinFlow, String> {
    let mut esc = std::mem::take(&mut view.sel_esc);
    // Right arrow is the caret TOGGLE on a workspace row, distinct from `l`'s
    // explicit expand: strip each COMPLETE `ESC [ C` from this chunk and handle
    // it below, so it never aliases onto `l` through the fold. A sequence the
    // read split lands in the carry and folds to `l` (expand) - a valid caret
    // open, never a mis-toggle. A modified Right (`ESC [ 1; 5 C`) does not
    // contain the contiguous triple, so it still drops as unmapped.
    let mut rights = 0usize;
    let mut shifts: Vec<isize> = Vec::new();
    let mut cleaned: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(&[0x1b, b'[', b'C']) {
            rights += 1;
            i += 3;
        } else {
            cleaned.push(bytes[i]);
            i += 1;
        }
    }
    let mut j = 0;
    let mut cleaned2: Vec<u8> = Vec::with_capacity(cleaned.len());
    while j < cleaned.len() {
        if cleaned[j..].starts_with(&[0x1b, b'[', b'1', b';', b'2', b'A']) {
            // Shift+Up reorders the focused agent row: the selector reads
            // J/K as the WORKSPACE reorder, so the agent move needs its own
            // strip (the generic fold drops a parameterised sequence whole).
            shifts.push(-1);
            j += 6;
        } else if cleaned[j..].starts_with(&[0x1b, b'[', b'1', b';', b'2', b'B']) {
            shifts.push(1);
            j += 6;
        } else {
            cleaned2.push(cleaned[j]);
            j += 1;
        }
    }
    let keys = fold_selector_keys(&mut esc, &cleaned2);
    view.sel_esc = esc;
    for _ in 0..rights {
        // Same cursor as every other selector command: pointer-follow works
        // through the hover-ARMED selector, and an explicit prefix+w
        // selector keeps keyboard control even with the pointer parked on an
        // inert sideline cell.
        let Some(cur) = view.selector else {
            break;
        };
        // On an AGENT row Right is the reach - the same row_action
        // path Enter takes, matching the peek overlay where Right already
        // reaches. A workspace row keeps the caret toggle; anything else says
        // why not.
        if matches!(view.display_rows().get(cur), Some(DisplayRow::Agent(_))) {
            selector_apply_row_action(view, cur, sock_w).await?;
            continue;
        }
        let squad = match view.display_rows().get(cur) {
            Some(DisplayRow::Sel(r)) if r.tab.is_none() => Some(r.squad),
            _ => None,
        };
        match squad.and_then(|sq| squad_key(&view.layout, sq)) {
            Some(key) => view.toggle_idle(key),
            None => view.set_notice("only a workspace row has a caret".into()),
        }
    }
    for delta in shifts.drain(..) {
        // The agent reorder acts on the same per-key cursor the other
        // selector commands read, so a close mid-chunk swallows the rest.
        let Some(cur) = view.selector else {
            break;
        };
        agent_group::reorder_agent_rows(view, cur, delta);
    }
    for &k in &keys {
        // Rows are re-read per key (via the View helpers below) so a layout
        // push or a close mid-chunk acts on the CURRENT catalog, never a stale
        // snapshot.
        let Some(cur) = view.selector else {
            break; // closed mid-chunk: swallow the rest, never forward
        };
        // Any key other than a J/K reorder drops the cursor-follow intent, so a
        // later Layout re-anchors normally instead of chasing a stale squad.
        if k != b'J' && k != b'K' {
            view.sel_follow = None;
        }
        match k {
            b'j' => view.selector = Some(view.selector_down(cur)),
            b'k' => view.selector = Some(view.selector_up(cur)),
            b'l' | b'h' => {
                // `l` on an AGENT row is the reach: the same
                // row_action path Enter takes, so Right (which folds to `l`
                // when the sequence splits across a read boundary) and the
                // peek overlay's `l` all agree on one meaning. `h` stays
                // inert there - collapse has no meaning for an agent row -
                // and BOTH keys keep their expand/collapse meanings on
                // workspace rows (AC7-REG). Every other row class no-ops
                // (matching today's tab rows).
                let on_agent = matches!(view.display_rows().get(cur), Some(DisplayRow::Agent(_)));
                if k == b'l' && on_agent {
                    selector_apply_row_action(view, cur, sock_w).await?;
                    continue;
                }
                // Expand/collapse applies to squad rows; every other variant
                // no-ops (matching today's tab rows). Materialize the owned id
                // before mutating - display_rows() borrows the layout.
                let squad = match view.display_rows().get(cur) {
                    Some(DisplayRow::Sel(r)) if r.tab.is_none() => Some(r.squad),
                    _ => None,
                };
                // `l`/`h` stay the EXPLICIT open/close pair (keeps the
                // tri-state cycle on the header click and prefix+z); `l` from
                // live-only opens back to the full section.
                if let Some(key) = squad.and_then(|sq| squad_key(&view.layout, sq)) {
                    let next = if k == b'l' {
                        SectionView::Expanded
                    } else {
                        SectionView::Collapsed
                    };
                    view.set_section_view(key, next);
                }
            }
            b'\r' | b'\n' => {
                selector_apply_row_action(view, cur, sock_w).await?;
            }
            b'd' => {
                let pane = match view.display_rows().get(cur) {
                    Some(DisplayRow::Agent(a)) if a.pane_id.is_some() && !a.exited => a.pane_id,
                    _ => None,
                };
                match pane {
                    Some(pane) => {
                        write_msg(sock_w, &ClientMsg::Command(Command::DetachPane { pane }))
                            .await
                            .map_err(|e| format!("detach send failed: {e}"))?;
                    }
                    None => view.set_notice("only a live pane-hosted worker can detach".into()),
                }
            }
            b'r' => view.rename_at_cursor(cur),
            b' ' => {
                // An agent row opens the reply dock: the peek under the
                // list, reply input focused. A workspace row shows its tabs
                // and members rendered locally, no wire round trip. Any
                // other row notices why. The selector stays open underneath.
                match view.display_rows().get(cur) {
                    Some(DisplayRow::Agent(a)) => {
                        reply_dock::open(view, cur, a.name.clone(), sock_w).await?
                    }
                    Some(DisplayRow::Sel(r)) if r.tab.is_none() => {
                        view.open_squad_peek(cur, r.squad);
                    }
                    _ => view.set_notice("only an agent or workspace row can be peeked".into()),
                }
            }
            b'\t' => {
                // Toggle a recruit mark on the focused row (; moved from
                // Space to Tab by, which took Space for peek). Markable
                // only if it is an attachable watch-only agent (live, has an
                // attach_id); anything else gives a notice, never zero feedback.
                let id = match view.display_rows().get(cur) {
                    Some(DisplayRow::Agent(a)) if a.attach_id.is_some() && !a.exited => {
                        a.attach_id.clone()
                    }
                    _ => None,
                };
                match id {
                    Some(id) => {
                        if !view.marks.remove(&id) {
                            view.marks.insert(id);
                        }
                    }
                    None => view.set_notice("not attachable".into()),
                }
            }
            b'R' => {
                // Open the recruit name prompt for the marked rows. With
                // no marks, fall back to marking the focused attachable row first
                // (the grid's single-recruit `m`, generalized); a non-attachable
                // focused row with no marks notices why (US3).
                if view.marks.is_empty() {
                    let id = match view.display_rows().get(cur) {
                        Some(DisplayRow::Agent(a)) if a.attach_id.is_some() && !a.exited => {
                            a.attach_id.clone()
                        }
                        _ => None,
                    };
                    match id {
                        Some(id) => {
                            view.marks.insert(id);
                            view.open_recruit();
                        }
                        None => {
                            view.set_notice("only a live attachable agent can be recruited".into())
                        }
                    }
                } else {
                    view.open_recruit();
                }
            }
            b'p' => {
                let picked = match view.display_rows().get(cur) {
                    Some(DisplayRow::Agent(a))
                        if a.pane_id.is_none() && a.attach_id.is_some() && !a.exited =>
                    {
                        Some((a.attach_id.clone().unwrap(), a.squad))
                    }
                    _ => None,
                };
                // Enter reaches the thread pane, so `p` is the
                // picker's only door. The two no-op cases stay distinct: "no
                // workspace" and "not attachable" are different problems to
                // report.
                let squads: Vec<u64> = view.attach_dst_squads();
                match picked {
                    Some((id, owner)) if !squads.is_empty() => {
                        view.open_attach_place(id, owner, squads)
                    }
                    Some(_) => view.set_notice("no workspace to attach into".into()),
                    None => view.set_notice("placement requires an attachable agent".into()),
                }
            }
            b'P' => {
                // `P` opens the portal PICKER: the OPEN portals, numbered,
                // plus a new-portal row PRE-SELECTED, so `P` Enter still sends
                // the exact wire gesture this key always had. The SERVER still
                // picks a new index - a client computing it from the rows it
                // last rendered races every other client onto the same number.
                // `P` pairs with `p` (the placement picker) the way `X`/`x`
                // pair; the row decision and the refusal words live with the
                // picker (portal_pick_decision).
                let on_agent = match view.display_rows().get(cur) {
                    // DisplayRow::Agent carries the row by reference, so the
                    // match under `.get()` binds it double; the decision wants
                    // one.
                    Some(DisplayRow::Agent(a)) => Some(*a),
                    _ => None,
                };
                match view.portal_pick_decision(on_agent) {
                    PortalPickDecision::Open(id) => view.open_portal_pick(id),
                    PortalPickDecision::Refuse(text) => view.set_notice(text),
                }
            }
            b'X' => {
                // Bulk reap: uppercase `X` from ANY agent row confirms
                // `fno-agent reap`. Contextual on agent rows only (headers stay
                // inert - no selector surgery); a non-agent row notices why
                // (US3). Too-short terminal refuses rather than arm an
                // invisible confirm.
                let on_agent = matches!(view.display_rows().get(cur), Some(DisplayRow::Agent(_)));
                if !on_agent {
                    view.set_notice("reap works on an agent row".into());
                } else if view.term.0 < MIN_ROWS_FOR_STATUS {
                    view.set_notice("terminal too short for the confirm prompt".into());
                } else {
                    view.open_confirm(ConfirmAction {
                        action: ConfirmKind::ReapAgents,
                        label: String::new(),
                    });
                }
                continue;
            }
            b'x' => {
                // A TOMBSTONE member row dismisses; a squad-header row
                // removes the squad, behind a confirm - disambiguated by
                // row type so one key serves both. A too-short terminal cannot
                // render the bottom-row prompt, so it refuses with a notice rather
                // than arm an INVISIBLE confirm; an unknown squad or a
                // tab/other row BELs.
                let dismiss = match view.display_rows().get(cur) {
                    Some(DisplayRow::Agent(a)) if a.tombstone => a.squad.zip(a.attach_id.clone()),
                    _ => None,
                };
                if let Some((squad, attach_id)) = dismiss {
                    write_msg(
                        sock_w,
                        &ClientMsg::Command(Command::DismissMember { squad, attach_id }),
                    )
                    .await
                    .map_err(|e| format!("dismiss send failed: {e}"))?;
                    continue;
                }
                // A WATCH-ONLY (paneless) agent row gets the lifecycle command
                // a live row (`!exited`) stops, an exited row removes.
                // The registry poll's state flip IS the stage separator - stop,
                // wait ≤1s for the row to flip exited, then `x` again removes (no
                // double-tap timer). The captured name (not the row index) rides
                // the confirm, so a row that races out resolves to the server's
                // stale-name refusal; too-short terminal refuses rather than arm
                // an invisible confirm, like RemoveSquad.
                //
                // `pane_id.is_none()` is load-bearing (codex review): a PANE-hosted
                // Agent row is either a real agent's pane or a bare shell pane that
                // agent_rows() surfaces as a first-class Agent row labelled from its
                // cmd/cwd - NOT a registry entry. Arming the command there would shell
                // `fno-agent` on a label that could collide with an unrelated
                // agent's name and stop the wrong one. Pane-hosted rows are managed
                // via their tab (CloseTab); only the paneless rows - the bg agents and the
                // watched headless runs that today linger until GC - are the gap this closes.
                //
                // An EXTERNAL row (claude-daemon roster or a persisted external
                // tombstone) routes by stable `attach_id` to the
                // External commands instead of `fno-agent` by name: a live row
                // (`!exited`) stops (`claude stop <id>`), a stopped tombstone
                // (`exited`) removes (`claude rm <id>`). The server re-validates
                // the exact id + gates rm on a persisted `stopped` state; a
                // failed/unknown tombstone renders `!exited` so its `x` retries
                // the stop. An external row without an attach_id (degenerate)
                // falls through to the by-name path, which the server refuses.
                let agent = match view.display_rows().get(cur) {
                    Some(DisplayRow::Agent(a)) if !a.tombstone && a.pane_id.is_none() => Some((
                        a.name.clone(),
                        a.exited,
                        a.external,
                        a.attach_id.clone(),
                        a.harness_session_id.clone(),
                        // The row's lattice state decides whether the
                        // remove carries the measure flag: a `?` row measures
                        // (rm's daemon gate) instead of paying a stop leg
                        // that times out against a session nothing answers.
                        agent_lattice_state(a) == LatticeState::Unmeasured,
                    )),
                    _ => None,
                };
                if let Some((name, exited, external, attach_id, harness_session_id, unmeasured)) =
                    agent
                {
                    if view.term.0 < MIN_ROWS_FOR_STATUS {
                        view.set_notice("terminal too short for the confirm prompt".into());
                        continue;
                    }
                    let action = match (external, attach_id) {
                        (true, Some(id)) if exited => ConfirmKind::RemoveExternal {
                            attach_id: id,
                            name: name.clone(),
                        },
                        (true, Some(id)) => ConfirmKind::StopExternal {
                            attach_id: id,
                            name: name.clone(),
                        },
                        // `x` states the intent ONCE: remove. rm
                        // alone is sent; the daemon's rm ends a live row's
                        // process itself. Stop-only lives on the row menu's
                        // Stop.
                        _ => ConfirmKind::RemoveAgent {
                            name: name.clone(),
                            sid: harness_session_id,
                            pane_id: None,
                            measure: unmeasured,
                        },
                    };
                    // (scope a+c) The slot feeds the post-commit
                    // re-anchor: the sideline stays open and the cursor stays
                    // on this row (or its neighbour) after the commit.
                    view.row_slot = Some(cur);
                    view.open_confirm(ConfirmAction {
                        action,
                        label: name,
                    });
                    continue;
                }
                let squad = match view.display_rows().get(cur) {
                    Some(DisplayRow::Sel(r)) if r.tab.is_none() => Some(r.squad),
                    _ => None,
                };
                match squad.and_then(|sq| {
                    view.layout
                        .squads
                        .iter()
                        .find(|s| s.id == sq)
                        .map(|s| (sq, s.name.clone(), s.panes))
                }) {
                    Some(_) if view.term.0 < MIN_ROWS_FOR_STATUS => {
                        view.set_notice("terminal too short for the confirm prompt".into())
                    }
                    Some((sq, name, panes)) => {
                        let last = view.layout.squads.len() == 1;
                        view.open_confirm(ConfirmAction {
                            action: ConfirmKind::RemoveSquad {
                                squad: sq,
                                panes,
                                last,
                            },
                            label: name,
                        });
                    }
                    None => view.set_notice("no action for this row".into()),
                }
            }
            b'J' | b'K' => {
                // Reorder the squad at the cursor down (`J`) / up (`K`) the
                // sideline. The cursor follows the squad via sel_follow
                // on the authoritative next Layout. Tab/other rows notice why.
                let squad = match view.display_rows().get(cur) {
                    Some(DisplayRow::Sel(r)) if r.tab.is_none() => Some(r.squad),
                    _ => None,
                };
                match squad {
                    Some(sq) => {
                        let delta = if k == b'J' { 1 } else { -1 };
                        view.sel_follow = Some(sq);
                        write_msg(
                            sock_w,
                            &ClientMsg::Command(Command::MoveSquad { squad: sq, delta }),
                        )
                        .await
                        .map_err(|e| format!("move-workspace send failed: {e}"))?;
                    }
                    None => view.set_notice("only a workspace row can be reordered".into()),
                }
            }
            b'm' => {
                // US2: `m` on an agent row,
                // or a band header whose section menu the mouse path
                // already opens - opens its context menu (mouse-off parity),
                // anchored at the row and sitting over the selector like peek;
                // Esc drops back into the selector. A header that refuses (a
                // band with nothing to clear) already set its notice; the key
                // is still swallowed.
                if matches!(
                    view.display_rows().get(cur),
                    Some(DisplayRow::Agent(_) | DisplayRow::Header { .. })
                ) {
                    // Screen row = index - the TableState offset (: the sideline
                    // owns row 0; there is no TAB_BAR_ROWS offset on this side
                    // of the divider).
                    let arow = (cur.saturating_sub(view.sideline_offset())) as u16;
                    // A row that refuses with its own notice (an all-live band)
                    // keeps it; one that refuses SILENTLY still gets a word here,
                    // because a swallowed key
                    // with zero feedback reads as a dead key. Compared against
                    // the notice BEFORE the call, so a stale unrelated notice
                    // within its TTL cannot mask the dead-key feedback.
                    let before = view.notice.clone();
                    if !view.open_row_menu(cur, Anchor::At { row: arow, col: 1 })
                        && view.notice == before
                    {
                        view.set_notice("this row has no menu".into());
                    }
                    continue;
                }
                // Move a tab into another squad: open the numbered
                // picker over the OTHER squads (a squad is moved with J/K, not
                // m). Tab rows left the sideline, so `m` on a squad row
                // targets that squad's ACTIVE tab - the one shown in the tab bar.
                // A non-squad row, or nowhere to move to (one squad), notices why.
                let picked = match view.display_rows().get(cur) {
                    Some(DisplayRow::Sel(r)) if r.tab.is_none() => Some(r.squad),
                    _ => None,
                }
                .and_then(|squad| {
                    let sq = view.layout.squads.iter().find(|s| s.id == squad)?;
                    let tid = sq.tabs.get(sq.active_tab).or_else(|| sq.tabs.first())?.id;
                    // Exclude the source: that rule lives in `move_dst_squads`,
                    // which the Move-to-workspace menu entry already uses - this
                    // site was a fourth hand-rolled copy of the same list.
                    let dsts: Vec<u64> = view.move_dst_squads(Some(squad));
                    (!dsts.is_empty()).then_some((tid, dsts))
                });
                match picked {
                    Some((tid, dsts)) => view.open_move_pick(MoveSrc::Tab(tid), dsts),
                    None => view.set_notice("no other workspace to move this tab to".into()),
                }
            }
            // Pinned rows lead every sort (the operator's 2026-10-07 ask);
            // ctrl+t is Claude's agents-view chord for the same pin.
            b'.' | 0x14 => agent_sort::toggle_pin(view, cur),
            0x1b | b'q' => view.selector = None,
            _ => {}
        }
    }
    // Follow the (possibly moved) cursor / expanded catalog into the scroll
    // window so a row driven below the fold stays visible.
    view.clamp_sideline_scroll();
    Ok(StdinFlow::Continue)
}
