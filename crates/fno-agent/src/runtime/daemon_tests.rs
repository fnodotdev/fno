#[path = "daemon/tests/blocking_bound_tests.rs"]
mod blocking_bound_tests;
#[path = "daemon/tests/store_socket_sweep_tests.rs"]
mod store_socket_sweep_tests;
use super::blocking_bound::directory_bytes_within;
use super::*;
use crate::client_commands::RowLiveness;

fn tmp_home(tag: &str) -> AgentsHome {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "fno-agent-daemon-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let home = AgentsHome::at(&p);
    home.ensure_root().unwrap();
    crate::paths::pin_test_claims_root(&p.join("claims-root"));
    home
}

// x-ef7f: the connect-probe singleton guard let a busy-but-alive
// incumbent read as absent, so every losing race added a new supervisor
// instead of replacing the incumbent. The flock-based guard must resolve
// N concurrent binders to exactly one winner.
#[tokio::test]
async fn bind_supervisor_socket_concurrent_only_one_survives() {
    // A real UnixListener::bind needs its path to fit sockaddr_un's short
    // sun_path buffer (104 bytes on macOS); tmp_home()'s long
    // tag+pid+nanos name overflows that once `/supervisor.sock` is
    // appended, so this test (the first here to actually bind a socket)
    // builds a short path directly under /tmp instead.
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::path::PathBuf::from(format!("/tmp/fa-cb-{}-{n}", std::process::id()));
    let home = AgentsHome::at(&dir);
    home.ensure_root().unwrap();
    let mut handles = Vec::new();
    for _ in 0..8 {
        let h = home.clone();
        handles.push(tokio::spawn(
            async move { bind_supervisor_socket(&h).await },
        ));
    }
    // Collect every result FIRST, then count: counting inside the join loop
    // dropped the first winner's lock guard to a task still mid-retry and
    // read as three winners. The production holder keeps its guard for life.
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }
    let mut ok_count = 0;
    let mut already_running = 0;
    for r in &results {
        match r {
            Ok(_) => ok_count += 1,
            Err(DaemonError::AlreadyRunning(_)) => already_running += 1,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert_eq!(ok_count, 1, "exactly one bind must survive the race");
    assert_eq!(already_running, 7);
    drop(results);
    std::fs::remove_dir_all(home.root()).ok();
}

fn read_events(home: &AgentsHome) -> Vec<Value> {
    // Committed rows, not journal bytes: the store cutover stopped journal
    // appends, so emitted events live only in the store beside the journal.
    let journal = home.events_jsonl();
    let _ = crate::event_store::import_all(&journal);
    crate::event_store::query_events(&journal, &crate::event_store::EventQuery::default())
        .unwrap_or_default()
        .iter()
        .filter_map(|r| serde_json::from_str::<Value>(&r.line).ok())
        .collect()
}

// Generic one-shot ask row builder (empty short_id + no pid, owns no
// worktree). The `exited_at` argument predates reverse-join retirement;
// it now only feeds the liveness ladder's heartbeat rung.
fn ask_row(name: &str, exited_at: Option<&str>) -> RegistryEntry {
    RegistryEntry {
        substrate: None,
        node: None,
        spawned_by_session: None,
        spawned_by_harness: None,
        spawned_by_cwd: None,
        launch_account: None,
        related_session_id: None,
        origin: None,
        name: name.into(),
        short_id: String::new(),
        legacy_provider: "claude".into(),
        provider: None,
        model: None,
        model_basis: None,
        effort: None,
        harness: None,
        // x-7bcd: needs a resolvable handle (leg 3); deterministic per
        // name so two rows never collide.
        harness_session_id: Some(format!("{name}-sess")),
        predecessor_session_ids: Vec::new(),
        forked_from_session_id: None,
        route_provider_id: None,
        model_name: None,
        account_record_id: None,
        cwd: "/tmp".into(),
        project_root: String::new(),
        session_id: None,
        spawn_trigger: None,
        legacy_claude_short_id: None,
        claude_session_uuid: None,
        messaging_socket_path: None,
        codex_session_id: None,
        gemini_session_id: None,
        mcp_channel_id: None,
        cc_session_id: None,
        host_mode: None,
        status: AgentStatus::Exited,
        last_message_at: None,
        created_at: "2020-01-01T00:00:00Z".into(),
        pid: None,
        pid_start_time: None,
        keeper_child_pid: None,
        log_path: None,
        last_reconciled_at: None,
        inside_leg: None,
        exited_at: exited_at.map(str::to_string),
        mux: None,
        screen_state: None,
        role_level: None,
        role_scope: None,
        role_grantor: None,
        route_settings_path: None,
        fno_id: None,
        delivery_policy: None,
        sandbox_posture: None,
        ..Default::default()
    }
}

fn claude_rm_row(name: &str, short_id: &str, session_id: &str) -> RegistryEntry {
    let mut row = ask_row(name, Some("2020-01-01T00:00:00Z"));
    row.short_id = short_id.to_string();
    row.harness = Some("claude".into());
    row.harness_session_id = Some(session_id.into());
    row
}

fn claude_row_then_absent(
    short_id: &'static str,
    state: &'static str,
) -> impl Fn() -> crate::claude_roster::ClaudeAgentsSnapshot {
    let calls = std::sync::atomic::AtomicUsize::new(0);
    move || {
        if calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
            crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
                crate::claude_roster::ClaudeAgentRow::new(short_id, Some(state)),
            ])
        } else {
            crate::claude_roster::ClaudeAgentsSnapshot::known(Vec::new())
        }
    }
}

#[tokio::test]
async fn rm_requires_the_post_list_to_prove_the_claude_row_is_gone() {
    let home = short_home("rmpostlist");
    let row = claude_rm_row(
        "stopped-worker",
        "aaabbb11",
        "aaabbb11-1111-2222-3333-444444444444",
    );
    state::update_registry(&home.registry_json(), |registry| registry.entries.push(row)).unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agent-worker"));
    let request = Request::new(1, "agent.rm", json!({"name": "stopped-worker"}));

    let response = handle_rm_with(
        &ctx,
        &request,
        &|| {
            crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
                crate::claude_roster::ClaudeAgentRow::new("aaabbb11", Some("stopped")),
            ])
        },
        &|_| Ok(()),
        &|_, _| crate::daemon::rm_teardown::StopLeg::Asked,
        &|_, _| Ok(true),
        &|_, _| PaneProbe::Unknown,
    )
    .await;

    assert!(response
        .error()
        .unwrap()
        .message
        .contains("survives successful claude rm"));
    assert_eq!(
        state::load_registry(&home.registry_json())
            .unwrap()
            .entries
            .len(),
        1
    );
    std::fs::remove_dir_all(home.root()).ok();
}

/// A temp dir whose `.git` is a FILE: linked-worktree shape, no real repo
/// behind it (the gate and the removal are injected, so none is needed).

#[test]
fn output_with_timeout_bounds_a_stalled_subprocess() {
    // The rm path's budget: a fast child answers, a stalled one is
    // bounded instead of parking the daemon's rm handler.
    let fast = output_with_timeout(
        {
            let mut c = std::process::Command::new("echo");
            c.arg("ok");
            c
        },
        10,
    );
    assert!(fast.is_some(), "a fast child answers");
    let stalled = output_with_timeout(
        {
            let mut c = std::process::Command::new("sleep");
            c.arg("30");
            c
        },
        1,
    );
    let stalled = stalled.expect("a stalled child is killed, not left running");
    assert!(
        !stalled.status.success(),
        "the killed child reads as a failed call"
    );
}

#[test]
fn stop_refusal_names_a_pane_kill_the_mux_parser_accepts() {
    // The refusal string and the parser drift independently: the refusal
    // once printed `main:76` while the parser demanded a bare number, so
    // the instrument named a way out that errored with EXIT_USAGE. Hold
    // both sides in one test: build the refusal through the same renderer
    // handle_stop prints (its probe-unanswered branch) and feed the
    // command to the real parse_pane_args, no hardcoded expected string
    // anywhere. Kept off handle_stop itself: the mux pane probe shells
    // bare `fno` from PATH, so on a machine whose live mux runs a session
    // named `main` the probe answers Absent and the pid branch, which
    // names no kill command, replaces this one.
    let printed =
        stop_refusal_detail::pane_row_refusal("pane-worker", "main", 76, PaneProbe::Unknown, None);
    let selector = printed
        .split("Kill the pane: `")
        .nth(1)
        .expect("refusal names the kill command")
        .split('`')
        .next()
        .expect("the printed command is backtick-closed")
        .strip_prefix("fno mux pane kill ")
        .expect("the printed command is the pane kill command");
    // The fno crate's twin test feeds this one command to parse_pane_args.
    assert_eq!(
        format!("fno mux pane kill {selector}"),
        stop_refusal_detail::pane_kill_command("main", 76)
    );
}

#[test]
fn linked_worktree_detection_separates_owners_from_passers_through() {
    // The whole ownership test is `.git` being a FILE (a `gitdir:` pointer)
    // rather than a directory. Getting it backwards would either pin every
    // row again or reap rows that DO own a dirty worktree.
    let dir = tempfile::tempdir().expect("tmpdir");
    let base = dir.path();

    let canonical = base.join("canonical");
    std::fs::create_dir_all(canonical.join(".git")).unwrap();
    assert!(
        !is_linked_worktree(canonical.to_str().unwrap()),
        "a .git DIRECTORY is the canonical checkout; the row owns nothing removable"
    );

    let linked = base.join("linked");
    std::fs::create_dir_all(&linked).unwrap();
    std::fs::write(linked.join(".git"), "gitdir: /somewhere/.git/worktrees/x\n").unwrap();
    assert!(
        is_linked_worktree(linked.to_str().unwrap()),
        "a .git FILE is a linked worktree; cleanliness decides its row"
    );

    let plain = base.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert!(!is_linked_worktree(plain.to_str().unwrap()));

    // Unreadable and empty both fail closed toward "owns nothing", so the row
    // is judged on terminal status and grace instead of waiting forever on a
    // cleanliness answer that can never arrive.
    assert!(!is_linked_worktree(""));
    assert!(!is_linked_worktree("/nonexistent/path/that/cannot/be/read"));
}

// The gc ladder, reap-receipt gate and plan_reconcile families, moved
// verbatim into their own module (file budget: test motion is the
// sanctioned shrink; this file is far over the shrink-only line).
#[path = "daemon/tests/gc_mux_member.rs"]
mod gc_mux_member;
#[path = "daemon/tests/gc_receipts.rs"]
mod gc_receipts;
#[path = "daemon/tests/keeper_sweep.rs"]
pub(in crate::runtime::daemon) mod keeper_sweep;
#[path = "daemon/tests/reap_holds.rs"]
mod reap_holds;
#[path = "daemon/tests/reap_session.rs"]
mod reap_session;
#[path = "daemon/tests/rm_refusal.rs"]
mod rm_refusal;
// The rm success family: same module motion as gc_receipts (file budget).
#[path = "daemon/tests/rm_success.rs"]
mod rm_success;
#[path = "daemon/tests/stop_claims.rs"]
mod stop_claims;
// The stale-sweep test family (x-39f4): same file-budget motion as the
// families above.
#[path = "daemon/tests/incident_pause.rs"]
mod incident_pause;

// The codex thread lane's spawn/registry/resume test family, moved
// verbatim into its own module for the same reason as gc_receipts above:
// file budget, test motion is the sanctioned shrink.
#[path = "daemon/tests/codex_thread_lane.rs"]
mod codex_thread_lane;

// --- plan_reconcile (US6.9): tri-state, status-aware transitions, budget ---

fn rentry(name: &str, status: AgentStatus, last_reconciled: Option<&str>) -> RegistryEntry {
    RegistryEntry {
        substrate: None,
        node: None,
        spawned_by_session: None,
        spawned_by_harness: None,
        spawned_by_cwd: None,
        launch_account: None,
        related_session_id: None,
        origin: None,
        name: name.into(),
        short_id: name.into(),
        legacy_provider: "codex".into(),
        provider: None,
        model: None,
        model_basis: None,
        effort: None,
        harness: None,
        harness_session_id: None,
        predecessor_session_ids: Vec::new(),
        forked_from_session_id: None,
        route_provider_id: None,
        model_name: None,
        account_record_id: None,
        cwd: "/tmp".into(),
        project_root: "/tmp".into(),
        session_id: Some("sid".into()),
        spawn_trigger: None,
        legacy_claude_short_id: None,
        claude_session_uuid: None,
        messaging_socket_path: None,
        codex_session_id: None,
        gemini_session_id: None,
        mcp_channel_id: None,
        host_mode: None,
        cc_session_id: None,
        status,
        last_message_at: None,
        created_at: "t".into(),
        pid: None,
        pid_start_time: None,
        keeper_child_pid: None,
        log_path: None,
        last_reconciled_at: last_reconciled.map(String::from),
        inside_leg: None,
        exited_at: None,
        mux: None,
        screen_state: None,
        role_level: None,
        role_scope: None,
        role_grantor: None,
        route_settings_path: None,
        fno_id: None,
        delivery_policy: None,
        sandbox_posture: None,
        ..Default::default()
    }
}

/// A codex THREAD row: no short_id, interactive host mode, full session
/// id, and a recorded rollout path (the durable resume object). Shared by
/// the reconcile/liveness family here and by the codex-thread-lane family
/// in `codex_thread_lane.rs`, which reaches it through the glob.
fn thread_entry(name: &str, status: AgentStatus, log_path: Option<String>) -> RegistryEntry {
    let mut entry = rentry(name, status, None);
    entry.pid = Some(999_999_999);
    entry.short_id = String::new();
    entry.legacy_provider = String::new();
    entry.harness = Some("codex".into());
    entry.host_mode = Some(crate::state::HOST_MODE_INTERACTIVE.into());
    entry.session_id = None;
    entry.harness_session_id = Some(format!("0198thread-{name}-00000000000000"));
    entry.log_path = log_path;
    entry
}

// --- find_uuid_backfill_row (x-c393): backfill a null-uuid bg row ---------

/// A `claude --bg` row: jobId in `short_id`, `claude_session_uuid` null.
/// A transcript file for a fixture row that is genuinely finished.
///
/// The corroboration gate needs a POSITIVE reading that a worker stopped
/// writing, and these tests run with a zero grace window, so any file that
/// already exists reads as stale. A fixture with no transcript at all reads
/// as UNKNOWN and is kept, which is the correct production behaviour and
/// would silently hollow out these assertions.
fn stale_log(dir: &std::path::Path) -> String {
    let p = dir.join("transcript.jsonl");
    std::fs::write(&p, "{}\n").unwrap();
    // BACKDATE IT. A file written this second reads as fresh even against a
    // zero-length window, so a fixture that only creates the file proves the
    // opposite of what it claims.
    assert!(std::process::Command::new("touch")
        .args(["-t", "200001010000", &p.to_string_lossy()])
        .status()
        .expect("touch runs")
        .success());
    p.to_string_lossy().into_owned()
}

fn bg_claude_row(name: &str, short_id: &str) -> RegistryEntry {
    let mut e = rentry(name, AgentStatus::Live, None);
    e.legacy_provider = "claude".into();
    e.short_id = short_id.into();
    e.claude_session_uuid = None;
    // A row fno itself spawned: the retirement origin gate retires only
    // these, so every retirement-path fixture starts from spawn.
    e.origin = Some("spawn".into());
    // x-7bcd: needs a resolvable handle; short_id is the transport key
    // this row is actually tested against, not one of the three legs.
    e.log_path = Some(format!("/tmp/{name}.log"));
    e
}

#[test]
fn find_uuid_backfill_row_refuses_ambiguous_short_collision() {
    // AC1-ERR: two null-uuid rows share the short-id -> refuse, don't guess.
    let rows = vec![
        bg_claude_row("w1", "3228ccad"),
        bg_claude_row("w2", "3228ccad"),
    ];
    assert!(matches!(
        find_uuid_backfill_row(&rows, "3228ccad-c078-4b53-a8c9-7199b831eae4"),
        UuidBackfill::Ambiguous
    ));
}

#[test]
fn reconcile_flips_unreachable_live_to_orphaned_and_recovers_orphaned() {
    let entries = vec![
        rentry("live-but-gone", AgentStatus::Live, None),
        rentry("back-from-dead", AgentStatus::Orphaned, None),
    ];
    let (changes, out) = plan_reconcile(
        &entries,
        |e| match e.name.as_str() {
            "live-but-gone" => Ok(false), // unreachable
            _ => Ok(true),                // reachable
        },
        || false,
        |_| true,
        |_| false,
        |_| false,
        |_| false,
        |_| false,
        |_| RowLiveness::Alive, // x-5d96 liveness: Alive flips nothing
        true,                   // roster readable: the flip needs a successful roster read
        1_700_000_000_000,
    );
    assert_eq!(out.orphans, vec!["live-but-gone".to_string()]);
    assert_eq!(out.recovered, vec!["back-from-dead".to_string()]);
    assert_eq!(out.updated.len(), 2);
    // Both probed -> both get a status change recorded.
    assert_eq!(
        changes[0].new_status,
        Some(AgentStatus::Orphaned),
        "unreachable live agent should orphan"
    );
    assert_eq!(changes[1].new_status, Some(AgentStatus::Live));
}

#[test]
fn reconcile_store_hit_does_not_resurrect_a_pid_dead_row() {
    // x-830c: a store that never evicts (opencode keeps its session rows
    // forever) answers Ok(true) long after the pane is gone. Recovery needs
    // the pid too, or every sweep would flip a dead orphan back to Live and
    // discovery would hand out a recipient nobody drains.
    let entries = vec![
        rentry("dead-orphan", AgentStatus::Orphaned, None),
        rentry("live-orphan", AgentStatus::Orphaned, None),
    ];
    let (changes, out) = plan_reconcile(
        &entries,
        |_| Ok(true), // session still in the store for both
        || false,
        |e| e.name == "live-orphan",
        |_| false,
        |_| false,
        |_| false,
        |_| false,
        |_| RowLiveness::Alive, // x-5d96 liveness: Alive flips nothing
        true,                   // roster readable: the flip needs a successful roster read
        1_700_000_000_000,
    );
    assert_eq!(
        changes[0].new_status, None,
        "a store hit must not recover a row whose pid is dead"
    );
    assert_eq!(
        changes[1].new_status,
        Some(AgentStatus::Live),
        "a store hit on a live pid still recovers"
    );
    assert_eq!(out.recovered, vec!["live-orphan".to_string()]);
}

#[test]
fn reconcile_does_not_reap_a_bg_thread_that_is_live_in_claudes_roster() {
    // x-beb7: a `claude --substrate bg` thread has claude's harness, no
    // footnote pid and no mux, so it matches `is_one_shot_ask` exactly like a
    // finished ask -- but it is a RUNNING process owned by claude's daemon.
    // Reaping it unprobed made `fno-agent wait --state done` return
    // "done (via exit)" within seconds for a worker whose transcript was
    // still growing, which reads to a waiting lead as a dead teammate.
    let entries = vec![bg_claude_row("think-web-copy", "35570a01")];
    assert!(
        entries[0].is_one_shot_ask(),
        "a bg thread must still match the ask shape, or this test proves nothing"
    );

    // Present in the roster == running: leave the row alone.
    let (changes, out) = plan_reconcile(
        &entries,
        |_| Ok(true),
        || false,
        |_| true,
        |_| true,
        |_| false,
        |_| false,
        |_| false,
        |_| RowLiveness::Alive, // x-5d96 liveness: Alive flips nothing
        true,                   // roster readable: the flip needs a successful roster read
        1_700_000_000_000,
    );
    assert_eq!(
        changes[0].new_status, None,
        "a bg thread claude's daemon still lists must not be reaped to exited"
    );
    assert!(out.updated.is_empty());

    // Absent from the roster with a silent ladder == genuinely gone: the ask
    // reap still applies, so this is a liveness check, not a blanket
    // exemption for claude rows.
    let (changes, out) = plan_reconcile(
        &entries,
        |_| Ok(true),
        || false,
        |_| true,
        |_| false,
        |_| false,
        |_| false,
        |_| false,
        |_| RowLiveness::Unknown, // absent + no positive marker reaps; a positive
        // marker outranks absence (law d-10a72d88), so the reap applies on
        // silence, never against evidence
        true, // roster readable: the flip needs a successful roster read
        1_700_000_000_000,
    );
    assert_eq!(changes[0].new_status, Some(AgentStatus::Exited));
    assert_eq!(out.updated, vec!["think-web-copy".to_string()]);
}

#[test]
fn buffer_pending_report_highest_seq_wins_and_is_bounded() {
    use std::collections::HashMap;
    let rep = |seq| state::InsideLegReport {
        state: state::InsideLegState::Working,
        seq,
        reason: None,
        received_at: "2026-06-27T00:00:00Z".into(),
        ttl_ms: None,
        posture: None,
    };
    let mut map: HashMap<String, state::InsideLegReport> = HashMap::new();

    // First buffer for a session: stored.
    assert!(matches!(
        buffer_pending_report(&mut map, "s1", rep(2)),
        BufferOutcome::Buffered
    ));
    assert_eq!(map["s1"].seq, 2);

    // A reordered/duplicate early push (seq <= buffered) is dropped, buffer unchanged.
    assert!(matches!(
        buffer_pending_report(&mut map, "s1", rep(1)),
        BufferOutcome::StaleSeq { last: 2 }
    ));
    assert_eq!(
        map["s1"].seq, 2,
        "stale early push must not regress the buffer"
    );

    // A newer push for the same session advances it.
    assert!(matches!(
        buffer_pending_report(&mut map, "s1", rep(5)),
        BufferOutcome::Buffered
    ));
    assert_eq!(map["s1"].seq, 5);

    // Fill to cap with distinct sessions, then a NEW session is dropped (Full),
    // while an existing session still advances.
    for i in 0..PENDING_INSIDE_LEG_CAP {
        buffer_pending_report(&mut map, &format!("fill{i}"), rep(1));
    }
    assert!(map.len() >= PENDING_INSIDE_LEG_CAP);
    assert!(matches!(
        buffer_pending_report(&mut map, "brand-new", rep(1)),
        BufferOutcome::Full
    ));
    assert!(!map.contains_key("brand-new"));
    assert!(
        matches!(
            buffer_pending_report(&mut map, "s1", rep(9)),
            BufferOutcome::Buffered
        ),
        "an already-buffered session advances even at cap (no new key)"
    );
}

/// A pane-hosted codex row: empty short_id (matches the real spawn shape),
/// a mux ref, and whatever pid/session id the caller sets afterward.
fn codex_pane_row(name: &str) -> RegistryEntry {
    let mut e = ask_row(name, None);
    e.harness = Some("codex".into());
    // x-7bcd: a real codex pane starts id-less (late-bind is what sets
    // harness_session_id), so undo ask_row's default and use a log_path
    // for the resolvable handle instead.
    e.harness_session_id = None;
    e.log_path = Some(format!("/tmp/{name}.log"));
    e.status = AgentStatus::Live;
    e.mux = Some(state::MuxRef {
        session: "main".into(),
        pane_id: 1,
    });
    e
}

#[test]
fn late_bind_surfaces_registry_write_failure() {
    let home = tmp_home("late-bind-write-failure");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let me = std::process::id();
    let Some(my_start) = process_start_time(me) else {
        return;
    };
    state::update_registry(&home.registry_json(), |r| {
        let mut existing = ask_row("existing", None);
        existing.harness = Some("codex".into());
        existing.harness_session_id = Some("duplicate-session".into());
        r.entries.push(existing);

        let mut candidate = codex_pane_row("pane-a");
        candidate.pid = Some(me);
        candidate.pid_start_time = Some(my_start);
        r.entries.push(candidate);
    })
    .unwrap();

    let error =
        late_bind_codex_sessions(&home, &emitter, &|_| Some("duplicate-session".to_string()))
            .expect_err("duplicate session identity must fail the late-bind write");

    let reg = state::load_registry(&home.registry_json()).unwrap();
    assert!(reg.find("pane-a").unwrap().harness_session_id.is_none());
    assert!(error.contains("late-bind registry write failed for pane-a"));
    let events = read_events(&home);
    assert!(events.iter().any(|e| {
        e.get("type").and_then(Value::as_str) == Some("agent_late_bind_failed")
            && e.get("data")
                .and_then(|data| data.get("name"))
                .and_then(Value::as_str)
                == Some("pane-a")
    }));
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn late_bind_does_not_apply_liveness_sampled_from_a_stale_predecessor() {
    let home = tmp_home("late-bind-stale-predecessor");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let me = std::process::id();
    let Some(my_start) = process_start_time(me) else {
        return;
    };
    state::update_registry(&home.registry_json(), |r| {
        let mut entry = codex_pane_row("pane-a");
        entry.pid = Some(me);
        entry.pid_start_time = Some(my_start);
        entry.harness_session_id = Some("session-a".into());
        r.entries.push(entry);
    })
    .unwrap();

    let switched = std::cell::Cell::new(false);
    late_bind_codex_sessions_with_transition(
        &home,
        &emitter,
        &|_| {
            if !switched.replace(true) {
                state::update_registry(&home.registry_json(), |r| {
                    r.find_mut("pane-a").unwrap().harness_session_id = Some("session-c".into());
                })
                .unwrap();
            }
            Some("session-b".to_string())
        },
        &|session| (session == "session-a").then_some(false),
    )
    .unwrap();

    let reg = state::load_registry(&home.registry_json()).unwrap();
    assert_eq!(reg.entries.len(), 1);
    assert_eq!(
        reg.find("pane-a").unwrap().harness_session_id.as_deref(),
        Some("session-c")
    );
    std::fs::remove_dir_all(home.root()).ok();
}

/// A stub family-1 probe answer for the tests that only pin the state.
/// `observed_model` null here stands for "this probe did not answer it",
/// which the row renders as `no-transcript` rather than inventing a model.
///
/// `reachability: None` deliberately exercises the COMPATIBILITY FALLBACK in
/// `rendered_status_from_truth` (a `fno` too old to emit the verdict), which
/// is what keeps these pre-existing state-mapping assertions meaningful.
/// `probe_reachable` below covers the current wire.
pub(super) fn probe(state: &str) -> Option<crate::truth_probe::TruthProbe> {
    Some(crate::truth_probe::TruthProbe {
        state: state.into(),
        reachability: None,
        basis: None,
        last_activity_age_s: None,
        last_activity_basis: None,
        last_event_at: None,
        last_message: None,
        observed_model: Value::Null,
        provider_refusal: None,
        harness_title: None,
    })
}

// -- The shared liveness ladder (x-5d96) ---------------------------------

/// A per-row prober mirroring [`fn@live_liveness_prober`] for the staged
/// answer: same positive-death fold, index built per call.
fn ladder_claude_row(name: &str, short_id: &str) -> RegistryEntry {
    let mut e = bg_claude_row(name, short_id);
    e.status = AgentStatus::Live;
    e
}

fn heartbeat(state: state::InsideLegState, received_at: &str) -> state::InsideLegReport {
    state::InsideLegReport {
        state,
        seq: 3,
        reason: None,
        received_at: received_at.into(),
        ttl_ms: None,
        posture: None,
    }
}

// -- Rung 4: codex rollout freshness (x-798a) ----------------------------

fn codex_truth_none(_uuid: &str) -> Option<String> {
    None
}

/// A codex registry row in the measured x-798a shape: live-ish status,
/// no claude surfaces, one harness session id, no exit stamp (no pid to
/// confirm dead, no reconcile to terminal - the row never gets one).
fn ladder_codex_row(name: &str, session_id: &str) -> RegistryEntry {
    let mut e = rentry(name, AgentStatus::Live, None);
    e.harness = Some("codex".into());
    e.harness_session_id = Some(session_id.into());
    e.claude_session_uuid = None;
    e
}

/// The real codex store shape: nested date dirs, the session id embedded
/// in a `rollout-*.jsonl` filename - what `HarnessStoreIndex` resolves.
fn write_rollout(root: &Path, session_id: &str) -> std::path::PathBuf {
    let dir = root.join("2026").join("09").join("01");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("rollout-2026-09-01T10-00-00-{session_id}.jsonl"));
    std::fs::write(&p, "{\"type\":\"session_meta\"}\n").unwrap();
    p
}

#[test]
fn a_stamped_codex_row_is_never_resurrected_by_a_fresh_rollout() {
    // The stop command writes a POSITIVE exit stamp; the rollout it leaves
    // behind stays fresh-written for the whole window. Rung 4 must go
    // silent on a stamped row, or the sweep would clear the stamp and
    // resurrect a deliberately stopped row on freshness alone (the codex
    // review finding). The heartbeat rung draws the same line - only
    // ADVANCEMENT past the stamp answers, never a quiet file.
    let home = tmp_home("ladder-codex-stamped");
    let codex = tempfile::tempdir().unwrap();
    write_rollout(codex.path(), "cdx-stopped");
    let mut e = ladder_codex_row("codex-stopped", "cdx-stopped");
    e.exited_at = Some("2026-09-01T10:30:00Z".into());
    let answer = crate::client_commands::row_liveness_with_codex_root(
        &e,
        &crate::claude_ask::ClaudeHome::at(home.root()),
        codex.path(),
        codex_truth_none,
    );
    assert_eq!(answer, RowLiveness::Unknown);
    assert_ne!(answer, RowLiveness::Alive);
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn the_ladder_never_returns_dead() {
    // Only a positive death proof may answer Dead, and absence never is
    // one. Sweep every rung combination the ladder can see; no input
    // yields Dead.
    let home = tmp_home("ladder-never-dead");
    let ch = crate::claude_ask::ClaudeHome::at(home.root());
    for truth in [
        None,
        Some("done"),
        Some("stalled"),
        Some("unreachable"),
        Some("working"),
        Some("watching"),
        Some("your-move"),
    ] {
        for exited in [None, Some("2026-08-01T00:00:02Z")] {
            for beat in [
                None,
                Some(("2026-07-31T00:00:00Z", false)), // before exited: silence
                Some(("2026-08-01T00:00:30Z", true)),  // after exited: Alive
            ] {
                let mut e = ladder_claude_row("row", "quiet02");
                e.exited_at = exited.map(String::from);
                e.inside_leg = beat.map(|(at, _)| heartbeat(state::InsideLegState::Working, at));
                e.claude_session_uuid = Some("3228ccad-c078-4b53-a8c9-7199b831eae4".into());
                let answer =
                    crate::client_commands::row_liveness_with(&e, &ch, |_| truth.map(String::from));
                assert_ne!(
                    answer,
                    RowLiveness::Dead,
                    "ladder answered Dead for truth={truth:?} exited={exited:?} beat={beat:?}"
                );
            }
        }
    }
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn an_unreadable_roster_never_flips_the_zombie_arm() {
    // codex P1 (PR 1329): an unreadable roster is unknown liveness. The
    // fail-closed bg_live reading must not combine with a transient
    // ladder failure to orphan every live bg worker - the flip needs a
    // roster read that SUCCEEDED.
    let entries = vec![bg_claude_row("maybe", "mayb0001")];
    let (changes, _) = plan_reconcile(
        &entries,
        |_| Ok(true),
        || false,
        |_| true,
        |_| true,
        |_| false, // bg_live fail-closed true on the unreadable roster
        |_| false,
        |_| false,
        |_| RowLiveness::Unknown,
        false, // the roster read itself FAILED
        1_700_000_000_000,
    );
    assert_eq!(changes[0].new_status, None);
}

/// Adapt a per-handle answer into the BATCH seam `handle_list_with_truth`
/// takes, for the rendering tests below - they assert what a row renders,
/// never how many processes paid for it.
///
/// Deliberately NOT used by
/// `list_pays_exactly_one_batch_call_for_the_whole_page`: an adapter that
/// fans a batch back out per handle would render identically whether the
/// handler called it once or once per row, so the call SHAPE needs its own
/// test against the raw seam.
fn per_handle(
    f: impl Fn(&str) -> Option<crate::truth_probe::TruthProbe>,
) -> impl Fn(
    &[String],
) -> (
    std::collections::HashMap<String, crate::truth_probe::TruthProbe>,
    crate::truth_probe::BatchOutcome,
) {
    move |handles: &[String]| {
        let map: std::collections::HashMap<String, crate::truth_probe::TruthProbe> = handles
            .iter()
            .filter_map(|h| Some((h.clone(), f(h)?)))
            .collect();
        (map, crate::truth_probe::BatchOutcome::Measured)
    }
}

/// A probe carrying the shared verdict, as a current `fno` emits it.
pub(super) fn probe_with_verdict(
    state: &str,
    reachability: &str,
) -> Option<crate::truth_probe::TruthProbe> {
    Some(crate::truth_probe::TruthProbe {
        state: state.into(),
        reachability: Some(reachability.into()),
        basis: Some("transcript".into()),
        last_activity_age_s: Some(12.0),
        last_activity_basis: None,
        last_event_at: Some("2026-08-15T17:00:00+00:00".into()),
        last_message: Some("Still growing (101 lines, 26 percent through the pytest run)".into()),
        observed_model: Value::Null,
        provider_refusal: None,
        harness_title: None,
    })
}

pub(super) fn test_ctx(home: AgentsHome, worker_bin: PathBuf) -> Ctx {
    // The emitter commits to the store beside the home journal: `/dev/null`
    // was a sink under the journal regime, but the store needs a real path.
    let events_path = home.events_jsonl();
    Ctx {
        emitter: EventEmitter::new(events_path, "daemon"),
        home,
        opts: DaemonOptions {
            idle_exit: Duration::from_secs(1800),
            worker_bin,
            reconcile_on_start: true,
            agents_config_cwd: PathBuf::from("/dev/null"),
            // Off in tests: a unit test must never spawn a real `fno inbox notify`.
            notify_on_blocked: false,
            notify_on_done: false,
        },
        started_at: std::time::Instant::now(),
        exe_fingerprint: crate::drift::ExeFingerprint::current(),
        pid_start_time: process_start_time(std::process::id()),
        pending_inside_leg: std::sync::Mutex::new(std::collections::HashMap::new()),
        codex_threads: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
    }
}

/// A SHORT-path agents home under `/tmp`: a worker's `<root>/<short_id>/worker.sock`
/// must fit in SUN_LEN (~104 chars on macOS) so switchboard tests bind real sockets.
pub(super) fn short_home(tag: &str) -> AgentsHome {
    use std::sync::atomic::{AtomicU32, Ordering};
    static C: AtomicU32 = AtomicU32::new(0);
    let n = C.fetch_add(1, Ordering::Relaxed);
    // Nanosecond tail: a pid alone repeats across runs (the 2026-10-31
    // keeper-sweep base wipe), so no recycled path is ever re-derived.
    let tail = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let p = PathBuf::from(format!("/tmp/fnosb{tag}{}_{n}_{tail}", std::process::id()));
    let home = AgentsHome::at(&p);
    home.ensure_root().unwrap();
    crate::paths::pin_test_claims_root(&p.join("claims-root"));
    home
}

/// Seed a held-stream-thread registry row (claude + full UUID + Live).
pub(super) fn seed_stream_row(home: &AgentsHome, name: &str, short_id: &str) {
    state::update_registry(&home.registry_json(), |r| {
        r.entries.push(RegistryEntry {
            substrate: None,
            node: None,
            spawned_by_session: None,
            spawned_by_harness: None,
            spawned_by_cwd: None,
            launch_account: None,
            related_session_id: None,
            origin: None,
            name: name.into(),
            short_id: short_id.into(),
            legacy_provider: "claude".into(),
            provider: None,
            model: None,
            model_basis: None,
            effort: None,
            harness: None,
            harness_session_id: None,
            predecessor_session_ids: Vec::new(),
            forked_from_session_id: None,
            route_provider_id: None,
            model_name: None,
            account_record_id: None,
            cwd: "/tmp".into(),
            project_root: "/tmp".into(),
            session_id: None,
            spawn_trigger: None,
            legacy_claude_short_id: None,
            claude_session_uuid: Some(format!("uuid-{short_id}")),
            messaging_socket_path: None,
            codex_session_id: None,
            gemini_session_id: None,
            mcp_channel_id: None,
            cc_session_id: None,
            host_mode: None,
            status: AgentStatus::Live,
            last_message_at: None,
            created_at: "2026-06-09T00:00:00Z".into(),
            pid: None,
            pid_start_time: None,
            keeper_child_pid: None,
            log_path: None,
            last_reconciled_at: None,
            inside_leg: None,
            exited_at: None,
            mux: None,
            screen_state: None,
            role_level: None,
            role_scope: None,
            role_grantor: None,
            route_settings_path: None,
            fno_id: None,
            delivery_policy: None,
            sandbox_posture: None,
            ..Default::default()
        });
    })
    .unwrap();
}

#[test]
fn list_row_key_set_matches_shared_contract() {
    const CONTRACT: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../schemas/agents-list-row.json"
    ));
    let contract: Value = serde_json::from_str(CONTRACT).expect("contract is valid JSON");
    assert_eq!(
        contract["projection_omissions"],
        json!([]),
        "model and model_basis are projected with their basis, never omitted"
    );
    let mut expected: std::collections::BTreeSet<String> = contract["required"]
        .as_array()
        .expect("required is an array")
        .iter()
        .map(|k| k.as_str().unwrap().to_string())
        .collect();
    expected.extend(
        contract["rust_only"]["keys"]
            .as_array()
            .expect("rust_only.keys is an array")
            .iter()
            .map(|k| k.as_str().unwrap().to_string()),
    );

    let home = short_home("listcontract");
    seed_stream_row(&home, "worker-contract", "abc12345");
    state::update_registry(&home.registry_json(), |r| {
        let e = &mut r.entries[0];
        // A pane-hosted row holds the mux ref INSTEAD of a transport key
        // (mux XOR worker XOR bg), so short_id is empty -- which is why
        // `session_id` resolves to null for exactly these rows and
        // `harness_session_id` is the only identity they carry.
        e.short_id = String::new();
        e.harness = Some("claude".into());
        e.harness_session_id = Some("e6f78b98-e594-47ed-ad81-84f8a78b8bb7".into());
        e.claude_session_uuid = Some("e6f78b98-e594-47ed-ad81-84f8a78b8bb7".into());
        e.mux = Some(crate::state::MuxRef {
            session: "main".into(),
            pane_id: 10,
        });
        e.role_level = Some(1);
        e.role_scope = Some("epic-x".into());
        e.role_grantor = Some("lead".into());
        // A vendor stamp on a claude-hosted row: the exact shape the
        // provider axis exists to describe, and the one the pre-split
        // alias lied about by carrying "claude" here.
        e.provider = Some("zai".into());
        e.effort = Some("xhigh".into());
        e.node = Some("x-cafe".into());
        e.context_used_pct = Some(26);
        e.context_used_tokens = Some(258_687);
        e.context_window_tokens = Some(1_000_000);
        e.context_measured_at = Some("2026-09-30T12:00:00Z".into());
        e.mail_unread = Some(1);
        // (x-7955) AC9-HP: the recorded lane rides verbatim, so a reader
        // can tell a paneless pane row from a thread row.
        e.substrate = Some("thread".into());
    })
    .unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agent-worker"));
    let req = Request::new(1, "agent.list", json!({}));

    let response =
        list_rows::handle_list_with_truth(&ctx, &req, per_handle(|_handle| probe("working")));
    let result = response.result().unwrap();
    let row = &result["agents"][0];
    assert_eq!(row["context_used_pct"], 26);
    assert_eq!(row["context_used_tokens"], 258_687);
    assert_eq!(row["context_window_tokens"], 1_000_000);
    assert_eq!(row["context_measured_at"], "2026-09-30T12:00:00Z");
    assert_eq!(row["mail_unread"], 1);

    let actual: std::collections::BTreeSet<String> =
        row.as_object().unwrap().keys().cloned().collect();
    assert_eq!(actual, expected, "list row key set drifted from contract");
    assert_eq!(
        result["fields_omitted"], contract["projection_omissions"],
        "list envelope omissions drifted from contract"
    );
    // (x-7955) The recorded lane, by VALUE: the projection reads the
    // registry's record, never an inference from mux or thread_id.
    assert_eq!(
        row["substrate"], "thread",
        "the list row carries the recorded substrate"
    );

    // By VALUE: an unknown request renders null, never a fabricated match.
    assert_eq!(row["requested_model"], Value::Null);
    assert_eq!(row["model_substituted"], Value::Null);
    state::update_registry(&home.registry_json(), |r| {
        r.entries[0].requested_model = Some("glm-5.3[1m]".into());
        r.entries[0].model = Some("glm-5.3-flash[1m]".into());
        r.entries[0].model_basis = Some("requested".into());
    })
    .unwrap();
    let mut contradicting = probe("working").unwrap();
    contradicting.observed_model = json!({"kind": "observed", "model": "glm-5.3-flash"});
    let response = list_rows::handle_list_with_truth(
        &ctx,
        &req,
        per_handle(|_handle| Some(contradicting.clone())),
    );
    let row = &response.result().unwrap()["agents"][0];
    assert_eq!(row["requested_model"], "glm-5.3[1m]");
    assert_eq!(
        (&row["model"], &row["model_basis"]),
        (&json!("glm-5.3-flash[1m]"), &json!("requested"))
    );
    assert_eq!(
        row["model_substituted"],
        json!({"requested": "glm-5.3[1m]", "observed": "glm-5.3-flash"})
    );
    // Suffix-only difference is a MATCH: the marker stays null.
    let mut agreeing = probe("working").unwrap();
    agreeing.observed_model = json!({"kind": "observed", "model": "glm-5.3"});
    let response =
        list_rows::handle_list_with_truth(&ctx, &req, per_handle(|_handle| Some(agreeing.clone())));
    let row = &response.result().unwrap()["agents"][0];
    assert_eq!(row["requested_model"], "glm-5.3[1m]");
    assert_eq!(row["model_substituted"], Value::Null);

    // An always-null key is the same lie as a missing one: assert values.
    assert_eq!(row["harness"], "claude");
    // `provider` carries the stored vendor axis, never a harness (AC8,
    // post x-f273). Emitting it from one serializer only would be worse
    // than emitting it from both: the field would then be present or
    // absent depending on which reader answered.
    assert_eq!(row["provider"], "zai");
    assert_ne!(row["provider"], row["harness"]);
    assert!(row.get("effort").is_some(), "effort key must be emitted");
    assert_eq!(row["effort"], "xhigh");
    assert_eq!(row["node"], "x-cafe");
    assert!(["no-pr", "graph-unreadable"].contains(&row["pr_basis"].as_str().unwrap()));
    assert_eq!(
        row["harness_session_id"],
        "e6f78b98-e594-47ed-ad81-84f8a78b8bb7"
    );
    // The mailbox address, asserted by VALUE and not merely by presence.
    // This row is the exact shape the address column exists for: a pane
    // worker with no transport key, whose only copyable identifier before
    // this key was `name` -- and a name-lane durable write is the largest
    // still-growing category of stranded mail on the bus. The value must
    // equal what `mail drain-self` computes for itself, which is the first
    // eight; the retired `<harness>-<short>` form is refused by the
    // resolver, so emitting it here would advertise an unreachable mailbox.
    assert_eq!(row["address"], "e6f78b98");
    // The pre-fix surface reported this row as having no identity at all:
    // session_id is legitimately null for a pane row (no transport key), so
    // harness_session_id is what has to carry it.
    assert!(row["session_id"].is_null());
    assert_eq!(row["mux"]["session"], "main");
    assert_eq!(row["mux"]["pane_id"], 10);
    assert_eq!(
        row["role"], "L1 epic-x",
        "same formatter as Python team_label"
    );
    // The raw team fields need value assertions too, not just presence:
    // hardcoding either to null passes a key-set check and the bare-row
    // null check, which is the "present but always null" lie again.
    assert_eq!(row["role_level"], 1);
    assert_eq!(row["role_scope"], "epic-x");
    assert_eq!(row["role_grantor"], "lead");

    // The uuid-backfill leg (x-52fa's third clause): a report from a claude
    // bg row that holds only its short-id prefix lands the canonical
    // harness_session_id beside the legacy claude_session_uuid, so every
    // consumer that reads the canonical field sees the id at once.
    // A poll-miss mint recorded the digest-named settings file; the
    // backfill publishes the session twin and re-points the row at it.
    let settings_dir = home.root().join("route-settings");
    std::fs::create_dir_all(&settings_dir).unwrap();
    let digest = settings_dir.join("0123456789abcdef.json");
    std::fs::write(&digest, "{}").unwrap();
    state::update_registry(&home.registry_json(), |r| {
        let mut row = bg_claude_row("worker-B", "3228ccad");
        row.route_settings_path = Some(digest.to_string_lossy().into_owned());
        r.entries.push(row);
    })
    .unwrap();
    let full = "3228ccad-c078-4b53-a8c9-7199b831eae4";
    let backfill = Request::new(
        3,
        "agent.report",
        json!({"session_id": full, "seq": 5, "state": "working"}),
    );
    assert_eq!(
        handle_report(&ctx, &backfill).result().unwrap()["stored"],
        true,
        "the short-id prefix backfills the report onto the null-uuid row"
    );
    let reg = state::load_registry(&home.registry_json()).unwrap();
    let bg = reg
        .entries
        .iter()
        .find(|e| e.name == "worker-B")
        .expect("the backfilled row");
    assert_eq!(bg.harness_session_id.as_deref(), Some(full));
    assert_eq!(bg.claude_session_uuid.as_deref(), Some(full));
    let twin = settings_dir.join(format!("session-{full}.json"));
    assert_eq!(
        bg.route_settings_path.as_deref(),
        Some(twin.to_string_lossy().as_ref())
    );
    assert_eq!(std::fs::read_to_string(&twin).unwrap(), "{}");

    std::fs::remove_dir_all(home.root()).ok();
}

fn stream_identity(short_id: &str) -> Value {
    json!({
        "harness": "claude",
        "session_id": format!("uuid-{short_id}"),
        "short_id": short_id,
        "created_at": "2026-06-09T00:00:00Z",
    })
}

fn switchboard_params(
    to: &str,
    to_short: &str,
    from: &str,
    from_short: Option<&str>,
    body: &str,
) -> Value {
    let mut params = json!({
        "to": to,
        "from": from,
        "body": body,
        "mirror": from_short.is_some(),
        "recipient_identity": stream_identity(to_short),
    });
    if let Some(short_id) = from_short {
        params["from_identity"] = stream_identity(short_id);
    }
    params
}

#[test]
fn list_renders_family1_truth_instead_of_stored_registry_status() {
    let home = short_home("listtruth");
    seed_stream_row(&home, "worker-list", "abc12345");
    state::update_registry(&home.registry_json(), |registry| {
        registry.entries[0].status = AgentStatus::Orphaned;
    })
    .unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agent-worker"));
    let req = Request::new(1, "agent.list", json!({"status": "writing"}));

    let response = list_rows::handle_list_with_truth(
        &ctx,
        &req,
        per_handle(|_handle| probe_with_verdict("working", "reachable")),
    );
    let result = response.result().unwrap();
    let agents = result["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["status"], "writing");

    std::fs::remove_dir_all(home.root()).ok();
}

// ---- Group 3 (ab-734fcd6c): claude stream-json front door --------------

/// AC1-EDGE single-writer: a second adopt of a session already held by a live
/// claude thread is refused (one writer per session), before any spawn. Uses
/// the lock-free one-host pre-check so it is hermetic (no worker, no claim).
#[tokio::test(flavor = "current_thread")]
async fn promote_claude_duplicate_session_refused() {
    let home = short_home("cldup");
    seed_stream_row(&home, "first", "swDup"); // claude_session_uuid = uuid-swDup, Live
    let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent-worker"));
    let req = Request::new(
        1,
        "agent.spawn",
        json!({
            "name": "second", "provider": "claude", "host_mode": "interactive",
            "resume_id": "uuid-swDup"
        }),
    );
    let resp = handle_spawn(&ctx, &req).await;
    match &resp.payload {
        crate::protocol::ResponsePayload::Err(e) => {
            assert_eq!(e.code, ErrorCode::InvalidParams);
            assert!(
                e.message.contains("already hosted") && e.message.contains("first"),
                "duplicate adopt must name the existing host; got: {}",
                e.message
            );
        }
        _ => panic!("expected single-writer refusal for duplicate adopt"),
    }
    std::fs::remove_dir_all(home.root()).ok();
}

#[tokio::test(flavor = "current_thread")]
async fn switchboard_refuses_replaced_recipient_identity() {
    let home = short_home("replaced");
    seed_stream_row(&home, "victim", "swB");
    let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent-worker"));
    let mut params = switchboard_params("victim", "swB", "ghost", None, "secret");
    params["recipient_identity"]["session_id"] = json!("uuid-swA");

    let response = handle_switchboard(&ctx, &Request::new(1, "agent.switchboard", params)).await;
    let result = response.result().expect("identity mismatch is a demotion");
    assert_eq!(result["delivered"], false);
    assert_eq!(result["reason"], "recipient-identity-changed");
    std::fs::remove_dir_all(home.root()).ok();
}

// --- codex thread lane: actor-driven ask / stop (the x-de10 probes) ---
//
// These three are the make-it-fail probes for the concurrency rewrite:
// each FAILS against the old Arc<Mutex<CodexThread>> shape (verified by
// running them on the pre-rewrite daemon) and passes against the actor.

// PATH + ask-wait env serialization across these tests AND every other
// PATH-mutating test in the lib (provider.rs, client_commands.rs): one shared
// mutex, never nested. The fakes install `codex` on PATH and one test
// retunes FNO_CODEX_ASK_WAIT_MS; a concurrent `set_var` from an unrelated
// test would make the child inherit a broken PATH (exit 127).

/// Point `CODEX_HOME` at a fake shared app-server daemon for the duration
/// of `body`. The fake lives in [`crate::codex_fake_daemon`]: one
/// implementation of the protocol, shared with the integration tests.
async fn with_fake_codex_daemon(
    behavior: crate::codex_fake_daemon::Behavior,
    body: impl std::future::Future<Output = ()>,
) {
    let _guard = crate::path_test_guard();
    let _daemon = crate::codex_fake_daemon::FakeDaemon::start(behavior);
    body.await;
}

/// Block until the worker's actor reports a DRIVING turn, or fail.
///
/// The tests below used a fixed 300ms sleep, which is a bet that the seed
/// turn started by then. Under a loaded parallel suite it does not always
/// hold, and losing it does not make a test slower, it makes it WRONG: an
/// interrupt arriving before the turn starts reads `NoTurnInFlight`, so
/// `stop` correctly reports `no-turn` and the assertion for `interrupted`
/// fails. Waiting on the actor's own turn id removes the bet.
async fn await_driving_turn(ctx: &Ctx, name: &str) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let handle = ctx.codex_threads.lock().await.get(name).cloned();
        if let Some(turn) = handle.and_then(|handle| handle.current_turn_id()) {
            return turn;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no turn ever started driving for {name}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

// ── gate record tests (Task 2.3) ─────────────────────────────────────────

// ---- inside-leg report (E3.2) ------------------------------------------

/// AC-X2 store: a report for a registered claude session lands on the row's
/// `inside_leg` field with the daemon-stamped `received_at`, and emits
/// `inside_leg_report`.
#[test]
fn handle_report_stores_on_matching_row() {
    let home = tmp_home("report-store");
    seed_stream_row(&home, "worker-A", "repA"); // claude_session_uuid = uuid-repA
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agent-worker"));
    let req = Request::new(
        1,
        "agent.report",
        json!({"session_id": "uuid-repA", "seq": 3, "state": "working", "reason": "running tests"}),
    );
    let resp = handle_report(&ctx, &req);
    assert!(!resp.is_err(), "report must return Ok: {resp:?}");
    assert_eq!(resp.result().unwrap()["stored"], true);

    let reg = state::load_registry(&home.registry_json()).unwrap();
    let rep = reg.entries[0]
        .inside_leg
        .as_ref()
        .expect("inside_leg stored");
    assert_eq!(rep.state, state::InsideLegState::Working);
    assert_eq!(rep.seq, 3);
    assert_eq!(rep.reason.as_deref(), Some("running tests"));
    assert!(!rep.received_at.is_empty(), "daemon stamps received_at");

    // A same-state repeat stores its newer seq and journals nothing.
    let repeat = Request::new(
        2,
        "agent.report",
        json!({"session_id": "uuid-repA", "seq": 4, "state": "working"}),
    );
    assert_eq!(
        handle_report(&ctx, &repeat).result().unwrap()["stored"],
        true
    );
    let events = read_events(&home);
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "inside_leg_report")
            .count(),
        1,
        "one inside_leg_report per state change: {events:?}"
    );
    std::fs::remove_dir_all(home.root()).ok();
}
