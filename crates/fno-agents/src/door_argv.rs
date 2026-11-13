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

#[cfg(test)]
mod tests {
    use super::{FNO_ARGVS, MERGE_CLOSE_RECONCILE};
    use fno::agents_menu::{classify, Agents};
    use std::ffi::OsString;

    /// Law D3141 (command must exist): every door argv row resolves to a
    /// real command. The runtime spawns `crate::scrape::fno_bin()` with a
    /// row of [`FNO_ARGVS`] - daemon arms, the check-in reader, the lead
    /// loop - and those run for months unwatched, so a rename that drops a
    /// word fails this test instead of failing a tick log. agents rows walk
    /// the front crate's live menu - the surface the org->hq rename broke
    /// for team_ledger and the check-in drain. mux rows hold against the
    /// generated native tree. Rows on other roots (the merge-close arm's
    /// `backlog reconcile`) stay covered end to end by their arm's own
    /// tests plus the baseline ratchet, and are asserted here to stay on
    /// those roots so an unexpected root change cannot sneak in.
    #[test]
    fn every_door_argv_row_resolves_to_a_real_command() {
        let tree = include_str!("../../../scripts/ci/native-command-tree.txt");
        for row in FNO_ARGVS {
            let argv: &[&str] = row;
            let Some((root, _)) = argv.split_first() else {
                panic!("door argv row is empty");
            };
            match root {
                "agents" => {
                    let args: Vec<OsString> = argv.iter().map(OsString::from).collect();
                    match classify(&args) {
                        Some(Agents::Run(_)) => {}
                        other => panic!(
                            "door argv {argv:?} does not resolve through the agents menu: {other:?}"
                        ),
                    }
                }
                "mux" => {
                    let words: Vec<&str> = argv
                        .iter()
                        .take_while(|w| !w.starts_with('-'))
                        .copied()
                        .collect();
                    let path = words.join(" ");
                    assert!(
                        tree.lines()
                            .any(|l| l.starts_with(&path.as_str())
                                && l[path.len()..].starts_with('\t')),
                        "door argv {argv:?} is not a native-tree row: {path}"
                    );
                }
                "backlog" => {
                    // The backlog row carries no menu or tree row: hold it
                    // to its exact words here, where the baseline ratchet
                    // cannot see it.
                    assert_eq!(
                        argv, MERGE_CLOSE_RECONCILE,
                        "the merge-close reconcile argv changed; re-check the door it names"
                    );
                }
                other => panic!("door argv root {other:?} has no resolver here"),
            }
        }
    }
}
