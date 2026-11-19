// Remote-less repo tests: no origin remote is a terminal local fact. The
// done-node shortcut takes the advisory terminal on local evidence, and a
// failed PR read names the remedy instead of promising a retry that can
// never happen. Every fixture is a real temp git repo (local git only, no
// network, no gh).
use super::*;

fn git(repo: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn temp_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().to_path_buf();
    git(&repo, &["init", "-q"]);
    (tmp, repo)
}

fn local_output_for(repo: &std::path::Path) -> impl Fn(&[&str]) -> Option<String> + '_ {
    move |args: &[&str]| {
        git_bounded("git", args, repo)
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

#[test]
fn remoteless_repo_reports_no_origin_url() {
    let (tmp, repo) = temp_repo();
    let read = local_output_for(&repo);
    assert_eq!(origin_remote_url(&read), None);

    git(
        &repo,
        &["remote", "add", "origin", "https://example.com/o/r.git"],
    );
    let read = local_output_for(&repo);
    assert_eq!(
        origin_remote_url(&read).as_deref(),
        Some("https://example.com/o/r.git")
    );
    drop(tmp);
}

#[test]
fn remoteless_clean_checkout_reaches_the_no_pr_terminal() {
    // The probe shape: one committed file, clean tree, no origin remote.
    // The unshipped conjunct must not fire, so the NoPr arm's DoneAdvisory
    // terminal is reachable.
    let (tmp, repo) = temp_repo();
    // A repo with no commit at all stays unshipped even with a clean tree:
    // the HEAD read is load-bearing against an empty checkout.
    let empty = local_output_for(&repo);
    assert!(unshipped_or_dirty(true, &empty));
    std::fs::write(repo.join("calc.py"), "def add(a, b):\n    return a + b\n").unwrap();
    git(&repo, &["add", "."]);
    git(
        &repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "slice",
        ],
    );
    let read = local_output_for(&repo);
    assert!(!unshipped_or_dirty(true, &read));

    // The same checkout with dirt stays unshipped: uncommitted work is not
    // delivered work even when nothing can ship anywhere.
    std::fs::write(repo.join("calc.py"), "def add(a, b):\n    return a\n").unwrap();
    assert!(unshipped_or_dirty(true, &read));
    drop(tmp);
}

#[test]
fn remote_read_still_gates_the_pushed_arm() {
    // A checkout WITH a remote keeps the old semantics: unpushed or dirty
    // reads unshipped; pushed and clean reads shipped.
    let (tmp, repo) = temp_repo();
    git(
        &repo,
        &["remote", "add", "origin", "https://example.com/o/r.git"],
    );
    std::fs::write(repo.join("f.txt"), "one\n").unwrap();
    git(&repo, &["add", "."]);
    git(
        &repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "one",
        ],
    );

    let read = local_output_for(&repo);
    // @{push} is unreadable until an upstream exists: unchanged behavior,
    // keeps the REST read.
    assert!(unshipped_or_dirty(false, &read));
    drop(tmp);
}

#[test]
fn remoteless_pr_failure_names_the_remedy_not_a_retry() {
    let reason = remoteless_pr_read_reason("pr_info_rest", "could not resolve owner/repo");
    assert!(!reason.contains("retrying next fire"));
    assert!(reason.contains("marked advisory"));
    assert!(reason.contains("pr_info_rest"));
    assert!(reason.contains("could not resolve owner/repo"));
}
