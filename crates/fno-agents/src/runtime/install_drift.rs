//! install-drift: one lead-beat reading that flags a deployed fno-agents
//! binary whose build predates crates/ commits the source checkout carries.
//!
//! The specimen (lead-cf20 retro, w11 08:04:03): gate fixes merged while this
//! machine's deployed binary predated them, and nothing surfaced the gap
//! until a lead diagnosed it by hand. Doctor answers the same question on
//! demand; this reading answers it in the beat the lead already reads. The
//! probe is four bounded subprocess calls: resolve the deployed binary, read
//! its self-reported `crates_rev`, resolve the source checkout through the
//! source-pin ladder, then compare subtree revs with ancestry direction.

use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lead_checkin_beat_budget::ReadResult;

/// The wall clock for one bounded git or version probe. The probes answer
/// single revs, far below any pipe buffer; the bound exists for a wedged
/// filesystem, not for output volume.
const PROBE_BUDGET_S: u64 = 5;

/// The typed payload the beat journals, renders, and diffs. `status` is
/// `fresh | stale | diverged | unknown`; the other fields name the evidence
/// or, on unknown, the reason.
#[derive(Deserialize, Default)]
pub(crate) struct Drift {
    pub(crate) status: Option<String>,
    pub(crate) behind_n: Option<i64>,
    pub(crate) binary_rev: Option<String>,
    pub(crate) source_rev: Option<String>,
    pub(crate) detail: Option<String>,
}

#[derive(Deserialize)]
struct BinaryVersion {
    #[serde(default)]
    crates_rev: Option<String>,
}

/// The beat reading. A hard failure (no deployed binary at all) is
/// `Err`, so the beat prints READER FAILED; every measured state, including
/// "unmeasurable on this machine", lands as a payload.
pub(crate) fn reading(cwd: &Path) -> ReadResult {
    reading_from(resolve_installed_binary(), cwd)
}

fn reading_from(binary: Option<String>, cwd: &Path) -> ReadResult {
    let binary = binary.ok_or_else(|| {
        "installed fno-agents binary not found beside the running exe or on PATH".to_string()
    })?;
    let binary_rev = binary_version(&binary)
        .and_then(|v| v.crates_rev)
        .filter(|rev| !rev.is_empty() && rev != "unknown");
    let Some(binary_rev) = binary_rev else {
        return Ok(unknown(
            &binary,
            None,
            None,
            "installed binary reports no crates/ rev",
        ));
    };
    let Some(source_rev) = source_root(cwd).and_then(|root| subtree_rev(&root)) else {
        return Ok(unknown(
            &binary,
            Some(&binary_rev),
            None,
            "no fno source checkout resolved, or its crates/ subtree rev is unreadable",
        ));
    };
    if source_rev == binary_rev {
        return Ok(payload(
            "fresh",
            &binary,
            Some(&binary_rev),
            Some(&source_rev),
            None,
            None,
        ));
    }
    let Some(root) = source_root(cwd) else {
        return Ok(unknown(
            &binary,
            Some(&binary_rev),
            Some(&source_rev),
            "source checkout root vanished mid-reading",
        ));
    };
    match is_ancestor(&root, &binary_rev, &source_rev) {
        Some(true) => {
            let behind = behind_count(&root, &binary_rev, &source_rev);
            Ok(payload(
                "stale",
                &binary,
                Some(&binary_rev),
                Some(&source_rev),
                behind,
                None,
            ))
        }
        Some(false) => Ok(payload(
            "diverged",
            &binary,
            Some(&binary_rev),
            Some(&source_rev),
            None,
            None,
        )),
        None => Ok(unknown(
            &binary,
            Some(&binary_rev),
            Some(&source_rev),
            "git merge-base --is-ancestor could not run",
        )),
    }
}

fn payload(
    status: &str,
    binary: &str,
    binary_rev: Option<&str>,
    source_rev: Option<&str>,
    behind_n: Option<i64>,
    detail: Option<&str>,
) -> Value {
    json!({
        "status": status,
        "behind_n": behind_n,
        "binary": binary,
        "binary_rev": binary_rev,
        "source_rev": source_rev,
        "detail": detail,
    })
}

fn unknown(
    binary: &str,
    binary_rev: Option<&str>,
    source_rev: Option<&str>,
    detail: &str,
) -> Value {
    payload(
        "unknown",
        binary,
        binary_rev,
        source_rev,
        None,
        Some(detail),
    )
}

/// The deployed set only: sibling of the running exe, then PATH. The cargo
/// dev target and `$FNO_AGENTS_BIN` are deliberately absent, matching the
/// doctor resolver's installed-first contract.
fn resolve_installed_binary() -> Option<String> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("fno-agents");
            if sibling.is_file() {
                return Some(sibling.to_string_lossy().into_owned());
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("fno-agents");
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

fn binary_version(binary: &str) -> Option<BinaryVersion> {
    let mut cmd = Command::new(binary);
    cmd.arg("version").arg("--json");
    let out = crate::bounded_cmd::output_with_timeout(cmd, PROBE_BUDGET_S)?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// The source checkout through the same ladder an update installs from:
/// the main checkout owning `cwd` first, the install cache second. No
/// override, no candidates: this is a read, never an install decision.
fn source_root(cwd: &Path) -> Option<PathBuf> {
    let home = crate::paths::AgentsHome::from_env();
    let cache = crate::reclaim::reclaim_state_root(&home)
        .join("install")
        .join("source-path");
    let cache = cache.to_string_lossy().into_owned();
    let checkout = crate::paths::canonical_repo_root(cwd)
        .map(|root| root.join("cli").to_string_lossy().into_owned());
    let answer = crate::source_pin::resolve_for_health(checkout, Some(cache));
    let cli = answer.path?;
    Path::new(&cli).parent().map(Path::to_path_buf)
}

/// One bounded git probe at the checkout root.
fn git_out(root: &Path, args: &[&str]) -> Option<std::process::Output> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root);
    for arg in args {
        cmd.arg(arg);
    }
    crate::bounded_cmd::output_with_timeout(cmd, PROBE_BUDGET_S)
}

/// The last commit that touched crates/ (doctor's `rust_subtree_rev`
/// probe: Python-era commits never flag the rust bins stale).
fn subtree_rev(root: &Path) -> Option<String> {
    let out = git_out(root, &["log", "-1", "--format=%H", "--", "crates/"])?;
    if !out.status.success() {
        return None;
    }
    let rev = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if rev.is_empty() {
        None
    } else {
        Some(rev)
    }
}

/// Some(true) when `binary_rev` is an ancestor of `source_rev` (the binary
/// predates merged work), Some(false) when git proves it is not, None when
/// the instrument cannot answer, so the caller refuses to guess.
fn is_ancestor(root: &Path, binary_rev: &str, source_rev: &str) -> Option<bool> {
    let out = git_out(
        root,
        &["merge-base", "--is-ancestor", binary_rev, source_rev],
    )?;
    match out.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// The crates/ commits the binary lacks.
fn behind_count(root: &Path, binary_rev: &str, source_rev: &str) -> Option<i64> {
    let range = format!("{binary_rev}..{source_rev}");
    let out = git_out(root, &["rev-list", "--count", &range, "--", "crates/"])?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn short8(rev: Option<&str>) -> String {
    rev.and_then(|r| r.get(..8).map(str::to_string))
        .unwrap_or_else(|| "?".to_string())
}

/// Lenient parse of one reading's payload, the way the beat renders it.
pub(crate) fn parse_drift(value: &Value) -> Drift {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

/// One beat line. Fresh stays quiet; every other state names its evidence
/// and, on stale, the remedy.
pub(crate) fn beat_line(drift: &Drift) -> String {
    match drift.status.as_deref() {
        Some("fresh") => "install: fresh".to_string(),
        Some("stale") => format!(
            "install: STALE - lacks {} merged crates/ commit(s); remedy: fno doctor update",
            drift
                .behind_n
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".to_string())
        ),
        Some("diverged") => format!(
            "install: diverged (binary {} is not an ancestor of source {})",
            short8(drift.binary_rev.as_deref()),
            short8(drift.source_rev.as_deref())
        ),
        _ => format!(
            "install: unmeasured ({})",
            drift.detail.as_deref().unwrap_or("reason unnamed")
        ),
    }
}

/// The derived-diff attention line, `Some` only when the reading is stale:
/// attention outranks "no change", so a silent beat still surfaces the gap.
pub(crate) fn attention_line(drift: &Drift) -> Option<String> {
    if drift.status.as_deref() != Some("stale") {
        return None;
    }
    Some(format!(
        "deployed fno-agents lacks {} merged crates/ commit(s) (built {}..{}); hooks and gates run pre-HEAD bytes; remedy: fno doctor update",
        drift
            .behind_n
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".to_string()),
        short8(drift.binary_rev.as_deref()),
        short8(drift.source_rev.as_deref())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// One git command, panicking on failure - fixtures abort loudly.
    fn git_in(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-C", dir.to_str().unwrap()])
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("git binary available");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A commit touching crates/, so the subtree rev moves.
    fn commit_crates(dir: &Path, name: &str) -> String {
        let file = dir.join("crates").join("fno").join(format!("{name}.rs"));
        fs::write(file, "pub fn stub() {}\n").unwrap();
        git_in(dir, &["add", "-A"]);
        git_in(dir, &["commit", "-q", "-m", name]);
        git_in(dir, &["rev-parse", "HEAD"]);
        String::from_utf8_lossy(
            &Command::new("git")
                .args(["-C", dir.to_str().unwrap(), "rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string()
    }

    /// An fno-source-shaped repo: `crates/fno/Cargo.toml` naming the package,
    /// the marker `is_fno_source` validates.
    fn source_repo(tag: &str) -> (tempfile::TempDir, PathBuf, String) {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join(tag);
        fs::create_dir_all(root.join("crates/fno")).unwrap();
        fs::write(
            root.join("crates/fno/Cargo.toml"),
            "[package]\nname = \"fno\"\nversion = \"0.0.0\"\n",
        )
        .unwrap();
        git_in(&root, &["init", "-q"]);
        let rev = commit_crates(&root, "c1");
        (base, root, rev)
    }

    /// An executable stub that answers `version --json` with `crates_rev`.
    fn fake_binary(dir: &Path, crates_rev: &str) -> String {
        let body = format!(
            "#!/bin/bash\necho '{{\"package\":\"0.1.0\",\"git_rev\":\"{crates_rev}\",\"crates_rev\":\"{crates_rev}\"}}'\n"
        );
        crate::write_exec_stub(dir, "fake-fno-agents", &body)
            .to_string_lossy()
            .into_owned()
    }

    /// Parse a reading payload leniently, the way the beat renders it.
    fn drift_of(value: &Value) -> Drift {
        serde_json::from_value(value.clone()).unwrap_or_default()
    }

    #[test]
    fn stale_names_the_missing_commit_count() {
        let (_guard, root, first) = source_repo("stale-repo");
        let bin_dir = tempfile::tempdir().unwrap();
        let binary = fake_binary(&bin_dir, &first);
        // A second crates/ commit the fake binary predates.
        let second = commit_crates(&root, "c2");
        let drift = drift_of(&reading_from(Some(binary), &root).unwrap());
        assert_eq!(drift.status.as_deref(), Some("stale"), "{drift:?}");
        assert_eq!(drift.behind_n, Some(1), "{drift:?}");
        assert_eq!(drift.binary_rev.as_deref(), Some(first.as_str()));
        assert_eq!(drift.source_rev.as_deref(), Some(second.as_str()));
        assert!(
            attention_line(&drift)
                .unwrap()
                .contains("remedy: fno doctor update"),
            "the stale attention line names the remedy"
        );
    }

    #[test]
    fn fresh_reads_when_the_binary_matches_the_source() {
        let (_guard, root, rev) = source_repo("fresh-repo");
        let bin_dir = tempfile::tempdir().unwrap();
        let binary = fake_binary(&bin_dir, &rev);
        let drift = drift_of(&reading_from(Some(binary), &root).unwrap());
        assert_eq!(drift.status.as_deref(), Some("fresh"), "{drift:?}");
        assert!(
            attention_line(&drift).is_none(),
            "fresh raises no attention"
        );
    }

    #[test]
    fn diverged_reads_when_the_binary_is_not_an_ancestor() {
        let (_guard, root, base) = source_repo("diverged-repo");
        git_in(&root, &["checkout", "-q", "-b", "side"]);
        let side = commit_crates(&root, "side-commit");
        git_in(&root, &["checkout", "-q", "-"]);
        let bin_dir = tempfile::tempdir().unwrap();
        let binary = fake_binary(&bin_dir, &side);
        let drift = drift_of(&reading_from(Some(binary), &root).unwrap());
        assert_eq!(drift.status.as_deref(), Some("diverged"), "{drift:?}");
        assert_ne!(drift.binary_rev.as_deref(), Some(base.as_str()));
    }

    #[test]
    fn unknown_when_the_binary_reports_no_rev() {
        let (_guard, root, _rev) = source_repo("unknown-rev-repo");
        let bin_dir = tempfile::tempdir().unwrap();
        let binary = fake_binary(&bin_dir, "unknown");
        let drift = drift_of(&reading_from(Some(binary), &root).unwrap());
        assert_eq!(drift.status.as_deref(), Some("unknown"), "{drift:?}");
        assert!(
            drift
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("no crates/ rev"),
            "the detail names the missing rev: {drift:?}"
        );
    }

    #[test]
    fn unknown_when_no_source_resolves() {
        let nowhere = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let binary = fake_binary(&bin_dir, "0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f");
        let drift = drift_of(&reading_from(Some(binary), nowhere.path()).unwrap());
        assert_eq!(drift.status.as_deref(), Some("unknown"), "{drift:?}");
        assert!(
            drift
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("no fno source"),
            "the detail names the missing checkout: {drift:?}"
        );
    }

    #[test]
    fn beat_line_covers_every_status() {
        let fresh = Drift {
            status: Some("fresh".into()),
            ..Default::default()
        };
        assert_eq!(beat_line(&fresh), "install: fresh");
        let stale = Drift {
            status: Some("stale".into()),
            behind_n: Some(5),
            binary_rev: Some("d3cc76b9d680".into()),
            source_rev: Some("c6bc448a43cd".into()),
            ..Default::default()
        };
        let line = beat_line(&stale);
        assert!(line.contains("STALE"), "{line}");
        assert!(line.contains('5'), "{line}");
        assert!(line.contains("fno doctor update"), "{line}");
        let unknown = Drift {
            status: Some("unknown".into()),
            detail: Some("why".into()),
            ..Default::default()
        };
        assert_eq!(beat_line(&unknown), "install: unmeasured (why)");
    }
}
