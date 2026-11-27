//! The unified needs-me queue, worst-first: the live badge leg (this session's
//! blocked and done-unseen rows) merged with the event-fold leg.

use super::*;

impl View {
    /// The unified needs-me queue, worst-first, sorted `(kind, ts, name)`. Owned
    /// rows so a per-key mutation of `answers` never aliases the borrow.
    pub(super) fn needs_queue(&self) -> Vec<NeedRow> {
        let mut rows: Vec<NeedRow> = Vec::new();

        // Leg 1: live badge rows from the current layout (no shell-out).
        for a in &self.layout.agents {
            if is_blocked_row(a) {
                let kind = if a.answerable.is_some() {
                    NeedKind::BlockedAnswerable
                } else {
                    NeedKind::BlockedFocusOnly
                };
                let reason = a.reason.clone().unwrap_or_else(|| {
                    if a.answerable.is_some() {
                        "needs an answer".into()
                    } else {
                        "needs focus".into()
                    }
                });
                rows.push(NeedRow {
                    kind,
                    name: a.name.clone(),
                    reason,
                    ts: String::new(),
                    id_key: a.name.clone(),
                    answerable: a.answerable.clone(),
                    pane_id: a.pane_id,
                    attach_id: a.attach_id.clone(),
                    squad: a.squad,
                    tab: a.tab,
                });
            } else if !a.exited
                && pane_state(a.badge, a.seen, a.pane_activity) == PaneState::DoneUnseen
            {
                rows.push(NeedRow {
                    kind: NeedKind::DoneUnseen,
                    name: a.name.clone(),
                    reason: a.reason.clone().unwrap_or_else(|| "done, unseen".into()),
                    ts: String::new(),
                    id_key: a.name.clone(),
                    answerable: None,
                    pane_id: a.pane_id,
                    attach_id: a.attach_id.clone(),
                    squad: a.squad,
                    tab: a.tab,
                });
            }
        }

        // Leg 2: event-fold items, joined to a roster row, else rendered
        // squadless when live, else dropped (a dead session's stale stop must
        // not nag forever - Locked 5).
        if let Some(items) = &self.needs_fold {
            for item in items {
                let kind = match item.kind.as_str() {
                    "review_wedged" => NeedKind::ReviewWedged,
                    "budget_stop" => NeedKind::BudgetStop,
                    "mail_question" => NeedKind::MailQuestion,
                    "operator_question" => NeedKind::Question,
                    "carveout_stale" | "stale_claims" => NeedKind::Decision,
                    _ => continue,
                };
                match self.join_fold_row(item) {
                    Some(a) => rows.push(NeedRow {
                        kind,
                        name: a.name.clone(),
                        reason: item.evidence.clone(),
                        ts: item.ts.clone(),
                        id_key: item.session_id.clone(),
                        answerable: None,
                        pane_id: a.pane_id,
                        attach_id: a.attach_id.clone(),
                        squad: a.squad,
                        tab: a.tab,
                    }),
                    None if item.live => rows.push(NeedRow {
                        kind,
                        name: item
                            .name
                            .clone()
                            .or_else(|| item.node.clone())
                            .unwrap_or_else(|| item.session_id.clone()),
                        reason: item.evidence.clone(),
                        ts: item.ts.clone(),
                        id_key: item.session_id.clone(),
                        answerable: None,
                        pane_id: None,
                        attach_id: None,
                        squad: None,
                        tab: None,
                    }),
                    None => {} // unjoined + not live: drop (stale-nag guard)
                }
            }
        }

        rows.sort_by(|a, b| {
            a.kind
                .cmp(&b.kind)
                .then_with(|| a.ts.cmp(&b.ts))
                .then_with(|| a.name.cmp(&b.name))
        });
        rows
    }
}

/// Why a session needs a human, worst-first. Declaration order IS the
/// severity contract: the needs-me queue is `(kind, ts, name)`-sorted, so the
/// worst band leads and the longest-waiting fold item tops its band (a live
/// badge row carries no ts, so it degenerates to name order within its band -
/// leg-1 and leg-2 never share a band, so the two orderings never mix). Same
/// declaration-order `Ord` trick as [`PaneState`]. `Decision` is fed
/// by two `needs.rs` fold arms - `carveout_stale` (an aged unharvested
/// carve-out pile), `stale_claims` (an aged orphaned `node:` claim pile) -
/// read from durable on-disk state rather than a recent event, so neither is
/// windowed by the fold's 24h `since` bound. `Question` is the third
/// arm, `operator_question`; it is split out from `Decision` because the
/// overlay renders it from a richer, separately-folded leg
/// ([`crate::needs_overlay::QuestionItem`]) instead - `Question` on a
/// `NeedRow` exists only so a blocked worker's roster badge still reflects an
/// open question; `View::needs_operator_queue` filters `Question` out of the
/// overlay for exactly that reason (the richer row replaces it there).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum NeedKind {
    /// Leads the order: an open operator question outranks everything
    /// downstream of it, including a mail-blocked worker.
    Question,
    /// An aged carve-out/claims pile gone stale - a decision with no asker to
    /// answer back to, so it stays a plain `NeedRow` even in the overlay.
    Decision,
    // A human-addressed mail escalation (question, or a live-miss to an
    // operator-attended recipient). Sits above the worker blocked-states: a
    // human the mail is waiting on outranks a worker awaiting focus.
    MailQuestion,
    BlockedAnswerable,
    BlockedFocusOnly,
    ReviewWedged,
    BudgetStop,
    DoneUnseen,
}

/// The leading glyph per need kind, matching the sideline's [`nav_glyph`]
/// vocabulary where they overlap (blocked `▲`, done `✓`).
pub(super) fn need_glyph(k: NeedKind) -> char {
    match k {
        NeedKind::Question | NeedKind::Decision => '⁉',
        NeedKind::MailQuestion => '✉',
        NeedKind::BlockedAnswerable | NeedKind::BlockedFocusOnly => '▲',
        NeedKind::ReviewWedged => '⏳',
        NeedKind::BudgetStop => '⏹',
        NeedKind::DoneUnseen => '✓',
    }
}

/// One unified needs-me-queue row: a live badge row (leg 1) or an event-fold
/// item joined to the roster (leg 2), reduced to what the overlay renders and
/// routes on. Identity for cursor re-anchor is `(kind, name)`.
#[derive(Clone)]
pub(crate) struct NeedRow {
    pub(super) kind: NeedKind,
    pub(super) name: String,
    pub(super) reason: String,
    /// The deciding event ts for a fold row (oldest-first tie-break); `""` for a
    /// live badge row (name-ordered within its band).
    pub(super) ts: String,
    /// A STABLE re-anchor key: a fold row's session id (survives a joined <->
    /// squadless flip, where `name` changes), a badge row's name. Not shown.
    pub(super) id_key: String,
    /// Present only on a `BlockedAnswerable` row - the digit-answer payload.
    pub(super) answerable: Option<AnswerablePrompt>,
    pub(super) pane_id: Option<u64>,
    pub(super) attach_id: Option<String>,
    pub(super) squad: Option<u64>,
    pub(super) tab: Option<u64>,
}

impl NeedRow {
    /// The re-anchor identity: a scrape tick / fold merge keeps the cursor on
    /// the same item, not the same index (AC3-UI). Keyed on `(kind, id_key)` -
    /// a stable session id for fold rows so a joined<->squadless transition (its
    /// `name` flips) does not drop the cursor (codex P2).
    pub(super) fn id(&self) -> (NeedKind, String) {
        (self.kind, self.id_key.clone())
    }
}

#[derive(Clone)]
pub(crate) enum NeedsOverlayRow {
    Mine(crate::needs_overlay::MineItem),
    Question(crate::needs_overlay::QuestionItem),
    Need(NeedRow),
}

impl NeedsOverlayRow {
    #[cfg(test)]
    pub(super) fn label(&self) -> &str {
        match self {
            Self::Mine(item) => &item.text,
            Self::Question(q) => &q.title,
            Self::Need(row) => &row.name,
        }
    }

    pub(super) fn id(&self) -> NeedsOverlayId {
        match self {
            Self::Mine(item) => NeedsOverlayId::Mine(item.n),
            Self::Question(q) => NeedsOverlayId::Question(q.id.clone()),
            Self::Need(row) => {
                let (kind, key) = row.id();
                NeedsOverlayId::Need(kind, key)
            }
        }
    }

    pub(super) fn need(&self) -> Option<&NeedRow> {
        match self {
            Self::Need(row) => Some(row),
            Self::Mine(_) | Self::Question(_) => None,
        }
    }

    /// The rich question payload, when this row is one - `answer_keys`' digit
    /// and Enter arms read this rather than `.need()` (a question is not a
    /// `NeedRow`; it has no pane, an asker instead).
    pub(super) fn question(&self) -> Option<&crate::needs_overlay::QuestionItem> {
        match self {
            Self::Question(q) => Some(q),
            Self::Mine(_) | Self::Need(_) => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum NeedsOverlayId {
    Mine(usize),
    Question(String),
    Need(NeedKind, String),
}

/// Cap on rendered rows (worst-first, so the cap drops only the least severe);
/// the footer states the drop count. Matches the sideline card cap.
pub(super) const NEEDS_CAP: usize = 10;

pub(super) const MINE_CAP: usize = 10;

/// Re-open cache: a fold younger than this is reused instantly (mashing
/// `prefix+a` never re-shells - Perspective B).
pub(super) const NEEDS_CACHE_TTL: Duration = Duration::from_secs(5);

/// Default fold window: the last 24h (the fold also windows server-side).
pub(super) const NEEDS_WINDOW_SECS: u64 = 24 * 60 * 60;

/// The footer state of the needs-me overlay: whether the event-fold leg is
/// still in flight, failed (loud degrade, AC2-ERR), or landed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NeedsFooter {
    Folding,
    Degraded,
    AsOf,
}
