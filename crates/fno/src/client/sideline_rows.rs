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
