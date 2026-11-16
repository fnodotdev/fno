//! Tests for the install-side downgrade guard. The ancestry contract needs
//! real commits: each fixture builds a tiny git repo in a tempdir.

use crate::install::downgrade_guard::{probe_rev, refuse_downgrade};
use std::path::Path;

/// One identity-configured empty commit in the fixture repo.
fn commit_as_t(dir: &std::path::Path, msg: &str) {
    assert!(std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", msg])
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .current_dir(dir)
        .output()
        .expect("git runs")
        .status
        .success());
}

/// One repo: c1 (base) <- c2 (newer) on main, plus an unrelated `side` commit.
fn repo_fixture() -> (tempfile::TempDir, String, String, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    run(&["init", "-q", "-b", "main"]);
    commit_as_t(dir.path(), "c1");
    let base = run(&["rev-parse", "HEAD"]);
    commit_as_t(dir.path(), "c2");
    let newer = run(&["rev-parse", "HEAD"]);
    run(&["checkout", "-q", "-b", "side"]);
    commit_as_t(dir.path(), "c3");
    let side = run(&["rev-parse", "side"]);
    (dir, base, newer, side)
}

#[test]
fn a_descending_candidate_installs() {
    let (dir, base, newer, _side) = repo_fixture();
    assert!(refuse_downgrade(dir.path(), &base, &newer).is_none());
}

#[test]
fn an_older_candidate_refuses() {
    let (dir, base, newer, _side) = repo_fixture();
    let err = refuse_downgrade(dir.path(), &newer, &base)
        .unwrap()
        .expect("refusal");
    assert!(err.contains("is not an ancestor"), "{err}");
    assert!(err.contains(&newer[..12]), "{err}");
    assert!(err.contains(&base[..12]), "{err}");
}

#[test]
fn an_unrelated_sidegrade_refuses() {
    let (dir, _base, newer, side) = repo_fixture();
    let err = refuse_downgrade(dir.path(), &newer, &side)
        .unwrap()
        .expect("refusal");
    assert!(err.contains("is not an ancestor"), "{err}");
}

#[test]
fn an_unknown_installed_rev_passes() {
    let (dir, _base, newer, _side) = repo_fixture();
    assert!(refuse_downgrade(dir.path(), "unknown", &newer).is_none());
}

#[test]
fn a_foreign_candidate_commit_passes() {
    let (dir, base, _newer, _side) = repo_fixture();
    assert!(refuse_downgrade(
        dir.path(),
        &base,
        "0123456789abcdef0123456789abcdef01234567"
    )
    .is_none());
}

#[test]
fn equal_revs_reinstall() {
    let (dir, base, _newer, _side) = repo_fixture();
    assert!(refuse_downgrade(dir.path(), &base, &base).is_none());
}

#[test]
fn the_probe_names_an_absent_or_nonrev_binary_unknown() {
    assert_eq!(probe_rev(Path::new("/nonexistent/fno-agent")), "unknown");
}
