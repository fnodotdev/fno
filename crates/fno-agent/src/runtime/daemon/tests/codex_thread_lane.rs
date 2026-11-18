//! The codex thread lane's spawn/registry/resume tests, moved verbatim
//! into their own module (file budget: this file is far over the
//! shrink-only line; test motion is the sanctioned shrink). Covers
//! `spawn_codex_thread_lane`, `build_codex_thread_entry`,
//! `ensure_codex_thread_handle`, and the `agent.spawn --substrate thread`
//! dispatch family. Shared helpers (`tmp_home`, `rentry`, `test_ctx`, ...)
//! stay in the parent tests module and resolve through the glob.
use super::*;
use crate::daemon::codex_thread_resume::recover_codex_threads;

// `thread_entry` stays in the parent tests module (used by the plan_reconcile
// liveness family too, not only this one) and reaches here through the glob.

#[test]
fn reconcile_settles_an_unhosted_thread_without_a_rollout_to_exited() {
    let entries = vec![
        thread_entry("t-gone", AgentStatus::Live, None),
        // the same silence behind an open PR. The hold outranks the
        // no-rollout exit: a thread waiting on CI or a merge keeps its seat.
        thread_entry("t-pr-gone", AgentStatus::Live, None),
    ];
    let (changes, _) = plan_reconcile(
        &entries,
        |_| Ok(false),
        || false,
        |_| true,
        |_| false,
        |e| e.name == "t-pr-gone", // the open-PR hold, read from the graph rows
        |_| false,                 // not hosted
        |_| false,                 // no rollout: the thread never got far enough to persist
        |_| RowLiveness::Unknown,  // no freshness signal: nothing to prove life
        true,                      // roster readable: the flip needs a successful roster read
        1_700_000_000_000,
    );
    assert_eq!(
        changes[0].new_status,
        Some(AgentStatus::Exited),
        "an unhosted thread with no rollout is gone, not Live-forever"
    );
    assert_eq!(
        changes[0].new_liveness, None,
        "absence of a rollout is not a measured death: the word is unmeasured"
    );
    assert_eq!(
        changes[1].new_status, None,
        "the hold outranks the no-rollout exit for a PR-busy thread"
    );
}

/// AC16: a codex PANE row (mux ref set) must refuse from the ask lane
/// naming the pane command, never reach ensure_codex_thread_handle and die
/// with the confusing "is not a Codex thread".
#[tokio::test(flavor = "current_thread")]
async fn ask_a_codex_pane_row_refuses_naming_the_pane_command() {
    let home = tmp_home("codex-pane-ask");
    state::update_registry(&home.registry_json(), |registry| {
        let mut entry = thread_entry("t-pane", AgentStatus::Live, None);
        entry.mux = Some(state::MuxRef {
            session: "main".into(),
            pane_id: 3,
        });
        entry.log_path = Some("/tmp/t-pane.log".into());
        registry.entries.push(entry);
    })
    .unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
    let resp = handle_ask(
        &ctx,
        &Request::new(1, "agent.ask", json!({"name": "t-pane", "message": "hi"})),
    )
    .await;
    match &resp.payload {
        crate::protocol::ResponsePayload::Err(e) => {
            assert_eq!(e.code, ErrorCode::InvalidStatus);
            assert!(
                e.message.contains("pane worker") && e.message.contains("mux pane send"),
                "refusal must name the pane command: {}",
                e.message
            );
            assert!(
                !e.message.contains("is not a Codex thread"),
                "the confusing thread refusal must not surface: {}",
                e.message
            );
        }
        _ => panic!("a pane row must refuse, got: {resp:?}"),
    }
    std::fs::remove_dir_all(home.root()).ok();
}

/// Recovery refuses a codex thread row whose cwd is gone before any
/// `thread/resume` frame leaves, and settles it Orphaned through the same
/// failed-resume arm: a resumed thread with no cwd reads alive and can
/// never run a turn. A row whose cwd still exists
/// resumes and goes Live. Two fake-daemon scopes because the registry
/// forbids two rows sharing one harness_session_id and the fake answers
/// one configured id.
#[tokio::test(flavor = "current_thread")]
async fn recovery_refuses_a_codex_thread_row_whose_cwd_is_gone() {
    // AC1-EDGE: an existing cwd reaches the fake as a thread/resume.
    let live_id = "0198f313-0000-7000-8000-00000000a11e";
    let behavior = crate::codex_fake_daemon::Behavior::quick().with_thread_id(live_id);
    let received = std::sync::Arc::clone(&behavior.received);
    with_fake_codex_daemon(behavior, async {
        let home = tmp_home("codex-recover-live-cwd");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        let worktree = home.root().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        state::update_registry(&home.registry_json(), |registry| {
            let mut entry = thread_entry("t-here", AgentStatus::Live, None);
            entry.cwd = worktree.to_string_lossy().into_owned();
            entry.project_root = entry.cwd.clone();
            entry.harness_session_id = Some(live_id.to_string());
            entry.codex_session_id = Some(live_id.to_string());
            entry.pid = None;
            // A fresh real-turn stamp: the recovery gate (x-a8ef) resumes
            // only rows whose last turn is recent.
            entry.last_message_at = Some(now_rfc3339_like());
            registry.entries.push(entry);
        })
        .unwrap();
        recover_codex_threads(&ctx).await;

        let registry = load_registry_offloaded(home.registry_json())
            .await
            .expect("registry readable");
        assert_eq!(
            registry.find("t-here").map(|entry| entry.status),
            Some(AgentStatus::Live),
            "a row whose cwd still exists must resume and go Live"
        );
        assert!(
            ctx.codex_threads.lock().await.contains_key("t-here"),
            "the existing-cwd row holds a hosted handle"
        );
        let resumes: Vec<_> = received
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|f| f["method"] == "thread/resume")
            .cloned()
            .collect();
        assert_eq!(resumes.len(), 1, "the existing-cwd row resumed once");
        assert_eq!(resumes[0]["params"]["threadId"], live_id);
        ctx.codex_threads.lock().await.remove("t-here");
        std::fs::remove_dir_all(home.root()).ok();
    })
    .await;

    // AC1-HP + AC1-ERR: a gone cwd refuses before any frame leaves.
    let gone_id = "0198f313-0000-7000-8000-00000000b0d2";
    let behavior = crate::codex_fake_daemon::Behavior::quick().with_thread_id(gone_id);
    let received = std::sync::Arc::clone(&behavior.received);
    with_fake_codex_daemon(behavior, async {
        let home = tmp_home("codex-recover-gone-cwd");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        state::update_registry(&home.registry_json(), |registry| {
            let mut entry = thread_entry("t-gone", AgentStatus::Live, None);
            entry.cwd = "/nonexistent-cwd-for-gone-f313".into();
            entry.project_root = entry.cwd.clone();
            entry.harness_session_id = Some(gone_id.to_string());
            entry.codex_session_id = Some(gone_id.to_string());
            entry.pid = None;
            // A fresh real-turn stamp: the recovery gate (x-a8ef) resumes
            // only rows whose last turn is recent.
            entry.last_message_at = Some(now_rfc3339_like());
            registry.entries.push(entry);
        })
        .unwrap();
        recover_codex_threads(&ctx).await;

        let registry = load_registry_offloaded(home.registry_json())
            .await
            .expect("registry readable");
        assert_eq!(
            registry.find("t-gone").map(|entry| entry.status),
            Some(AgentStatus::Orphaned),
            "a gone-cwd row must settle Orphaned, not Live-forever"
        );
        assert!(
            !ctx.codex_threads.lock().await.contains_key("t-gone"),
            "a gone-cwd row holds no handle"
        );
        let events = read_events(&home);
        assert!(
            events.iter().any(|event| {
                event["type"] == "daemon_recovery_error"
                    && event["data"]["op"] == "resume_codex_thread"
                    && event["data"]["name"] == "t-gone"
                    && event["data"]["error"]
                        .as_str()
                        .is_some_and(|error| error.contains("no longer exists"))
            }),
            "the refusal event must name the row and the missing cwd: {:?}",
            events
        );

        // The direct call refuses before any resume frame leaves.
        let entry = registry.find("t-gone").cloned().unwrap();
        let error = match ensure_codex_thread_handle(&ctx, &entry).await {
            Ok(_) => panic!("a gone-cwd row must refuse"),
            Err(error) => error,
        };
        assert!(
            error.contains("no longer exists"),
            "refusal must name the missing cwd: {error}"
        );
        let resumes = received
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|f| f["method"] == "thread/resume")
            .count();
        assert_eq!(resumes, 0, "the fake must receive no resume frame");
        std::fs::remove_dir_all(home.root()).ok();
    })
    .await;
}

// --- startup-recovery freshness (x-a8ef) -----------------------------------

/// A young row that never completed a turn (fresh created_at, no turn
/// stamps) still resumes: it was mid-first-flight when the daemon died, and
/// its silence is age, not death (x-a8ef).
#[tokio::test(flavor = "current_thread")]
async fn recovery_resumes_a_young_row_that_never_turned() {
    let young_id = "0198f313-0000-7000-8000-00000000d1ce";
    let behavior = crate::codex_fake_daemon::Behavior::quick().with_thread_id(young_id);
    with_fake_codex_daemon(behavior, async {
        let home = tmp_home("codex-recover-young");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        let worktree = home.root().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        state::update_registry(&home.registry_json(), |registry| {
            let mut entry = thread_entry("t-young", AgentStatus::Live, None);
            entry.cwd = worktree.to_string_lossy().into_owned();
            entry.project_root = entry.cwd.clone();
            entry.harness_session_id = Some(young_id.to_string());
            entry.codex_session_id = Some(young_id.to_string());
            entry.pid = None;
            entry.created_at = now_rfc3339_like();
            registry.entries.push(entry);
        })
        .unwrap();
        recover_codex_threads(&ctx).await;

        let registry = load_registry_offloaded(home.registry_json())
            .await
            .expect("registry readable");
        assert_eq!(
            registry.find("t-young").map(|entry| entry.status),
            Some(AgentStatus::Live),
            "a young row resumes and reads Live"
        );
        assert!(
            ctx.codex_threads.lock().await.contains_key("t-young"),
            "a young row holds a hosted handle"
        );
        std::fs::remove_dir_all(home.root()).ok();
    })
    .await;
}

/// The attach lane WITHOUT a harness-owned server (claude) must refuse
/// with the client-side-lane pointer, never reach the codex app-server
/// lane: `thread_lane` answers "attach" for claude too, so a bare lane
/// test would hand a claude thread spawn to codex's app-server.
#[tokio::test(flavor = "current_thread")]
async fn handle_spawn_thread_attach_without_server_refuses_with_client_pointer() {
    let home = tmp_home("spawn-thread-attach-client");
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agent-worker"));
    let req = Request::new(
        1,
        "agent.spawn",
        json!({"name": "test-agent", "provider": "claude", "substrate": "thread"}),
    );
    let resp = handle_spawn(&ctx, &req).await;
    match &resp.payload {
        crate::protocol::ResponsePayload::Err(e) => {
            assert_eq!(e.code, ErrorCode::InvalidParams);
            assert!(
                e.message.contains("a bare `fno agent spawn`"),
                "attach-without-server must point at the client-side lane; got: {}",
                e.message
            );
            assert!(
                !e.message.contains("retired at G4"),
                "this refusal is a lane split, not PTY retirement; got: {}",
                e.message
            );
        }
        _ => panic!("expected refusal for an attach lane without a server"),
    }
    std::fs::remove_dir_all(home.root()).ok();
}

/// Spawn a codex thread worker through the real handle_spawn and return
/// its response.
async fn spawn_codex_thread_for_test(ctx: &Ctx, home: &AgentsHome, seed: &str) -> Response {
    let worktree = home.root().join("worktree");
    std::fs::create_dir_all(&worktree).unwrap();
    let req = Request::new(
        1,
        "agent.spawn",
        json!({
            "name": "t",
            "provider": "codex",
            "substrate": "thread",
            "cwd": worktree.to_string_lossy(),
            "message": seed,
        }),
    );
    handle_spawn(ctx, &req).await
}

/// The zombie-stop probe: an interrupt the daemon never confirms must NOT
/// report a stop.
///
/// With a private app-server, `kill_on_drop` made every stop terminal, so
/// `stopped: true` was always true. Against the shared daemon nothing ends
/// the turn but the interrupt itself, and an unconfirmed one leaves the
/// model taking that turn in the worker's worktree. Reporting `stopped:
/// true` there marks the row Exited, hides it from recovery, and discards
/// the interrupt handle, while the work continues unobserved.
///
/// The fake acks the interrupt and never completes the turn, which is
/// exactly that state.
#[tokio::test(flavor = "current_thread")]
async fn codex_thread_stop_refuses_over_a_turn_the_interrupt_never_settled() {
    let behavior = crate::codex_fake_daemon::Behavior::long().with_interrupt(
        crate::codex_fake_daemon::Interrupt::AckOnly(std::time::Duration::ZERO),
    );
    with_fake_codex_daemon(behavior, async {
        // A Drop guard, not a teardown line: an assertion below panics
        // out of this body, and a leaked bound would silently shorten
        // every later test's interrupt wait in the same process.
        struct BoundGuard;
        impl Drop for BoundGuard {
            fn drop(&mut self) {
                std::env::remove_var("FNO_CODEX_INTERRUPT_BOUND_MS");
            }
        }
        std::env::set_var("FNO_CODEX_INTERRUPT_BOUND_MS", "1500");
        let _bound = BoundGuard;
        let home = tmp_home("codex-zombie-stop");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        let spawned = spawn_codex_thread_for_test(&ctx, &home, "long seed turn").await;
        assert!(spawned.result().is_some(), "spawn failed: {spawned:?}");
        await_driving_turn(&ctx, "t").await;

        let stop = handle_stop(&ctx, &Request::new(3, "agent.stop", json!({"name": "t"}))).await;
        let res = stop.result().expect("stop errored");
        assert_eq!(
            res["stopped"], false,
            "an unsettled interrupt must not report a stop: {res:?}"
        );
        assert_eq!(
            res["interrupt"], "timeout-turn-still-running",
            "the response names why: {res:?}"
        );

        // The row stays non-terminal, so recovery can still see it, and
        // the handle stays so the live turn keeps an interrupt handle.
        let registry = load_registry_offloaded(home.registry_json())
            .await
            .expect("registry");
        assert_ne!(
            registry.find("t").map(|entry| entry.status),
            Some(AgentStatus::Exited),
            "a refused stop must not stamp the row terminal"
        );
        assert!(
            ctx.codex_threads.lock().await.contains_key("t"),
            "the actor must survive a refused stop; it holds the interrupt handle"
        );

        ctx.codex_threads.lock().await.remove("t");
        std::fs::remove_dir_all(home.root()).ok();
    })
    .await;
}

/// The resume half (x-fd66): the actor's report seq starts ABOVE the row's
/// current seq, so the resumed thread's first report clears the gate instead
/// of dying under the previous incarnation's seq. The row is seeded with a
/// done report at seq 7, as a prior life of this thread would have left it.
#[tokio::test(flavor = "current_thread")]
async fn codex_thread_resume_writes_above_the_row_seq() {
    // CODEX_HOME is process-global: hold the same guard every other fake
    // user holds, or a parallel test's driver reads THIS test's fake.
    let _guard = crate::path_test_guard();
    let _daemon = crate::codex_fake_daemon::FakeDaemon::start(
        crate::codex_fake_daemon::Behavior::quick().with_thread_id("thread-seq"),
    );
    let home = tmp_home("codex-inside-leg-resume-seq");
    state::update_registry(&home.registry_json(), |registry| {
        let mut entry = thread_entry("t-seq", AgentStatus::Live, None);
        let cwd = home.root().join("worktree");
        std::fs::create_dir_all(&cwd).unwrap();
        entry.cwd = cwd.to_string_lossy().into_owned();
        entry.project_root = entry.cwd.clone();
        entry.harness_session_id = Some("thread-seq".into());
        entry.codex_session_id = Some("thread-seq".into());
        entry.inside_leg = Some(state::InsideLegReport {
            state: state::InsideLegState::Done,
            seq: 7,
            reason: None,
            received_at: "2020-01-01T00:00:00Z".into(),
            ttl_ms: None,
            posture: None,
        });
        registry.entries.push(entry);
    })
    .unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
    let entry = state::load_registry(&home.registry_json())
        .unwrap()
        .find("t-seq")
        .cloned()
        .unwrap();
    ensure_codex_thread_handle(&ctx, &entry)
        .await
        .expect("the fake daemon answers thread/resume");

    // One completed turn: the seeded counter (8) is what the row accepts.
    let ask = handle_ask(
        &ctx,
        &Request::new(2, "agent.ask", json!({"name": "t-seq", "message": "hi"})),
    )
    .await;
    let res = ask.result().expect("ask errored");
    assert_eq!(
        res["reply"], "REPLY-1",
        "the resumed thread answers: {res:?}"
    );
    let done = poll_thread_row_named(&home.registry_json(), "t-seq", |report| {
        report.state == state::InsideLegState::Done
    })
    .await;
    assert!(
        done.seq >= 8,
        "the resumed thread's write must clear a row at seq 7, got seq {}",
        done.seq
    );

    ctx.codex_threads.lock().await.remove("t-seq");
    std::fs::remove_dir_all(home.root()).ok();
}

/// The episode gate (x-fd66 + x-dd84): notify intent fires on the EDGE into
/// done, once per episode, never on a repeat; a stale-seq write is dropped by
/// the same gate; the scrape verdict is cleared on the flip; a row holding no
/// such session is a no-op. The thread writer and the hook flush share this
/// core, so one test pins both.
#[test]
fn gate_inside_leg_onto_row_notifies_once_per_done_episode() {
    let mut registry = state::Registry::default();
    let mut row = thread_entry("t-gate", AgentStatus::Live, None);
    row.codex_session_id = Some("sid-gate".into());
    registry.entries.push(row);
    registry.entries[0].screen_state = Some(state::ScreenStateReport {
        state: "working".into(),
        rule: "busy".into(),
        seq: 1,
        at: "2020-01-01T00:00:00Z".into(),
        ttl_ms: None,
        answerable: None,
    });

    let rep = |seq, st| state::InsideLegReport {
        state: st,
        seq,
        reason: None,
        received_at: "2020-01-01T00:00:00Z".into(),
        ttl_ms: None,
        posture: None,
    };

    // Working: accepted, no notify, scrape verdict cleared.
    let n = gate_inside_leg_onto_row(
        &mut registry,
        "sid-gate",
        rep(1, state::InsideLegState::Working),
    );
    assert_eq!(n, None, "working is not an episode edge");
    assert_eq!(registry.entries[0].inside_leg.as_ref().unwrap().seq, 1);
    assert!(
        registry.entries[0].screen_state.is_none(),
        "the flip clears the scrape verdict"
    );

    // Done: the episode edge, exactly one intent, and it names is_done.
    let n = gate_inside_leg_onto_row(
        &mut registry,
        "sid-gate",
        rep(2, state::InsideLegState::Done),
    );
    assert_eq!(n, Some(("done".to_string(), true)));

    // A repeat Done (seq 3): accepted by the seq gate but NOT a new episode.
    let n = gate_inside_leg_onto_row(
        &mut registry,
        "sid-gate",
        rep(3, state::InsideLegState::Done),
    );
    assert_eq!(n, None, "a repeat done must not re-fire the episode");
    assert_eq!(registry.entries[0].inside_leg.as_ref().unwrap().seq, 3);

    // A stale-seq write (seq 3 again): dropped entirely.
    let n = gate_inside_leg_onto_row(
        &mut registry,
        "sid-gate",
        rep(3, state::InsideLegState::Working),
    );
    assert_eq!(n, None, "seq <= current is dropped");
    assert_eq!(
        registry.entries[0].inside_leg.as_ref().unwrap().state,
        state::InsideLegState::Done
    );

    // After a reboot the hook's monotonic seq restarts below the stored one.
    // Days later that low seq is a new turn, not a reordered sibling: it lands.
    let after_reboot = state::InsideLegReport {
        received_at: "2020-01-03T00:00:00Z".into(),
        ..rep(1, state::InsideLegState::Working)
    };
    gate_inside_leg_onto_row(&mut registry, "sid-gate", after_reboot);
    let leg = registry.entries[0].inside_leg.as_ref().unwrap();
    assert_eq!((leg.state, leg.seq), (state::InsideLegState::Working, 1));

    // A row holding no such session: no-op.
    let n = gate_inside_leg_onto_row(
        &mut registry,
        "sid-other",
        rep(9, state::InsideLegState::Done),
    );
    assert_eq!(n, None);
}

/// Poll until the row named `t` carries an inside-leg report matching
/// `pred`, and return it. Polling instead of a fixed sleep: the writer runs
/// off the actor task, so a sleep bets the write landed; this waits for the
/// marker itself.
async fn poll_thread_row_named(
    registry_path: &std::path::Path,
    name: &str,
    pred: impl Fn(&state::InsideLegReport) -> bool,
) -> state::InsideLegReport {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let report = load_registry_offloaded(registry_path.to_path_buf())
            .await
            .ok()
            .and_then(|registry| registry.find(name).cloned())
            .and_then(|entry| entry.inside_leg);
        if let Some(report) = report {
            if pred(&report) {
                return report;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no matching inside-leg report ever landed for {name}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// The fenced-token path, end to end on the fake shared daemon: `-c` pairs
/// land in the `thread/start` config map with TOML-typed values, `--add-dir`
/// rides the state-root grant onto every turn, the spawn-request effort rides
/// `turn/start`, and the row stores the raw tokens for startup recovery
/// (AC2-HP, AC3-HP). The request stays UNPROMOTED on purpose: the bounded
/// posture is what carries the `writableRoots` grant this test asserts, and
/// the role door has its own tests below (the request's role half-pair lands
/// on the row AT MINT - grantor read off the request's parent edge, never
/// caller-supplied - so the seed turn can never submit to an unpromoted row;
/// a scope a non-terminal row already terms mints UNPROMOTED, and a lone half
/// refuses with no row written). A promoted spawn now widens to full access,
/// whose turn policy echoes without roots - that shape is the order tests'
/// contract, not this one.
#[tokio::test(flavor = "current_thread")]
async fn codex_thread_spawn_carries_harness_args_config_add_dir_and_effort() {
    let behavior = crate::codex_fake_daemon::Behavior::quick();
    let received = std::sync::Arc::clone(&behavior.received);
    with_fake_codex_daemon(behavior, async {
        let home = tmp_home("codex-harness-args");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        let worktree = home.root().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        let req = Request::new(
            1,
            "agent.spawn",
            json!({
                "name": "t",
                "provider": "codex",
                "substrate": "thread",
                "cwd": worktree.to_string_lossy(),
                "message": "seed turn",
                "effort": "high",
                "spawned_by_session": "parent-session-uuid",
                "harness_args": [
                    "-c", "sandbox_workspace_write.network_access=true",
                    "--add-dir", "/tmp/x",
                ],
            }),
        );
        let resp = handle_spawn(&ctx, &req).await;
        assert!(resp.result().is_some(), "spawn failed: {resp:?}");

        let frames = |received: &std::sync::Arc<std::sync::Mutex<Vec<Value>>>| {
            received
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        };
        let start = frames(&received)
            .into_iter()
            .find(|f| f["method"] == "thread/start")
            .expect("a thread/start frame");
        assert_eq!(
            start["params"]["config"]["sandbox_workspace_write.network_access"],
            json!(true),
            "the -c pair rides thread/start as a TOML boolean: {start}"
        );
        // The seed submit is async in the actor; poll for the frame rather
        // than racing it.
        let turn = (async {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let turn = frames(&received)
                    .into_iter()
                    .find(|f| f["method"] == "turn/start");
                if turn.is_some() || std::time::Instant::now() >= deadline {
                    break turn;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the seed turn frame");
        assert_eq!(
            turn["params"]["effort"], "high",
            "effort rides turn/start: {turn}"
        );
        assert!(
            turn["params"]["sandboxPolicy"]["writableRoots"]
                .as_array()
                .unwrap()
                .iter()
                .any(|root| root == "/tmp/x"),
            "the add-dir rides the state-root grant: {turn}"
        );

        // The row keeps the raw tokens so startup recovery re-parses them.
        let registry = load_registry_offloaded(home.registry_json())
            .await
            .expect("registry");
        assert_eq!(
            registry
                .find("t")
                .map(|entry| entry.harness_args.as_slice()),
            Some(
                &[
                    "-c".to_string(),
                    "sandbox_workspace_write.network_access=true".to_string(),
                    "--add-dir".to_string(),
                    "/tmp/x".to_string(),
                ][..]
            ),
            "the row stores the fenced tokens verbatim"
        );
        // The mint door's own asserts live in the phases below, whose
        // requests carry the role pair; this phase's request stays
        // unpromoted so the bounded writableRoots grant above keeps its
        // shape.
        ctx.codex_threads.lock().await.remove("t");
        std::fs::remove_dir_all(home.root()).ok();

        // A held scope (fresh home: the fake mints one thread id per
        // process, and the successor must not collide with the phase-1 row):
        // the mint declines to unpromoted, and the sitting lead keeps its
        // role.
        let held = tmp_home("codex-role-held");
        let held_ctx = test_ctx(held.clone(), PathBuf::from("/nonexistent"));
        let held_wt = held.root().join("worktree");
        std::fs::create_dir_all(&held_wt).unwrap();
        let mut lead = thread_entry("sitting-lead", AgentStatus::Live, None);
        lead.role_level = Some(1);
        lead.role_scope = Some("x-aaaa".into());
        let inserted = update_registry_offloaded(held.registry_json(), move |registry| {
            registry.entries.push(lead);
            true
        })
        .await;
        assert!(inserted.is_ok(), "fixture row: {inserted:?}");
        let successor_req = json!({
            "name": "successor",
            "provider": "codex",
            "substrate": "thread",
            "cwd": held_wt.to_string_lossy(),
            "message": "seed turn",
            "role_level": 2,
            "role_scope": "x-aaaa",
        });
        let resp = handle_spawn(&held_ctx, &Request::new(1, "agent.spawn", successor_req)).await;
        assert!(resp.result().is_some(), "spawn failed: {resp:?}");
        let registry = load_registry_offloaded(held.registry_json())
            .await
            .expect("registry");
        let successor = registry.find("successor").expect("the successor row");
        assert_eq!(
            successor.role_level, None,
            "an occupied scope mints unpromoted: {successor:?}"
        );
        assert_eq!(successor.role_scope, None);
        assert_eq!(successor.role_grantor, None);
        assert_eq!(
            registry.find("sitting-lead").unwrap().role_level,
            Some(1),
            "the sitting lead keeps its role"
        );
        ctx.codex_threads.lock().await.remove("successor");
        std::fs::remove_dir_all(held.root()).ok();

        // A lone half, or an out-of-ladder level, refuses: no thread, no
        // row. A scopeless level stamps a role that rules nothing and a
        // levelless scope can never deserialize, so the door fails closed.
        let refusals = tmp_home("codex-role-lone-half");
        let ref_ctx = test_ctx(refusals.clone(), PathBuf::from("/nonexistent"));
        let ref_wt = refusals.root().join("worktree");
        std::fs::create_dir_all(&ref_wt).unwrap();
        for half in ["role_level", "role_scope"] {
            let mut params = json!({
                "name": "t2",
                "provider": "codex",
                "substrate": "thread",
                "cwd": ref_wt.to_string_lossy(),
                "message": "seed turn",
                "role_level": 2,
                "role_scope": "x-bbbb",
            });
            params.as_object_mut().unwrap().remove(half);
            let resp = handle_spawn(&ref_ctx, &Request::new(1, "agent.spawn", params)).await;
            let refused = match &resp.payload {
                crate::protocol::ResponsePayload::Err(error) => error.message.clone(),
                _ => panic!("a lone role half must refuse: {half} -> {resp:?}"),
            };
            assert!(
                refused.contains("both role_level and role_scope"),
                "the refusal names the missing half {half}: {refused}"
            );
        }
        assert_eq!(
            load_registry_offloaded(refusals.registry_json())
                .await
                .expect("registry")
                .find("t2"),
            None,
            "no row for a refused spawn"
        );
        std::fs::remove_dir_all(refusals.root()).ok();
    })
    .await;
}

// ---------------------------------------------------------------------------
// Node-backed hosted Codex target threads start in their node worktree
// (x-ecf4). The fixture gives the lane a REAL canonical checkout (its git
// common dir lives inside it) plus a fake `fno` whose worktree ensure
// answers `answer` and exits `exit`; FNO_BIN is what the ensure resolves, so
// the fake IS the ensure.
// ---------------------------------------------------------------------------

/// A canonical-checkout fixture: a real git repo plus a fake `fno`. The
/// tempdir must stay bound for the fixture's lifetime; the returned path is
/// the binary to publish on FNO_BIN under the crate's env lock.
fn codex_target_launch_cwd_fixture(answer: &str, exit: i32) -> (tempfile::TempDir, PathBuf) {
    let repo = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo.path())
        .status()
        .unwrap();
    let bin = crate::write_exec_stub(
        repo.path(),
        "fno",
        &format!(
            "#!/bin/sh\ncat >/dev/null\necho '{answer}'\necho 'worktree ensure: reusing ... created=false' >&2\nexit {exit}\n"
        ),
    );
    (repo, bin)
}

fn codex_target_launch_cwd_registry_row(home: &AgentsHome, name: &str) -> serde_json::Value {
    let Ok(registry) = crate::registry_store::read(&home.registry_json()) else {
        return serde_json::Value::Null;
    };
    registry["agents"]
        .as_array()
        .and_then(|entries| entries.iter().find(|entry| entry["name"] == name).cloned())
        .unwrap_or(serde_json::Value::Null)
}

/// AC2: a refused ensure refuses the WHOLE spawn - no thread, no registry
/// row, no birth event, and the error names the node and the failing ensure
/// condition. Starting on canonical as a fallback would be the exact
/// outcome this lane exists to prevent.
#[tokio::test(flavor = "current_thread")]
async fn codex_target_launch_cwd_refusal_starts_nothing() {
    let _env = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let behavior = crate::codex_fake_daemon::Behavior::quick();
    with_fake_codex_daemon(behavior, async {
        let home = tmp_home("codex-target-cwd-refusal");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        let (repo, bin) = codex_target_launch_cwd_fixture("", 1);
        let _fno_bin = crate::claims::EnvVarGuard::set("FNO_BIN", bin.to_str().unwrap());

        let req = Request::new(
            1,
            "agent.spawn",
            json!({
                "name": "t",
                "provider": "codex",
                "substrate": "thread",
                "cwd": repo.path().to_string_lossy(),
                "node": "x-ecf4",
                "message": "/fno:target seed",
            }),
        );
        let spawned = handle_spawn(&ctx, &req).await;
        let message = match &spawned.payload {
            crate::protocol::ResponsePayload::Err(e) => e.message.clone(),
            _ => panic!("spawn should refuse: {spawned:?}"),
        };
        assert!(
            message.contains("x-ecf4") && message.contains("worktree ensure"),
            "refusal names the node and the failing ensure condition: {message}"
        );
        assert_eq!(
            codex_target_launch_cwd_registry_row(&home, "t"),
            serde_json::Value::Null,
            "no registry row for a refused spawn"
        );
        let events = read_events(&home);
        assert!(
            !events
                .iter()
                .any(|event| event["type"] == "agent_spawned" && event["data"]["name"] == "t"),
            "no birth event for a refused spawn: {events:?}"
        );
        std::fs::remove_dir_all(home.root()).ok();
    })
    .await;
}

// --- the daemon spawn lanes' spare-name verdict ---------------------------
// The shared verdict runs off the async executor before either lane
// launches anything; fixtures come from the shared idle-spare module.

/// A live working holder still refuses, with the holder pid and the way
/// out, and the refusal lands BEFORE the driver starts: no hosted handle
/// exists after the refusal, and the holder's row stays.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn codex_thread_lane_refuses_a_live_holder_before_the_driver_starts() {
    let home = tmp_home("codex-live-name");
    let mut worker = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let mut row = rentry("t", AgentStatus::Live, None);
    row.pid = Some(worker.id());
    row.pid_start_time = crate::runtime::process_probe::process_start_time(worker.id());
    update_registry_offloaded(home.registry_json(), move |reg| {
        reg.entries.push(row);
        true
    })
    .await
    .unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
    let spawned = spawn_codex_thread_for_test(&ctx, &home, "seed turn").await;
    match spawned.payload {
        crate::protocol::ResponsePayload::Err(error) => {
            assert_eq!(error.code, ErrorCode::AgentExists);
            assert!(
                error
                    .message
                    .contains(&format!("holder pid {}", worker.id())),
                "{}",
                error.message
            );
            assert!(
                error.message.contains("fno agent rm t"),
                "{}",
                error.message
            );
        }
        _ => panic!("a live holder must refuse, got {spawned:?}"),
    }
    assert!(
        ctx.codex_threads.lock().await.is_empty(),
        "no driver may start behind the refusal"
    );
    let registry = load_registry_offloaded(home.registry_json())
        .await
        .expect("registry readable");
    assert!(
        registry.find("t").is_some(),
        "the refusal keeps the holder's row"
    );
    worker.kill().unwrap();
    worker.wait().unwrap();
    std::fs::remove_dir_all(home.root()).ok();
}
