// Remote-less repo tests: no origin remote is a terminal local fact. The
// done-node shortcut takes the advisory terminal on local evidence, and a
// failed PR read names the remedy instead of promising a retry that can
// never happen. Every fixture is a real temp git repo (local git only, no
// network, no gh).
use super::*;

fn git(repo: &std::path::Path, args: &[&str]) {
    let out = git_bounded("git", args, repo).expect("git runs to a bounded answer");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr_tail)
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
fn the_terminal_decision_reads_the_repo_shape() {
    // One contract, two rows (D2147: the happy path, the refusal). Row 1
    // is the probe shape: no origin remote, one committed file, clean
    // tree - the unshipped conjunct must not fire, so the NoPr arm's
    // DoneAdvisory terminal is reachable on local evidence alone. Row 2 is
    // the refusal: a checkout WITH a remote reads unshipped while @{push}
    // is unreadable, the unchanged behavior that keeps the REST read, so
    // the run still waits on PR truth.

    // Row 1: the remote fact itself, absent.
    let (tmp, repo) = temp_repo();
    let read = local_output_for(&repo);
    assert_eq!(origin_remote_url(&read), None);

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

    // Dirt stays unshipped: uncommitted work is not delivered work even
    // when nothing can ship anywhere.
    std::fs::write(repo.join("calc.py"), "def add(a, b):\n    return a\n").unwrap();
    assert!(unshipped_or_dirty(true, &read));

    // Row 2: the remote fact flips, and the push-side conjunct governs.
    git(
        &repo,
        &["remote", "add", "origin", "https://example.com/o/r.git"],
    );
    let read = local_output_for(&repo);
    assert_eq!(
        origin_remote_url(&read).as_deref(),
        Some("https://example.com/o/r.git")
    );
    // @{push} is unreadable until an upstream exists: still unshipped, the
    // REST read keeps answering.
    assert!(unshipped_or_dirty(false, &read));
    drop(tmp);

    // The failed PR read names the remedy, never a retry: the wording the
    // gate prints when the owner/repo resolution fails for good.
    let reason = remoteless_pr_read_reason("pr_info_rest", "could not resolve owner/repo");
    assert!(!reason.contains("retrying next fire"));
    assert!(reason.contains("marked advisory"));
    assert!(reason.contains("pr_info_rest"));
    assert!(reason.contains("could not resolve owner/repo"));
}
