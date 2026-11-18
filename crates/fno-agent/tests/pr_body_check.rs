//! `pr-body-check` against a real temp git repo and stub guard scripts, the
//! same process-level discipline as tests/pr_push.rs: the built binary runs
//! as its own process, so each test gets a clean environment and the
//! receipt/refusal text is asserted on real stdout/stderr.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_EDITOR", "true")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A work repo whose `origin` is a local bare remote with a main branch, so
/// `git merge-base origin/main HEAD` resolves (pr_rebase's pattern).
fn init_repo_with_origin(tmp: &Path) -> PathBuf {
    let bare = tmp.join("origin.git");
    git(
        tmp,
        &["init", "--bare", "-b", "main", bare.to_str().unwrap()],
    );
    let work = tmp.join("work");
    git(tmp, &["init", "-b", "main", work.to_str().unwrap()]);
    git(&work, &["config", "user.email", "t@t.t"]);
    git(&work, &["config", "user.name", "t"]);
    git(&work, &["config", "core.hooksPath", "/dev/null"]);
    std::fs::write(work.join("base.txt"), "base\n").unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-m", "init"]);
    git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&work, &["push", "-u", "origin", "main"]);
    work
}

/// An executable stub guard under `<root>/scripts/ci/`.
fn write_guard(root: &Path, name: &str, body: &str) {
    let dir = root.join("scripts").join("ci");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

const PASS_STUB: &str = "#!/bin/sh\nexit 0\n";
const FAIL_STUB: &str = "#!/bin/sh\necho 'stub violation in the body' >&2\nexit 1\n";

fn body_file(dir: &Path, body: &str) -> PathBuf {
    let p = dir.join("body.md");
    std::fs::write(&p, body).unwrap();
    p
}

fn run_command(cwd: &Path, extra: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agent"))
        .envs(fno_agent::test_run::self_owner_env())
        .args(["pr-body-check"])
        .arg("--cwd")
        .arg(cwd)
        .args(extra)
        .output()
        .expect("run fno-agent");
    (
        out.status.code().unwrap_or(1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn all_guards_run_even_after_a_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let work = init_repo_with_origin(tmp.path());
    write_guard(&work, "check-pr-node-closure.sh", FAIL_STUB);
    write_guard(&work, "check-oos-tracked.sh", FAIL_STUB);
    let body = body_file(&work, "body\n");
    let (code, out, err) = run_command(&work, &["--body-file", body.to_str().unwrap()]);
    assert_eq!(code, 1, "{out}\n{err}");
    assert!(out.contains("fail check-pr-node-closure.sh"), "{out}");
    assert!(out.contains("fail check-oos-tracked.sh"), "{out}");
    assert!(out.contains("3 ran, 2 failed"), "{out}");
}

#[test]
fn a_missing_body_file_names_the_path_and_exits_2() {
    let tmp = tempfile::tempdir().unwrap();
    let work = init_repo_with_origin(tmp.path());
    let (code, out, err) = run_command(&work, &["--body-file", "/nonexistent/b.md"]);
    assert_eq!(code, 2, "{out}\n{err}");
    assert!(err.contains("/nonexistent/b.md"), "{err}");
}

#[test]
fn every_guard_gets_the_same_env_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let work = init_repo_with_origin(tmp.path());
    let log = tmp.path().join("env.log");
    write_guard(
        &work,
        "check-pr-node-closure.sh",
        &format!(
            "#!/bin/sh\nprintf 'BODY=%s\\nTITLE=%s\\nHEADREF=%s\\nHEADSHA=%s\\nBASESHA=%s\\n' \
             \"$PR_BODY\" \"$PR_TITLE\" \"$PR_HEAD_REF\" \"$PR_HEAD_SHA\" \"$PR_BASE_SHA\" > {}\nexit 0\n",
            log.to_str().unwrap().escape_default()
        ),
    );
    write_guard(&work, "check-oos-tracked.sh", PASS_STUB);
    let body = body_file(&work, "the body text\n");
    let (code, out, err) = run_command(
        &work,
        &[
            "--body-file",
            body.to_str().unwrap(),
            "--title",
            "the title",
            "--head",
            "feature/x-3830",
        ],
    );
    assert_eq!(code, 0, "{out}\n{err}");
    let env = std::fs::read_to_string(&log).unwrap();
    assert!(env.contains("BODY=the body text\n"), "{env}");
    assert!(env.contains("TITLE=the title\n"), "{env}");
    assert!(env.contains("HEADREF=feature/x-3830\n"), "{env}");
    assert!(env.contains("HEADSHA=HEAD\n"), "{env}");
    let expected_base = git(&work, &["merge-base", "origin/main", "HEAD"]);
    assert!(
        env.contains(&format!("BASESHA={}", expected_base.trim())),
        "{env}"
    );
}

/// A seeded FNO_HOME: one live decision id, so a body citing it passes and
/// a body citing any other id is refused. Integration tests run the binary
/// as its own process, so the env change cannot race other tests.
fn seed_fno_home(tmp: &Path) -> PathBuf {
    let home = tmp.join("fno-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("decisions.jsonl"),
        "{\"type\":\"operator_decision\",\"ts\":\"2026-09-12T00:00:00Z\",\"data\":\
         {\"decision_id\":\"d-aaaa0001\",\"subject\":\"s\",\"decision\":\"R.\",\
         \"text\":\"R.\",\"authority_source\":\"operator\"}}\n",
    )
    .unwrap();
    home
}

fn run_command_with_fno_home(cwd: &Path, fno_home: &Path, extra: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agent"))
        .envs(fno_agent::test_run::self_owner_env())
        .env("FNO_HOME", fno_home)
        .args(["pr-body-check"])
        .arg("--cwd")
        .arg(cwd)
        .args(extra)
        .output()
        .expect("run fno-agent");
    (
        out.status.code().unwrap_or(1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_body_citing_an_unknown_ruling_id_fails_ruling_citations() {
    let tmp = tempfile::tempdir().unwrap();
    let work = init_repo_with_origin(tmp.path());
    let home = seed_fno_home(tmp.path());
    let body = body_file(&work, "per d-deadbeef we ruled\n");
    let (code, out, err) =
        run_command_with_fno_home(&work, &home, &["--body-file", body.to_str().unwrap()]);
    assert_eq!(code, 1, "{out}\n{err}");
    assert!(out.contains("fail ruling-citations"), "{out}");
    assert!(out.contains("d-deadbeef"), "{out}");
    assert!(out.contains("1 failed"), "{out}");
}
