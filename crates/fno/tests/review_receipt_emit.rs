//! The review receipt through the real doors, in a temp git repo: `fno do
//! review read` records it and `fno doctor event emit` checks it. A stub
//! `fno-agent` supplies the session identity.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const STUB: &str = "#!/bin/sh
if [ \"$1\" = \"attest\" ]; then
  printf '%s' '{\"attester_session_id\":\"s-test\",\"attester_witness\":\"process\",\"observed_model\":{\"kind\":\"unobserved\"}}'
  exit 0
fi
exit 1
";

/// Bare `Command::output` is barred by the clippy gate; spawn and wait.
fn collect(command: &mut Command) -> Output {
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the command spawns")
        .wait_with_output()
        .expect("the command finishes")
}

struct Rig {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    journal: PathBuf,
}

impl Rig {
    fn git(&self, args: &[&str]) -> String {
        let mut command = Command::new("git");
        command
            .current_dir(&self.repo)
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com");
        let out = collect(&mut command);
        assert!(out.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit(&self, file: &str, text: &str) -> String {
        std::fs::write(self.repo.join(file), text).unwrap();
        self.git(&["add", file]);
        self.git(&["commit", "-m", file]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn fno(&self, args: &[&str]) -> Output {
        let stub = self.root.join("fno-agent-stub");
        let mut command = Command::new(env!("CARGO_BIN_EXE_fno"));
        command
            .current_dir(&self.repo)
            .args(args)
            .env("HOME", &self.root)
            .env("FNO_AGENT_BIN", &stub)
            .env("FNO_STATE_DIR", self.root.join("state"))
            .env("FNO_AGENT_HOME", self.root.join("agents"))
            .env("FNO_EVENTS_PATH", &self.journal);
        collect(&mut command)
    }

    fn emit(&self, head: &str, branch: &str) -> Output {
        let data = format!(
            r#"{{"reviewer":"code-review","verdict":"pass","head_sha":"{head}","branch":"{branch}","session_id":"s-test"}}"#
        );
        let journal = self.journal.to_string_lossy().to_string();
        self.fno(&[
            "doctor",
            "event",
            "emit",
            "-t",
            "review_attestation",
            "--source",
            "test",
            "--events",
            &journal,
            "--data",
            &data,
        ])
    }

    fn rows(&self) -> usize {
        std::fs::read_to_string(&self.journal)
            .unwrap_or_default()
            .lines()
            .count()
    }
}

fn rig() -> Rig {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().unwrap();
    let stub = root.join("fno-agent-stub");
    std::fs::write(&stub, STUB).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let rig = Rig {
        journal: root.join("events.jsonl"),
        repo,
        root,
        _tmp: tmp,
    };
    rig.git(&["init", "-b", "main"]);
    rig.commit("a.txt", "one\n");
    rig.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    rig.git(&["checkout", "-b", "feature/x"]);
    rig
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn refused(out: &Output, why: &str) {
    assert!(!out.status.success(), "emit should refuse: {why}");
    assert!(
        stderr(out).contains(why),
        "expected {why:?} in: {}",
        stderr(out)
    );
}

#[test]
fn the_emit_door_needs_a_receipt_for_this_head_and_branch() {
    let rig = rig();
    let first = rig.commit("b.txt", "two\n");

    refused(&rig.emit(&first, "feature/x"), "no receipt for this head");
    assert_eq!(rig.rows(), 0, "a refused emit writes no row");

    let read = rig.fno(&["do", "review", "read"]);
    assert!(read.status.success(), "read failed: {}", stderr(&read));
    assert!(String::from_utf8_lossy(&read.stdout).contains("+two"));
    let ok = rig.emit(&first, "feature/x");
    assert!(ok.status.success(), "emit failed: {}", stderr(&ok));
    assert_eq!(rig.rows(), 1);

    let stale = "0".repeat(40);
    refused(&rig.emit(&stale, "feature/x"), "head_sha");
    refused(&rig.emit(&first, "feature/other"), "branch");
    assert_eq!(rig.rows(), 1, "mismatched claims write no row");

    let second = rig.commit("c.txt", "three\n");
    refused(&rig.emit(&second, "feature/x"), "no receipt for this head");
    assert_eq!(rig.rows(), 1, "a new commit voids the old receipt");
}
