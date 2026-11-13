use super::*;
use crate::event_store::{query_events, EventQuery};
use crate::launch_record::Launch;
use crate::paths::AgentsHome;
use crate::state::{self, Lineage, RegistryEntry};
use serde_json::{json, Value};

fn stamp(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap()
        .to_rfc3339()
}

fn no_roster() -> ClaudeAgentsSnapshot {
    ClaudeAgentsSnapshot::known(Vec::new())
}

fn live_row(name: &str, sid: &str) -> crate::claude_roster::ClaudeAgentRow {
    let mut row = crate::claude_roster::ClaudeAgentRow::new("4ac2b5b1", Some("busy"));
    row.pid = Some(33159);
    row.name = Some(name.into());
    row.session_id = Some(sid.into());
    row
}

#[test]
fn a_live_roster_session_is_a_worker_not_a_gate_stall() {
    let watch = Watch {
        name: Some("t-docs".into()),
        harness: "claude".into(),
        ..Default::default()
    };
    let live = ClaudeAgentsSnapshot::known(vec![live_row("t-docs", "sid-1")]);
    assert_eq!(roster_session(&live, &watch).as_deref(), Some("sid-1"));
    let other = ClaudeAgentsSnapshot::known(vec![live_row("t-other", "sid-2")]);
    assert_eq!(roster_session(&other, &watch), None);
    let mut dead = live_row("t-docs", "sid-1");
    dead.pid = None;
    let mut alive = live_row("t-x", "sid-3");
    alive.pid = Some(1);
    let dead_listing = ClaudeAgentsSnapshot::known(vec![dead, alive]);
    assert_eq!(roster_session(&dead_listing, &watch), None);
    let unnamed = Watch {
        name: None,
        ..watch
    };
    assert_eq!(roster_session(&live, &unnamed), None);
}

#[test]
fn a_spawn_reports_its_stuck_stage_once_and_its_first_turn_quietly() {
    let _lock = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let prior = std::env::var_os("FNO_CLAIMS_ROOT");
    let temp = tempfile::tempdir().unwrap();
    std::env::set_var("FNO_CLAIMS_ROOT", temp.path());
    let home = AgentsHome::at(temp.path().join("agents"));
    let cwd = temp.path().join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = cwd.to_string_lossy().into_owned();
    let caller = "lead-1";
    let t0 = crate::claims::now_ms();
    for name in ["quick", "stuck", "refused"] {
        let launch = Launch {
            harness: "claude".into(),
            resume: None,
            name: Some(name.into()),
            node: None,
            slot: None,
            provider: None,
            cwd: cwd.clone(),
        };
        crate::launch_record::write_record(home.root(), launch, caller, Some("claude"), &cwd, t0)
            .unwrap();
    }
    // The native lane writes its own record of "quick" after admission: one
    // launch, one watch.
    let twin = Launch {
        harness: "claude".into(),
        resume: None,
        name: Some("quick".into()),
        node: None,
        slot: None,
        provider: None,
        cwd: cwd.clone(),
    };
    crate::launch_record::write_record(home.root(), twin, caller, Some("claude"), &cwd, t0 + 5_000)
        .unwrap();
    // A one-shot the caller waits on in its own Bash call is never watched.
    crate::launch_record::record_command(home.root(), "pi -p hello", &cwd, Some(caller), None, t0)
        .unwrap();

    // "quick" resumed a transcript with an old turn, booted, and answered
    // 70s after the call.
    let transcript = temp.path().join("quick.jsonl");
    std::fs::write(
        &transcript,
        format!(
            "{}\n{}\n{}\n",
            json!({"type": "assistant", "timestamp": stamp(t0 - 3_600_000)}),
            json!({"type": "user", "timestamp": stamp(t0 + 20_000)}),
            json!({"type": "assistant", "timestamp": stamp(t0 + 70_000)}),
        ),
    )
    .unwrap();
    let mut row = RegistryEntry::new(
        Some("quick-session".into()),
        Lineage::captured((
            Some(caller.into()),
            Some("claude".into()),
            Some(cwd.clone()),
        )),
    );
    row.name = "quick".into();
    row.harness = Some("claude".into());
    row.substrate = Some("thread".into());
    row.created_at = stamp(t0 + 10_000);
    row.transcript_path = Some(transcript.to_string_lossy().into_owned());
    state::update_registry(&home.registry_json(), |r| r.entries.push(row)).unwrap();

    // The caller already holds the refusal: its launch leaves the claim pool.
    settle_refused(&home, Some(caller), "refused", "no_wait");
    assert!(
        !crate::launch_record::records(home.root(), 0)
            .iter()
            .any(|(_, r)| r.launch.name.as_deref() == Some("refused")),
        "a refused launch is no longer claimable"
    );

    let mut notices: Vec<String> = Vec::new();
    let mut deliver = |argv: &[String], _: &str| {
        assert_eq!(argv[8], caller);
        notices.push(argv[9].clone());
        (0, "accepted-new-turn".to_string(), String::new())
    };
    run_pass_with(&home, t0 + 60_000, &mut deliver, &no_roster).unwrap();

    std::fs::write(watch_dir(&home).join("corrupt.json"), "{broken").unwrap();
    assert!(
        run_pass_with(&home, t0 + DEADLINE_MS, &mut deliver, &no_roster).is_err(),
        "an unreadable watch is reported"
    );
    std::fs::remove_file(watch_dir(&home).join("corrupt.json")).unwrap();
    run_pass_with(&home, t0 + DEADLINE_MS + 60_000, &mut deliver, &no_roster).unwrap();

    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].starts_with("spawn stuck: no first turn 4m"),
        "{}",
        notices[0]
    );
    assert!(notices[0].contains("Stuck at gate"), "{}", notices[0]);
    // The style door refuses a semicolon, so a notice with one never lands.
    assert!(notices.iter().all(|n| !n.contains(';')), "{notices:?}");

    let turns: Vec<Value> = query_events(
        &home.events_jsonl(),
        &EventQuery {
            include_rejected: true,
            ..Default::default()
        },
    )
    .unwrap_or_default()
    .iter()
    .filter_map(|r| serde_json::from_str::<Value>(&r.line).ok())
    .filter(|l| l["type"] == "spawn_first_turn")
    .collect();
    assert_eq!(turns.len(), 1, "{turns:?}");
    assert_eq!(turns[0]["data"]["name"], "quick");
    assert_eq!(turns[0]["data"]["latency_ms"], 70_000);
    assert_eq!(turns[0]["data"]["row_ms"], 10_000);
    assert_eq!(turns[0]["data"]["substrate"], "thread");
    assert!(turns[0]["data"].get("machine").is_some());

    match prior {
        Some(value) => std::env::set_var("FNO_CLAIMS_ROOT", value),
        None => std::env::remove_var("FNO_CLAIMS_ROOT"),
    }
}

#[test]
fn a_synthetic_model_error_is_no_first_turn_and_names_itself_in_the_stage() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("dead.jsonl");
    let t0 = crate::claims::now_ms();
    let synthetic = json!({
        "type": "assistant",
        "timestamp": stamp(t0 + 11_000),
        "message": {
            "role": "assistant",
            "model": "<synthetic>",
            "content": [{"type": "text", "text": "There's an issue with the selected model (opus). It may not exist or you may not have access to it. Run /model to pick a different model."}]
        }
    });
    // The synthetic row alone is no first turn: the watch stays open.
    std::fs::write(&path, format!("{synthetic}\n")).unwrap();
    assert_eq!(
        first_turn(&path, t0, t0 + 60_000),
        None,
        "a synthetic error row is no first turn"
    );

    // A real turn after it is found, past the synthetic rows.
    let real = json!({"type": "assistant", "timestamp": stamp(t0 + 70_000)});
    std::fs::write(&path, format!("{synthetic}\n{real}\n")).unwrap();
    assert_eq!(first_turn(&path, t0, t0 + 60_000), Some(t0 + 70_000));
}

#[test]
fn a_string_content_synthetic_row_is_evidence_too() {
    let temp = tempfile::tempdir().unwrap();
    let t0 = crate::claims::now_ms();
    let path = temp.path().join("dead.jsonl");
    std::fs::write(
        &path,
        format!(
            "{}\n",
            json!({
                "type": "assistant",
                "timestamp": stamp(t0 + 11_000),
                "message": {
                    "role": "assistant",
                    "model": "<synthetic>",
                    "content": "selected model unavailable; try another lane"
                }
            })
        ),
    )
    .unwrap();
    let watch = Watch {
        child_session: Some("f676976f-dead-beef".into()),
        ..Default::default()
    };
    let (stage, evidence) = stage(&watch, None, Some(&path));
    assert_eq!(stage, "first prompt");
    assert!(
        evidence.contains("unavailable. try another lane"),
        "{evidence}"
    );
    assert!(!evidence.contains(';'), "{evidence}");
}

#[test]
fn a_long_synthetic_quote_folds_to_what_the_notice_door_accepts() {
    let temp = tempfile::tempdir().unwrap();
    let t0 = crate::claims::now_ms();
    let path = temp.path().join("dead.jsonl");
    let long = (1..=30)
        .map(|n| format!("word{n}"))
        .collect::<Vec<_>>()
        .join(" ");
    std::fs::write(
        &path,
        format!(
            "{}\n",
            json!({
                "type": "assistant",
                "timestamp": stamp(t0 + 11_000),
                "message": {
                    "role": "assistant",
                    "model": "<synthetic>",
                    "content": [{"type": "text", "text": long}]
                }
            })
        ),
    )
    .unwrap();
    let watch = Watch {
        child_session: Some("dead-beef-cafe".into()),
        ..Default::default()
    };
    // The folded quote itself: 25 words plus the ellipsis.
    let quote = synthetic_error(&path).expect("synthetic evidence");
    assert!(quote.ends_with("..."), "{quote}");
    assert_eq!(quote.split_whitespace().count(), 26, "{quote}");
    assert!(!quote.contains(';'), "{quote}");
    // The stage line carries the quote behind its prefix and still folds.
    let (_, evidence) = stage(&watch, None, Some(&path));
    assert!(evidence.ends_with("..."), "{evidence}");
    assert!(evidence.contains("word25"), "{evidence}");
    assert!(!evidence.contains("word26"), "{evidence}");
}
