// The fixtures index Values directly; the slicing ratchet would red every
// fixture edit, so the whole test module holds the allow.
#![allow(clippy::indexing_slicing)]

use super::*;
use crate::event_store::{query_events, EventQuery};

const NOW: u64 = 1_793_000_000;

fn stamp(epoch: u64) -> String {
    rfc3339(epoch as f64).unwrap()
}

fn assistant(uuid: &str, epoch: u64, text: &str) -> String {
    json!({"type": "assistant", "uuid": uuid, "timestamp": stamp(epoch), "requestId": "req_1",
        "message": {"id": "msg_01a", "model": "claude-opus-5-5",
            "content": [{"type": "text", "text": text}]}})
    .to_string()
}

fn user(uuid: &str, epoch: u64, text: &str) -> String {
    json!({"type": "user", "uuid": uuid, "timestamp": stamp(epoch),
        "message": {"role": "user", "content": text}})
    .to_string()
}

struct Fx {
    _dir: tempfile::TempDir,
    projects: PathBuf,
    journal: PathBuf,
    cache: PathBuf,
}

fn fixture(lines: &[String]) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");
    std::fs::create_dir_all(projects.join("p")).unwrap();
    std::fs::write(
        projects.join("p").join("sess.jsonl"),
        lines.join("\n") + "\n",
    )
    .unwrap();
    Fx {
        journal: dir.path().join("events.jsonl"),
        cache: dir.path().join("cache").join("cursors.json"),
        projects,
        _dir: dir,
    }
}

fn pass(fx: &Fx, row_cap: usize, now: u64) -> FoldReceipt {
    fn inputs(fx: &Fx, row_cap: usize, now: u64) -> FoldInputs {
        FoldInputs {
            cache: fx.cache.clone(),
            journal: fx.journal.clone(),
            sources: vec![Box::new(ClaudeSource {
                projects_dir: fx.projects.clone(),
                roots: None,
            })],
            bus: BusIndex::empty(),
            witness: SubmitIndex::load(&fx.journal),
            detect: DetectFacts::default(),
            now_epoch: now,
            budget: TICK_READ_BUDGET,
            row_cap,
        }
    }
    let mut receipt = fold_once(inputs(fx, row_cap, now));
    // A CI runner can serve a listing that misses the fixture written
    // milliseconds earlier, and the fold's fail-open rule reads that as a
    // clean zero-file tick. The fixture is a precondition here, so re-run
    // the pass until the listing sees it. A skipped pass never listed
    // anything and is final; a failed append already names itself on the
    // receipt.
    let mut tries = 0;
    while !receipt.skipped && receipt.listed_files == 0 && tries < 3 {
        std::thread::sleep(std::time::Duration::from_millis(250));
        receipt = fold_once(inputs(fx, row_cap, now));
        tries += 1;
    }
    receipt
}

fn rows(fx: &Fx) -> Vec<Value> {
    query_events(
        &fx.journal,
        &EventQuery {
            types: vec!["agent_corrected".into()],
            ..Default::default()
        },
    )
    .unwrap_or_default()
    .iter()
    .filter_map(|r| serde_json::from_str::<Value>(&r.line).ok())
    .map(|v| v["data"].clone())
    .collect()
}

#[test]
fn ac1_an_exit_code_refusal_stamps_command_and_bucket_and_a_plain_failure_lands_no_row() {
    let tool_row = |id: &str, cmd: &str| {
        json!({"type": "assistant", "uuid": format!("a-{id}"), "timestamp": stamp(NOW + 2),
            "requestId": "req_1",
            "message": {"id": format!("msg_{id}"), "model": "claude-opus-5-5",
                "content": [{"type": "tool_use", "id": id, "name": "Bash",
                    "input": {"command": cmd}}]}})
        .to_string()
    };
    let result_row = |id: &str, content: &str| {
        json!({"type": "user", "uuid": format!("r-{id}"), "timestamp": stamp(NOW + 3),
            "message": {"role": "user", "content": [{"type": "tool_result",
                "tool_use_id": id, "is_error": true, "content": content}]}})
        .to_string()
    };
    // AC1-HP: a mail style-lint refusal (Exit code 1) lands one machine row
    // keyed on the command, bucketed, signed by the rule line.
    let mail_refusal = "Exit code 1\nmessage blocked by the style rules.\nrule 2 (one meaning per word): filler \"Thanks.\"\n1 of 1 messages blocked";
    let fx = fixture(&[
        assistant("a1", NOW + 1, "Sending."),
        tool_row("toolA", "fno agents mail send x \"a; b\""),
        result_row("toolA", mail_refusal),
    ]);
    pass(&fx, MAX_ROWS_PER_TICK, NOW);
    let got = rows(&fx);
    assert_eq!(got.len(), 1, "expected one refusal row, got {got:?}");
    assert_eq!(got[0]["corrected_by"], "machine");
    assert_eq!(got[0]["kind"], "refused");
    assert_eq!(got[0]["refusal_class"], "command_failed");
    assert_eq!(got[0]["command"], "fno agents mail send");
    assert_eq!(got[0]["refusal_bucket"], "style_lint");
    assert_eq!(
        got[0]["signature"],
        "rule N (one meaning per word): filler \"thanks.\""
    );

    // AC1-ERR: a cargo test failure (Exit code 101) matches no bucket and
    // lands no row.
    let cargo_failure = "Exit code 101\nfailures:\n    friction_log::tests::counts\nerror: test failed, to rerun pass `--lib`";
    let fx = fixture(&[
        assistant("a1", NOW + 1, "Running tests."),
        tool_row("toolB", "cargo test --lib friction_log"),
        result_row("toolB", cargo_failure),
    ]);
    pass(&fx, MAX_ROWS_PER_TICK, NOW);
    let got = rows(&fx);
    assert!(
        got.is_empty(),
        "a bucket-less exit code is not a refusal: {got:?}"
    );
}

#[test]
fn ac3_ac5_a_refold_holds_one_row_and_an_old_turn_reads_backfilled() {
    let fx = fixture(&[
        assistant("a1", NOW + 1, "I will use main."),
        user("u1", NOW + 2, "no, use the other branch"),
        assistant("a2", NOW + 3, "Done?"),
        user("u2", NOW + 4, "wait, did you run it?"),
    ]);
    let first = pass(&fx, MAX_ROWS_PER_TICK, NOW);
    assert_eq!(first.rows_appended, 1, "{first:?}");
    pass(&fx, MAX_ROWS_PER_TICK, NOW);
    // A wiped cursor replays the file, and the store still holds one row.
    std::fs::remove_file(&fx.cache).unwrap();
    let replay = pass(&fx, MAX_ROWS_PER_TICK, NOW);
    assert_eq!(replay.rows_appended, 0);
    let got = rows(&fx);
    assert_eq!(got[0]["seat"]["basis"], "unmeasured");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0]["corrected_by"], "user");
    assert_eq!(got[0]["kind"], "correct");
    assert_eq!(got[0]["model"]["value"], "claude-opus-5-5");
    assert_eq!(got[0]["quote"], "no, use the other branch");
    assert_eq!(got[0]["typed"], "unwitnessed");
    assert_eq!(
        got[0]["correction_id"].as_str().unwrap(),
        format!(
            "corr:{}",
            crate::intel_corpus::row_id("sess", "u1", "reaction", "0")
        )
    );
    // The typed-turn tally is the rate's denominator.
    let cache: CacheFile =
        serde_json::from_str(&std::fs::read_to_string(&fx.cache).unwrap()).unwrap();
    assert_eq!(cache.typed_turns.values().sum::<u64>(), 2);

    // AC5: a turn older than first run.
    {
        let fx = fixture(&[
            assistant("a1", NOW - 100, "Done."),
            user("u1", NOW - 99, "actually, keep the old name"),
            assistant("a2", NOW + 1, "Done."),
            user("u2", NOW + 2, "no, keep the new name"),
        ]);
        pass(&fx, MAX_ROWS_PER_TICK, NOW);
        let got = rows(&fx);
        let old = got.iter().find(|r| r["turn"] == "u1").unwrap();
        assert_eq!(old["backfilled"], true);
        for fact in ["account", "head_sha", "fno_version", "pr"] {
            assert_eq!(old[fact]["basis"], "unmeasured", "{fact}");
        }
        assert_eq!(old["model"]["value"], "claude-opus-5-5");
        let new = got.iter().find(|r| r["turn"] == "u2").unwrap();
        assert_eq!(new["backfilled"], false);
        assert_eq!(new["account"]["basis"], "unmeasured");
        assert!(new["account"]["reason"]
            .as_str()
            .unwrap()
            .contains("registry"));
    }
}

#[test]
fn a_replay_with_new_stamps_keeps_the_first_row_and_moves_the_cursor() {
    let fx = fixture(&[
        assistant("a1", NOW + 1, "Done."),
        user("u1", NOW + 2, "no, use the other branch"),
    ]);
    assert_eq!(pass(&fx, MAX_ROWS_PER_TICK, NOW).rows_appended, 1);
    let first = rows(&fx);
    std::fs::remove_file(&fx.cache).unwrap();
    let replay = pass(&fx, MAX_ROWS_PER_TICK, NOW + 3_600);
    assert_eq!(replay.append_error, None);
    assert_eq!(replay.rows_replayed, 1);
    assert_eq!(rows(&fx), first);
    let cache: CacheFile =
        serde_json::from_str(&std::fs::read_to_string(&fx.cache).unwrap()).unwrap();
    assert_eq!(cache.cursors.len(), 1);
}

#[test]
fn ac4_ac9_a_held_lock_skips_the_cap_defers_and_each_corrector_names_itself() {
    let mut lines = Vec::new();
    for i in 0..3u64 {
        lines.push(assistant(&format!("a{i}"), NOW + 10 * i, "Working."));
        lines.push(user(
            &format!("u{i}"),
            NOW + 10 * i + 1,
            "no, the other one",
        ));
    }
    let fx = fixture(&lines);
    std::fs::create_dir_all(fx.cache.parent().unwrap()).unwrap();
    {
        let _held = FileLock::try_acquire(&lock_path(&fx.cache)).unwrap();
        let skipped = pass(&fx, MAX_ROWS_PER_TICK, NOW);
        assert!(skipped.skipped);
        assert!(rows(&fx).is_empty(), "a skipped tick journals no row");
    }
    let capped = pass(&fx, 2, NOW);
    assert_eq!(capped.rows_appended, 2, "capped pass receipt: {capped:?}");
    assert!(capped.rows_capped);
    assert_eq!(rows(&fx).len(), 2);
    pass(&fx, 2, NOW);
    assert_eq!(rows(&fx).len(), 3);

    // AC9: each corrector names itself.
    {
        let refusal = json!({"type": "user", "uuid": "r1", "timestamp": stamp(NOW + 5),
        "message": {"role": "user", "content": [{"type": "tool_result",
            "tool_use_id": "tool9", "is_error": true,
            "content": "PreToolUse:Bash hook error: blocked by a gate\nmore"}]}})
        .to_string();
        let rejected = json!({"type": "user", "uuid": "r2", "timestamp": stamp(NOW + 7),
        "message": {"role": "user", "content": [{"type": "tool_result",
            "tool_use_id": "tool10", "is_error": true,
            "content": "The user doesn't want to proceed with this tool use."}]}})
        .to_string();
        let fx = fixture(&[
        assistant("a1", NOW + 1, "Ready."),
        user(
            "m1",
            NOW + 2,
            "<fno_mail from=\"peer\" model=\"gpt-6.1-sol\" harness=\"codex\">no, the PR is red</fno_mail>",
        ),
        assistant("a2", NOW + 3, "Fixing."),
        refusal,
        assistant("a3", NOW + 6, "Trying again."),
        rejected,
        assistant("a4", NOW + 8, "Ok."),
        user("u1", NOW + 9, "no, use the other branch"),
    ]);
        pass(&fx, MAX_ROWS_PER_TICK, NOW);
        let got = rows(&fx);
        let by = |who: &str, kind: &str| {
            got.iter()
                .find(|r| r["corrected_by"] == who && r["kind"] == kind)
                .unwrap_or_else(|| panic!("no {who}/{kind} in {got:?}"))
        };
        let agent = by("agent", "correct");
        assert_eq!(agent["verified"], false);
        assert_eq!(agent["corrected_by_model"]["value"], "gpt-6.1-sol");
        assert_eq!(agent["corrected_by_harness"]["value"], "codex");
        let machine = by("machine", "refused");
        assert_eq!(machine["refusal_class"], "hook_blocked");
        assert_eq!(
            machine["reason"],
            "PreToolUse:Bash hook error: blocked by a gate"
        );
        by("user", "reject");
        by("user", "correct");
    }
}
