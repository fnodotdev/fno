//! The lead driver arm, end to end: a teamed session's stop gate reads the
//! board through the real in-process collector, decides, and terminates.
//!
//! Split from loop_check.rs: that file is over the line budget and
//! shrink-only, and this family is self-contained - its own fixture (a real
//! graph under a temp home, stub gh/fno on PATH), its own spawn
//! helper, and exactly one read still served by a mock (the escalation
//! command). The board itself is read in process, so the canned-payload mocks
//! are gone; a spec names the graph the fixture writes instead.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn make_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    fs::write(&tmp, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut perms = fs::metadata(&tmp).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&tmp, perms).unwrap();
    fs::rename(&tmp, &path).unwrap();
    path
}

fn event_text(path: &Path) -> String {
    fno_agent::event_store::journal_text(path, &[])
}

// ── the lead driver arm ───────────────────────────────────────────────────────
//
// A lead has no PR, so none of the target conjuncts above apply. These drive
// the same command with `--driver lead` over a lead manifest and a mocked board.

/// A `created_at` inside every default `span:96h` term, so a fixture that
/// declares no term of its own never trips the Stop-hook term gate: these
/// tests exercise the board/bound machinery downstream of that gate, not the
/// term itself (`lead_term_gate.rs`-equivalent coverage lives in
/// `lead_state.rs`'s and `loopcheck.rs`'s own unit tests).
fn recent_created_at() -> String {
    (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339()
}

fn lead_manifest(dir: &Path, fno_id: &str) -> PathBuf {
    lead_manifest_with_budget(dir, fno_id, 40)
}

fn lead_manifest_with_budget(dir: &Path, fno_id: &str, budget: u64) -> PathBuf {
    let path = dir.join("lead-state.md");
    let created_at = recent_created_at();
    fs::write(
        &path,
        format!(
            "---\nfno_id: {fno_id}\ncreated_at: {created_at}\nscope: drain\n\
             harness: claude\nbudget_max_iterations: {budget}\n---\n"
        ),
    )
    .unwrap();
    path
}

fn lead_manifest_with_session(dir: &Path, fno_id: &str, session_id: &str) -> PathBuf {
    let path = lead_manifest(dir, fno_id);
    let content = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        content.replacen(
            "harness: claude",
            &format!("harness: claude\nharness_session_id: {session_id}"),
            1,
        ),
    )
    .unwrap();
    path
}

fn write_stand_down_transcript(cwd: &Path) {
    fs::write(
        cwd.join("transcript.jsonl"),
        r#"{"type":"user","uuid":"turn-stand-down","timestamp":"2026-09-06T21:00:00.000Z","message":{"role":"user","content":"Perhaps our lead has overstayed its welcome"}}
"#,
    )
    .unwrap();
}

/// A board the fixture serves. The canned payloads died with the subprocess
/// board read (x-25b8: the stop gate reads the collector in process), so a
/// spec now names the graph the fixture writes: the rows of the spec's
/// undispatched queue become planned, ready, unclaimed nodes, and the
/// decision comes out the real pipeline. An unparseable spec is the blind
/// case: the graph source goes dark.
fn org_board_bin(dir: &Path, payload: &str, _exit: i32) -> PathBuf {
    let path = dir.join("board-spec.json");
    fs::write(&path, payload).unwrap();
    path
}

/// The spec's undispatched row ids, or None when the spec never parsed.
fn lead_spec_rows(spec: &str) -> Option<Vec<String>> {
    let parsed: serde_json::Value = serde_json::from_str(spec).ok()?;
    let rows = parsed["queues"]
        .as_array()?
        .iter()
        .find(|q| q["name"] == "undispatched")?["rows"]
        .as_array()?
        .clone();
    Some(
        rows.iter()
            .filter_map(|r| r["id"].as_str().map(str::to_string))
            .collect(),
    )
}

/// The drain answer the fixture's fno stub owes: the epic plus every spec row
/// reads undelivered, and a clean spec writes the epic itself done, so the
/// graph and the stub answer the same question. An unparseable spec is the
/// blind case and must refuse, never claim drained.
fn lead_drain_reply(spec: &str) -> String {
    match lead_spec_rows(spec) {
        None => "exit 1".to_string(),
        Some(rows) if rows.is_empty() => {
            "echo '{\"scope\":\"drain\",\"undelivered\":0}'".to_string()
        }
        Some(rows) => format!(
            "echo '{{\"scope\":\"drain\",\"undelivered\":{}}}'",
            rows.len() + 1
        ),
    }
}

/// Write the fixture graph for `board_spec` and pin config + lane at `cwd`.
/// The epic `drain` matches the manifest scope; every spec row becomes one
/// undispatchable planned node.
fn lead_prepare_fixture(cwd: &Path, home: &Path, board_spec: &Path) {
    let fno_dir = cwd.join(".fno");
    fs::create_dir_all(&fno_dir).unwrap();
    let graph = home.join("graph.json");
    let spec = fs::read_to_string(board_spec).unwrap_or_default();
    let ids = lead_spec_rows(&spec);
    if ids.is_none() {
        // Blind: the spec never parsed, so the graph source goes dark and the
        // collector answers with unreadable queues instead of a payload that
        // was never possible to fake here.
        let _ = fs::remove_file(&graph);
    } else {
        let ids = ids.unwrap_or_default();
        // A clean spec writes the epic itself done: the lead's terminations
        // key on the team draining, so a fixture board with no rows must
        // read as a drained team, not an eternally open one.
        let epic = if ids.is_empty() {
            serde_json::json!(
                {"id": "drain", "type": "epic", "status": "done",
                 "completed_at": "2026-08-18T00:00:00Z", "priority": "p1"}
            )
        } else {
            serde_json::json!(
                {"id": "drain", "type": "epic", "status": "ready", "priority": "p1"}
            )
        };
        let nodes: Vec<serde_json::Value> = std::iter::once(epic)
            .chain(ids.into_iter().map(|id| {
                // parent: the manifest scope compiles to the epic plus its
                // descendants, so a workable row is a child of `drain`.
                serde_json::json!({"id": id.clone(), "slug": id.clone(), "title": id.clone(), "type": "feature", "status": "ready",
                                   "priority": "p0", "plan_path": "/plans/p.md",
                                   "parent": "drain"})
            }))
            .collect();
        let mut rows = nodes;
        for row in &mut rows {
            let obj = row.as_object_mut().unwrap();
            obj.entry("slug")
                .or_insert_with(|| serde_json::json!("drain"));
            obj.entry("title")
                .or_insert_with(|| serde_json::json!("drain"));
            obj.entry("priority")
                .or_insert_with(|| serde_json::json!("p1"));
        }
        fno_agent::graph_store::seed_rows(&graph, &rows).unwrap();
    }
    fs::write(
        fno_dir.join("config.toml"),
        format!(
            "state_dir = \"{}\"\n[paths]\noperator_lane = \"{}\"\n\n\
             # The scope queue's project map reads work.workspaces; a machine \
             # with no global config.toml must see the fixture as complete.\n\
             [work.workspaces]\n",
            home.display(),
            home.join("lane.md").display()
        ),
    )
    .unwrap();
    fs::write(home.join("lane.md"), "").unwrap();
    let stubs = home.join("stubs");
    fs::create_dir_all(&stubs).unwrap();
    fs::write(stubs.join("gh"), "#!/bin/sh\necho '[]'\n").unwrap();
    // The batched truth probe shells bare `fno` (claude_ask.rs), so without
    // this stub every fire pays a real installed-CLI cold start and probes the
    // operator's live sessions - slow AND non-hermetic.
    fs::write(stubs.join("fno"), "#!/bin/sh\necho '{}'\n").unwrap();
    // Stage the default mock only when absent: a test may stage its
    // argv-recording version FIRST, and this fixture prep runs after it.
    let mock = home.join("escalate-mock");
    if !mock.is_file() {
        make_script(
            home,
            "escalate-mock",
            &format!(
                "if [ \"$1\" = \"agents\" ] && [ \"$2\" = \"hq\" ] && [ \"$4\" = \"drain\" ]; \
                 then\n\
                 \x20 {}\n\
                 \x20 exit 0\n\
                 fi\n\
                 if [ \"$1\" = \"agents\" ] && [ \"$2\" = \"hq\" ] && [ \"$3\" = \"escalate\" ]; then\n\
                 \x20 echo q-mock\n\
                 \x20 exit 0\n\
                 fi\n\
                 exit 0",
                lead_drain_reply(&spec)
            ),
        );
    };
    #[cfg(unix)]
    for stub in ["gh", "fno"] {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(stubs.join(stub), fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn lead_spawn(state: &Path, cwd: &Path, events: &Path, home: &Path) -> (i32, serde_json::Value) {
    lead_spawn_with(state, cwd, events, home, &[])
}

/// `extra` carries per-fire CLI overrides, e.g. a short `--read-timeout-ms`
/// for the wedged-source test. The bound is the whole FIRE's ceiling (the
/// board's budget derives from it minus the serialization reserve), so only
/// a fire that needs a killed read passes one.
fn lead_spawn_with(
    state: &Path,
    cwd: &Path,
    events: &Path,
    home: &Path,
    extra: &[&str],
) -> (i32, serde_json::Value) {
    let stubs = home.join("stubs");
    let real_path = std::env::var("PATH").unwrap_or_default();
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_fno-agent"));
    cmd.envs(fno_agent::test_run::self_owner_env());
    cmd.args([
        "loop-check",
        "--driver",
        "lead",
        "--state",
        state.to_str().unwrap(),
        "--transcript",
        cwd.join("transcript.jsonl").to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--events",
        events.to_str().unwrap(),
        "--global-events",
        events.to_str().unwrap(),
    ]);
    // The board is read in process, so the fixture `fno` serves exactly one
    // live read: the escalation command. Its mock is staged by the fixture and
    // overwritten by tests that need the argv recorder.
    cmd.arg("--fno-bin").arg(home.join("escalate-mock"));
    cmd.args(extra);
    let out = cmd
        .env("FNO_CLAIMS_ROOT", home)
        .env("FNO_HOME", home)
        .env("FNO_AGENT_HOME", home.join("agents"))
        .env(
            "FNO_OPERATOR_CAPTURE_DIR",
            home.join(".fno/operator-capture"),
        )
        .env("PATH", format!("{}:{}", stubs.display(), real_path))
        .output()
        .unwrap();
    let json = serde_json::from_str(&String::from_utf8_lossy(&out.stdout))
        .unwrap_or(serde_json::Value::Null);
    (out.status.code().unwrap_or(-1), json)
}

fn lead_fire(
    state: &Path,
    cwd: &Path,
    events: &Path,
    board_spec: &Path,
) -> (i32, serde_json::Value) {
    let home = board_spec.parent().unwrap();
    lead_prepare_fixture(cwd, home, board_spec);
    lead_spawn(state, cwd, events, home)
}

const BOARD_TWO_ACTIONABLE: &str = r#"{
  "actionable": 2, "unreadable": 0,
  "queues": [
    {"name":"undispatched","status":"ok","actionable":true,"count":2,
     "rows":[{"id":"x-1234"},{"id":"x-5678"}],"error":"","truncated":0,"note":"","source":"s"}
  ]
}"#;

const BOARD_CLEAN: &str = r#"{
  "actionable": 0, "unreadable": 0,
  "queues": [
    {"name":"undispatched","status":"ok","actionable":true,"count":0,
     "rows":[],"error":"","truncated":0,"note":"","source":"s"}
  ]
}"#;

#[test]
fn lead_arm_blocks_while_the_board_is_not_empty() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let state = lead_manifest(cwd, "k-block");
    let events = cwd.join("events.jsonl");
    let bin_dir = TempDir::new().unwrap();
    let fno = org_board_bin(bin_dir.path(), BOARD_TWO_ACTIONABLE, 0);

    let (code, d) = lead_fire(&state, cwd, &events, &fno);

    assert_eq!(code, 0, "a non-empty board must block: {d}");
    assert_eq!(d["decision"], "block");
    assert_eq!(d["actionable"], 2);
    let reason = d["reason"].as_str().unwrap();
    assert!(
        reason.contains("undispatched") && reason.contains("x-1234"),
        "the block reason must name the top actionable row: {reason}"
    );
}

#[test]
fn lead_nowork_is_the_clean_terminal_for_an_empty_board() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let state = lead_manifest(cwd, "k-clean");
    let events = cwd.join("events.jsonl");
    let bin_dir = TempDir::new().unwrap();
    let fno = org_board_bin(bin_dir.path(), BOARD_CLEAN, 0);

    let (code, d) = lead_fire(&state, cwd, &events, &fno);

    assert_eq!(code, 0);
    assert_eq!(d["decision"], "allow");
    assert_eq!(d["termination_reason"], "NoWork");

    let journal = event_text(&events);
    let row: serde_json::Value = journal
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|v| v["type"] == "termination")
        .expect("a termination event must be appended");
    assert_eq!(row["data"]["reason"], "NoWork");
    assert_eq!(
        row["data"]["session_id"], "k-clean",
        "the event must carry the lead session id so the journal reader matches it"
    );
    assert_eq!(row["data"]["driver"], "lead");
}

#[test]
fn an_unacked_stand_down_turn_blocks_a_clean_board() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let bin_dir = TempDir::new().unwrap();
    let state = lead_manifest_with_session(cwd, "k-stand-down", "s-stand");
    let events = cwd.join("events.jsonl");
    let fno = org_board_bin(bin_dir.path(), BOARD_CLEAN, 0);
    lead_prepare_fixture(cwd, bin_dir.path(), &fno);
    write_stand_down_transcript(cwd);

    let (code, d) = lead_spawn(&state, cwd, &events, bin_dir.path());

    assert_eq!(code, 0);
    assert_eq!(d["decision"], "block", "decision: {d}");
    assert_eq!(d["termination_reason"], serde_json::Value::Null);
    let reason = d["reason"].as_str().unwrap();
    assert!(reason.contains("turn-stand-down"), "reason: {reason}");
    assert!(
        reason.contains("fno inbox operator ack"),
        "reason: {reason}"
    );
}

#[test]
fn a_team_with_no_checkin_gets_one_hook_row_per_missed_beat() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let state = lead_manifest(cwd, "k-hook");
    let events = cwd.join("events.jsonl");
    let bin_dir = TempDir::new().unwrap();
    let fno = org_board_bin(bin_dir.path(), BOARD_TWO_ACTIONABLE, 0);

    let (code1, d1) = lead_fire(&state, cwd, &events, &fno);
    let (code2, d2) = lead_fire(&state, cwd, &events, &fno);

    assert_eq!(code1, 0, "fire 1: {d1}");
    assert_eq!(code2, 0, "fire 2: {d2}");
    assert_eq!(d1["decision"], "block", "fire 1: {d1}");
    assert_eq!(d2["decision"], "block", "fire 2: {d2}");
    let rows = event_text(&events);
    let checkins: Vec<&str> = rows
        .lines()
        .filter(|l| l.contains("\"lead_checkin\""))
        .collect();
    assert_eq!(checkins.len(), 1, "exactly one mechanical row: {rows}");
    assert!(
        checkins[0].contains("\"source\":\"hook\""),
        "row: {}",
        checkins[0]
    );
    let row: serde_json::Value = serde_json::from_str(checkins[0]).unwrap();
    assert_eq!(row["data"]["scope"], "drain");
    assert!(!row["data"]["change"].as_str().unwrap_or("").is_empty());
}

#[test]
fn lead_arm_blocks_rather_than_certifying_a_board_it_cannot_read() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let state = lead_manifest(cwd, "k-blind");
    let events = cwd.join("events.jsonl");
    let bin_dir = TempDir::new().unwrap();
    let fno = org_board_bin(bin_dir.path(), "not json at all", 1);

    let (code, d) = lead_fire(&state, cwd, &events, &fno);

    // The transport's exit-2 fail-closed path died with the subprocess read:
    // the collector always answers, and blindness degrades into unreadable
    // queues. The contract that survives is the one that matters - a blind
    // board never certifies the lead done.
    assert_eq!(code, 0, "the fire decides on a blind board: {d}");
    assert_eq!(d["decision"], "block", "blind is not clean: {d}");
    assert_ne!(
        d["termination_reason"], "NoWork",
        "a blind board is not a clean terminal: {d}"
    );
}

/// Append one event row to a lead journal.
fn lead_event(events: &Path, event_type: &str, data: serde_json::Value) {
    use std::io::Write;
    let row = serde_json::json!({
        "ts": "2026-08-18T00:00:00Z",
        "type": event_type,
        "source": "hook",
        "data": data,
    });
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(events)
        .unwrap();
    writeln!(f, "{row}").unwrap();
}

#[test]
fn lead_noprogress_ends_a_board_that_refuses_to_shrink() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let state = lead_manifest(cwd, "k-stuck");
    let events = cwd.join("events.jsonl");
    let bin_dir = TempDir::new().unwrap();
    let fno = org_board_bin(bin_dir.path(), BOARD_TWO_ACTIONABLE, 0);

    lead_event(
        &events,
        "lead_loop_check",
        serde_json::json!({"session_id": "k-stuck"}),
    );
    lead_event(
        &events,
        "lead_loop_check",
        serde_json::json!({"session_id": "k-stuck"}),
    );

    let (code, d) = lead_fire(&state, cwd, &events, &fno);

    assert_eq!(code, 0);
    assert_eq!(d["termination_reason"], "NoProgress");
    let reason = d["reason"].as_str().unwrap();
    assert!(
        reason.contains('2') && reason.contains("actionable"),
        "the terminal must name what stayed unshrunk: {reason}"
    );
}

#[test]
fn a_repeated_lead_action_is_not_progress() {
    // The specific way this loop would fail to converge, and it passes every
    // naive test: `stalled_holder` rows survive the one action a lead has for
    // them, so re-waking the same node forever would reset the counter forever.
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let state = lead_manifest(cwd, "k-repeat");
    let events = cwd.join("events.jsonl");
    let bin_dir = TempDir::new().unwrap();
    let fno = org_board_bin(bin_dir.path(), BOARD_TWO_ACTIONABLE, 0);

    let wake = serde_json::json!({"session_id": "k-repeat", "kind": "wake", "target_id": "x-1234"});
    lead_event(&events, "lead_action", wake.clone());
    lead_event(
        &events,
        "lead_loop_check",
        serde_json::json!({"session_id": "k-repeat"}),
    );
    lead_event(&events, "lead_action", wake.clone());
    lead_event(
        &events,
        "lead_loop_check",
        serde_json::json!({"session_id": "k-repeat"}),
    );

    let (code, d) = lead_fire(&state, cwd, &events, &fno);

    assert_eq!(
        code, 0,
        "a repeated action must not hold the loop open: {d}"
    );
    assert_eq!(d["termination_reason"], "NoProgress");
}

#[test]
fn an_unknown_driver_is_refused_rather_than_run_against_the_wrong_gate() {
    let (code, json) = fno_agent::loopcheck::run_loop_check_capture(&[
        "loop-check".to_string(),
        "--driver".to_string(),
        "emperor".to_string(),
        "--state".to_string(),
        "/nonexistent".to_string(),
        "--transcript".to_string(),
        "/nonexistent".to_string(),
        "--cwd".to_string(),
        "/tmp".to_string(),
    ]);
    assert_eq!(code, 2);
    let d: serde_json::Value = serde_json::from_str(&json).unwrap();
    let err = d["error"].as_str().unwrap();
    assert!(
        err.contains("emperor") && err.contains("lead"),
        "got: {err}"
    );
}

// ── the quiet-board bound (x-1959's defect) ──────────────────────────────────
//
// A board reading zero actionable while scope nodes sit undelivered used to
// return above the manifest ceiling and the dry-fire backstop: blocking was
// unbounded and the parked state was never recorded. These drive the bound.
