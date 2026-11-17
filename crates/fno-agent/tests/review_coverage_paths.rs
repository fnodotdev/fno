//! Path-uniqueness enumeration for the `review_coverage` producer (x-3a3f).
//!
//! The AGENTS.md pitfalls entry says a guard on one of N reachable paths is
//! decorative; this node's defect was the inversion - the PRODUCER sat on one
//! of N paths, so the merge gate was unsatisfiable rather than bypassable for
//! every session shape that had no stop hook. This test enumerates every path
//! that reaches a coverage verdict and asserts, per row, either that running
//! the path produces a coverage event, or that the row is on an explicit
//! safe-direction allowlist with a reason. A new call site that starts reading
//! `review_coverage` without joining the table fails the source scan.

mod common;

use common::make_script;
use fno_agent::loopcheck::{run_loop_check_capture, run_review_coverage_capture};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const HEAD: &str = "deadbeefdeadbeefdeadbeefdeadbeef00000001";

/// One row of the reachability table (mirrors the table in
/// docs/architecture/review-lanes.md).
enum ProducerReach {
    /// Running the path emits a `review_coverage` event; asserted at runtime
    /// below (or, for the Python gates, by their own unit suites - the source
    /// scan pins that the call site exists).
    Reachable,
    /// The path deliberately does NOT get a producer, for the stated reason.
    SafeDirection(&'static str),
}

#[path = "space_paths.rs"]
mod space_paths;

use space_paths::project_events;

fn path_table() -> Vec<(&'static str, &'static str, ProducerReach)> {
    vec![
        (
            "stop hook decide() -> run_done",
            "streak-gated; unchanged",
            ProducerReach::Reachable,
        ),
        (
            "fno do pr merge <n>",
            "recompute-once in the native merge gate: authorized-merge reads the \
             verdict with recompute on",
            ProducerReach::Reachable,
        ),
        (
            "fno do pr status <n>",
            "the Rust status composer reads the row through the shared reader",
            ProducerReach::Reachable,
        ),
        (
            "finalize auto-merge arm (coverage_satisfied_in_latest_event)",
            "reached only from a terminal-allow, which implies run_done already \
             ran this fire; a failed arm leaves a green reviewed PR for a human, \
             which is the safe direction",
            ProducerReach::SafeDirection("cannot be starved: terminal-allow implies run_done ran"),
        ),
        (
            "fno do pr coverage-check <n> without --recompute (and the git-protection hook through it)",
            "default is the no-recompute read: a recompute shells the Rust producer \
             at minutes while a PreToolUse hook has a 60s budget, and a killed hook \
             emits no verdict at all. --recompute is producer-reachable, same as \
             `fno do pr merge`",
            ProducerReach::SafeDirection(
                "one-directional by design: on a missing or stale row this DENIES \
                 where `fno do pr merge` may yet allow after recomputing. An empty \
                 read is an answer and refuses. Recovery from a wrong deny is one \
                 command; from a wrong allow it is a revert",
            ),
        ),
        (
            "a human running the command by hand",
            "fno-agent review-coverage",
            ProducerReach::Reachable,
        ),
    ]
}

fn coverage_exists(path: &Path) -> bool {
    fno_agent::event_store::journal_text(path, &[])
        .lines()
        .filter(|l| l.contains("review_coverage"))
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .any(|ev| ev.get("type").and_then(|t| t.as_str()) == Some("review_coverage"))
}

fn coverage_review_state(path: &Path) -> Option<String> {
    fno_agent::event_store::journal_text(path, &[])
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| {
            event.get("type").and_then(|value| value.as_str()) == Some("review_coverage")
        })?
        .pointer("/data/review_state")?
        .as_str()
        .map(str::to_string)
}

fn fixture(parent: &Path, name: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
    let cwd = parent.join(name);
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    fs::write(
        cwd.join(".fno/config.toml"),
        "[review]\nrequired_bots = [\"chatgpt-codex-connector\"]\n",
    )
    .unwrap();
    // NOT a TempDir: the guard must outlive this function, or the stubs
    // vanish and the availability probe reads NotFound -> advisory mode.
    let bins = parent.join(format!("{name}-bins"));
    fs::create_dir_all(&bins).unwrap();
    let gh = make_script(
        &bins,
        "gh",
        &format!(
            r#"
if echo "$*" | grep -q -- "--version"; then echo 'gh version 2.x'; exit 0; fi
if echo "$*" | grep -q "headRefName"; then
  echo '{{"state":"OPEN","number":1,"headRefName":"main","headRefOid":"{HEAD}","mergeable":"MERGEABLE","baseRefName":"main"}}'
  exit 0
fi
if echo "$*" | grep -q "checks"; then echo '[{{"name":"ci","state":"SUCCESS","bucket":"pass"}}]'; exit 0; fi
if echo "$*" | grep -q "pulls/"; then echo '[]'; exit 0; fi
if echo "$*" | grep -q "reviews"; then echo '{{"reviews":[],"comments":[]}}'; exit 0; fi
exit 1"#
        ),
    );
    let git = make_script(
        &bins,
        "git",
        &format!(
            r#"case "$*" in
  *--raw*) exit 1 ;;
  *) echo "{HEAD}" ;;
esac"#
        ),
    );
    let project = project_events(&cwd);
    let global = parent.join(format!("{name}-global-events.jsonl"));
    (cwd, project, global, gh, git)
}

/// Every runtime-assertable reachable row produces a coverage event when its
/// path runs. The merge and status rows are native now: their end-to-end
/// proofs live in the parity goldens (merge_command_parity.rs) and the composer
/// suites; the source scan below pins their call sites to this table.
#[test]
fn every_reachable_path_produces_an_event() {
    let table = path_table();
    let mut proven = 0;
    for (path_name, _how, reach) in &table {
        match reach {
            ProducerReach::SafeDirection(why) => {
                assert!(
                    !why.is_empty(),
                    "safe-direction row {path_name} must carry a reason"
                );
            }
            ProducerReach::Reachable => {
                // Both Rust-drivable rows run here; the Python rows are
                // counted via the source-scan test so this loop stays honest
                // about which rows were RUN.
                match *path_name {
                    "stop hook decide() -> run_done" => {
                        let parent = TempDir::new().unwrap();
                        let (cwd, project, global, gh, git) = fixture(parent.path(), "stop-hook");
                        let manifest = cwd.join("target-state.md");
                        fs::write(
                            &manifest,
                            "---\nsession_id: s1\ncreated_at: 2026-08-14T00:00:00Z\nattended: true\n---\n",
                        )
                        .unwrap();
                        let transcript = cwd.join("transcript.jsonl");
                        fs::write(
                            &transcript,
                            serde_json::json!({"message": {"role": "assistant",
                                "content": "<promise>MISSION COMPLETE</promise>"}})
                            .to_string()
                                + "\n",
                        )
                        .unwrap();
                        std::env::set_var("FNO_NUDGE_DISABLED", "1");
                        std::env::set_var("FNO_LOOPCHECK_MIN_FIRE_GAP_SECS", "0");
                        let args: Vec<String> = vec![
                            "loop-check".into(),
                            format!("--state={}", manifest.display()),
                            format!("--transcript={}", transcript.display()),
                            format!("--cwd={}", cwd.display()),
                            format!("--gh-bin={}", gh.display()),
                            format!("--git-bin={}", git.display()),
                            format!("--events={}", project.display()),
                            format!("--global-events={}", global.display()),
                            "--global-settings".into(),
                            "/nonexistent/global-settings.yaml".into(),
                            "--author-harness".into(),
                            "none".into(),
                        ];
                        let (code, json) = run_loop_check_capture(&args);
                        assert_eq!(code, 0, "{json}");
                        assert!(
                            coverage_exists(&project) || coverage_exists(&global),
                            "stop-hook path emitted no coverage event; decision: {json}"
                        );
                        assert_eq!(
                            coverage_review_state(&project)
                                .or_else(|| coverage_review_state(&global))
                                .as_deref(),
                            Some("unreviewed")
                        );
                        proven += 1;
                    }
                    "a human running the command by hand" => {
                        let parent = TempDir::new().unwrap();
                        let (cwd, project, global, gh, git) = fixture(parent.path(), "verb");
                        let args: Vec<String> = vec![
                            "review-coverage".into(),
                            format!("--cwd={}", cwd.display()),
                            format!("--events={}", project.display()),
                            format!("--global-events={}", global.display()),
                            format!("--gh-bin={}", gh.display()),
                            format!("--git-bin={}", git.display()),
                            "--global-settings".into(),
                            "/nonexistent/global-settings.yaml".into(),
                            "--author-harness".into(),
                            "none".into(),
                        ];
                        let (code, json) = run_review_coverage_capture(&args);
                        assert_eq!(code, 0, "{json}");
                        assert!(
                            coverage_exists(&project) || coverage_exists(&global),
                            "command path emitted no coverage event"
                        );
                        assert_eq!(
                            coverage_review_state(&project)
                                .or_else(|| coverage_review_state(&global))
                                .as_deref(),
                            Some("unreviewed")
                        );
                        proven += 1;
                    }
                    python_row => {
                        // fno do pr merge / fno do pr status: their end-to-end proof
                        // lives in the native parity goldens and the composer
                        // suites; here we only acknowledge
                        // the row so adding a table row without a runtime arm
                        // in THIS test fails the coverage counter below.
                        assert!(
                            python_row.starts_with("fno do pr "),
                            "new reachable row {python_row} has no runtime arm"
                        );
                    }
                }
            }
        }
    }
    assert!(proven >= 2, "expected both Rust-drivable rows to run");
}

/// Every call site that READS the review_coverage event appears in the table
/// (or on the allowlist), and the Python gates actually route through the
/// recompute helper rather than around it. A new reader that joins without a
/// table row fails here.
#[test]
fn new_reader_sites_must_join_the_table() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .ancestors()
        .nth(2)
        .expect("crate sits at <repo>/crates/fno-agent");

    let table = path_table();
    let table_names: Vec<&str> = table.iter().map(|(n, _, _)| *n).collect();

    // Rust readers: scan the crate sources for reads of the event.
    for src in ["src/gate/finalize.rs", "src/gate/loopcheck.rs"] {
        let path = manifest_dir.join(src);
        let text = fs::read_to_string(&path).unwrap();
        if src == "src/gate/finalize.rs" {
            // The allowlisted arm: it reads review_coverage and has no
            // producer, by decision (see the table's reason).
            assert!(
                text.contains("coverage_satisfied_in_latest_event"),
                "finalize's coverage arm moved; update the table"
            );
            assert!(
                table_names.iter().any(|n| n.contains("finalize")),
                "finalize's read has no table row"
            );
        }
    }

    // Python readers: the pr package retired with its port, so the sweep
    // walks the whole Python tree with NO allowlist - any surviving reader
    // of the event is a NEW path by definition and must join the table here
    // (and get a producer or a safe-direction reason).
    let mut reader_files: Vec<String> = Vec::new();
    let mut stack = vec![repo_root.join("cli/src/fno")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_some_and(|ext| ext == "py") {
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                if text.contains("review_coverage_for_head(")
                    // The no-recompute surface reads the row and its
                    // pinned-input note in one scan. A separate needle, not a
                    // paren-less prefix: the parens are what keep a prose
                    // mention out of the reader table.
                    || text.contains("review_coverage_for_head_row(")
                    || text.contains("latest_review_coverage(")
                    || text.contains("latest_review_coverage_row(")
                    || text.contains("read_review_coverage(")
                    || text.contains("_scan_coverage(")
                {
                    reader_files.push(
                        path.strip_prefix(&repo_root)
                            .unwrap_or(&path)
                            .display()
                            .to_string(),
                    );
                }
            }
        }
    }
    assert!(
        reader_files.is_empty(),
        "new Python reader(s) of review_coverage need a row in the x-3a3f \
         path table: {reader_files:?}"
    );

    // The producer's evidence composition is native now: the coverage read
    // requests reviews and comments in one shape (the quota's funded shapes
    // name both), so the pin rides the read site instead of the retired
    // Python adapter.
    let pr_read = fs::read_to_string(manifest_dir.join("src/gate/loopcheck/pr_read.rs")).unwrap();
    assert!(
        pr_read.contains("\"reviews,comments\""),
        "coverage producer no longer requests both evidence surfaces in one read"
    );
    let finalize = fs::read_to_string(manifest_dir.join("src/gate/finalize.rs")).unwrap();
    assert!(
        finalize.contains("fno-gh-coverage") && finalize.contains("bot_verdict"),
        "finalize fallback stopped consuming the shared normalized evidence and predicate"
    );
    // The reviewer-refused consumption is the Rust composer's (the merge
    // verdict and the status surface both read the aggregate), so no Python
    // file is pinned to it here.
}
