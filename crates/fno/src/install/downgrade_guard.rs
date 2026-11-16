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

/// None when an install may replace `installed` with `candidate`, judged in
/// the checkout at `repo`; Some names the refusal. Equal or unknown revs
/// pass: an unknown installed rev is a first install or a pre-rev-stamp
/// binary; an unknown candidate is a foreign channel artifact the doctor
/// staleness verdict still governs. A refusal names both revs and the
/// repair. The guard reads fail closed: a commit-type or ancestry read that
/// cannot answer refuses the install and names the override, because a
/// guard that passes on an unread answer lets the exact downgrade through
/// in the field.
pub(crate) fn refuse_downgrade(repo: &Path, installed: &str, candidate: &str) -> Option<String> {
    verdict(repo, installed, candidate, &git_ancestor)
}

/// The verdict core, parameterized on the ancestry reader so a test can
/// inject a read that cannot answer.
pub(crate) fn verdict(
    repo: &Path,
    installed: &str,
    candidate: &str,
    ancestor: &dyn Fn(&Path, &str, &str) -> Option<bool>,
) -> Option<String> {
    if installed == candidate || installed == "unknown" || candidate == "unknown" {
        return None;
    }
    // A real "not a commit" answer allows: a foreign channel artifact or a
    // first install. An answer that cannot be read refuses, fail closed.
    match (known_commit(repo, installed), known_commit(repo, candidate)) {
        (Some(true), Some(true)) => {}
        (Some(_), Some(_)) => return None,
        _ => {
            return Some(format!(
                "fno doctor update: refusing the install: the commit-type read \
                 could not answer (installed {} vs candidate {}); git spawn or \
                 plumbing failed. Rerun with --force to override, or repair git \
                 and retry.",
                short(installed),
                short(candidate)
            ))
        }
    }
    match ancestor(repo, installed, candidate) {
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
        None => Some(format!(
            "fno doctor update: refusing the install: the ancestry read could not \
             answer (installed {} vs candidate {}); git spawn or plumbing failed. \
             Rerun with --force to override, or repair git and retry.",
            short(installed),
            short(candidate)
        )),
    }
}

/// `Some(true)` when `rev` names a commit object in `repo`, `Some(false)`
/// when git answers and it does not, `None` when git cannot answer (spawn
/// killed, no exit code). The kill is not an answer: the caller refuses.
fn known_commit(repo: &Path, rev: &str) -> Option<bool> {
    let out = run_admitted(|| {
        let mut cmd = crate::process_admission::std_command("git");
        cmd.arg("-C")
            .arg(repo)
            .args(["cat-file", "-t", rev])
            .stdin(Stdio::null());
        cmd
    })?;
    match out.status.code() {
        Some(0) => Some(String::from_utf8_lossy(&out.stdout).trim() == "commit"),
        Some(_) => Some(false),
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
/// clippy-disallowed. Both streams are piped here because `wait_with_output`
/// captures only piped streams and a default Command inherits; an inherited
/// stdout reads back empty, which a reader would mistake for a real empty
/// answer.
fn run_admitted(build: impl FnOnce() -> std::process::Command) -> Option<std::process::Output> {
    let mut cmd = build();
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
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
