//! The evals port's characterization: the native dispatch's bytes ARE the
//! contract the deleted Python forwarding leaf (`fno.evals.cli`) transported,
//! frozen in the goldens under `tests/golden/evals/`. The capture script
//! (`tests/fixtures/evals/capture_golden.sh`) re-derives every fixture and
//! freezes fresh outputs through the fno-agents binary itself; this test
//! asserts the binary still answers the frozen bytes.
//!
//! parity-stage: characterization
//! parity-oracle: the pre-port golden bytes (the leaf inserted no flags, so
//! the frozen bytes are the dispatch's own)

use std::path::{Path, PathBuf};
use std::process::Command;

/// Machine-varying bytes mask before comparison: the macOS realpath prefix
/// the capture host's Path.resolve answers (/private/tmp), and the fixture
/// repo's git sha (the bank_rev echo), whose bytes depend on the capturing
/// machine's git object encoding. The tokens' positions and shapes stay
/// pinned. (Fixture paths and wall-clock stamps are already masked to
/// FIXTURE and <TS> by the capture script at freeze time.)
fn mask(text: &str) -> String {
    let normalized = text.replace("/privateFIXTURE", "FIXTURE");
    let re40 = regex::Regex::new(r"[0-9a-f]{40}").expect("compiles");
    re40.replace_all(&normalized, "<SHA40>").into_owned()
}

fn read_file(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Re-derive every fixture through the capture script and assert the
/// dispatch answers the frozen bytes: exit codes, stdout, stderr - all 28
/// cases.
#[test]
fn the_leaf_answers_the_frozen_python_goldens() {
    // crates/fno-agents -> crates -> repo root
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("manifest sits two levels under the repo root")
        .to_path_buf();
    let golden_dir = repo_root
        .join("crates/fno-agents/tests/golden/evals")
        .canonicalize()
        .expect("the committed goldens exist");
    let fresh = tempfile::tempdir().expect("scratch dir");

    let status = Command::new("bash")
        .arg(repo_root.join("tests/fixtures/evals/capture_golden.sh"))
        .envs(fno_agents::test_run::self_owner_env())
        .env("FNO_CAPTURE_AGENTS_BIN", env!("CARGO_BIN_EXE_fno-agents"))
        .env("FNO_CAPTURE_GOLDEN_OUT", fresh.path())
        .env_remove("FNO_TASK_CONTEXT_FILE")
        .env_remove("FNO_CLAIMS_ROOT")
        .status()
        .expect("runs the capture script");
    assert!(status.success(), "the capture run must succeed");

    let mut cases: Vec<String> = std::fs::read_dir(&golden_dir)
        .expect("golden dir reads")
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_suffix(".exit").map(str::to_string)
        })
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "the golden set is not empty");

    for case in &cases {
        for (suffix, kind) in [("exit", "exit code"), ("out", "stdout"), ("err", "stderr")] {
            let frozen = read_file(&golden_dir.join(format!("{case}.{suffix}")));
            let fresh_text = read_file(&fresh.path().join(format!("{case}.{suffix}")));
            assert_eq!(
                mask(&fresh_text),
                mask(&frozen),
                "{case}: {kind} diverged from the frozen golden"
            );
            if suffix == "exit" {
                assert_eq!(fresh_text.trim(), frozen.trim(), "{case}: exit code");
            }
        }
    }
}

/// AC1-HP, AC2-ERR: a worker spawn the machine refuses is an unavailable
/// attempt, never a failed grade. The real eval command reports three
/// refusals as incomplete, not as pass^3=FAIL, and persists them unavailable
/// and unstarted; the same task with spawns admitted grades and passes.
#[test]
fn brake_refusal_roundtrip() {
    let scratch = tempfile::tempdir().expect("scratch dir");
    let base = scratch.path().canonicalize().unwrap();
    let repo = base.join("repo");
    let bank = repo.join("evals/bank");
    std::fs::create_dir_all(&bank).unwrap();
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "t@example.test"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("seed.txt"), "seed body\n").unwrap();
    git(&["add", "seed.txt"]);
    git(&["commit", "-q", "-m", "seed"]);
    std::fs::write(
        bank.join("refused.yaml"),
        "id: refused\ntier: capability\nrepo_fixture: HEAD\nprompt: \"leave the tree as it is\"\n\
         grade:\n  - kind: file-exists\n    path: \"seed.txt\"\ntimeout_minutes: 1\n",
    )
    .unwrap();
    let state = base.join("state");
    let history = state.join("evals-history.jsonl");
    std::fs::write(
        base.join("config.toml"),
        format!(
            "state_dir = \"{s}\"\n[paths]\nworktrees_base = \"{w}\"\nevals_history = \"{h}\"\n\
             [evals]\nenabled = true\n",
            s = state.display(),
            w = base.join("wtbase").display(),
            h = history.display()
        ),
    )
    .unwrap();
    // The spawner: it refuses the way the brake does while FAKE_REFUSE is
    // set, and is otherwise a worker that changes nothing and exits clean.
    let fake = base.join("fno");
    std::fs::write(
        &fake,
        "#!/bin/sh\nif [ -n \"$FAKE_REFUSE\" ]; then\n  echo 'fno agents spawn: process admission refused: machine runaway brake holds (60s left): probe' >&2\n  exit 1\nfi\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let run = |refuse: bool| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
        cmd.args(["evals", "run", "--bank"])
            .arg(&bank)
            .args(["--task", "refused", "--repeat", "3", "-y"])
            .current_dir(&repo)
            .envs(fno_agents::test_run::self_owner_env())
            .env_remove("FNO_TASK_CONTEXT_FILE")
            .env_remove("FNO_CLAIMS_ROOT")
            .env_remove("FNO_EVENTS_PATH")
            .env_remove("FNO_HOME")
            .env_remove("FNO_REPO_ROOT")
            .env("FNO_CONFIG", base.join("config.toml"))
            .env("FNO_STATE_DIR", &state)
            .env("FNO_NO_CANONICAL_CONFIG", "1")
            .env("FNO_AGENTS_BIN", env!("CARGO_BIN_EXE_fno-agents"))
            .env("FNO_BIN", &fake);
        if refuse {
            cmd.env("FAKE_REFUSE", "1");
        } else {
            cmd.env_remove("FAKE_REFUSE");
        }
        let out = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
            .wait_with_output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{stderr}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let rows = || -> Vec<serde_json::Value> {
        read_file(&history)
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    };

    let out = run(true);
    assert!(
        out.contains("0/3 pass, 3 unavailable  (pass^3=INCOMPLETE)"),
        "{out}"
    );
    assert!(!out.contains("pass^3=FAIL"), "{out}");
    assert!(out.contains("unavailable: spawn exit 1"), "{out}");
    assert!(out.contains("reliability is incomplete"), "{out}");
    // A lost attempt reruns once, so three slots write three to six rows.
    let refused = rows();
    assert!((3..=6).contains(&refused.len()), "{refused:?}");
    for row in &refused {
        assert_eq!(row["status"], "unavailable", "{row}");
        let started = row.pointer("/obs/worker_started");
        assert_eq!(started, Some(&serde_json::json!(false)), "{row}");
    }

    let out = run(false);
    assert!(out.contains("3/3 pass  (pass^3=PASS)"), "{out}");
    let admitted: Vec<_> = rows().into_iter().skip(refused.len()).collect();
    assert_eq!(admitted.len(), 3, "{admitted:?}");
    assert!(
        admitted.iter().all(|r| r["status"] == "graded"),
        "{admitted:?}"
    );
}
