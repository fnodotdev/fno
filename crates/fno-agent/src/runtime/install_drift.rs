//! install-drift: one lead-beat reading that flags a deployed fno-agent
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
#[derive(Deserialize, Default, Debug)]
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
    let home = crate::paths::AgentsHome::from_env();
    let cache = crate::reclaim::reclaim_state_root(&home)
        .join("install")
        .join("source-path");
    reading_from(
        resolve_installed_binary(),
        cwd,
        Some(cache.to_string_lossy().into_owned()),
    )
}

fn reading_from(binary: Option<String>, cwd: &Path, cache: Option<String>) -> ReadResult {
    let binary = binary.ok_or_else(|| {
        "installed fno-agent binary not found beside the running exe or on PATH".to_string()
    })?;
    let binary_rev = binary_version(&binary)
        .and_then(|v| v.crates_rev)
        .filter(|rev| !rev.is_empty() && rev != "unknown");
    let Some(binary_rev) = binary_rev else {
        let detail = revless_detail(&binary);
        return Ok(unknown(&binary, None, None, &detail));
    };
    let Some(root) = source_root(cwd, cache) else {
        return Ok(unknown(
            &binary,
            Some(&binary_rev),
            None,
            "no fno source checkout resolved",
        ));
    };
    let Some(source_rev) = subtree_rev(&root) else {
        return Ok(unknown(
            &binary,
            Some(&binary_rev),
            None,
            "the source checkout's crates/ subtree rev is unreadable",
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
            behind_count(&root, &binary_rev, &source_rev),
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
/// dev target and `$FNO_AGENT_BIN` are deliberately absent, matching the
/// doctor resolver's installed-first contract.
fn resolve_installed_binary() -> Option<String> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("fno-agent");
            if sibling.is_file() {
                return Some(sibling.to_string_lossy().into_owned());
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("fno-agent");
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

fn binary_version(binary: &str) -> Option<BinaryVersion> {
    let out = version_json_raw(binary)?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// The unfiltered `version --json` answer, kept raw so a rev-less answer
/// can name its evidence instead of reading blind.
fn version_json_raw(binary: &str) -> Option<std::process::Output> {
    let mut cmd = Command::new(binary);
    cmd.arg("version").arg("--json");
    crate::bounded_cmd::output_with_timeout(cmd, PROBE_BUDGET_S)
}

/// The human `--version` line. A failure-path probe: it runs only when the
/// json read gave no rev. Whitespace collapses, because the answer feeds a
/// one-line beat line whatever the binary printed.
fn human_version_line(binary: &str) -> Option<String> {
    let mut cmd = Command::new(binary);
    cmd.arg("--version");
    let out = crate::bounded_cmd::output_with_timeout(cmd, PROBE_BUDGET_S)?;
    if !out.status.success() {
        return None;
    }
    let line = flatten_line(&String::from_utf8_lossy(&out.stdout));
    if line.is_empty() {
        None
    } else {
        Some(line)
    }
}

/// One line out of possibly many: whitespace runs collapse to single
/// spaces, so a banner-printing binary cannot split the beat line.
fn flatten_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The detail a rev-less answer reports: which binary, what its json read
/// returned, what its human line says. The 2026-11-28 blind read was
/// diagnosed by hand; this composes the same evidence into the beat line.
fn revless_detail(binary: &str) -> String {
    let mut parts = vec!["installed binary reports no crates/ rev".to_string()];
    match version_json_raw(binary) {
        Some(out) => {
            let code = out
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".to_string());
            parts.push(format!("{binary} version --json exit {code}"));
            let head: String = String::from_utf8_lossy(&out.stdout)
                .chars()
                .take(160)
                .collect();
            let head = flatten_line(&head);
            if !head.is_empty() {
                parts.push(format!("stdout: {head}"));
            }
        }
        None => parts.push(format!("{binary} version --json failed or timed out")),
    }
    match human_version_line(binary) {
        Some(line) => parts.push(format!("--version: {line}")),
        None => parts.push("--version: unreadable".to_string()),
    }
    parts.join("; ")
}

/// The source checkout through the same ladder an update installs from:
/// the main checkout owning `cwd` first, the install cache second. No
/// override, no candidates: this is a read, never an install decision.
fn source_root(cwd: &Path, cache: Option<String>) -> Option<PathBuf> {
    let checkout = crate::paths::canonical_repo_root(cwd)
        .map(|root| root.join("cli").to_string_lossy().into_owned());
    let answer = crate::source_pin::resolve_for_health(checkout, cache);
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
            "install: diverged (binary {} is not an ancestor of source {}; lacks {} crates/ commit(s))",
            short8(drift.binary_rev.as_deref()),
            short8(drift.source_rev.as_deref()),
            count(drift.behind_n)
        ),
        _ => format!(
            "install: unmeasured ({})",
            drift.detail.as_deref().unwrap_or("reason unnamed")
        ),
    }
}

fn count(n: Option<i64>) -> String {
    n.map(|n| n.to_string()).unwrap_or_else(|| "?".to_string())
}

/// The derived-diff attention line: attention outranks "no change", so a
/// silent beat still surfaces the gap. Stale always raises it. Diverged
/// raises it when the binary lacks one or more merged crates/ commits: a
/// binary built off a side branch runs pre-HEAD bytes just as a stale one
/// does, and only an empty gap means its crates/ code is current.
pub(crate) fn attention_line(drift: &Drift) -> Option<String> {
    match drift.status.as_deref() {
        Some("stale") => {}
        Some("diverged") if drift.behind_n.is_some_and(|n| n > 0) => {
            return Some(format!(
                "deployed fno-agent is diverged and lacks {} merged crates/ commit(s) (binary {} is not an ancestor of source {}); hooks and gates run pre-HEAD bytes; remedy: fno doctor update",
                count(drift.behind_n),
                short8(drift.binary_rev.as_deref()),
                short8(drift.source_rev.as_deref())
            ));
        }
        _ => return None,
    }
    Some(format!(
        "deployed fno-agent lacks {} merged crates/ commit(s) (built {}..{}); hooks and gates run pre-HEAD bytes; remedy: fno doctor update",
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
        let env = [
            ("GIT_AUTHOR_NAME", "t"),
            ("GIT_AUTHOR_EMAIL", "t@t"),
            ("GIT_COMMITTER_NAME", "t"),
            ("GIT_COMMITTER_EMAIL", "t@t"),
        ];
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(dir.to_str().unwrap()).args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = crate::bounded_cmd::output_with_timeout(cmd, PROBE_BUDGET_S)
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
        String::from_utf8_lossy(&git_out(dir, &["rev-parse", "HEAD"]).unwrap().stdout)
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
        crate::write_exec_stub(dir, "fake-fno-agent", &body)
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
        let binary = fake_binary(bin_dir.path(), &first);
        // A second crates/ commit the fake binary predates.
        let second = commit_crates(&root, "c2");
        let drift = drift_of(
            &reading_from(
                Some(binary),
                &root,
                Some("/nonexistent/install/source-path".into()),
            )
            .unwrap(),
        );
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
        let binary = fake_binary(bin_dir.path(), &rev);
        let drift = drift_of(
            &reading_from(
                Some(binary),
                &root,
                Some("/nonexistent/install/source-path".into()),
            )
            .unwrap(),
        );
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
        let binary = fake_binary(bin_dir.path(), &side);
        let drift = drift_of(
            &reading_from(
                Some(binary),
                &root,
                Some("/nonexistent/install/source-path".into()),
            )
            .unwrap(),
        );
        assert_eq!(drift.status.as_deref(), Some("diverged"), "{drift:?}");
        assert_ne!(drift.binary_rev.as_deref(), Some(base.as_str()));
    }

    #[test]
    fn unknown_when_the_binary_reports_no_rev() {
        let (_guard, root, _rev) = source_repo("unknown-rev-repo");
        let bin_dir = tempfile::tempdir().unwrap();
        let binary = fake_binary(bin_dir.path(), "unknown");
        let drift = drift_of(
            &reading_from(
                Some(binary),
                &root,
                Some("/nonexistent/install/source-path".into()),
            )
            .unwrap(),
        );
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
        let binary = fake_binary(bin_dir.path(), "0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f");
        let drift = drift_of(
            &reading_from(
                Some(binary),
                nowhere.path(),
                Some("/nonexistent/install/source-path".into()),
            )
            .unwrap(),
        );
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

    #[test]
    fn diverged_raises_attention_only_when_it_lacks_merged_commits() {
        let mut diverged = Drift {
            status: Some("diverged".into()),
            behind_n: Some(4),
            binary_rev: Some("d3cc76b9d680".into()),
            source_rev: Some("c6bc448a43cd".into()),
            ..Default::default()
        };
        let line = attention_line(&diverged).expect("4 missing commits raise attention");
        assert!(line.contains("lacks 4 merged"), "{line}");
        assert!(line.contains("fno doctor update"), "{line}");
        diverged.behind_n = Some(0);
        assert_eq!(attention_line(&diverged), None);
    }

    /// A binary answering in the real `version_json` shape, with the fixture
    /// rev spliced in as crates_rev: the version string the probe parses is
    /// the one version.rs prints, not a test-local invention.
    fn fake_binary_real_shape(dir: &Path, crates_rev: &str) -> String {
        let mut v = crate::version::version_json();
        v.as_object_mut()
            .expect("version_json is an object")
            .insert(
                "crates_rev".to_string(),
                serde_json::Value::String(crates_rev.to_string()),
            );
        let body = format!(
            "#!/bin/bash\necho '{}'\n",
            serde_json::to_string(&v).unwrap()
        );
        crate::write_exec_stub(dir, "fake-fno-agent", &body)
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn reads_fresh_and_stale_off_the_real_version_json_shape() {
        let (_guard, root, first) = source_repo("real-shape-repo");
        let bin_dir = tempfile::tempdir().unwrap();
        let binary = fake_binary_real_shape(bin_dir.path(), &first);
        let fresh = drift_of(
            &reading_from(
                Some(binary.clone()),
                &root,
                Some("/nonexistent/install/source-path".into()),
            )
            .unwrap(),
        );
        assert_eq!(fresh.status.as_deref(), Some("fresh"), "{fresh:?}");
        // A second crates/ commit the binary predates: same real shape, stale read.
        commit_crates(&root, "c2");
        let stale_bin = tempfile::tempdir().unwrap();
        let stale_binary = fake_binary_real_shape(stale_bin.path(), &first);
        let stale = drift_of(
            &reading_from(
                Some(stale_binary),
                &root,
                Some("/nonexistent/install/source-path".into()),
            )
            .unwrap(),
        );
        assert_eq!(stale.status.as_deref(), Some("stale"), "{stale:?}");
        assert_eq!(stale.behind_n, Some(1), "{stale:?}");
    }

    #[test]
    fn revless_detail_names_path_exit_and_human_line() {
        let (_guard, root, _rev) = source_repo("revless-detail-repo");
        let bin_dir = tempfile::tempdir().unwrap();
        // Answer version --json with a rev-less object; --version with the
        // human line a real build prints. Both answers carry a second line,
        // so the detail must collapse them: the beat line is one line.
        let body = "#!/bin/bash\nif [ \"$1\" = \"version\" ]; then echo '{}'; echo 'banner line'; else echo 'fno-agent 0.0.17 (deadbeef1234, release)'; echo 'second line'; fi\n";
        let binary = crate::write_exec_stub(bin_dir.path(), "fake-fno-agent", body)
            .to_string_lossy()
            .into_owned();
        let drift = drift_of(
            &reading_from(
                Some(binary.clone()),
                &root,
                Some("/nonexistent/install/source-path".into()),
            )
            .unwrap(),
        );
        assert_eq!(drift.status.as_deref(), Some("unknown"), "{drift:?}");
        let detail = drift.detail.as_deref().unwrap_or_default();
        assert!(detail.contains("no crates/ rev"), "{detail}");
        assert!(detail.contains("exit 0"), "{detail}");
        assert!(detail.contains(&binary), "{detail}");
        assert!(
            detail.contains("fno-agent 0.0.17 (deadbeef1234, release)"),
            "{detail}"
        );
        assert!(
            detail.contains("banner line") && !detail.contains('\n'),
            "the detail stays one line: {detail}"
        );
    }
}
