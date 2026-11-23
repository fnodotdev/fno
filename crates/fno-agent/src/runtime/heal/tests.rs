//! Tests for the heal drive loop, in their own module: `heal.rs` sat pinned
//! at the file-budget cap and the starvation fix had no room to land. The
//! move is pure test motion; raw-string stub contents stay byte-exact and
//! the code lines shed the one indent level the brace nesting paid.

use super::*;
use std::path::Path;

/// A real `cargo fmt --check (pinned)` failure, timestamps and all.
const FMT_LOG: &str = concat!(
    "2026-09-03T03:06:02.1431957Z ##[group]Run cargo \"+$RUSTFMT_TOOLCHAIN\" fmt --all --check\n",
    "2026-09-03T03:06:03.2159034Z Diff in /home/runner/work/footnote/footnote/crates/fno/src/mux/server.rs:17553:\n",
    "2026-09-03T03:06:03.2234373Z -            core.pane_send(\n",
    "2026-09-03T03:06:04.8929731Z ##[error]Process completed with exit code 1.\n",
);

fn ctx<'a>(check: &'a str, log: &'a str) -> Ctx<'a> {
    Ctx {
        check,
        log,
        bucket: "fail",
        link: "",
    }
}

#[test]
fn classify_rows() {
    let stripped = strip_timestamps(FMT_LOG);
    assert!(
        stripped.contains("Diff in /home/runner/work/footnote/footnote/crates/fno/src/"),
        "got: {stripped}"
    );
    assert!(!stripped.contains("2026-09-03T03:06"), "got: {stripped}");

    let log = strip_timestamps(FMT_LOG);
    let f = classify(&ctx("cargo fmt --check (pinned)", &log), false);
    assert_eq!(f.signature, "rustfmt-drift");
    match f.remedy {
        Remedy::Auto { run, verify } => {
            assert_eq!(run.len(), 1, "one crate drifted: {run:?}");
            assert_eq!(run[0].cwd, "crates/fno");
            assert_eq!(run[0].argv, vec!["cargo", "+1.94.1", "fmt", "--all"]);
            assert_eq!(verify[0].argv.last().unwrap(), "--check");
        }
        other => panic!("expected Auto, got {other:?}"),
    }

    let log = strip_timestamps(&format!(
        "{FMT_LOG}2026-09-03T03:06:03Z Diff in /home/runner/work/footnote/footnote/crates/fno-agent/src/runtime/heal.rs:1:\n"
    ));
    let f = classify(&ctx("cargo fmt --check (pinned)", &log), false);
    match f.remedy {
        Remedy::Auto { run, .. } => {
            let dirs: Vec<&str> = run.iter().map(|c| c.cwd.as_str()).collect();
            assert_eq!(dirs, vec!["crates/fno", "crates/fno-agent"]);
        }
        other => panic!("expected Auto, got {other:?}"),
    }

    let f = classify(&ctx("cargo fmt --check (pinned)", "log unavailable"), false);
    assert_eq!(f.signature, "rustfmt-drift");
    match f.remedy {
        Remedy::Auto { run, .. } => assert_eq!(run.len(), 2, "{run:?}"),
        other => panic!("expected Auto, got {other:?}"),
    }

    // The shape ruff actually prints: code first, location beneath.
    let log = strip_timestamps(concat!(
        "2026-09-03T07:15:29.7310174Z F821 Undefined name `Callable`\n",
        "2026-09-03T07:15:29.7310931Z    --> src/fno/lead/board.py:886:21\n",
        "2026-09-03T07:15:29.7322711Z Found 1 error.\n",
    ));
    let f = classify(&ctx("Python static correctness (495 sources)", &log), false);
    assert_eq!(f.signature, "ruff-lint");
    match f.remedy {
        Remedy::Auto { run, verify } => {
            assert_eq!(run.len(), 1, "one command, the gate's own: {run:?}");
            assert_eq!(run[0].cwd, "cli");
            assert!(run[0].argv.contains(&"--fix".to_string()), "{run:?}");
            // The gate reads src/ only and runs no formatter; a remedy
            // that reached wider would rewrite files nothing checks.
            assert!(!run[0].argv.iter().any(|a| a == "tests"), "{run:?}");
            assert!(!run[0].argv.iter().any(|a| a == "format"), "{run:?}");
            assert_eq!(verify[0].argv.last().unwrap(), "src/");
        }
        other => panic!("expected Auto, got {other:?}"),
    }

    let log = "src/fno/x.py:12: error: Incompatible return value type";
    let f = classify(&ctx("Python static correctness", log), false);
    assert_eq!(f.signature, "mypy");
    assert!(
        matches!(&f.remedy, Remedy::Escalate { repro } if repro.contains("mypy")),
        "{:?}",
        f.remedy
    );
}

// ── the command: push discipline ───────────────────────────────────────────
// Stub `gh` and `git`, not a real remote: exactly one push, never a push
// over a run in flight, provable rather than argued.

use crate::write_exec_stub as write_exec;

/// A stub `gh` answering the four reads heal makes. `pending` decides
/// whether the SECOND check-runs read (the pre-push one) reports a run in
/// flight, which is how the in-flight guard is exercised.
fn stub_gh(dir: &Path, pending_on_second_read: bool) -> std::path::PathBuf {
    let flip = if pending_on_second_read {
        r#"if [ -f "$D/seen" ]; then B=in_progress; else touch "$D/seen"; fi"#
    } else {
        ""
    };
    write_exec(
        dir,
        "gh",
        &format!(
            r#"#!/bin/sh
D="$(dirname "$0")"
echo "gh $*" >> "$D/gh.log"
for a in "$@"; do case "$a" in
  */pulls/*) echo '{{"head":{{"sha":"deadbeef","ref":"feature/x","repo":{{"full_name":"o/r"}}}},"base":{{"ref":"main","repo":{{"full_name":"o/r"}}}},"author_association":"OWNER","body":"b"}}'; exit 0 ;;
  */check-runs) B=completed; {flip}
     if [ "$B" = in_progress ]; then
       echo '{{"check_runs":[{{"name":"cargo fmt --check (pinned)","status":"in_progress","conclusion":null,"html_url":"https://github.com/o/r/actions/runs/1/job/9"}}]}}'
     else
       echo '{{"check_runs":[{{"name":"cargo fmt --check (pinned)","status":"completed","conclusion":"failure","html_url":"https://github.com/o/r/actions/runs/1/job/9"}}]}}'
     fi
     exit 0 ;;
  */logs) echo "Diff in /w/w/crates/fno-agent/src/x.rs:1:"; exit 0 ;;
  */status) echo '{{"statuses":[]}}'; exit 0 ;;
esac; done
echo '[]'
"#
        ),
    )
}

/// A stub `git` recording every invocation. `dirty` decides what
/// `status --porcelain` answers.
fn stub_git(dir: &Path, branch: &str, dirty: bool) -> std::path::PathBuf {
    let porcelain = if dirty { "echo ' M src/x.rs'" } else { ":" };
    write_exec(
        dir,
        "git",
        &format!(
            r#"#!/bin/sh
D="$(dirname "$0")"
echo "git $*" >> "$D/git.log"
case "$1 $2" in
  "rev-parse --abbrev-ref") echo {branch}; exit 0 ;;
  "status --porcelain") if [ -f "$D/fixed" ]; then echo ' M src/x.rs'; else {porcelain}; fi; exit 0 ;;
esac
exit 0
"#
        ),
    )
}

/// A stub `cargo` on PATH that "fixes" the drift: the first run dirties
/// the tree, and the verify then passes. The crate directory is created
/// because a remedy runs IN it, and a missing cwd fails the spawn -- which
/// heal correctly reads as "the fix did not succeed".
fn stub_cargo(dir: &Path) {
    std::fs::create_dir_all(dir.join("crates/fno-agent")).unwrap();
    write_exec(
        dir,
        "cargo",
        r#"#!/bin/sh
D="$(dirname "$0")"
echo "cargo $*" >> "$D/cargo.log"
for a in "$@"; do [ "$a" = "--check" ] && exit 0; done
touch "$D/fixed"
exit 0
"#,
    );
}

/// A stub `cargo` that always succeeds and never touches the tree. It
/// stands for a remedy whose red does not reproduce locally.
fn stub_cargo_noop(dir: &Path) {
    std::fs::create_dir_all(dir.join("crates/fno-agent")).unwrap();
    std::fs::create_dir_all(dir.join("crates/fno")).unwrap();
    write_exec(
        dir,
        "cargo",
        r#"#!/bin/sh
D="$(dirname "$0")"
echo "cargo $*" >> "$D/cargo.log"
exit 0
"#,
    );
}

/// A stub `git` whose `commit` always fails, standing for a rejecting
/// pre-commit hook or a signing failure.
fn stub_git_commit_fails(dir: &Path, branch: &str) -> std::path::PathBuf {
    write_exec(
        dir,
        "git",
        &format!(
            r#"#!/bin/sh
D="$(dirname "$0")"
echo "git $*" >> "$D/git.log"
case "$1 $2" in
  "rev-parse --abbrev-ref") echo {branch}; exit 0 ;;
  "status --porcelain") if [ -f "$D/fixed" ]; then echo ' M src/x.rs'; fi; exit 0 ;;
esac
case "$1" in
  commit) echo "pre-commit hook refused" >&2; exit 1 ;;
esac
exit 0
"#
        ),
    )
}

fn log_of(dir: &Path, name: &str) -> String {
    // The events journal is store-committed: read committed rows, not
    // journal bytes. Other stub logs (gh.log, fno.log) stay raw files.
    if name == "events.jsonl" {
        return crate::events::committed_journal_text(&dir.join(name));
    }
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
}

fn args_for(dir: &Path, extra: &[&str]) -> Vec<String> {
    let mut v = vec![
        "1".to_string(),
        "--gh-bin".to_string(),
        dir.join("gh").to_string_lossy().into_owned(),
        "--git-bin".to_string(),
        dir.join("git").to_string_lossy().into_owned(),
        "--cwd".to_string(),
        dir.to_string_lossy().into_owned(),
    ];
    v.push("--bin-dir".to_string());
    v.push(dir.to_string_lossy().into_owned());
    // The journal rides the same test seam as drive_args below: without it
    // the default resolves through $HOME and the hermetic guard panics.
    v.push("--events-file".to_string());
    v.push(dir.join("events.jsonl").to_string_lossy().into_owned());
    v.extend(extra.iter().map(|s| s.to_string()));
    v
}

#[test]
fn remedy_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh(d, false);
    stub_git(d, "feature/x", false);
    stub_cargo(d);
    let code = run_heal(&args_for(d, &[]));
    assert_eq!(code, EXIT_ESCALATIONS.max(EXIT_CLEAN), "dry run reports");
    assert_eq!(log_of(d, "cargo.log"), "", "no remedy ran");
    assert!(!log_of(d, "git.log").contains("push"), "no push");

    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh(d, false);
    stub_git(d, "feature/x", true);
    stub_cargo(d);
    let code = run_heal(&args_for(d, &["--apply"]));
    assert_eq!(code, EXIT_CWD_REFUSAL);
    assert_eq!(log_of(d, "cargo.log"), "", "no remedy ran");
    assert!(!log_of(d, "git.log").contains("push"));

    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh(d, false);
    stub_git(d, "main", false);
    stub_cargo(d);
    assert_eq!(run_heal(&args_for(d, &["--apply"])), EXIT_CWD_REFUSAL);
    assert_eq!(log_of(d, "cargo.log"), "");

    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh(d, false);
    stub_git(d, "feature/x", false);
    stub_cargo(d);
    run_heal(&args_for(d, &["--apply"]));
    let git = log_of(d, "git.log");
    assert_eq!(git.matches("git commit").count(), 1, "{git}");
    assert_eq!(git.matches("git push").count(), 1, "{git}");

    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    // The pre-push re-read reports a check still running.
    stub_gh(d, true);
    stub_git(d, "feature/x", false);
    stub_cargo(d);
    let code = run_heal(&args_for(d, &["--apply"]));
    assert_eq!(code, EXIT_IN_FLIGHT);
    let git = log_of(d, "git.log");
    assert_eq!(git.matches("git commit").count(), 1, "the fix is kept");
    assert!(!git.contains("git push"), "but never pushed: {git}");

    // `dirty` is a WHOLE-WORKTREE question and the remedies share one
    // worktree, so an earlier remedy's uncommitted edit made a later
    // no-op remedy read as dirty and take credit for work it did not do.
    // Each remedy is measured against its own before/after now.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh(d, false);
    stub_git(d, "feature/x", false);
    stub_cargo_noop(d);

    let mut findings = vec![classify(
        &ctx("cargo fmt --check (pinned)", "log unavailable"),
        false,
    )];
    let args = parse_args(&args_for(d, &["--apply"])).unwrap();
    // The tree is already dirty when the remedy runs, exactly as it would
    // be after a previous finding's fix.
    std::fs::write(d.join("fixed"), "").unwrap();

    let healed = apply_auto(&args, &mut findings);
    assert!(
        healed.is_empty(),
        "a no-op remedy is never healed: {healed:?}"
    );
    assert!(
        matches!(&findings[0].remedy, Remedy::Escalate { repro } if repro.contains("changed nothing")),
        "{:?}",
        findings[0].remedy
    );

    // The fix is in the worktree but no commit and no push happened, so
    // the check is still red on the remote. Exiting 0 there is the false
    // green the no-op arm was written to close, left open on this path.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh(d, false);
    stub_git_commit_fails(d, "feature/x");
    stub_cargo(d);

    let code = run_heal(&args_for(d, &["--apply"]));
    assert_ne!(code, EXIT_CLEAN, "a failed commit is not a clean PR");
    assert!(
        !log_of(d, "git.log").contains("git push"),
        "and nothing is pushed"
    );

    // --dry-run rehearses --all --apply; on the single-PR path it would
    // silently mean "the report you already get without --apply".
    assert_eq!(
        run_heal(&[
            "1".to_string(),
            "--apply".to_string(),
            "--dry-run".to_string()
        ]),
        EXIT_READ_ERROR
    );
    // The fold undo against real git. The drive tests stub `git`, so this
    // call alone proves what a real `merge --abort` leaves behind.
    abort_fold_leaves_a_conflicted_worktree_as_found();
}

// ── the drive loop ─────────────────────────────────────────────────────
//
// Driven against stub gh/git/cargo/fno executables, so the four refusals
// and the tick row are provable rather than argued.

/// A stub `git` that both lists one worktree (on feature/x-1111) and
/// answers run_one's branch/porcelain/commit/push questions inside it.
fn stub_git_drive(dir: &Path) -> std::path::PathBuf {
    let wt = dir.join("wt");
    let wt = wt.to_string_lossy().into_owned();
    write_exec(
        dir,
        "git",
        &format!(
            r#"#!/bin/sh
D="$(dirname "$0")"
echo "git $*" >> "$D/git.log"
case "$1 $2" in
  "worktree list") printf 'worktree {wt}\nHEAD aaa\nbranch refs/heads/feature/x-1111\n\n'; exit 0 ;;
  "rev-parse --abbrev-ref") echo feature/x-1111; exit 0 ;;
  "status --porcelain") if [ -f "$D/fixed" ]; then echo ' M src/x.rs'; fi; exit 0 ;;
esac
exit 0
"#
        ),
    )
}

/// A stub `fno` logging every invocation. Heal files fleet tasks by
/// writing the store directly, so the inbox lanes are gone; the stub
/// exists for the commands the seams still shell (`do pr push`, `backlog`).
fn stub_fno(dir: &Path) {
    write_exec(
        dir,
        "fno",
        r#"#!/bin/sh
D="$(dirname "$0")"
echo "fno $*" >> "$D/fno.log"
exit 0
"#,
    );
}

/// Hold a live claim on `x-2222` under the given root, as a worker would.
fn hold_claim(dir: &Path) {
    let opts = crate::claims::AcquireOpts {
        pid: Some(std::process::id()),
        root: Some(dir.to_path_buf()),
        events_dir: Some(dir.to_path_buf()),
        ..Default::default()
    };
    match crate::claims::acquire("node:x-2222", "worker:t-x", opts) {
        crate::claims::AcquireOutcome::Acquired(_) => {}
        other => panic!("claim setup failed: {other:?}"),
    }
}

/// Drive-loop argv with the claim root and the events file pinned to the
/// test dir, so the two side effects never touch the real `~/.fno`.
fn drive_args(dir: &Path, extra: &[&str]) -> Vec<String> {
    let mut v = args_for(dir, &["--all", "--apply"]);
    v.push("--claims-root".to_string());
    v.push(dir.to_string_lossy().into_owned());
    v.push("--events-file".to_string());
    v.push(dir.join("events.jsonl").to_string_lossy().into_owned());
    v.extend(extra.iter().map(|s| s.to_string()));
    v
}

/// The stub_gh_drive_rerun gh plus the run-jobs endpoint the incident probe
/// reads: `jobs_pages_body` verbatim as the endpoint's echo payload, so the
/// test picks the page shape the probe must parse.
fn stub_gh_drive_rerun_jobs(dir: &Path, jobs_pages_body: &str) -> std::path::PathBuf {
    let script = r#"#!/bin/sh
D="$(dirname "$0")"
echo "gh $*" >> "$D/gh.log"
for a in "$@"; do case "$a" in
  *'pulls?state=open'*)
     echo '[{"number":1,"head":{"sha":"aaa1","ref":"feature/x-1111"},"body":"b"},{"number":2,"head":{"sha":"bbb2","ref":"feature/x-2222"},"body":"b"}]'
     exit 0 ;;
  *pulls/1*) echo '{"head":{"sha":"aaa1","ref":"feature/x-1111","repo":{"full_name":"o/r"}},"base":{"ref":"main","repo":{"full_name":"o/r"}},"author_association":"OWNER","body":"b"}'; exit 0 ;;
  *pulls/2*) echo '{"head":{"sha":"bbb2","ref":"feature/x-2222","repo":{"full_name":"o/r"}},"base":{"ref":"main","repo":{"full_name":"o/r"}},"author_association":"OWNER","body":"b"}'; exit 0 ;;
  *check-runs) echo '{"check_runs":[{"name":"ci","status":"completed","conclusion":"cancelled","html_url":"https://github.com/o/r/actions/runs/777/job/9"}]}'; exit 0 ;;
  *'runs/777/jobs'*) echo '"#
        .to_string()
        + jobs_pages_body
        + r#"'; exit 0 ;;
  */logs) echo "the run was cancelled before any step ran"; exit 0 ;;
  */status) echo '{"statuses":[]}'; exit 0 ;;
esac; done
echo '[]'
"#;
    write_exec(dir, "gh", &script)
}

/// A stub `curl` answering the status page from `incidents.json` beside it,
/// logging every consult. The heal probe resolves curl through bin-dir.
fn stub_curl(dir: &Path, body: &str) {
    std::fs::write(dir.join("incidents.json"), body).unwrap();
    write_exec(
        dir,
        "curl",
        r#"#!/bin/sh
D="$(dirname "$0")"
echo "curl $*" >> "$D/curl.log"
case "$*" in
  *githubstatus.com*) cat "$D/incidents.json"; exit 0 ;;
esac
exit 0
"#,
    );
}

const LIVE_INCIDENTS: &str = r#"{"incidents":[{"name":"GitHub Actions: Failed runs","status":"investigating","incident_updates":[{"body":"Actions runs are failing to start","created_at":"2026-10-05T19:11:58Z"}]}]}"#;

#[test]
fn rerun_holds_for_runnerless_incident() {
    // The pure verdict: casualty jobs plus a live Actions incident hold;
    // either read failing to confirm does not.
    let casualty = serde_json::json!({
        "name": "ci", "status": "completed", "conclusion": "cancelled",
        "runner_name": null, "started_at": null,
    });
    let incidents = serde_json::json!([
        {"name": "Incident with Actions", "status": "investigating",
         "created_at": "2026-10-05T19:11:58Z", "updated_at": "2026-10-05T21:32:31Z"},
    ]);
    assert_eq!(
        crate::gh_incident::rerun_hold(&[casualty.clone()], &incidents),
        Some(
            "GitHub Actions incident: investigating since 2026-10-05T21:32:31Z - the \
             cancelled run never got a runner; wait out the incident instead of rerunning"
                .to_string()
        )
    );
    assert_eq!(
        crate::gh_incident::rerun_hold(&[casualty], &Value::Null),
        None,
        "no live incident, no hold"
    );

    // Drive loop, runnerless jobs, live incident: the rerun holds. The page
    // read was spent (the shape matched first), the hold row lands with the
    // dedup key, the tick's rerun_keys stay empty, and the next tick
    // re-probes instead of skipping as unchanged.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let casualty_page = r#"{"total_count":1,"jobs":[{"name":"ci","conclusion":"cancelled","runner_name":null,"started_at":null}]}"#;
    stub_gh_drive_rerun_jobs(d, casualty_page);
    stub_curl(d, LIVE_INCIDENTS);
    stub_git_drive(d);
    stub_fno(d);
    hold_claim(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agent")).unwrap();
    let code = run_heal(&drive_args(d, &[]));
    assert_eq!(
        code, EXIT_ESCALATIONS,
        "the held rerun is an escalation row"
    );
    let gh = log_of(d, "gh.log");
    assert!(
        !gh.contains("run rerun"),
        "no rerun into the dead queue: {gh}"
    );
    assert!(
        log_of(d, "curl.log").contains("githubstatus.com"),
        "the page confirmed the hold"
    );
    let events = log_of(d, "events.jsonl");
    assert!(
        events.contains("\"pr_heal_hold\"") && events.contains("\"key\":\"aaa1:777\""),
        "the hold row lands with the dedup key: {events}"
    );
    assert!(
        !events.contains("\"rerun_keys\":[\"aaa1:777\"]"),
        "a held rerun never reads as an issued rerun: {events}"
    );
    run_heal(&drive_args(d, &[]));
    let gh = log_of(d, "gh.log");
    assert!(
        !gh.contains("run rerun"),
        "the hold survives the next tick: {gh}"
    );

    // The P1 resume proof: the incident clears, the next tick must RE-PROBE
    // (the hold door reopens the PR) and issue the rerun it once held.
    stub_curl(d, r#"{"incidents":[]}"#);
    let code = run_heal(&drive_args(d, &[]));
    assert_eq!(
        code, EXIT_CLEAN,
        "after the incident clears the rerun acted"
    );
    assert_eq!(
        log_of(d, "gh.log").matches("run rerun 777").count(),
        1,
        "exactly one rerun, after the incident cleared"
    );
    assert_eq!(
        log_of(d, "curl.log").matches("githubstatus.com").count(),
        3,
        "every held tick re-probed the page"
    );

    // Same casualty shape, page clear on the first sighting: the rerun
    // issues exactly once, like the pre-hold behavior.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_drive_rerun_jobs(d, casualty_page);
    stub_curl(d, r#"{"incidents":[]}"#);
    stub_git_drive(d);
    stub_fno(d);
    hold_claim(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agent")).unwrap();
    let code = run_heal(&drive_args(d, &[]));
    assert_eq!(code, EXIT_CLEAN, "no incident, the rerun acted");
    assert_eq!(
        log_of(d, "gh.log").matches("run rerun 777").count(),
        1,
        "exactly one rerun"
    );

    // A started job is a human or timeout cancel, not a casualty: the rerun
    // issues and the status page was never spent.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_drive_rerun_jobs(
        d,
        r#"{"total_count":1,"jobs":[{"name":"ci","conclusion":"cancelled","runner_name":null,"started_at":"2026-10-05T19:27:00Z"}]}"#,
    );
    stub_curl(d, LIVE_INCIDENTS);
    stub_git_drive(d);
    stub_fno(d);
    hold_claim(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agent")).unwrap();
    let code = run_heal(&drive_args(d, &[]));
    assert_eq!(code, EXIT_CLEAN, "a human cancel is no casualty");
    assert_eq!(
        log_of(d, "curl.log"),
        "",
        "the page is only spent once the shape matched"
    );
    assert_eq!(
        log_of(d, "gh.log").matches("run rerun 777").count(),
        1,
        "the rerun issued"
    );

    // More jobs than one page holds: the probe reads EVERY page before the
    // predicate, so a casualty first page over a started later page reruns.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_drive_rerun_jobs(
        d,
        r#"[{"total_count":1,"jobs":[{"name":"ci","conclusion":"cancelled","runner_name":null,"started_at":null}]},{"total_count":2,"jobs":[{"name":"rollup","conclusion":"cancelled","runner_name":null,"started_at":"2026-10-05T19:27:00Z"}]}]"#,
    );
    stub_curl(d, LIVE_INCIDENTS);
    stub_git_drive(d);
    stub_fno(d);
    hold_claim(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agent")).unwrap();
    let code = run_heal(&drive_args(d, &[]));
    assert_eq!(code, EXIT_CLEAN, "a casualty first page is no verdict");
    assert_eq!(
        log_of(d, "curl.log"),
        "",
        "the second page was read before the page was spent"
    );
    assert_eq!(
        log_of(d, "gh.log").matches("run rerun 777").count(),
        1,
        "the rerun issued"
    );
}

// ── the rebase triggers ───────────────────────────────────────────────

// ── the detached drive loop ───────────────────────────────────────────

/// The drive-loop gh with PR 1 red on the closure gate. The branch rides
/// `stub_git_drive`'s worktree branch (x-1111 is letter-free but never
/// parsed here; the demand comes from the log), the log names x-0000 as the
/// demanded id -- an allowlisted synthetic id the number tier never mints --
/// and `pr edit` marks `heal-wrote` so the next read sees the owner's
/// rewritten body. The bodies are caller-named and must arrive
/// JSON-escaped: they substitute into the pull object `read_pr` parses.
fn stub_gh_drive_closure(dir: &Path, body_before: &str, owner_body: &str) {
    let body = r#"#!/bin/sh
D="$(dirname "$0")"
echo "gh $*" >> "$D/gh.log"
for a in "$@"; do case "$a" in
  *'pulls?state=open'*)
     echo '[{"number":1,"head":{"sha":"aaa1","ref":"feature/x-1111"},"body":"b"}]'
     exit 0 ;;
  *pulls/1*)
     if [ -f "$D/heal-wrote" ]; then printf '%s' '{"head":{"sha":"aaa1","ref":"feature/x-1111","repo":{"full_name":"o/r"}},"base":{"ref":"main","repo":{"full_name":"o/r"}},"author_association":"OWNER","body":"OWNER_BODY"}'; else printf '%s' '{"head":{"sha":"aaa1","ref":"feature/x-1111","repo":{"full_name":"o/r"}},"base":{"ref":"main","repo":{"full_name":"o/r"}},"author_association":"OWNER","body":"BEFORE_BODY"}'; fi
     exit 0 ;;
  *check-runs)
     n=$(($(cat "$D/tick" 2>/dev/null || echo 0) + 1)); echo "$n" > "$D/tick"
     echo "{\"check_runs\":[{\"name\":\"check-pr-node-closure ($n)\",\"status\":\"completed\",\"conclusion\":\"failure\",\"html_url\":\"https://github.com/o/r/actions/runs/$n/job/9\"}]}"; exit 0 ;;
  */logs) echo "check-pr-node-closure: HEAD ref 'feature/x-1111' names x-0000, and the exact trailer claims none of them."; exit 0 ;;
  */status) echo '{"statuses":[]}'; exit 0 ;;
  *"pr edit"*) touch "$D/heal-wrote"; exit 0 ;;
esac; done
echo '[]'
"#
    .replace("BEFORE_BODY", body_before)
    .replace("OWNER_BODY", owner_body);
    write_exec(dir, "gh", &body);
}

/// The stub `fno` the closure edit needs: the trailer command prints the
/// one line on stdout (an empty answer reads as an unknown id and fails
/// the edit), everything else just logs.
fn stub_fno_trailer(dir: &Path) {
    write_exec(
        dir,
        "fno",
        r#"#!/bin/sh
D="$(dirname "$0")"
echo "fno $*" >> "$D/fno.log"
case "$*" in
  *"do pr closure-trailer"*) echo "Fixes x-0000"; exit 0 ;;
esac
exit 0
"#,
    );
}

#[test]
fn closure_edit_writes_once_per_head_then_stands_down() {
    // The Q3736 fight: heal wrote the line, the PR owner rewrote it by
    // hand, and heal wrote it back every tick forever. One write per head;
    // a demand unmet AGAIN at the same head means the owner intends the
    // new line, so heal stands down, escalates once, and never edits.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_drive_closure(d, "b", "the owner rewrote the line by hand");
    stub_git_drive(d);
    stub_fno_trailer(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agent")).unwrap();

    let code1 = run_heal(&drive_args(d, &[]));
    assert_eq!(code1, EXIT_CLEAN, "the first tick writes the line");
    let gh = log_of(d, "gh.log");
    assert_eq!(gh.matches(" pr edit").count(), 1, "one edit: {gh}");
    let events = log_of(d, "events.jsonl");
    assert!(
        events.contains(r#""bodywrite_keys":["bodywrite:1:aaa1"]"#),
        "the write lands its once-per-head key: {events}"
    );

    let code2 = run_heal(&drive_args(d, &[]));
    assert_eq!(
        code2, EXIT_ESCALATIONS,
        "the owner's line stands; heal escalates instead of rewriting"
    );
    let gh = log_of(d, "gh.log");
    assert_eq!(
        gh.matches(" pr edit").count(),
        1,
        "no second edit at the same head: {gh}"
    );
    let store = crate::event_store::journal_text(&d.join("questions.jsonl"), &["fleet_task"]);
    assert_eq!(
        store.matches(r#""type":"fleet_task""#).count(),
        1,
        "exactly one stand-down task: {store}"
    );
    assert!(store.contains("changed after heal's write"), "{store}");

    // The third tick re-reads the same fight and re-files nothing.
    run_heal(&drive_args(d, &[]));
    let store = crate::event_store::journal_text(&d.join("questions.jsonl"), &["fleet_task"]);
    assert_eq!(
        store.matches(r#""type":"fleet_task""#).count(),
        1,
        "the open task is never re-filed: {store}"
    );
}

/// The push door leaves a conflicted fold in progress, and that tree
/// refuses every later push and heal. The heal is not the caller that
/// resolves it, so it hands the PR's worktree back as the push found it.
fn abort_fold_leaves_a_conflicted_worktree_as_found() {
    use crate::git_test_helpers::{git_bin, git_run};
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let wt = tmp.path().join("wt");
    let id: &[&str] = &["-c", "user.name=t", "-c", "user.email=t@t"];
    let git = |args: &[&str], cwd: &Path| {
        let out = git_run(args, cwd).unwrap();
        assert!(
            out.status.success(),
            "git {args:?} in {} failed: {}",
            cwd.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    std::fs::create_dir_all(&repo).unwrap();
    git(&["init", "-q", "-b", "main"], &repo);
    std::fs::write(repo.join("f.txt"), "base\n").unwrap();
    git(&[id, ["add", "f.txt"].as_slice()].concat(), &repo);
    git(
        &[id, ["commit", "-q", "-m", "base"].as_slice()].concat(),
        &repo,
    );
    git(&["branch", "pr"], &repo);
    let wt_dir = wt.to_str().unwrap();
    git(&["worktree", "add", "-q", wt_dir, "pr"], &repo);
    std::fs::write(repo.join("f.txt"), "main-side\n").unwrap();
    git(
        &[id, ["commit", "-q", "-am", "main edit"].as_slice()].concat(),
        &repo,
    );
    std::fs::write(wt.join("f.txt"), "pr-side\n").unwrap();
    git(
        &[id, ["commit", "-q", "-am", "pr edit"].as_slice()].concat(),
        &wt,
    );
    let head = git(&["rev-parse", "HEAD"], &wt);
    // The fold conflicts, as the push door's does. A real merge needs a
    // committer identity before it starts. git_run pins every config source
    // off, so the identity rides the call.
    let merge = git_run(
        &[id, ["merge", "--no-edit", "main"].as_slice()].concat(),
        &wt,
    )
    .unwrap();
    assert!(
        !merge.status.success(),
        "the fixture must conflict: {}",
        String::from_utf8_lossy(&merge.stderr)
    );
    assert_eq!(git(&["status", "--porcelain"], &wt), "UU f.txt\n");
    let bin = git_bin().to_string_lossy().into_owned();
    assert_eq!(abort_fold(&bin, &wt, crate::pr_push::READ_TIMEOUT), None);
    assert_eq!(git(&["status", "--porcelain"], &wt), "", "clean again");
    assert_eq!(git(&["rev-parse", "HEAD"], &wt), head, "HEAD unmoved");
    // The other arm: with no merge in progress git refuses, and the helper says so.
    assert!(abort_fold(&bin, &wt, crate::pr_push::READ_TIMEOUT).is_some());
}
