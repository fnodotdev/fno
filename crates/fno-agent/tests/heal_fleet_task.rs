//! The heal -> fleet-task seam, driven through the public command: a fold
//! conflict files one deduped fleet task beside the events journal, a clean
//! fold closes it with reason `rebased`, and two roots with the same
//! conflict file two tasks distinguished by cwd (one heal process serves
//! several repos and PR numbers repeat across them, so identity is
//! lane + key + cwd). The heal undoes the fold it started, one conflict
//! holds the PR's next fold for 24h, and a refused undo keeps the door's
//! words in the task. Those contracts share one declaration, because the
//! test-delta scope holds new declarations at zero.
//!
//! These live here rather than beside the rest of the heal drive tests
//! because heal.rs sits at the file budget; the stubs replicate the in-file
//! ones against common::make_script.

use common::make_script;
use std::path::Path;

mod common;

fn args_for(dir: &Path, extra: &[&str]) -> Vec<String> {
    args_on_journal(dir, "events.jsonl", extra)
}

/// The journal is caller-named. The fleet-task store is `questions.jsonl`
/// beside it, whichever journal a run names.
fn args_on_journal(dir: &Path, journal: &str, extra: &[&str]) -> Vec<String> {
    let mut v = vec![
        "1".to_string(),
        "--gh-bin".to_string(),
        dir.join("gh").to_string_lossy().into_owned(),
        "--git-bin".to_string(),
        dir.join("git").to_string_lossy().into_owned(),
        "--claims-root".to_string(),
        dir.to_string_lossy().into_owned(),
        "--cwd".to_string(),
        dir.to_string_lossy().into_owned(),
    ];
    v.push("--bin-dir".to_string());
    v.push(dir.to_string_lossy().into_owned());
    // The journal rides the same test seam as the in-file drive_args: the
    // fleet-task store is questions.jsonl beside it.
    v.push("--events-file".to_string());
    v.push(dir.join(journal).to_string_lossy().into_owned());
    v.extend(extra.iter().map(|s| s.to_string()));
    v
}

/// PR 1 carries mergeable=MERGEABLE (the test names the value) and PR 2 has
/// no worktree. Its one red check is rustfmt-drift, so if the loop ever
/// reached the heal path the remedy would run and cargo.log would name it.
fn stub_gh_rebase(dir: &Path, pr1_mergeable: &str) {
    let body = r#"#!/bin/sh
D="$(dirname "$0")"
echo "gh $*" >> "$D/gh.log"
for a in "$@"; do case "$a" in
  *'pulls?state=open'*)
     echo '[{"number":1,"head":{"sha":"aaa1","ref":"feature/x-1111","repo":{"full_name":"o/r"}},"base":{"ref":"main","repo":{"full_name":"o/r"}},"author_association":"OWNER","mergeable":MERGEABLE,"body":"b"},{"number":2,"head":{"sha":"bbb2","ref":"feature/x-2222","repo":{"full_name":"o/r"}},"base":{"ref":"main","repo":{"full_name":"o/r"}},"author_association":"OWNER","mergeable":null,"body":"b"}]'
     exit 0 ;;
  *pulls/1*) echo '{"head":{"sha":"aaa1","ref":"feature/x-1111","repo":{"full_name":"o/r"}},"base":{"ref":"main","repo":{"full_name":"o/r"}},"author_association":"OWNER","mergeable":MERGEABLE,"body":"b"}'; exit 0 ;;
  *pulls/2*) echo '{"head":{"sha":"bbb2","ref":"feature/x-2222","repo":{"full_name":"o/r"}},"base":{"ref":"main","repo":{"full_name":"o/r"}},"author_association":"OWNER","mergeable":null,"body":"b"}'; exit 0 ;;
  *check-runs) echo '{"check_runs":[{"name":"cargo fmt --check (pinned)","status":"completed","conclusion":"failure","html_url":"https://github.com/o/r/actions/runs/1/job/9"}]}'; exit 0 ;;
  */logs) echo "Diff in /w/w/crates/fno-agent/src/x.rs:1:"; exit 0 ;;
  */status) echo '{"statuses":[]}'; exit 0 ;;
esac; done
echo '[]'
"#
    .replace("MERGEABLE", pr1_mergeable);
    make_script(dir, "gh", &body);
}

/// A stub `git` that lists one worktree (on feature/x-1111) and answers
/// run_one's branch/porcelain questions inside it. `merge --abort` answers
/// `abort_exit`, and every call logs its physical cwd after the arguments.
fn stub_git_drive(dir: &Path, abort_exit: u8) {
    let wt = dir.join("wt").to_string_lossy().into_owned();
    make_script(
        dir,
        "git",
        &format!(
            r#"#!/bin/sh
D="$(dirname "$0")"
echo "git $* @ $(pwd -P)" >> "$D/git.log"
case "$*" in
  "merge --abort") echo "fatal: fixture refusal" >&2; exit ABORT_EXIT ;;
esac
case "$1 $2" in
  "worktree list") printf 'worktree {wt}\nHEAD aaa\nbranch refs/heads/feature/x-1111\n\n'; exit 0 ;;
  "rev-parse --abbrev-ref") echo feature/x-1111; exit 0 ;;
  "status --porcelain") exit 0 ;;
esac
exit 0
"#
        )
        .replace("ABORT_EXIT", &abort_exit.to_string()),
    );
}

fn stub_cargo(dir: &Path) {
    make_script(
        dir,
        "cargo",
        r#"#!/bin/sh
D="$(dirname "$0")"
echo "cargo $*" >> "$D/cargo.log"
exit 0
"#,
    );
}

/// A stub `fno` whose `do pr push` seam answers a caller-named
/// stdout/exit/stderr triple; the backlog lane logs and answers.
fn stub_fno_push(dir: &Path, push_stdout: &str, push_exit: u8, push_stderr: &str) {
    let body = r#"#!/bin/sh
D="$(dirname "$0")"
echo "fno $*" >> "$D/fno.log"
case "$*" in
  *"do pr push"*) printf '%s' 'PUSH_STDOUT' ; printf '%s' 'PUSH_STDERR' >&2; exit PUSH_EXIT ;;
  *"backlog idea"*) echo "backlog node fno-abc9 created"; exit 0 ;;
esac
exit 0
"#
    .replace("PUSH_STDOUT", push_stdout)
    .replace("PUSH_EXIT", &push_exit.to_string())
    .replace("PUSH_STDERR", push_stderr);
    make_script(dir, "fno", &body);
}

fn log_of(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
}

/// Writers commit to the event store, so the projection reads committed
/// rows plus the live tail, never the raw file alone.
fn store_text(dir: &Path) -> String {
    fno_agent::event_store::journal_text(&dir.join("questions.jsonl"), &[])
}

const CONFLICT_PUSH_STDERR: &str = "pr-push: the branch is not safely mergeable onto origin/main (status merge_conflict; files: crates/fno-agent/src/a.rs, crates/fno/src/b.rs). Resolve, then `git merge --continue`, and re-run the push.";

#[test]
fn a_fold_conflict_files_one_fleet_task_and_a_clean_fold_closes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_rebase(d, "false");
    stub_git_drive(d, 0);
    stub_cargo(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agent")).unwrap();
    stub_fno_push(d, "", 3, CONFLICT_PUSH_STDERR);
    fno_agent::heal::run_heal(&args_for(d, &["--all", "--apply"]));
    let store = store_text(d);
    assert_eq!(
        store.matches(r#""type":"fleet_task""#).count(),
        1,
        "one open task: {store}"
    );
    assert!(
        store.contains("fold conflict")
            && store.contains("crates/fno-agent/src/a.rs")
            && store.contains("crates/fno/src/b.rs"),
        "{store}"
    );
    assert!(
        store.contains(r#""run":"fno do pr push --pr 1""#),
        "{store}"
    );
    assert!(!store.contains("fno do pr rebase"), "{store}");
    assert!(
        !log_of(d, "fno.log").contains("outstanding ask"),
        "no question was filed"
    );
    // The skip receipt itself stays asserted in-file: the events journal is
    // store-committed, and the committed read is crate-private.
    // The door leaves the conflicted fold in progress. The heal is not the
    // caller that resolves it, so it undoes the fold in the PR's worktree.
    let in_wt = format!("@ {}", d.join("wt").canonicalize().unwrap().display());
    let undo = aborts(d);
    assert_eq!(
        undo.len(),
        1,
        "one undo per conflict: {}",
        log_of(d, "git.log")
    );
    assert!(
        undo.iter().all(|l| l.ends_with(&in_wt)),
        "the undo runs in the PR worktree: {undo:?}"
    );
    assert!(
        store.contains("undid its fold"),
        "the task names the true state: {store}"
    );
    // A fold conflict spends the 24h cap like a rebase does. The first
    // conflict's receipt holds the next run on the same journal, so a
    // conflict costs one fold attempt per PR per day, not one per tick.
    fno_agent::heal::run_heal(&args_for(d, &["--all", "--apply"]));
    assert_eq!(
        pushes(d),
        1,
        "the second run folds again: {}",
        log_of(d, "fno.log")
    );
    assert_eq!(aborts(d).len(), 1, "a held run undoes nothing");
    // The cap holds PR 1 for 24h after the conflict. The clean fold runs
    // as the next day would: a fresh journal holds no cap row, and the
    // task store is still the one beside it. The clean fold closes the task.
    stub_fno_push(
        d,
        "pr-push: origin/main behind-before=9 behind-after=0 preflight=full ci=settled sha=abc pushed=1",
        0,
        "",
    );
    fno_agent::heal::run_heal(&args_on_journal(
        d,
        "events-day-2.jsonl",
        &["--all", "--apply"],
    ));
    let store = store_text(d);
    assert!(
        store.contains(r#""type":"fleet_task_closed""#) && store.contains(r#""reason":"rebased""#),
        "the clean fold closed the task: {store}"
    );
    assert_eq!(aborts(d).len(), 1, "a clean fold undoes nothing");
    // The other arm of the undo, on its own fixture: the door reports the
    // conflict and git refuses the abort.
    a_refused_undo_keeps_the_conflict_words_in_the_task();
}

/// The stub `git` calls that undid a fold, one log line each.
fn aborts(dir: &Path) -> Vec<String> {
    log_of(dir, "git.log")
        .lines()
        .filter(|l| l.contains("merge --abort"))
        .map(str::to_string)
        .collect()
}

/// How many times the stub `fno` ran `do pr push`.
fn pushes(dir: &Path) -> usize {
    log_of(dir, "fno.log")
        .lines()
        .filter(|l| l.contains("do pr push"))
        .count()
}

/// A refused undo must not read as a clean tree. The task keeps the door's
/// own words, because the conflict still sits in the worktree.
fn a_refused_undo_keeps_the_conflict_words_in_the_task() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_rebase(d, "false");
    stub_git_drive(d, 128);
    stub_cargo(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agent")).unwrap();
    stub_fno_push(d, "", 3, CONFLICT_PUSH_STDERR);
    fno_agent::heal::run_heal(&args_for(d, &["--all", "--apply"]));
    let store = store_text(d);
    assert!(
        store.contains("could not undo its fold (fatal: fixture refusal)"),
        "{store}"
    );
    assert!(store.contains("`git merge --continue`"), "{store}");
    assert!(!store.contains("as found"), "{store}");
}

#[test]
fn two_roots_with_the_same_conflict_file_two_tasks_distinguished_by_cwd() {
    // One heal process serves several roots and PR numbers repeat across
    // repos, so identity is lane + key + cwd.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let root1 = d.join("root-1");
    std::fs::create_dir_all(root1.join(".git")).unwrap();
    std::fs::create_dir_all(d.join(".git")).unwrap();
    stub_gh_rebase(d, "false");
    stub_git_drive(d, 0);
    stub_cargo(d);
    stub_fno_push(d, "", 3, CONFLICT_PUSH_STDERR);
    // The push seam spawns from the PR's worktree; a missing cwd is a
    // spawn NotFound even when the stub binaries exist.
    std::fs::create_dir_all(d.join("wt/crates/fno-agent")).unwrap();
    let mut argv = args_for(d, &["--all", "--apply"]);
    argv.push("--cwd".to_string());
    argv.push(root1.to_string_lossy().into_owned());
    fno_agent::heal::run_heal(&argv);
    let store = store_text(d);
    let tasks: Vec<&str> = store
        .lines()
        .filter(|l| l.contains(r#""type":"fleet_task""#))
        .collect();
    assert_eq!(tasks.len(), 2, "one task per root: {store}");
    assert!(
        tasks[0].contains(r#""key":"PR 1 fold conflict""#)
            && tasks[1].contains(r#""key":"PR 1 fold conflict""#)
            && tasks
                .iter()
                .any(|t| t.contains(&format!(r#""cwd":"{}""#, d.display())))
            && tasks
                .iter()
                .any(|t| t.contains(&format!(r#""cwd":"{}""#, root1.display()))),
        "same key, different cwd: {store}"
    );
}
