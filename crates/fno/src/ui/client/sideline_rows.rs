use super::*;

/// One rendered sideline line. The actionable variants (`Sel`, `Agent`, `Card`,
/// `NewSquad`) resolve through [`View::row_action`] via the selector's Enter or a
/// mouse click; the inert variants (`Header`, `Sub`, `Blank`) are
/// skipped by the selector, never hover-highlighted, and return `None` from
/// `row_action` - see [`row_is_inert`].
pub(super) enum DisplayRow<'a> {
    Sel(SelRow),
    Agent(&'a AgentRow),
    /// (US1+US2) A section header: a full-width INVERSE band with a
    /// right-aligned per-state rollup strip. `rollup` is folded at
    /// `display_rows` time from the section's own rows (orphans / cards), so the
    /// painter renders it without re-deriving section membership.
    Header {
        label: String,
        rollup: Vec<(LatticeState, usize)>,
        /// Which section this header owns, so a click cycles it
        /// without the action path re-deriving the section from `label`.
        key: SectionKey,
        /// The section's view state at fold time, for the caret glyph.
        view: SectionView,
    },
    /// The `+` create-workspace affordance, a footer under the squad
    /// list. A click opens the name-input overlay.
    NewSquad,
    /// An inert one-line separator retained in card mode.
    Blank,
    CardDetail(&'a AgentRow),
    CardMetrics(&'a AgentRow),
    /// The dashed rule between adjacent cards; inert (one display row).
    CardRule,
    /// The extended table's column-header line, carrying the current
    /// sort label so a toggle is never invisible - even when the two orders
    /// happen to coincide (one agent, or all rows in one band), the label
    /// changes. Inert like `Sub`: one painted line, one display row, so the
    /// hit-test math is untouched.
    TableHead,
    /// The extended table's zero-agent line. Inert, like `TableHead`:
    /// a header with nothing under it reads as a stalled table, so the empty
    /// state is stated rather than implied.
    TableEmpty,
    /// The top-K idle fold: a squad with more live idle rows than the
    /// cap folds the overflow behind this one row. `hidden` is the foldable
    /// count (the idle rows past the cap, always > 0 when this row is emitted).
    /// Unlike the inert rows above it is ACTIONABLE - a click / selector Enter
    /// toggles the squad's idle expansion (the idle sibling of a header's
    /// `CycleSection`), so it is NOT in `row_is_inert` and the cursor can rest
    /// on it. Folded it paints `+N more`; expanded it paints `- fewer`.
    IdleFold {
        key: SectionKey,
        hidden: usize,
        expanded: bool,
    },
    /// Line `n` of the open draft card, which sits under its workspace's
    /// header. The text, the focused token and the tap spans all come from
    /// [`Launcher::card_lines`], so paint and hit-test read one source.
    Card(usize),
    /// A held draft's one-line ghost under its workspace's header; a tap
    /// reopens the card.
    Ghost(u64),
}

/// True for a non-actionable sideline row: the selector skips it, it is
/// never hover/selection-highlighted, and [`View::row_action`] returns `None`.
/// Header (a section label), Sub (an agent's dim subline), and Blank (a section
/// spacer). One predicate so paint, hit-test, and the selector never diverge.
pub(super) fn row_is_inert(drow: &DisplayRow) -> bool {
    matches!(
        drow,
        DisplayRow::Header { .. }
            | DisplayRow::Blank
            | DisplayRow::CardDetail(..)
            | DisplayRow::CardMetrics(..)
            | DisplayRow::CardRule
            | DisplayRow::TableHead
            | DisplayRow::TableEmpty
            | DisplayRow::Card(..)
            | DisplayRow::Ghost(..)
    )
}

/// What a left-click on chrome resolves to: server commands to send, or a
/// local one-line hint for a row that isn't directly actionable.
#[derive(Debug, Clone)]
pub(super) enum ChromeHit {
    Cmds(Vec<Command>),
    /// A row on another machine: open or focus its link pane.
    OpenRemote(crate::proto::RemoteRow),
    /// The strip row's view word (`Agents  Messages`, R15): switch the
    /// sideline to that view.
    TopRow(crate::view_store::SidelineView),
    /// The need-input count on the strip row: opens or closes the
    /// pinned Needs input section.
    NeedInput,
    Bell(bell::Hit),
    /// Owned, not `&'static`: an in-flight card's notice carries the
    /// server-computed `where_hint` (v18), which is per-card data.
    Notice(String),
    /// Open the new-workspace name-input overlay; the `+` footer.
    OpenCreate,
    /// The footer's `+ session` button: what prefix+i does.
    ToggleComposer,
    AddRemoteHost,
    /// Flip the active squad row's caret locally; no socket write.
    /// Advance one sideline section through the view cycle.
    CycleSection(SectionKey),
    /// Sort the extended table by the clicked header column.
    SortColumn(AgentSortColumn),
    /// Toggle a squad's top-K idle expansion - the idle sibling of
    /// `CycleSection`. A pure local set flip, no socket write.
    ToggleIdle(SectionKey),
    /// Advance the sideline density: the top-right button's click,
    /// routed to the same mutation the keybind runs so the two cannot diverge.
    CycleDensity,
    /// Open the sideline MENU popup anchored at the footer's menu cell (
    /// US4). Carries the click cell so the popup anchors under the pointer.
    OpenSidelineMenu {
        row: u16,
        col: u16,
    },
    /// Open the activity feed's provenance view for one row. Carries the item
    /// BY VALUE: a later fold replaces the row list, so an index would open
    /// the view onto a different event than the one clicked.
    OpenFeedDetail(crate::feed_overlay::FeedItem),
    /// The feed panel's own chrome: a column title or a group header.
    Feed(super::feed_view::FeedHit),
    /// Open the questions detail overlay on one block row. Carries the id,
    /// not the index: a fold between click and open must not retarget it.
    OpenQuestionDetail(String),
    /// A card's node tap: the plan in Obsidian, else the node details pane.
    OpenNode(String),
    OpenPr(String),
    /// The `+` on a workspace header: open that workspace's draft card.
    OpenDraft(u64),
    /// A tap on the open card.
    Card(agent_launcher::card::CardHit),
}

impl ChromeHit {
    /// The stable control id a resolved click ships to the usage events arm
    /// ([`crate::ui_usage`]). One constant per control, never a label: the
    /// analytics question is which controls get used, so an id may be
    /// coarse (every `Cmds` row action reads its first command) but never
    /// per-instance.
    pub(super) fn control_id(&self) -> &'static str {
        match self {
            ChromeHit::Cmds(cmds) => match cmds.first() {
                Some(Command::SelectTab(_)) => "tab_row",
                Some(Command::SelectSquad(_)) => "squad_row",
                Some(Command::FocusPane(_)) | Some(Command::AttachAgent { .. }) => "agent_row",
                _ => "row_action",
            },
            ChromeHit::OpenRemote(_) => "remote_row",
            ChromeHit::TopRow(_) => "view_word",
            ChromeHit::NeedInput => "need_input",
            ChromeHit::Bell(_) => "bell",
            ChromeHit::Notice(_) => "notice",
            ChromeHit::OpenCreate => "new_workspace",
            ChromeHit::ToggleComposer => "composer_toggle",
            ChromeHit::AddRemoteHost => "add_remote",
            ChromeHit::CycleSection(_) => "section",
            ChromeHit::SortColumn(_) => "sort_header",
            ChromeHit::ToggleIdle(_) => "idle_caret",
            ChromeHit::CycleDensity => "density",
            ChromeHit::OpenSidelineMenu { .. } => "menu_open",
            ChromeHit::OpenFeedDetail(_) => "feed_row",
            ChromeHit::Feed(_) => "feed_chrome",
            ChromeHit::OpenQuestionDetail(_) => "question_row",
            ChromeHit::OpenNode(_) => "node_link",
            ChromeHit::OpenPr(_) => "pr_link",
            ChromeHit::OpenDraft(_) => "draft_open",
            ChromeHit::Card(_) => "card",
        }
    }
}

impl View {
    /// Columns the card and the header `+` get: the sideline text width less
    /// the rail border and the one spare cell bands keep.
    pub(super) fn card_width(&self) -> usize {
        self.sideline_paint_w().saturating_sub(2)
    }

    /// The header's `+` cells, a click target at the right edge. `None` when
    /// the band is too narrow to spare them.
    pub(super) fn header_plus_cols(&self) -> Option<std::ops::Range<usize>> {
        // Slim has no room for a card, so its header carries no `+`.
        if self.density == Density::Slim {
            return None;
        }
        let band_w = self.card_width();
        (band_w >= 12).then(|| band_w - 2..band_w)
    }

    /// Insert the open card under its workspace's header and one ghost line
    /// under each header whose held draft carries text. The post-pass runs on
    /// the finished rows, so every grouping and density shares it, and the
    /// card lands above the pinned cards the sort passes placed after the
    /// header. Slim has no room for a card.
    pub(super) fn with_draft_cards<'a>(
        &'a self,
        mut rows: Vec<DisplayRow<'a>>,
        mut depths: Vec<usize>,
    ) -> (Vec<DisplayRow<'a>>, Vec<usize>) {
        if self.density == Density::Slim {
            return (rows, depths);
        }
        let header_at = |rows: &[DisplayRow], squad: u64| {
            rows.iter().position(
                |r| matches!(r, DisplayRow::Sel(s) if s.tab.is_none() && s.squad == squad),
            )
        };
        let mut inserts: Vec<(usize, Vec<DisplayRow<'a>>)> = Vec::new();
        for (squad, held) in &self.launcher_closed {
            if held.draft.message.is_empty() {
                continue;
            }
            if let Some(at) = header_at(&rows, *squad) {
                inserts.push((at + 1, vec![DisplayRow::Ghost(*squad)]));
            }
        }
        if let Some(open) = self.launcher.as_ref() {
            // A card whose header is filtered out still paints, at the top,
            // so the keyboard owner is never invisible.
            let at = header_at(&rows, open.squad).map_or(0, |i| i + 1);
            let n = open.card_lines(self, self.card_width()).len();
            inserts.push((at, (0..n).map(DisplayRow::Card).collect()));
        }
        inserts.sort_by(|a, b| b.0.cmp(&a.0));
        for (at, ins) in inserts {
            for _ in 0..ins.len() {
                depths.insert(at, 0);
            }
            rows.splice(at..at, ins).for_each(drop);
        }
        (rows, depths)
    }
}

impl View {
    /// The sideline's display order (4a-G2): each squad's squad/tab rows, that
    /// squad's agent rows, then the `+ new workspace` footer, a catch-all
    /// section for agents matched to no squad, and the work-queue lane. The
    /// ONE row enumeration: painting, hover, mouse hit-testing, and
    /// the prefix+w selector all index into it.
    /// The sideline's rows for the CURRENT density - the one enumeration every
    /// consumer indexes into (: painting, hover, hit-test, and the
    /// selector share this index space in all three densities).
    ///
    /// A delegation to [`Self::display_rows_with_depths`]`.0`: one builder, so
    /// the card expansion hooks two places, not three.
    pub(super) fn display_rows(&self) -> Vec<DisplayRow<'_>> {
        self.display_rows_with_depths().0
    }

    /// [`Self::display_rows`] plus the lineage depth of each rendered
    /// row, aligned index-for-index. The depths come from the SAME pass that
    /// builds the rows, so they are computed over exactly the set that paints:
    /// an idle parent the fold removed, or an exited parent a LiveOnly section
    /// hides, is ABSENT, and its child roots instead of indenting under a row
    /// that never rendered. Non-agent rows (headers, sublines, spacers) carry
    /// 0. Slim filters the pair exactly as it filters rows; Extended retains
    /// the same lineage depths inside its agent-name cells.
    pub(super) fn display_rows_with_depths(&self) -> (Vec<DisplayRow<'_>>, Vec<usize>) {
        let (rows, depths) = match self.density {
            Density::Regular => {
                let (rows, depths) = self.tree_rows_with_depths();
                let (rows, depths) = self.sort_agent_runs(rows, depths);
                let (rows, depths) = self.pin_needs(rows, depths);
                self.card_rows(rows, depths)
            }
            Density::Slim => {
                let (rows, depths) = self.tree_rows_with_depths();
                // Small mode (q-334c5e9d option 1): one line per live agent,
                // `glyph slug`, beside the workspace headers. Exited rows
                // hide; the paint clips to the 16-column rail.
                let mut kept_rows = Vec::with_capacity(rows.len());
                let mut kept_depths = Vec::with_capacity(depths.len());
                for (r, d) in rows.into_iter().zip(depths) {
                    let keep = matches!(r, DisplayRow::Sel(s) if s.tab.is_none())
                        || matches!(r, DisplayRow::Header { .. })
                        || matches!(r, DisplayRow::Agent(a) if !a.exited);
                    if keep {
                        kept_rows.push(r);
                        kept_depths.push(d);
                    }
                }
                self.pin_needs(kept_rows, kept_depths)
            }
            Density::Extended => {
                let (rows, depths) = self.table_rows_with_depths();
                self.pin_needs(rows, depths)
            }
        };
        self.with_draft_cards(rows, depths)
    }

    // The sideline tree, with the top-K idle cap applied. A PURE
    // function of state: a squad shows its idle overflow only when the operator
    // toggled its `+N more` row open (`idle_expanded`). No per-frame,
    // selector-driven force-expand - that needs to know which row the selector
    // rests on, but the selector is an index INTO this very output, so any
    // attempt to resolve it here is circular: resolving against a rebuilt
    // (capped or uncapped) enumeration mis-identifies the row once a fold sits
    // above the cursor, and navigating onto a newly exposed overflow row then
    // collapses under the next render (codex P1/P2 on #566/#568). The invariant
    // that actually matters - the cursor never rests on a folded row - holds for
    // free: folded rows are absent from this list, so selector navigation only
    // ever lands on rendered rows. Overflow is reached by the fold row's own
    // Enter/click toggle, which persists in `idle_expanded`.
    /// This machine's tree rows with the per-row lineage depth beside it, computed over
    /// the EMITTED set after every display filter, never re-derived in the painter.
    /// [`Self::tree_rows_with_depths`] wraps them in the machine layer.
    pub(super) fn local_rows_with_depths(&self) -> (Vec<DisplayRow<'_>>, Vec<usize>) {
        if self.agent_group != AgentGroup::Workspace {
            return agent_group::grouped_rows_with_depths(self);
        }
        let mut out = Vec::new();
        // Display index -> lineage depth for agent rows, filled at emit time.
        let mut agent_depth_at: HashMap<usize, usize> = HashMap::new();
        // (US3) Section spacing only with more than one workspace: a
        // single squad has no groups to separate (US3 verify: absent with 1
        // squad).
        let multi_squad = self.layout.squads.len() > 1;
        for (idx, s) in self.layout.squads.iter().enumerate() {
            // One spacer between consecutive workspace groups (never before the
            // first, so no leading blank and never doubled).
            if multi_squad && idx > 0 {
                out.push(DisplayRow::Blank);
            }
            out.push(DisplayRow::Sel(SelRow {
                squad: s.id,
                tab: None,
            }));
            // Agents-first (Locked 4): the caret gates the squad's agent
            // rows; tab rows are gone (tabs live in the top tab bar). A collapsed
            // squad shows only its name row + the rollup glyph, so the
            // rollup is the sole signal there - no more agent rows rendering
            // unconditionally under a folded squad.
            // Tri-state: `LiveOnly` drops the exited rows in place
            // while the header's `✗N` rollup keeps them discoverable; live rows
            // keep their original order. Display filtering only - nothing is
            // reaped (that is).
            let key = section_key(s);
            let view = self.section_view(&key);
            if view != SectionView::Collapsed {
                let mut squad_agents: Vec<&AgentRow> = self
                    .layout
                    .agents
                    .iter()
                    .filter(|a| a.squad == Some(s.id))
                    .filter(|a| view == SectionView::Expanded || !a.exited)
                    .collect();
                // Order by lineage: children render beneath their
                // parent, pre-order. Keyed by row identity (input index), never
                // display name - two panes can share a label. A squad with no
                // parent edges keeps its input order (pane-tree, then
                // name-sorted watch-only), so it paints byte-identical.
                let (order, _) = lineage_layout(
                    &squad_agents,
                    |a| a.harness_session_id.as_deref(),
                    |a| lineage_parent(a),
                );
                squad_agents = order.into_iter().map(|i| squad_agents[i]).collect();
                // Top-K idle cap: attention rows (live, non-idle) always
                // render; idle rows fill to SQUAD_ROW_CAP live rows total; the
                // idle overflow folds into one `+N more` row. Dead rows (present
                // only in Expanded) sit OUTSIDE the budget under the view's
                // control, so `+N more` and the header's `✗N` never double-count.
                let attention = squad_agents
                    .iter()
                    .filter(|&a| !a.exited && !is_idle_row(a))
                    .count();
                let idle_total = squad_agents.iter().filter(|&a| is_idle_row(a)).count();
                let idle_budget = SQUAD_ROW_CAP.saturating_sub(attention);
                let hidden = idle_total.saturating_sub(idle_budget);
                // A squad shows all its idle rows only when the operator toggled
                // its fold open (persisted for the session in `idle_expanded`).
                let show_all_idle = self.idle_expanded.contains(&key);
                let mut idle_shown = 0usize;
                let mut emitted: Vec<&AgentRow> = Vec::with_capacity(squad_agents.len());
                for &a in &squad_agents {
                    if is_idle_row(a) && !show_all_idle {
                        if idle_shown >= idle_budget {
                            continue; // folded into the `+N more` row below
                        }
                        idle_shown += 1;
                    }
                    emitted.push(a);
                }
                // Depth over exactly the emitted set: an idle parent
                // the fold removed is ABSENT, so its visible child roots
                // instead of indenting under a row that never painted.
                let (_, emitted_depths) = lineage_layout(
                    &emitted,
                    |a| a.harness_session_id.as_deref(),
                    |a| lineage_parent(a),
                );
                for (a, depth) in emitted.iter().zip(emitted_depths) {
                    agent_depth_at.insert(out.len(), depth);
                    out.push(DisplayRow::Agent(a));
                }
                // Emit the fold row whenever there is idle overflow: `+N more`
                // when folded, `- fewer` when shown (so the expansion reverses
                // from the same spot). No row when nothing overflows (no `+0`).
                if hidden > 0 {
                    out.push(DisplayRow::IdleFold {
                        key: key.clone(),
                        hidden,
                        expanded: show_all_idle,
                    });
                }
            }
        }
        // One spacer sets the footer off from the workspace list above so the
        // `+ new workspace` / menu row doesn't read as another workspace. Gated on
        // `multi_squad` like the other US3 spacers (a lone workspace needs no
        // separation, and single-squad layouts stay byte-identical).
        if multi_squad {
            out.push(DisplayRow::Blank);
        }
        // The `+` create-workspace affordance sits directly under the squad list
        //, above the agents/work-queue sections.
        out.push(DisplayRow::NewSquad);
        let orphans = self.orphans();
        if !orphans.is_empty() {
            // Orphans (cwd matched no squad) keep one flat section in the same
            // row grammar; the header reads `~ elsewhere` (Locked 6). One spacer
            // precedes the header (US3), not doubled with the group
            // separators above (the `+ new workspace` footer sits between).
            if multi_squad {
                out.push(DisplayRow::Blank);
            }
            let rollup = section_rollup(orphans.iter().map(|&a| agent_lattice_state(a)));
            let view = self.section_view(&SectionKey::Elsewhere);
            out.push(DisplayRow::Header {
                label: "~ elsewhere".to_string(),
                rollup,
                key: SectionKey::Elsewhere,
                view,
            });
            // Orphans keep their line-1 ` (basename)` suffix (every orphan is
            // foreign by definition); a Sub row would double it, so the
            // `~ elsewhere` section emits no sublines (US3).
            if view != SectionView::Collapsed {
                let visible: Vec<&AgentRow> = orphans
                    .iter()
                    .filter(|a| view == SectionView::Expanded || !a.exited)
                    .copied()
                    .collect();
                // Same lineage ordering as the squad sections, over
                // this section's own visible set: an orphan spawned by another
                // orphan nests beneath it. Index-keyed, like the squads.
                let (order, depths) = lineage_layout(
                    &visible,
                    |a| a.harness_session_id.as_deref(),
                    |a| lineage_parent(a),
                );
                for &i in &order {
                    agent_depth_at.insert(out.len(), depths[i]);
                    out.push(DisplayRow::Agent(visible[i]));
                }
            }
        }
        // Materialize the aligned depth vec: agent rows carry the
        // depth their emit site recorded (keyed by display index); every other
        // row (headers, sublines, spacers, fold rows) indents nothing.
        let out_depths = (0..out.len())
            .map(|i| agent_depth_at.get(&i).copied().unwrap_or(0))
            .collect();
        (out, out_depths)
    }
}
