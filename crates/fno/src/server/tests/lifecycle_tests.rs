//! Lifecycle resolution (AC2-EDGE): a sideline lifecycle keypress that
//! carries the row's harness session id resolves by that identity, never by
//! the label. The handler then hands the subprocess the row's CURRENT label
//! (`resolve_lifecycle_target(..).map(|a| a.name.clone())`), so a
//! harness-side rename between capture and keypress still reaches the right
//! session. Mounted as a child of server.rs's `mod tests` (use super::*), so
//! server.rs itself only pays two lines for it.

use super::*;

#[tokio::test]
async fn checked_pane_kill_refuses_a_replaced_child_before_closing_the_pane() {
    async fn request(core: &mut Core, pane: u64, child: u32, command: &str) -> (ServerMsg, bool) {
        let (mut peer, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        let resolver = Arc::new(Mutex::new(Resolver::default()));
        let mut worker = tokio::spawn(handle_client(
            server,
            tx,
            resolver,
            1,
            Arc::new(RwLock::new(HashMap::new())),
        ));
        let wire = crate::proto::PROTO_VERSION + u32::from(command == "PaneKillCheckedFuture");
        let raw = serde_json::json!({"Control": {
            "proto": wire,
            "build": crate::proto::BUILD_VERSION,
            "verb": {command: {"pane":pane,"expected_child_pid":child,"hand_off_to":null}}
        }});
        write_msg(&mut peer, &raw).await.unwrap();
        let routed = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                msg = rx.recv() => {
                    match msg {
                        Some(msg) => { core.handle(msg); true },
                        None => false,
                    }
                },
                outcome = &mut worker => {
                    outcome.unwrap();
                    false
                }
            }
        })
        .await
        .unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(5), read_msg(&mut peer))
            .await
            .unwrap()
            .unwrap();
        if routed {
            worker.await.unwrap();
        }
        (reply, routed)
    }

    let _store = StoreScratch::new("checked-pane-kill");
    let (mut core, pane) = template_core();
    let observed_child = core.panes.get(&pane).unwrap().pty.child_pid().unwrap();
    let replacement = core.spawn_pane(24, 80, "/tmp").unwrap();
    let successor = core.panes.remove(&replacement).unwrap();
    let successor_child = successor.pty.child_pid().unwrap();
    assert_ne!(observed_child, successor_child);
    core.panes.insert(pane, successor).unwrap().pty.kill();

    let (unknown, routed) = request(&mut core, pane, observed_child, "PaneKillCheckedFuture").await;
    assert!(
        !routed,
        "an unknown guarded command must never reach the effect"
    );
    match unknown {
        ServerMsg::Err { code, msg } => {
            assert_eq!(code, err_code::BAD_REQUEST);
            assert!(msg.contains("PaneKillCheckedFuture"), "{msg}");
            assert!(
                msg.contains(&format!("v{}", crate::proto::PROTO_VERSION)),
                "{msg}"
            );
        }
        other => panic!("expected a typed unknown-command refusal, got {other:?}"),
    }
    assert!(core.panes.contains_key(&pane));

    let (mismatch, _) = request(&mut core, pane, observed_child, "PaneKillChecked").await;
    assert!(
        matches!(
            mismatch,
            ServerMsg::Err {
                code: err_code::TARGET_IDENTITY_MISMATCH,
                ..
            }
        ),
        "a replaced child must be refused at the effect: {mismatch:?}"
    );
    assert_eq!(
        core.panes.get(&pane).unwrap().pty.child_pid(),
        Some(successor_child)
    );
    assert_eq!(unsafe { libc::kill(successor_child as libc::pid_t, 0) }, 0);

    let (matched, routed) = request(&mut core, pane, successor_child, "PaneKillChecked").await;
    assert!(routed);
    assert!(matches!(matched, ServerMsg::Ok), "{matched:?}");
    assert!(!core.panes.contains_key(&pane));
}

#[test]
fn lifecycle_resolution_prefers_identity_over_label() {
    let mut core = empty_core();
    let mut row = exited_claude_row("label", Some("11111111-1111-4111-8111-111111111111"));
    row.harness_session_id = Some("11111111-1111-4111-8111-111111111111".into());
    core.agents = vec![row];
    // The captured label is stale ("stale"); the carried id answers the row.
    let resolved =
        core.resolve_lifecycle_target("stale", Some("11111111-1111-4111-8111-111111111111"));
    assert_eq!(
        resolved.map(|a| a.name.clone()).as_deref(),
        Ok("label"),
        "identity resolves the row; the handler gets its current label"
    );
    // The same keypress without the id falls back to the label and refuses.
    assert!(
        core.resolve_lifecycle_target("stale", None).is_err(),
        "label-only resolution keeps today's fail-closed refusal"
    );
}

#[test]
fn lifecycle_identity_never_resolves_an_identityless_external_row() {
    // The external refusal keys on OWNERSHIP: a row with no registry
    // identity at all (a synthesized foreign row) is managed from its own
    // session, and neither the label nor a carried id may act on it. A row
    // that carries ids is an fno registry row (the upgraded orphan) and
    // resolves like any other.
    let mut core = empty_core();
    let mut row = exited_claude_row("ext", None);
    row.external = true;
    core.agents = vec![row];
    assert!(
        core.resolve_lifecycle_target("ext", Some("22222222-2222-4222-8222-222222222222"))
            .is_err(),
        "an identityless external row is refused even by a carried id"
    );
}
