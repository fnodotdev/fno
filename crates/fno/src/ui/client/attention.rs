//! Attention ranking: what state a pane or agent row is in, and which row
//! needs a look first. Moved out of client.rs (file budget).

use super::*;

/// The rollup state of a pane/agent, worst-first. The navigator's state filter
///, the squad-row rollup, and seen/unseen surfacing
/// all consume it. Derived, never wire-serialized - computed from [`AgentBadge`]
/// + the seen bit at render time. The derive orders it `Blocked < Working <
/// DoneUnseen < Unmeasured < Idle < Empty`, so a squad rollup is
/// `agents.map(pane_state).min()` (the worst state wins - `min` and
/// this filter agree on the ordering). `Unmeasured` ranks worse than
/// every settled state: a no-reading row deserves a look before an idle one
/// does. `Empty` is a pristine shell - nothing running, nothing done,
/// the least severe reading there is.
///
/// NOT the same ordering as [`SEVERITY_ORDER`], and neither is "the single
/// ordering authority" the two comments used to each claim. They answer
/// different questions and have disagreed since before: this one
/// answers WHICH ROW IS WORST, for a `min()` rollup and the navigator filter,
/// so `Working` outranks `DoneUnseen`. `SEVERITY_ORDER` answers IN WHAT ORDER
/// A SECTION'S COUNTS ARE LISTED AND TRUNCATED, where a finished-but-unseen
/// row leads because it is the one asking for a human. Read whichever answers
/// the question at hand, and change neither to "match" the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum PaneState {
    Blocked,
    Working,
    DoneUnseen,
    Unmeasured,
    Idle,
    Empty,
}

/// Derive a [`PaneState`] from an agent's badge, whether its output has been
/// seen (`AgentRow.seen`, server-owned), and the pane's own OSC 133
/// activity reading (`AgentRow.pane_activity`). A present badge wins
/// (the registry worker's own in-TTL report); a `Done` badge folds to `Idle`
/// once seen, else `DoneUnseen`. With NO badge the vt reading decides, and an
/// absent/unmeasured reading renders `Unmeasured` - never a blind `Idle`, the
/// fold that drew four working panes and thirty empty shells as one circle.
pub(super) fn pane_state(
    badge: Option<AgentBadge>,
    seen: bool,
    activity: Option<ShellActivity>,
) -> PaneState {
    match badge {
        Some(AgentBadge::Blocked) => PaneState::Blocked,
        Some(AgentBadge::Working) => PaneState::Working,
        Some(AgentBadge::Done) if seen => PaneState::Idle,
        Some(AgentBadge::Done) => PaneState::DoneUnseen,
        None => match activity {
            // ACCEPTED COST, do not relitigate: a bare shell running `less`,
            // `nvim` or a long build also lands here, and over-surfacing a
            // shell costs one row while under-surfacing a working agent is
            // the founding defect this branch shipped to delete. A quiet
            // shell reads `Idle` (the user's ruling: a shell tab is live,
            // not a mystery to measure).
            Some(ShellActivity::Running) => PaneState::Working,
            Some(ShellActivity::Idle) => PaneState::Idle,
            Some(ShellActivity::Empty) => PaneState::Idle,
            Some(ShellActivity::Unmeasured) | None => PaneState::Idle,
        },
    }
}

/// Attention display window: a transcript-backed row silent longer than this
/// reads as neglected and floats to the top of the agents table. Deliberately
/// much tighter than session-truth's two-hour stall window - that one is the
/// reap-safety window and 7200s is correct for it, but its verdict reads
/// `reachable` for anything dead inside it, and that gap is exactly where a
/// stale-live worker hides. Ten minutes catches a twenty-minute floor with
/// headroom. Display and ordering only: no verdict, falsifier, or reap
/// decision keys off this constant.
pub(super) const STALE_ATTENTION_S: u64 = 600;

/// Where a row sits on the evidence-of-neglect scale. Built ONLY from fields
/// that carry their evidence with them (`basis`, `last_activity_age_s`,
/// `exited`, `unmeasured`) - never from `status` or the reachability verdict,
/// both of which read healthy for a worker dead under two hours, and never
/// from the in-TTL badge, which is a scraped report rather than a fact.
pub(super) fn evidence_rank(a: &AgentRow) -> u8 {
    let basis = a.basis.as_deref();
    // A fired falsifier, or an exit with positive corroboration (a confirmed
    // dead pid / gone pane): the sunk tier. Archived, snoozed and pane-dead
    // all collapse here - none of them needs the operator's attention now.
    // `exit-recorded` fires when reconcile already nulled the pid, which can
    // leave the mux's OWN `exited`/`unmeasured` pair reading dormant (
    // review: the probe saw the tombstone the mux's liveness derivation
    // cannot see); the basis string is the more complete reading here and
    // must win. Mirrors the falsifier set `fno agent list` and the daemon
    // projection both sink to their tier 5.
    if matches!(
        basis,
        Some("process-gone") | Some("pane-gone") | Some("exit-recorded")
    ) || (a.exited && !a.unmeasured)
    {
        return 5;
    }
    // Claiming to work, silent past the attention window: the row that most
    // needs a human is the row that says it is fine and is not.
    if basis == Some("transcript")
        && a.last_activity_age_s
            .is_some_and(|s| s >= STALE_ATTENTION_S)
    {
        return 0;
    }
    if basis == Some("silent") {
        return 1;
    }
    if basis == Some("no-evidence") {
        return 2;
    }
    // Dormant but resumable, with no corroboration either way: the operator
    // has to look before acting, which outranks a healthy worker.
    if a.exited && a.unmeasured {
        return 3;
    }
    // Genuinely working, no probe answer yet, or fresh transcript: needs
    // nothing. `basis: None` (an old server that never sends the field)
    // deliberately lands here too - absence of a reading is not urgency.
    4
}

/// The attention key: needs-me fold rank, then evidence of neglect, then
/// longest-silent first, then name so the table never shuffles on a scrape
/// tick. Term 3 treats an absent age as 0 (youngest): an absent reading has
/// two explanations and a sort cannot tell them apart, so it must never float
/// a row to the top.
pub(super) fn attention_key(
    a: &AgentRow,
    need: Option<NeedKind>,
) -> (u8, u8, std::cmp::Reverse<u64>, &str) {
    // `NeedKind`'s declaration order IS the severity contract (pinned by
    // test); `as u8` reads it without re-declaring a second authority.
    let need_rank = need.map_or(7, |k| k as u8);
    (
        need_rank,
        evidence_rank(a),
        std::cmp::Reverse(a.last_activity_age_s.unwrap_or(0)),
        a.name.as_str(),
    )
}

/// A LIVE idle row - the top-K cap's fold target. Exited is checked
/// first, exactly as [`agent_lattice_state`] does, so a dead worker (whose
/// `pane_state` also reads `Idle`) is never mistaken for live idle and swept
/// into `+N more`: dead rows are the section view's business, not the cap's.
/// The fold takes every NON-ATTENTION state, which after this branch
/// means three of them, not one. `pane_state` used to answer `Idle` for any
/// badgeless row; the split sends a pristine shell to `Empty` and a row with
/// no reading to `Unmeasured`, and `server.rs` hard-codes `pane_activity:
/// None` on watch-only paneless rows - so a bg `/target` worker between turns
/// is `Unmeasured`. Testing `== Idle` alone therefore counted every shell AND
/// every bg worker as `attention`, drove `idle_budget` to zero and killed the
/// fold outright on any real fleet.
///
/// Folding an unmeasured row is safe only because the fold row says `+N more`
/// rather than `+N idle`. An earlier revision of this comment claimed the `?`
/// glyph carried the honesty "wherever the row renders", which is exactly
/// wrong for a row the fold REMOVED: the count would then have stood in for a
/// measurement nothing took, on the surface this branch exists to fix. The
/// label carries it instead, and it asserts only that the rows are hidden.
///
/// A pinned row never folds: the pin is the operator's promise that the row
/// leads its workspace, and a fold would hide it behind `+N more`.
///
/// This predicate is display density, not truth. Attention states (`Blocked`,
/// `Working`, `DoneUnseen`) are enumerated by exclusion on purpose - a state
/// added later does not fold, so a new signal fails VISIBLE, never hidden.
pub(super) fn is_idle_row(a: &AgentRow) -> bool {
    !a.exited
        && !a.pinned
        && matches!(
            pane_state(a.badge, a.seen, a.pane_activity),
            PaneState::Idle | PaneState::Empty | PaneState::Unmeasured
        )
}

/// An agent row's icon-lattice state (US2): exit beats badge beats
/// liveness. Unlike [`nav_agent_state`] this keeps `Exited` distinct (`✗`)
/// rather than folding it to `Idle` - a row shows its own exit, but a squad/tab
/// rollup ignores it. Within the exited case, `unmeasured` further
/// splits `Exited` (`✗`, confirmed dead, respawn is safe) from `Unmeasured`
/// (`?`, no corroboration, look before you spawn) - the operator's routing
/// decision turns on telling the two apart at a glance. The non-exit case goes
/// through `pane_state`, so the row respects the `seen` bit exactly
/// as the nav/tab rollups do: a Done pane the operator has already viewed
/// folds to `Idle` (`○`) instead of holding a stale bold `✓` needs-attention
/// marker - one system across every surface.
pub fn agent_lattice_state(a: &AgentRow) -> LatticeState {
    if a.exited {
        if a.unmeasured {
            LatticeState::Unmeasured
        } else {
            LatticeState::Exited
        }
    } else {
        pane_to_lattice(pane_state(a.badge, a.seen, a.pane_activity))
    }
}

/// Fold a tab's LIVE panes to their worst lattice state for the tab-strip
/// rollup (US4). Exited panes are filtered BEFORE the fold, so `None`
/// (no glyph) means "no live panes" - an empty tab or an all-exited tab, which
/// both render byte-identically to a stateless tab (AC2-EDGE). A tab with live
/// panes always rolls up, so a live-idle tab yields `Some(Idle)` -> the outline
/// `○` (the tab state machine distinguishes a live-idle tab from a dead one).
/// Severity is `PaneState`'s Ord (Blocked < Working < DoneUnseen < Idle), so
/// `.min()` is the worst pane.
pub(super) fn tab_rollup_state(
    agents: &[AgentRow],
    squad: u64,
    tab: TabId,
) -> Option<LatticeState> {
    let worst = agents
        .iter()
        .filter(|a| a.squad == Some(squad) && a.tab == Some(tab) && !a.exited)
        .map(nav_agent_state)
        .min()?;
    Some(pane_to_lattice(worst))
}

/// The lane fg for one agent row, shared by both sideline arms:
/// the fixed cascade over the row's axes, with the lattice color standing
/// on Blocked and DoneUnseen (attention and done are never re-colored) and
/// the lattice fg as the fallback when nothing declares the lane.
pub(super) fn agent_lane_fg(a: &AgentRow, st: LatticeState, fallback: Color) -> Color {
    if matches!(st, LatticeState::Blocked | LatticeState::DoneUnseen) {
        return fallback;
    }
    sideline_color::resolve_lane_color(
        a.harness.as_deref(),
        a.model.as_deref(),
        a.route.as_deref(),
        a.account.as_deref(),
    )
    .unwrap_or(fallback)
}

/// The sideline/nav fold state maps 1:1 onto the lattice (no `Exited` - a folded
/// pane's exit is already flattened to `Idle` by `nav_agent_state`).
pub(super) fn pane_to_lattice(s: PaneState) -> LatticeState {
    match s {
        PaneState::Blocked => LatticeState::Blocked,
        PaneState::Working => LatticeState::Working,
        PaneState::DoneUnseen => LatticeState::DoneUnseen,
        PaneState::Unmeasured => LatticeState::Unmeasured,
        PaneState::Idle => LatticeState::Idle,
        PaneState::Empty => LatticeState::Empty,
    }
}
