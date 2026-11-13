//! The argvs the Rust runtime spawns through the `fno` front door, one const
//! per caller: the daemon arms, the check-in reader and the lead loop. Law
//! D3141 (command must exist): the fno crate resolves every row of
//! [`FNO_ARGVS`] against the live command surface in a unit test, so a
//! rename that drops a word fails the test instead of failing a fleet arm's
//! tick or a check-in's reading. Rows are the single source the call sites
//! pass to `Command`, so the table cannot drift from what actually spawns.

/// The team_ledger arm's page render
/// ([`run_ledger`](crate::backlog::rundown::run_ledger)). The
/// `hq chart rundown` door injects the faithful-root `--out`; a bare
/// `lead ledger` names lead.html, which the /team route does not read.
pub const TEAM_LEDGER: &[&str] = &["agents", "hq", "chart", "rundown"];

/// The worktree sweep's merged-cleanup pass (fleet_arms cleanup_command).
pub const WORKTREE_CLEANUP: &[&str] = &["agents", "workspace", "worktree", "cleanup", "--merged"];

/// The merge-close arm's board reconcile (gate/merge_close.rs).
pub const MERGE_CLOSE_RECONCILE: &[&str] = &["backlog", "reconcile", "--json"];

/// The pane keeper's raw key injection (fleet_arms pane send). Prefix only:
/// the pane id and the key bytes are dynamic tails.
pub const PANE_SEND: &[&str] = &["mux", "pane", "send"];

/// The scope drain read (lead_checkin r_drain, loop_lead's undelivered
/// probe). Prefix only: the scope word is a dynamic tail.
pub const HQ_DRAIN: &[&str] = &["agents", "hq", "chart", "drain"];

/// Every row for the resolver test. A caller that grows a door spawn joins
/// this list in the same PR as its call site.
pub const FNO_ARGVS: &[&[&str]] = &[
    TEAM_LEDGER,
    WORKTREE_CLEANUP,
    MERGE_CLOSE_RECONCILE,
    PANE_SEND,
    HQ_DRAIN,
];
