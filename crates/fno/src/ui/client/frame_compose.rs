//! What one frame holds: the compose pass, the name and confirm modals, and
//! the overlay layouts. Moved out of client.rs (file budget).

use super::*;

/// One table cell: text in a proto fg + flag set, left- or right-aligned in
/// its column.
pub(super) fn rt_cell(text: String, fg: Color, flags: u8, right: bool) -> RtCell<'static> {
    let mut line = Line::from(text).style(
        RtStyle::new()
            .fg(rt_color(fg))
            .add_modifier(rt_modifier(flags)),
    );
    if right {
        line = line.alignment(Alignment::Right);
    }
    RtCell::from(line)
}

/// One full-width text row painted straight into the compositor's cells -
/// the bands (squad and section headers), sublines, idle fold, empty state
/// and footer that a column split would clip. Advances by display columns
/// and marks a wide glyph's second cell with WIDE_SPACER, exactly as the
/// old draw_sideline row loop did.
pub(super) fn paint_legacy_row(
    cells: &mut [Cell],
    r: usize,
    cols: usize,
    text_w: usize,
    text: &str,
    flags: u8,
) {
    let mut col = 0usize;
    for ch in text.chars() {
        let w = glyph_cols(ch);
        if col + w > text_w {
            break;
        }
        cells[r * cols + col] = Cell {
            c: ch,
            fg: Color::Default,
            bg: Color::Default,
            flags,
        };
        if w == 2 {
            cells[r * cols + col + 1] = Cell {
                c: ' ',
                fg: Color::Default,
                bg: Color::Default,
                flags: flags | cell_flags::WIDE_SPACER,
            };
        }
        col += w;
    }
}

impl View {
    pub(super) fn active_overlay_layout(&self) -> Option<OverlayLayout> {
        let rows = self.term.0 as usize;
        if let Some(action) = &self.confirm {
            return Some(self.confirm_overlay_layout(rows, action));
        }
        if let Some(name) = &self.create {
            return Some(self.create_modal_layout(name));
        }
        if let Some((target, name)) = &self.rename {
            let noun = match target {
                RenameTarget::Tab(_) => "tab",
                RenameTarget::Squad(_) => "workspace",
                RenameTarget::Agent(_) => "row",
            };
            // An agent label is never derived, so there is no auto to reset
            // to: the hint names the grammar instead of the blank-clears
            // semantics the tab/squad targets share.
            let hint = match target {
                RenameTarget::Agent(_) => Some("a-z 0-9 - _ (1-64 chars)"),
                _ => Some("empty resets to auto"),
            };
            return Some(self.name_modal_layout(&format!("rename {noun}"), name, hint));
        }
        if let Some(name) = &self.recruit {
            return Some(self.name_modal_layout(
                &format!("recruit {} into", self.marks.len()),
                name,
                Some("create-if-absent"),
            ));
        }
        // The two Full-chrome overlays below draw through the same
        // layout_lines_overlay pair (draw arm + this hit-test layout), so the
        // footer's esc close words resolve to the glyph they paint. Order
        // mirrors the draw chain: connections paints above peek.
        if let Some(conn) = &self.connections {
            return Some(self.connections_overlay_layout(rows, conn));
        }
        if let Some(peek) = &self.peek {
            return Some(self.peek_overlay_layout(rows, peek));
        }
        None
    }

    /// Compose the full-terminal frame: tab bar, sideline, dividers, panes.
    /// All drawing machinery (row diff, styles) stays in [`Compositor`].
    pub(super) fn compose(&self) -> Frame {
        crate::paint_stats::mark_compose_start();
        self.compose_at(Instant::now())
    }

    pub(super) fn compose_at(&self, now: Instant) -> Frame {
        let (rows, cols) = self.term;
        let (rows, cols) = (rows.max(1) as usize, cols.max(1) as usize);
        let mut cells = vec![Cell::default(); rows * cols];
        // The card paints inside the sideline and records its editor cursor
        // there when no picker holds the keyboard; last frame's is stale.
        self.card_cursor.set(None);
        let panel_w = self.panel_w() as usize;
        chrome::close_chips_begin();
        backlog_style::node_spans_begin();

        let agents_full =
            self.sideline_full && self.sideline_view == crate::view_store::SidelineView::Agents;

        // Full-screen sideline: the agent table takes the terminal width
        // with the composer at the bottom, and the panes do not paint. The
        // strip still owns row 0, so the sideline composes below it (the
        // click mappers invert the same offset via `sideline_top`), and the
        // server's viewport is untouched - no `Resize` travels either way.
        if agents_full {
            self.draw_tab_bar(&mut cells, cols);
            let top = TAB_BAR_ROWS as usize;
            self.draw_sideline(&mut cells[top * cols..], rows - top, cols, cols);
        } else {
            self.draw_tab_bar(&mut cells, cols);
            if panel_w > 0 {
                self.draw_sideline(&mut cells, rows, cols, panel_w);
            }
        }

        if !agents_full {
            let origin_r = TAB_BAR_ROWS as usize;
            let origin_c = panel_w as usize;
            // Blit, frames, underline, grips, indicator, letterbox +
            // dividers, empty state, drop band, reveal (the pane-pass move).
            self.paint_panes(&mut cells, rows, cols, origin_r, origin_c, now);
        }
        self.draw_bottom_row(&mut cells, rows, cols);
        key_grid::paint_agents(self, &mut cells, rows, cols);
        // Chrome, not an overlay: after panes, before modals.
        self.draw_feed_panel(&mut cells, rows, cols);
        file_viewer::draw(self, &mut cells, rows, cols);
        bell::draw(self, &mut cells, rows, cols);
        let (overlay_origin, overlay_dims) = self.overlay_viewport();
        if self.report.is_some() {
            report::draw(self, &mut cells, rows, cols);
        } else if let Some(m) = &self.feed_detail {
            draw_popup_overlay(&mut cells, rows, cols, &m.popup, self.term, &self.theme);
        } else if questions::draw_detail(
            self,
            &mut cells,
            (rows, cols),
            overlay_origin,
            overlay_dims,
        ) {
        } else if overlay_keys::draw_digest(
            self,
            &mut cells,
            (rows, cols),
            overlay_origin,
            overlay_dims,
        ) {
        } else if persona_overlay::draw_detail(
            self,
            &mut cells,
            (rows, cols),
            overlay_origin,
            overlay_dims,
        ) {
        } else if let Some(m) = &self.keys_modal {
            draw_popup_overlay(&mut cells, rows, cols, &m.popup, self.term, &self.theme);
        } else if let Some(m) = &self.row_menu {
            // A menu opened from the navigator paints over it, not instead.
            self.draw_nav(&mut cells, rows, cols, overlay_origin, overlay_dims);
            // US2: the anchored row context menu, drawn at the pointer.
            draw_popup_overlay(&mut cells, rows, cols, &m.popup, self.term, &self.theme);
        } else if let Some(m) = &self.aux {
            // US4/US5: the sideline MENU popup or settings modal.
            draw_popup_overlay(&mut cells, rows, cols, &m.popup, self.term, &self.theme);
        } else if let Some(sel) = self.answers {
            // needs-me queue (grown from the answer overlay,
            // folded MINE in as the first lane): MINE then the
            // severity-ranked THEY NEED YOU union, on the shared inverse-video
            // chrome. Always drawn while open - an empty union renders
            // "nothing needs you", a pending/failed fold renders its footer
            // notice (never a blank overlay).
            let projection = self.needs_projection();
            let total = projection.mine_shown + projection.need_shown;
            let sel = sel.min(total.saturating_sub(1));
            let lines =
                needs_overlay_lines(&projection, sel, self.mine_footer(), self.needs_footer());
            let chrome = chrome::Chrome::new("needs me", Anchor::Center).footer("q close");
            draw_lines_overlay(
                &mut cells,
                rows,
                cols,
                overlay_origin,
                overlay_dims,
                &chrome,
                &lines,
                &self.theme,
                // This queue is cursored (`n`/`N`) and unbounded, so it needs
                // the same follow the pickers do: on a short terminal the
                // selected row would otherwise sit below the fold while still
                // being the row Enter acts on. selected_line() accounts for
                // the MINE/THEY NEED YOU heading + footer lines between the
                // two lanes, which a flat `sel + 1` no longer can.
                Some(projection.selected_line(sel)),
            );
        } else if let Some(picker) = &self.move_pick {
            // move picker: `move tab to:` / `move pane to:` + one
            // numbered line per candidate squad.
            let lines = self.move_pick_lines(picker);
            let chrome = chrome::Chrome::new("move to", Anchor::Center).footer("esc cancel");
            draw_lines_overlay(
                &mut cells,
                rows,
                cols,
                overlay_origin,
                overlay_dims,
                &chrome,
                &lines,
                &self.theme,
                // +1 for the header line: an uncapped list must not park its
                // cursor behind the fold (same reason as the attach picker).
                Some(picker.cursor + 1),
            );
        } else if let Some(picker) = &self.attach_place {
            let lines = self.attach_place_lines(picker);
            let chrome = chrome::Chrome::new("attach", Anchor::Center).footer("esc cancel");
            draw_lines_overlay(
                &mut cells,
                rows,
                cols,
                overlay_origin,
                overlay_dims,
                &chrome,
                &lines,
                &self.theme,
                // +1 for the header line: keep the CURSOR row on screen. Without
                // this an uncapped list on a short terminal would put the 10th
                // workspace behind the fold while still selecting it, which is
                // the same unreachability the .take(9) caused.
                Some(picker.cursor + 1),
            );
        } else if let Some(pick) = &self.portal_pick {
            // The portal-placement picker, the attach picker's sibling:
            // same cursor-on-screen discipline (+1 for the header line).
            let lines = self.portal_pick_lines(pick);
            let chrome = chrome::Chrome::new("portal", Anchor::Center).footer("esc cancel");
            draw_lines_overlay(
                &mut cells,
                rows,
                cols,
                overlay_origin,
                overlay_dims,
                &chrome,
                &lines,
                &self.theme,
                Some(pick.cursor + 1),
            );
        } else if let Some(conn) = &self.connections {
            // Connections modal: accounts + combos lists. Drawn from the
            // modal's own render (pure). This and the settings modal are the
            // pair from the operator's screenshot - they now share one chrome.
            self.draw_connections(conn, &mut cells, rows, cols);
        } else if let Some(peek) = &self.peek {
            // peek overlay: the peeked agent row (re-read LIVE from the
            // layout, navigator-style) header + transcript. Drawn above nav
            // (mutually exclusive modes).
            let drows = self.display_rows();
            let agent = drows.get(peek.cursor).and_then(|r| match r {
                DisplayRow::Agent(a) => Some(*a),
                _ => None,
            });
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let reply = self.peek_input.as_ref().map(|(_, buf)| buf.as_str());
            let lines = peek_overlay_lines(agent, peek, reply, now_secs);
            let title = agent.map(|a| a.name.as_str()).unwrap_or("peek");
            let chrome = chrome::Chrome::new(title, Anchor::Center).footer("esc close");
            let layout = self.peek_layout(&chrome, &lines);
            draw_overlay_layout(&mut cells, rows, cols, &layout, &self.theme);
        } else if self.backlog_board.is_some() {
            // The board's whole surface - docked column, drill-down,
            // pickers, centered or full-screen overlay - paints from its
            // own module (the file-budget gate keeps client.rs shrinking).
            self.draw_board(&mut cells, rows, cols);
        } else if self.nav.is_some() {
            self.draw_nav(&mut cells, rows, cols, overlay_origin, overlay_dims);
        } else if self.org_board.is_some() && self.board_full {
            org_board::paint(self, &mut cells, rows, cols, cols, rows);
        } else if self.messages_board.is_some() {
            messages_view::paint_full(self, &mut cells, rows, cols);
        } else if self.decisions_board.is_some() {
            decisions_view::paint_full(self, &mut cells, rows, cols);
        }

        // Terminal cursor: the card's editor while it is open (the keyboard
        // owner), else the FOCUSED pane's, offset into its rect - the one
        // place the cursor may sit (AC1-UI/AC5-UI).
        let (cur_r, cur_c, cur_vis) = terminal_cursor::compose_cursor(self, self.card_cursor.get());
        *self.close_chips.borrow_mut() = chrome::close_chips_end();
        backlog_style::node_spans_end();
        // Last paint: the notice, one row below the tab bar.
        row_stamp::paint_notice_below_tab_bar(&mut cells, cols, self.notice_overlay(cols));
        Frame {
            rows: rows as u16,
            cols: cols as u16,
            cells,
            cursor_row: cur_r,
            cursor_col: cur_c,
            cursor_visible: cur_vis,
            // The composed full-terminal frame is not itself scrolled; the
            // per-pane indicator is drawn INTO the cells above from each pane
            // frame's own scroll_offset.
            scroll_offset: 0,
            history_lines: 0,
        }
    }

    /// The centered name-entry modal (create / rename / recruit), wearing the
    /// shared `Chrome` + `frame` + `blit` path every other modal takes: the
    /// target as the title, the blank-clears rule as the footer, the typed
    /// name plus cursor as the body. The bottom chrome row stays blanked so a
    /// stale row never shows under the modal.
    pub(super) fn name_modal_layout(
        &self,
        label: &str,
        name: &str,
        hint: Option<&str>,
    ) -> OverlayLayout {
        let (origin, dims) = self.overlay_viewport();
        // The typed name plus its cursor IS the body; the target and the
        // blank-clears rule move into the chrome, where every other modal puts
        // them.
        let mut chrome = chrome::Chrome::new(label, Anchor::Center);
        if let Some(h) = hint {
            chrome = chrome.footer(h);
        }
        // The footer widens the frame to fit, so on a narrow viewport it has to
        // be clamped or the right border leaves the screen.
        chrome = chrome.fit_to(dims.1.saturating_sub(chrome::Chrome::FRAME_COLS));
        // A name longer than the body wraps onto more lines, so the whole name
        // and its `_` cursor stay on screen.
        // Body width: the chrome minimum or the wider input floor, capped to
        // the viewport so a narrow terminal still fits the frame.
        let viewport_w = dims.1.saturating_sub(chrome::Chrome::FRAME_COLS);
        let body_w = chrome.min_inner_w().max(40).min(viewport_w).max(1);
        let text = format!("{name}_");
        let line = TLine::line(TextStyle::Item, format!("{text:<body_w$}"));
        layout_lines_overlay(origin, dims, &chrome, &[line], None, OverlayAnchor::Center)
    }

    pub(super) fn draw_name_modal(
        &self,
        cells: &mut [Cell],
        rows: usize,
        cols: usize,
        label: &str,
        name: &str,
        hint: Option<&str>,
    ) {
        if rows > 0 {
            for c in 0..cols {
                cells[(rows - 1) * cols + c] = Cell::default();
            }
        }
        let layout = self.name_modal_layout(label, name, hint);
        draw_overlay_layout(cells, rows, cols, &layout, &self.theme);
    }

    /// Paint the confirm prompt over the bottom row (dispatch;
    /// squad removal). Blank first (the divider-bleed gotcha), then the
    /// BOLD prompt whose wording tracks the action being confirmed.
    /// The outer row a live confirm prompt paints on: the acted-on
    /// sideline row when it is still in the catalog and visible, else the bottom
    /// row. Anchoring the prompt at the target row is AC2-UI - a bottom-row
    /// prompt far from the row reads as "nothing happened". The target is
    /// resolved by IDENTITY every paint (via [`View::confirm_target_index`]), not
    /// a captured index, so a scrape/layout push that reorders or removes rows
    /// re-anchors to the still-valid row or dismisses to the bottom - it can never
    /// paint beside an unrelated row that drifted under a stale index (AC1-FR,
    /// codex P2). The dispatch is likewise identity-keyed, so it is drift-safe too.
    pub(super) fn confirm_anchor_row(&self, rows: usize, action: &ConfirmAction) -> usize {
        let bottom = rows.saturating_sub(1);
        match self.confirm_target_index(action) {
            Some(i) => {
                let off = self.sideline_offset();
                if i >= off && (i - off) < bottom {
                    i - off
                } else {
                    bottom
                }
            }
            None => bottom,
        }
    }

    pub(super) fn confirm_text(&self, action: &ConfirmAction) -> String {
        let label = &action.label;
        let text = match &action.action {
            ConfirmKind::RemoveSquad {
                panes, last: true, ..
            } => format!(
                "close workspace {label} ({panes} panes) - last workspace, ends the session?"
            ),
            ConfirmKind::RemoveSquad {
                panes, last: false, ..
            } => {
                format!("close workspace {label} ({panes} panes)?")
            }
            ConfirmKind::StopAgent { .. } => format!("stop {label}?"),
            ConfirmKind::RemoveAgent { measure: true, .. } => {
                format!("measure and remove {label}?")
            }
            ConfirmKind::RemoveAgent { .. } => format!("remove {label}?"),
            ConfirmKind::ReapAgents => "reap all exited fno agent?".to_string(),
            ConfirmKind::StopExternal { .. } => format!("stop {label}?"),
            ConfirmKind::RemoveExternal { .. } => {
                format!("remove {label} and worktree?")
            }
            ConfirmKind::DismissMember { .. } => format!("dismiss {label}?"),
            ConfirmKind::ClearDead { dead, .. } => {
                format!("clear {dead} dead row(s) in {label}?")
            }
            // A tab close is a GROUP close on the wire: the server reaps every
            // leaf in the tab, not the focused pane. Say the count when there is
            // more than one, so the prompt names what the Enter destroys. Read
            // from the LIVE layout at draw time, never from the capture: the
            // prompt may have sat open while panes came and went, and the
            // commit re-resolves the tab for exactly the same reason.
            ConfirmKind::CloseTab { tab } => match self.find_tab(*tab) {
                Some((_, _, t)) if t.panes.len() > 1 => {
                    format!("close tab {label} and its {} panes?", t.panes.len())
                }
                _ => format!("close tab {label}?"),
            },
        };
        text
    }

    pub(super) fn confirm_overlay_layout(
        &self,
        rows: usize,
        action: &ConfirmAction,
    ) -> OverlayLayout {
        let (origin, dims) = self.overlay_viewport();
        let chrome = chrome::Chrome::new("confirm", Anchor::Center)
            .footer("enter confirm · esc cancel")
            .fit_to(dims.1.saturating_sub(chrome::Chrome::FRAME_COLS));
        let lines = [self.confirm_text(action)];
        let anchor = if matches!(&action.action, ConfirmKind::CloseTab { .. }) {
            OverlayAnchor::Center
        } else {
            OverlayAnchor::At {
                row: self.confirm_anchor_row(rows, action),
                col: origin.1,
            }
        };
        layout_lines_overlay(origin, dims, &chrome, &lines, None, anchor)
    }

    /// The Connections hit-test layout: the same chrome and body overlay the
    /// draw arm builds, so a click lands on the cell the user sees.
    pub(super) fn connections_overlay_layout(
        &self,
        _rows: usize,
        conn: &crate::connections_view::ConnectionsView,
    ) -> OverlayLayout {
        let (origin, dims) = self.overlay_viewport();
        let chrome = chrome::Chrome::new("connections", Anchor::Center).footer("esc close");
        let (body, _) = conn.render_body(&self.layout.agents, overlay_text_cols(dims.1));
        overlay_paint::layout_body_overlay(
            origin,
            dims,
            &chrome,
            &body,
            None,
            OverlayAnchor::Center,
        )
    }

    /// The peek overlay's hit-test layout, rebuilt from the same LIVE inputs
    /// the draw arm reads (layout rows, reply buffer, wall clock). A second
    /// tick between draw and click can move a width by a character; the hit
    /// spans are wide enough that this never unclicks the footer.
    pub(super) fn peek_overlay_layout(&self, _rows: usize, peek: &PeekView) -> OverlayLayout {
        let drows = self.display_rows();
        let agent = drows.get(peek.cursor).and_then(|r| match r {
            DisplayRow::Agent(a) => Some(*a),
            _ => None,
        });
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let reply = self.peek_input.as_ref().map(|(_, buf)| buf.as_str());
        let lines = peek_overlay_lines(agent, peek, reply, now_secs);
        let title = agent.map(|a| a.name.as_str()).unwrap_or("peek");
        let chrome = chrome::Chrome::new(title, Anchor::Center).footer("esc close");
        self.peek_layout(&chrome, &lines)
    }

    pub(super) fn draw_confirm_line(
        &self,
        cells: &mut [Cell],
        rows: usize,
        cols: usize,
        action: &ConfirmAction,
    ) {
        if rows > 0 {
            for c in 0..cols {
                cells[(rows - 1) * cols + c] = Cell::default();
            }
        }
        let layout = self.confirm_overlay_layout(rows, action);
        draw_overlay_layout(cells, rows, cols, &layout, &self.theme);
    }
}
