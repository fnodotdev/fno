//! The daemon's worktree sweep: one report per repo on a 6h floor.
//! Report-only until a merge proof stands - a minted cleanup order, or the
//! command's own dry-run judgment for an outside merge - then applying.
//! Moved out of daemon.rs (shrink-only file) with its tests; the event
//! carries `enumerated` and `judged` alongside the judged-bucket counts so a
//! truncated read cannot be told from a partial sweep.
use crate::events::EventEmitter;
use crate::paths::AgentsHome;
use serde_json::{json, Value};

/// How long between worktree report sweeps. A 24-hour reap order spans at
/// least three complete windows even when its mint cannot clear the stamp.
pub(crate) const WORKTREE_SWEEP_INTERVAL_SECS: u64 = 21_600;

/// Distinct canonical repo roots the registry knows about, deduplicated.
///
/// A linked worktree is not its own repo, so its rows fold into the checkout
/// that owns them and the sweep runs once per repo rather than once per row.
/// Each root is canonicalised before dedupe: raw registry paths could spell
/// the same repository four ways (symlinked bases, /var vs /private/var on
/// macOS), and one repo read as four roots swept four times per window and
/// collided on the sweep lock (measured 2026-09-14T22:31:45Z). A row whose
/// fallback cwd resolves to no repository is skipped: a head launched
/// outside any repo is not a sweepable root.
pub(crate) fn registry_repo_roots(home: &AgentsHome) -> Vec<String> {
    let Ok(loaded) = crate::state::load_registry(&home.registry_json()) else {
        return Vec::new();
    };
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for e in &loaded.entries {
        let raw = if e.project_root.is_empty() {
            e.cwd.clone()
        } else {
            e.project_root.clone()
        };
        if raw.is_empty() {
            continue;
        }
        let root = std::path::Path::new(&raw);
        if !root.is_dir() {
            continue;
        }
        // A head registered from a cwd outside any repo (HOME launch, empty
        // project_root) used to enter the set as the raw dir and the sweep
        // read HOME for a repo that never lived there. Unresolvable means
        // skip, not sweep-as-is.
        let Some(canonical) =
            crate::paths::canonical_repo_root(root).map(|p| p.to_string_lossy().into_owned())
        else {
            continue;
        };
        seen.insert(canonical);
    }
    // The request read spans the rotated generation too (merge_reap's reader),
    // so a repo whose only request rotated aside stays in the roots.
    for repo in crate::merge_reap::merge_cleanup_request_repos(home) {
        if std::path::Path::new(&repo).is_dir() {
            seen.insert(repo);
        }
    }
    seen.into_iter().collect()
}

/// One repo's worktree-sweep reading, parsed from the command's `Summary:` line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorktreeSweepReport {
    pub eligible: usize,
    pub kept: usize,
    pub dirty: usize,
    pub enumerated: Option<usize>,
    pub judged: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeSweepOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeSweepOrderRead {
    pub standing: Option<bool>,
    pub exit_code: Option<i32>,
    pub stderr: String,
}

impl From<bool> for WorktreeSweepOrderRead {
    fn from(standing: bool) -> Self {
        Self {
            standing: Some(standing),
            exit_code: Some(0),
            stderr: String::new(),
        }
    }
}

/// Parse `fno agent workspace worktree cleanup --merged`'s summary line.
///
/// Returns `None` rather than a zeroed report when the line is absent. A sweep
/// that could not read its own output must not report "0 eligible, 0 dirty",
/// which is indistinguishable from a clean machine: an absence has two
/// explanations and a count must only ever come from a real reading.
///
/// The command differs by mode (`would archive` dry-run vs `archived` apply), so
/// the eligible count reads from whichever the line carries. `enumerated` /
/// `judged` are Option: a summary from an older script carries neither, and
/// an absent token is never a fabricated zero.
pub fn parse_worktree_sweep(stdout: &str) -> Option<WorktreeSweepReport> {
    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("Summary:"))?;
    let num_before = |needle: &str| -> Option<usize> {
        let idx = line.find(needle)?;
        line[..idx].split_whitespace().last()?.parse().ok()
    };
    let num_after = |needle: &str| -> Option<usize> {
        let idx = line.find(needle)?;
        line[idx + needle.len()..]
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    };
    let eligible = num_before(" would archive").or_else(|| num_before(" archived"))?;
    Some(WorktreeSweepReport {
        eligible,
        kept: num_before(" kept (")?,
        dirty: num_before(" dirty")?,
        enumerated: num_after("enumerated "),
        judged: num_after("judged "),
    })
}

/// The sweep's apply authority for one repository, decided from evidence
/// reads alone.
///
/// A pending `merge_cleanup_requested` envelope is the fno-merge proof. An
/// outside merge (`gh pr merge`, the web button) mints no envelope, so with
/// none standing the command's own dry-run summary is the same proof in git
/// form: a tree it would archive reads merged-or-done and clean, and the
/// `--apply` pass re-judges every guard tree by tree. An unreadable or
/// empty dry-run authorizes nothing: a probe that cannot read never
/// widens the sweep's authority.
pub fn apply_authority(pending: bool, dry_run_summary: Option<&str>) -> bool {
    pending
        || dry_run_summary
            .and_then(parse_worktree_sweep)
            .is_some_and(|report| report.eligible > 0)
}

/// Worktree sweep, one line per repo, on a 6h floor: report-only until a
/// merge proof stands, then applying.
///
/// A timer tick proves nothing on its own, so an unearned tick still only
/// REPORTS. Removal is merge-evidenced: `fno do pr merge` (and the post-merge
/// ritual, as its second mint site) writes the `merge_cleanup_requested`
/// envelope, and while a pending request stands for a repository (`orders`
/// injects that scoped read) that repository's pass runs with `--apply`. An
/// outside merge mints no envelope, so `orders` may also read the command's own
/// dry-run judgment there (`apply_authority` carries that policy). The
/// primary consumer is the merge reaper (merge_reap.rs), which stops the
/// harness, drops the rows, and takes the tree; this sweep only catches what
/// that pass leaves behind. The sweep's own guards - reapable, live claim,
/// rooted processes - still decide tree by tree. There is no config knob,
/// because two off-switches for one decision strand whoever flips the wrong
/// one.
///
/// `orders` and `run` are injected so the policy is testable without shelling
/// out.
pub fn worktree_sweep(
    home: &AgentsHome,
    emitter: &EventEmitter,
    now: i64,
    roots: &[String],
    orders: &dyn Fn(&str) -> WorktreeSweepOrderRead,
    run: &dyn Fn(&str, bool) -> WorktreeSweepOutput,
) -> usize {
    let stamp = home.root().join("worktree-sweep.stamp");
    let last = std::fs::read_to_string(&stamp)
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0);
    if now.saturating_sub(last) < WORKTREE_SWEEP_INTERVAL_SECS as i64 {
        return 0;
    }
    let mut swept = 0;
    for root in roots {
        let order_read = orders(root);
        let Some(apply) = order_read.standing else {
            let stderr = order_read.stderr.lines().next().unwrap_or("");
            let _ = emitter.emit(
                "worktree_sweep",
                &json!({
                    "repo": root,
                    "error": "unreadable-orders",
                    "exit_code": order_read.exit_code,
                    "stderr": stderr,
                }),
            );
            continue;
        };
        let mode = if apply { "apply-orders" } else { "report-only" };
        // Emit for EVERY repo, including the ones that read zero. A tick that
        // stays silent when it finds nothing cannot be told from a tick that
        // never ran, and this sweep exists precisely to surface what the
        // ritual missed.
        let output = run(root, apply);
        let report = (output.exit_code == Some(0))
            .then(|| parse_worktree_sweep(&output.stdout))
            .flatten();
        match report {
            Some(r) => {
                let mut payload = json!({
                    "repo": root,
                    "eligible": r.eligible,
                    "kept": r.kept,
                    "dirty": r.dirty,
                    "mode": mode,
                });
                if let Some(n) = r.enumerated {
                    payload["enumerated"] = Value::from(n);
                }
                if let Some(n) = r.judged {
                    payload["judged"] = Value::from(n);
                }
                let _ = emitter.emit("worktree_sweep", &payload);
                swept += 1;
            }
            None => {
                let stderr = output.stderr.lines().next().unwrap_or("");
                let _ = emitter.emit(
                    "worktree_sweep",
                    &json!({
                        "repo": root,
                        "mode": mode,
                        "error": "unreadable-summary",
                        "exit_code": output.exit_code,
                        "stderr": stderr,
                    }),
                );
            }
        }
    }
    let _ = std::fs::write(&stamp, now.to_string());
    swept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::AgentsHome;

    fn tmp_home(tag: &str) -> AgentsHome {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "fno-agent-wt-sweep-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = AgentsHome::at(&p);
        home.ensure_root().unwrap();
        home
    }

    #[test]
    fn sweep_skips_a_repo_when_its_order_probe_is_unreadable() {
        let home = tmp_home("order-unreadable");
        let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
        let ran_cleanup = std::sync::atomic::AtomicBool::new(false);

        let swept = worktree_sweep(
            &home,
            &emitter,
            1_000_000,
            &["/repo/a".into()],
            &|_| WorktreeSweepOrderRead {
                standing: None,
                exit_code: Some(7),
                stderr: "claim store unreadable\nextra detail\n".into(),
            },
            &|_, _| {
                ran_cleanup.store(true, std::sync::atomic::Ordering::Relaxed);
                unreachable!("an unreadable order probe must skip cleanup")
            },
        );

        assert_eq!(swept, 0);
        assert!(!ran_cleanup.load(std::sync::atomic::Ordering::Relaxed));
        let log = crate::events::committed_journal_text(&home.events_jsonl());
        assert!(log.contains("\"error\":\"unreadable-orders\""));
        assert!(log.contains("\"exit_code\":7"));
        assert!(log.contains("\"stderr\":\"claim store unreadable\""));
        assert!(!log.contains("extra detail"));
        assert!(!log.contains("report-only"));
    }

    #[test]
    fn apply_authority_no_false_authority() {
        // A probe that cannot read never widens the sweep's authority, and
        // zero eligible trees is not a proof either.
        let zero = "Summary: 0 would archive, 4 kept (2 unmerged, 1 dirty), 0 failed\n";
        assert!(!apply_authority(false, Some(zero)));
        assert!(!apply_authority(false, None));
        assert!(!apply_authority(false, Some("no summary line\n")));
    }
}
