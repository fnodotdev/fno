//! parity-stage: characterization
//! parity-oracle: cli/src/fno/pr/cli.py (publish-review typer door)
//!
//! Characterization tests for the publish-review argv door (x-1d1e wave 4).
//! The Python typer command was captured into tests/golden/publish_review_argv/
//! before its deletion; each case rebuilds the same fixture (git repo, fake
//! gh, pinned HOME/FNO_STATE_DIR) and runs the BINARY's `publish-review`
//! argv door, asserting (exit, stdout, stderr) against the frozen golden.
//! The bot leg's skip paths touch no network before the config gate, and
//! the round leg's gh reads answer from the fake gh, so nothing here
//! reaches GitHub.
//!
//! The kept case carries the door's exit-and-receipt contract as three rows
//! of one table: a dry run proves the round leg writes nothing, a posted
//! round lifts a bot-leg skip to 0, and a round carried across a rebase
//! (the attestation head behind the checkout's HEAD) reports already-posted
//! through the branch fallback, never the exit-2 attestation miss.

mod common;

use common::Golden;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    gh_dir: PathBuf,
}

impl Fixture {
    fn bin_path(&self) -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_fno-agent"))
    }
}

// ── xtail: fixture + cases (appended below) ─────────────────────────────────

/// The attested head every fixture rows on: a fixed sha, so the golden bytes
/// never depend on the fixture repo's real commit.
const FAKE_HEAD: &str = "1111111111111111111111111111111111111111";

/// The fake gh: answers the reads the legs make, keyed by mode like the
/// capture's fake transport. `list-marker` already carries the round marker
/// (an idempotent re-run); the empty list lets the round leg POST. The PR
/// view answers with the fixed head and a parseable slug URL.
fn write_fake_gh(root: &Path, mode: &str) -> PathBuf {
    let list = match mode {
        "list-marker" => {
            format!("[{{\"body\":\"r <!-- fno-review-round branch=main head={FAKE_HEAD} -->\"}}]")
        }
        _ => "[]".to_string(),
    };
    let dir = root.join("fakegh");
    fs::create_dir_all(&dir).unwrap();
    let gh = dir.join("gh");
    fs::write(
        &gh,
        format!(
            "#!/bin/sh\ncase \"$*\" in\n  *--paginate*) echo '{list}' ;;\n  *) echo '{{\"number\":41,\"headRefOid\":\"{FAKE_HEAD}\",\"headRefName\":\"main\",\"url\":\"https://github.com/o/r/pull/41\"}}' ;;\nesac\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

/// One `review_attestation` row on branch `main` at the fixed head. It lives
/// at `<repo>/.fno/events.jsonl`: `events_path` migrates the checkout
/// journal into the space store on first resolve, which is the channel the
/// real door reads journals through.
fn seed_attestation(repo: &Path) {
    let dir = repo.join(".fno");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("events.jsonl"),
        format!(
            "{{\"type\":\"review_attestation\",\"ts\":\"2026-11-20T00:00:00Z\",\"data\":{{\"reviewer\":\"code-review\",\"head_sha\":\"{FAKE_HEAD}\",\"verdict\":\"pass\",\"branch\":\"main\",\"findings\":[]}}}}\n"
        ),
    )
    .unwrap();
}

fn fixture(tag: &str, gh_mode: &str) -> Fixture {
    let tmp = tempfile::TempDir::new().unwrap().keep();
    let repo = tmp.join(tag);
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    fs::write(repo.join("file.txt"), "v1\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "c0"]);
    fs::create_dir_all(tmp.join("home")).unwrap();
    let home = tmp.join("home");
    let gh_dir = write_fake_gh(&tmp, gh_mode);
    Fixture {
        root: tmp,
        repo,
        home,
        gh_dir,
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "golden")
        .env("GIT_AUTHOR_EMAIL", "golden@example.com")
        .env("GIT_COMMITTER_NAME", "golden")
        .env("GIT_COMMITTER_EMAIL", "golden@example.com")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Identity markers scrubbed like the capture, so nothing environmental
/// reaches the door.
const MARKERS: [&str; 7] = [
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_SESSION_ID",
    "GEMINI_SESSION_ID",
    "OPENCODE_SESSION_ID",
    "CLAUDE_SESSION_ID",
    "FNO_HARNESS_SESSION_ID",
];

// ── xtail: run + tests (appended below) ─────────────────────────────────────

/// Run one `publish-review` argv invocation against the built binary with
/// the fixture env. Returns the normalized (exit, stdout, stderr).
fn run(fx: &Fixture, args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(fx.bin_path());
    cmd.arg("publish-review");
    cmd.args(args);
    cmd.current_dir(&fx.repo);
    cmd.envs(fno_agent::test_run::self_owner_env());
    cmd.env("HOME", &fx.home);
    cmd.env("FNO_STATE_DIR", fx.root.join("state"));
    cmd.env("FNO_EVENTS_PATH", fx.root.join("events.jsonl"));
    cmd.env("FGH_MODE", "open");
    let path = format!(
        "{}:{}",
        fx.gh_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    cmd.env("PATH", path);
    for m in MARKERS {
        cmd.env_remove(m);
    }
    let out = cmd.output().expect("run publish-review");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (code, stdout, stderr)
}

/// Three rows of the one exit-and-receipt contract at the binary boundary.
/// Red when the round leg writes under a dry run, when a posted round fails
/// to lift a bot-identity skip to 0, or when a round carried across a
/// rebase answers anything but already-posted (the head-pinned miss used to
/// exit 2 with no receipt at all).
#[test]
fn publish_round_receipt_and_exit() {
    // Row 1, dry run: the round leg must write nothing. The receipt reads
    // the dry-run skip, the bot-identity skip keeps its exit, and no POST
    // can have fired (the round receipt is unreachable after a POST).
    let fx = fixture("dry", "list-empty");
    seed_attestation(&fx.repo);
    let (rc, out, err) = run(&fx, &["--dry-run", "--pr-number", "41"]);
    let rust = Golden {
        exit: Some(rc),
        streams: vec![out, err],
    };
    common::assert_golden(
        "publish_review_argv",
        "publish dry run no identity",
        &rust,
        None,
    );

    // Row 2, posted under caller auth: the round proof lands on the PR and
    // lifts the bot-identity skip to exit 0, both receipts on stderr.
    let fx = fixture("posted", "list-empty");
    seed_attestation(&fx.repo);
    let (rc, out, err) = run(&fx, &["--pr-number", "41"]);
    let rust = Golden {
        exit: Some(rc),
        streams: vec![out, err],
    };
    common::assert_golden(
        "publish_review_argv",
        "publish round posted lifts a bot skip",
        &rust,
        None,
    );

    // Row 3, carried across a rebase: the attestation head is behind the
    // checkout's HEAD, so only the branch fallback can see it; the marker is
    // already on the PR, and the answer is already-posted at exit 0.
    let fx = fixture("carry", "list-marker");
    seed_attestation(&fx.repo);
    let (rc, out, err) = run(&fx, &["--pr-number", "41"]);
    let rust = Golden {
        exit: Some(rc),
        streams: vec![out, err],
    };
    common::assert_golden(
        "publish_review_argv",
        "publish carried round reports already posted",
        &rust,
        None,
    );
}

#[test]
fn publish_missing_pr_number() {
    let fx = fixture("repo", "list-empty");
    let (rc, out, err) = run(&fx, &[]);
    let rust = Golden {
        exit: Some(rc),
        streams: vec![out, err],
    };
    common::assert_golden(
        "publish_review_argv",
        "publish missing pr number",
        &rust,
        None,
    );
}
