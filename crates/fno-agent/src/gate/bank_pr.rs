//! One path for a daemon-opened PR: scratch worktree off origin/main, one
//! commit, push, `gh pr create`, worktree removal. `main_revert` and the eval
//! arms call it so the sequence has a single owner. `ship` adds the batch
//! shape the golden drafter needs: push onto the open PR when one holds the
//! branch, else open one, with the existence check and the content landing
//! in one step.

use std::path::{Path, PathBuf};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(120);

/// Why a daemon PR step refused. New expected failures carry this typed
/// error (R6); callers turn it into their own error shape at the boundary.
#[derive(Debug)]
pub struct BankPrError(pub String);

impl std::fmt::Display for BankPrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for BankPrError {
    fn from(s: String) -> Self {
        BankPrError(s)
    }
}

fn run(cwd: &Path, bin: &str, args: &[&str]) -> Result<String, String> {
    match crate::pr_push::run_labeled("bank-pr", bin, args, cwd, TIMEOUT)? {
        (true, out, _) => Ok(out.trim().to_string()),
        (false, _, err) => Err(format!("{bin} {}: {}", args.join(" "), err.trim())),
    }
}

/// URL of the open PR whose head is `branch`, if one exists.
pub(crate) fn existing_pr(root: &Path, branch: &str) -> Result<Option<String>, String> {
    let args = [
        "pr", "list", "--head", branch, "--state", "open", "--json", "url", "--jq", ".[0].url",
    ];
    let out = run(root, "gh", &args)?;
    Ok((!out.is_empty()).then_some(out))
}

/// Open a PR from `branch` with the commit `step` leaves in the worktree.
/// `step` runs inside the new worktree and must commit its own change. An
/// open PR on `branch` is returned as is: nothing is pushed twice.
pub fn open_with(
    root: &Path,
    branch: &str,
    title: &str,
    body: &str,
    step: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<String, String> {
    // An unreadable list falls through: `gh pr create` itself refuses a
    // second PR on the same head.
    if let Ok(Some(url)) = existing_pr(root, branch) {
        return Ok(url);
    }
    let tree = std::env::temp_dir().join(format!("fno-bank-pr-{}", branch.replace('/', "-")));
    let tree_s = tree.to_string_lossy().into_owned();
    let _ = run(root, "git", &["worktree", "remove", "--force", &tree_s]);
    run(root, "git", &["fetch", "origin", "main"])?;
    // No open PR holds this branch, so a remote copy is the leftover of a run
    // whose `gh pr create` failed after its push. A fresh cut cannot push over it.
    let heads = ["ls-remote", "--heads", "origin", branch];
    if run(root, "git", &heads).is_ok_and(|out| !out.is_empty()) {
        run(root, "git", &["push", "origin", "--delete", branch])?;
    }
    run(
        root,
        "git",
        &["worktree", "add", "-b", branch, &tree_s, "origin/main"],
    )?;
    let create = [
        "pr", "create", "--base", "main", "--head", branch, "--title", title, "--body", body,
    ];
    let url = step(&tree)
        .and_then(|_| run(&tree, "git", &["push", "-u", "origin", branch]))
        .and_then(|_| run(&tree, "gh", &create));
    let _ = run(root, "git", &["worktree", "remove", "--force", &tree_s]);
    let _ = run(root, "git", &["branch", "-D", branch]);
    url
}

/// Write `edits` (repo-relative path, bytes) and commit them as one commit.
pub fn open(
    root: &Path,
    branch: &str,
    title: &str,
    body: &str,
    edits: &[(PathBuf, Vec<u8>)],
) -> Result<String, String> {
    open_with(root, branch, title, body, |tree| {
        let mut rels = Vec::new();
        for (rel, bytes) in edits {
            let path = tree.join(rel);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
            }
            std::fs::write(&path, bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
            rels.push(rel.to_string_lossy().into_owned());
        }
        let mut add = vec!["add", "--"];
        add.extend(rels.iter().map(String::as_str));
        run(tree, "git", &add)?;
        run(tree, "git", &["commit", "-m", title])?;
        Ok(())
    })
}

/// Write `edits` into the worktree and commit them as one commit. Skips the
/// commit when nothing is staged, so a retry over content the branch
/// already carries is a no-op rather than a refusal.
fn commit_edits(tree: &Path, edits: &[(PathBuf, Vec<u8>)], msg: &str) -> Result<(), BankPrError> {
    let mut rels = Vec::new();
    for (rel, bytes) in edits {
        let path = tree.join(rel);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
        }
        std::fs::write(&path, bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
        rels.push(rel.to_string_lossy().into_owned());
    }
    let mut add = vec!["add", "--"];
    add.extend(rels.iter().map(String::as_str));
    run(tree, "git", &add)?;
    // `diff --cached --quiet` exits nonzero when staged changes exist.
    if run(tree, "git", &["diff", "--cached", "--quiet"]).is_err() {
        run(tree, "git", &["commit", "-m", msg])?;
    }
    Ok(())
}

/// Ship `edits` to the batch PR for `branch`: one commit pushed onto the
/// open PR when one holds the branch (its body then lists every golden it
/// does not name yet), else one fresh PR off origin/main. The existence
/// check and the content landing are one step, so a PR that merges or
/// appears between the check and the work cannot strand the edits. Any
/// refusal leaves the caller's staged files intact for the next pass.
pub fn ship(
    root: &Path,
    branch: &str,
    title: &str,
    body: &str,
    edits: &[(PathBuf, Vec<u8>)],
) -> Result<String, BankPrError> {
    if let Some(url) = existing_pr(root, branch)? {
        let tree =
            std::env::temp_dir().join(format!("fno-bank-pr-push-{}", branch.replace('/', "-")));
        let tree_s = tree.to_string_lossy().into_owned();
        let _ = run(root, "git", &["worktree", "remove", "--force", &tree_s]);
        run(root, "git", &["fetch", "origin", branch])?;
        run(
            root,
            "git",
            &[
                "worktree",
                "add",
                "--detach",
                &tree_s,
                &format!("origin/{branch}"),
            ],
        )?;
        let shipped = commit_edits(&tree, edits, "add golden eval drafts").and_then(|_| {
            run(
                &tree,
                "git",
                &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
            )
        });
        let _ = run(root, "git", &["worktree", "remove", "--force", &tree_s]);
        shipped?;
        append_body_ids(root, branch, edits)?;
        return Ok(url);
    }
    let tree = std::env::temp_dir().join(format!("fno-bank-pr-{}", branch.replace('/', "-")));
    let tree_s = tree.to_string_lossy().into_owned();
    let _ = run(root, "git", &["worktree", "remove", "--force", &tree_s]);
    run(root, "git", &["fetch", "origin", "main"])?;
    // No open PR holds this branch, so a remote copy is the leftover of a
    // run whose `gh pr create` failed after its push. A fresh cut cannot
    // push over it.
    let heads = ["ls-remote", "--heads", "origin", branch];
    if run(root, "git", &heads).is_ok_and(|out| !out.is_empty()) {
        run(root, "git", &["push", "origin", "--delete", branch])?;
    }
    run(
        root,
        "git",
        &["worktree", "add", "-b", branch, &tree_s, "origin/main"],
    )?;
    let create = [
        "pr", "create", "--base", "main", "--head", branch, "--title", title, "--body", body,
    ];
    let url = commit_edits(&tree, edits, title)
        .and_then(|_| run(&tree, "git", &["push", "-u", "origin", branch]))
        .and_then(|_| run(&tree, "gh", &create));
    let _ = run(root, "git", &["worktree", "remove", "--force", &tree_s]);
    let _ = run(root, "git", &["branch", "-D", branch]);
    url.map_err(BankPrError)
}

/// Append the ids of `edits` the PR body does not name yet, so a retry
/// after a failed edit never lists a golden twice.
fn append_body_ids(
    root: &Path,
    branch: &str,
    edits: &[(PathBuf, Vec<u8>)],
) -> Result<(), BankPrError> {
    let listed = run(
        root,
        "gh",
        &["pr", "view", branch, "--json", "body", "--jq", ".body"],
    )?;
    let mut new_ids: Vec<String> = Vec::new();
    for (rel, _) in edits {
        let Some(id) = Path::new(rel)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
        else {
            continue;
        };
        if !listed.contains(&id) && !new_ids.contains(&id) {
            new_ids.push(id);
        }
    }
    if new_ids.is_empty() {
        return Ok(());
    }
    let list: String = new_ids.iter().map(|id| format!("- `{id}`\n")).collect();
    let body = format!("{listed}\n\nAdded from later corrections:\n{list}");
    run(root, "gh", &["pr", "edit", branch, "--body", &body])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn scratch() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin.git");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(dir.path(), &["init", "--bare", "-b", "main", "origin.git"]);
        git(&work, &["init", "-b", "main"]);
        std::fs::write(work.join("a.txt"), "a").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-m", "init"]);
        git(
            &work,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&work, &["push", "-u", "origin", "main"]);
        (dir, work)
    }

    #[test]
    fn step_commits_and_worktree_is_removed() {
        let (_dir, work) = scratch();
        let branch = "evals/test-x";
        let seen = std::cell::RefCell::new(PathBuf::new());
        // No gh in the scratch repo: the call fails after the push, which is
        // the part under test.
        let _ = open_with(&work, branch, "t", "b", |tree| {
            *seen.borrow_mut() = tree.to_path_buf();
            std::fs::write(tree.join("one.txt"), "1").unwrap();
            std::fs::write(tree.join("two.txt"), "2").unwrap();
            git(tree, &["add", "."]);
            git(tree, &["commit", "-m", "both"]);
            Ok(())
        });
        assert!(!seen.borrow().exists(), "worktree removed");
        let log = Command::new("git")
            .args(["--git-dir", "../origin.git", "log", branch, "--format=%s"])
            .current_dir(&work)
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&log.stdout).to_string();
        assert_eq!(log.lines().next(), Some("both"));
    }

    #[test]
    fn commit_edits_commits_once_and_skips_an_empty_retry() {
        let (_dir, work) = scratch();
        let tree = work.parent().unwrap().join("push-tree");
        git(
            &work,
            &[
                "worktree",
                "add",
                "--detach",
                tree.to_str().unwrap(),
                "HEAD",
            ],
        );
        let edits = [
            (PathBuf::from("evals/bank/golden-a.yaml"), b"a\n".to_vec()),
            (
                PathBuf::from("evals/fixtures/metrics/golden-a.yaml"),
                b"f\n".to_vec(),
            ),
        ];
        commit_edits(&tree, &edits, "add drafts").unwrap();
        // A retry finds the content already on the branch: nothing staged,
        // nothing committed, no refusal.
        commit_edits(&tree, &edits, "add drafts again").unwrap();
        let log = Command::new("git")
            .args(["log", "--format=%s"])
            .current_dir(&tree)
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&log.stdout).to_string();
        assert_eq!(log.lines().next(), Some("add drafts"));
        git(
            &work,
            &["worktree", "remove", "--force", tree.to_str().unwrap()],
        );
    }
}
