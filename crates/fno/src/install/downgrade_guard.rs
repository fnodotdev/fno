//! The install-side downgrade guard. A writer outside the update door once
//! put an older, unrelated build back over a fresher install and a merge ran
//! on it: the update legs answer to a freshness verdict, the stray writer
//! answered to nothing. Every install write answers here: a candidate that
//! does not descend from the installed rev is a downgrade or an unrelated
//! sidegrade, never an update.
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
/// None when an install may replace `installed` with `candidate`, judged in
/// the checkout at `repo`; Some names the refusal. Equal or unknown revs
/// pass: an unknown installed rev is a first install or a pre-rev-stamp
/// binary; an unknown candidate is a foreign channel artifact the doctor
/// staleness verdict still governs. A refusal names both revs and the
/// repair. An unreadable git reads as no verdict and passes - absence fails
/// open exactly like a crashed check, and the doctor staleness verdict still
/// governs.
pub(crate) fn refuse_downgrade(repo: &Path, installed: &str, candidate: &str) -> Option<String> {
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
        return None;
    }
    match git_ancestor(repo, installed, candidate) {
        Some(true) => None,
        Some(false) => Some(format!(
            "fno doctor update: refusing the install: installed rev {} is not an \
             ancestor of candidate {}; the candidate is a downgrade or an unrelated \
             build. Install a descendant of {} or replace the bytes by hand for a \
             deliberate rollback.",
            short(installed),
            short(candidate),
            short(installed)
        )),
        // git itself unreadable: no verdict, fail open.
        None => None,
    }
}

/// The 12-char prefix of a rev, or the whole rev when that is not a char
/// boundary: a probe can carry text that is not hex and a byte slice panics.
fn short(rev: &str) -> &str {
    rev.get(..12).unwrap_or(rev)
}

/// The installed rev a probe of `bin` reports, "unknown" when the binary is
/// absent, does not run, or reports no rev (the front door reports none
/// today, so its refresh passes through until it stamps revs).
pub(crate) fn probe_rev(bin: &Path) -> String {
    let out = run_admitted(|| {
        let mut cmd = crate::process_admission::std_command(bin);
        cmd.arg("version").arg("--json").stdin(Stdio::null());
        cmd
    });
    let Some(out) = out else {
        return "unknown".into();
    };
    if !out.status.success() {
        return "unknown".into();
    }
    #[derive(serde::Deserialize)]
    struct ProbeRev {
        #[serde(default)]
        git_rev: Option<String>,
    }
    serde_json::from_slice::<ProbeRev>(&out.stdout)
        .ok()
        .and_then(|v| v.git_rev)
        .unwrap_or_else(|| "unknown".into())
}

/// Spawn through the process-admission seam and collect the output. The one
/// runner for this module: bare `Command::output` bypasses admission and is
/// clippy-disallowed.
fn run_admitted(build: impl FnOnce() -> std::process::Command) -> Option<std::process::Output> {
    let mut cmd = build();
    crate::process_admission::std_spawn(&mut cmd)
        .ok()?
        .wait_with_output()
        .ok()
}

/// `Some(true)` when `older` is an ancestor of `newer`, `Some(false)` when
/// git answers no, `None` when git cannot answer (not a repo, plumbing
/// failure). `git_in` collapses exit 1 into None, so this helper reads the
/// exit code itself.
fn git_ancestor(repo: &Path, older: &str, newer: &str) -> Option<bool> {
    let out = run_admitted(|| {
        let mut cmd = crate::process_admission::std_command("git");
        cmd.arg("-C")
            .arg(repo)
            .args(["merge-base", "--is-ancestor", older, newer])
            .stdin(Stdio::null());
        cmd
    })?;
    match out.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}
