//! The install-side downgrade guard. On 2026-11-16 a writer outside the
//! update door put a 33b47750fe72 build back over a fresher install and PR
//! 284's merge ran on it: the update legs answer to a freshness verdict, the
//! stray writer answered to nothing. Every install write answers here: a
//! candidate that does not descend from the installed rev is a downgrade or
//! an unrelated sidegrade, never an update.
//!
//! The release/adopt install stays the deliberate lane (`--force`,
//! `--published <rev>`, and the release installer carry an operator's
//! explicit choice), so the guard yields to them and is wired only into the
//! update door's install arms.

use std::path::Path;
use std::process::Stdio;

/// Ok when an install may replace `installed` with `candidate`, judged in
/// the checkout at `repo`. Equal or unknown revs pass: an unknown installed
/// rev is a first install or a pre-rev-stamp binary; an unknown candidate is
/// a foreign channel artifact the doctor staleness verdict still governs.
/// A refusal names both revs and the repair. An unreadable git reads as no
/// verdict and passes - absence fails open exactly like a crashed check, and
/// the doctor staleness verdict still governs.
pub(crate) fn refuse_downgrade(
    repo: &Path,
    installed: &str,
    candidate: &str,
) -> Result<(), String> {
    let known_commit = |rev: &str| {
        crate::install::doctor_update::git_in(repo, &["cat-file", "-t", rev]).as_deref()
            == Some("commit")
    };
    if installed == candidate
        || installed == "unknown"
        || candidate == "unknown"
        || !known_commit(installed)
        || !known_commit(candidate)
    {
        return Ok(());
    }
    match git_ancestor(repo, installed, candidate) {
        Some(true) => Ok(()),
        Some(false) => Err(format!(
            "fno doctor update: refusing the install: installed rev {} is not an \
             ancestor of candidate {}; the candidate is a downgrade or an unrelated \
             build. Install a rev that descends from {}, or replace the bytes by \
             hand if this is a deliberate rollback.",
            short(installed),
            short(candidate),
            short(installed)
        )),
        // git itself unreadable: no verdict, fail open.
        None => Ok(()),
    }
}

/// The 12-char prefix of a rev, on a char boundary: a probe or marker can
/// carry text that is not hex, and a raw byte slice would panic on it.
fn short(rev: &str) -> &str {
    let mut end = rev.len().min(12).max(0);
    while end > 0 && !rev.is_char_boundary(end) {
        end -= 1;
    }
    &rev[..end]
}

/// The installed rev a probe of `bin` reports, "unknown" when the binary is
/// absent, does not run, or reports no rev (the front door reports none
/// today, so its refresh passes through until it stamps revs).
pub(crate) fn probe_rev(bin: &Path) -> String {
    let out = crate::process_admission::std_command(bin)
        .arg("version")
        .arg("--json")
        .stdin(Stdio::null())
        .output();
    let Ok(out) = out else {
        return "unknown".into();
    };
    if !out.status.success() {
        return "unknown".into();
    }
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .ok()
        .and_then(|v| {
            v.get("git_rev")
                .and_then(|r| r.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".into())
}

/// `Some(true)` when `older` is an ancestor of `newer`, `Some(false)` when
/// git answers no, `None` when git cannot answer (not a repo, plumbing
/// failure). `git_in` collapses exit 1 into None, so this helper reads the
/// exit code itself.
fn git_ancestor(repo: &Path, older: &str, newer: &str) -> Option<bool> {
    let mut cmd = crate::process_admission::std_command("git");
    cmd.arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", older, newer])
        .stdin(Stdio::null());
    let out = cmd.output().ok()?;
    match out.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}
