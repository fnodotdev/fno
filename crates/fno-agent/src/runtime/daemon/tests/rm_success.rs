//! The rm tests that drive `handle_rm_with` to a success response. Every
//! success tail calls `release_stopped_claims_into`, which resolves the
//! claims root from the process-global `FNO_CLAIMS_ROOT`, so each of these
//! holds `test_env_lock` for its whole body. Moved verbatim out of
//! daemon.rs for file budget: test motion is the sanctioned shrink.

use super::*;

#[tokio::test]
async fn rm_closes_the_dead_childs_pane_and_preserves_a_reused_coordinate() {
    let _env = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for (failure, identityless, stored_pane, listing_failure, replaced_child, substrate) in [
        (false, false, 76, false, false, Some("pane")),
        (true, false, 76, false, false, Some("pane")),
        (false, true, 99, false, false, Some("pane")),
        (false, true, 76, false, false, Some("pane")),
        (false, false, 76, true, false, Some("pane")),
        (false, false, 76, false, true, Some("pane")),
        (false, true, 99, false, false, None),
        (false, false, 76, false, false, Some("thread")),
        (true, false, 76, false, false, Some("thread")),
    ] {
        let home = short_home(if failure {
            "rmdeadpanefail"
        } else {
            "rmdeadpane"
        });
        let spaces = home.root().join("spaces");
        let _spaces = crate::claims::EnvVarGuard::set("FNO_SPACES_DIR", spaces.to_str().unwrap());
        let sid = "finished-pane-session";
        let session = crate::footnote_harness::transcript::sessions_root()
            .join("fixture")
            .join(sid);
        std::fs::create_dir_all(&session).unwrap();
        let pid = 0x7fff_fff0;
        let record = crate::footnote_harness::transcript::transcript_file(&session, sid);
        std::fs::write(
            &record,
            json!({"type":"heartbeat", "ts":"2020-01-01T00:00:00Z", "data":{"pid":pid}})
                .to_string(),
        )
        .unwrap();
        let panes = home.root().join("panes.json");
        let successor = json!({"pane_id":76,"child_pid":std::process::id()});
        std::fs::write(
            &panes,
            json!([{"pane_id":99,"child_pid":pid}, successor.clone()]).to_string(),
        )
        .unwrap();
        let current_child = home.root().join("current-child");
        std::fs::write(&current_child, pid.to_string()).unwrap();
        let kill = if failure {
            "echo 'pane server unreachable' >&2; exit 1".to_string()
        } else {
            format!(
                "printf '%s' '{}' > '{}'",
                json!([successor.clone()]),
                panes.display()
            )
        };
        let list = if listing_failure {
            "echo 'pane listing unavailable' >&2; exit 1".to_string()
        } else if replaced_child {
            format!(
                "cat '{}'; printf '%s' '{}' > '{}'; printf '%s' '{}' > '{}'",
                panes.display(),
                std::process::id(),
                current_child.display(),
                json!([{"pane_id":99,"child_pid":std::process::id()}, successor.clone()]),
                panes.display()
            )
        } else {
            format!("cat '{}'", panes.display())
        };
        let retire = if failure {
            format!(
                "printf '%s' '{}'",
                json!({"retired":0,"panes_closed":0,"skipped_typing":1})
            )
        } else {
            format!(
                "printf '%s' '{}' > '{}'; printf '%s' '{}'",
                json!([successor.clone()]),
                panes.display(),
                json!({"retired":1,"panes_closed":1,"skipped_typing":0})
            )
        };
        let stub = crate::write_exec_stub(
            home.root(),
            "fno",
            &format!(
                "#!/bin/sh\ncase \"$*\" in\n'mux pane ls --server main --json') {} ;;\n'mux pane kill --server main 99 --expect-child-pid '*) if [ \"$8\" != \"$(cat '{}')\" ]; then echo 'pane child changed' >&2; exit 1; fi; {} ;;\n'mux retire-session main --harness footnote --session-id finished-pane-session --json') {} ;;\n*) echo \"unexpected: $*\" >&2; exit 2 ;;\nesac\n",
                list, current_child.display(), kill, retire
            ),
        );
        let _fno = crate::claims::EnvVarGuard::set("FNO_BIN", stub.to_str().unwrap());
        let mut row = ask_row("finished-pane", Some("2020-01-01T00:00:00Z"));
        row.harness = Some("footnote".into());
        row.harness_session_id = (!identityless).then(|| sid.into());
        row.log_path = Some(record.to_string_lossy().into_owned());
        row.substrate = substrate.map(str::to_string);
        row.pid = (!identityless).then_some(pid);
        row.mux = Some(state::MuxRef {
            session: "main".into(),
            pane_id: stored_pane,
        });
        state::update_registry(&home.registry_json(), |registry| registry.entries.push(row))
            .unwrap();
        let ctx = test_ctx(home.clone(), PathBuf::from("fno-agent-worker"));
        let response = handle_rm_with(
            &ctx,
            &Request::new(1, "agent.rm", json!({"name":"finished-pane"})),
            &|| panic!("a footnote row never reads the claude roster"),
            &|_| panic!("a footnote row never calls claude rm"),
            &|_, _| panic!("a footnote row never calls claude stop"),
            &|server, entry| {
                assert_eq!(entry.substrate.as_deref(), Some("thread"));
                crate::gc_native::retire_attached_session(server, entry)
            },
            &|_, _| PaneProbe::Unknown,
        )
        .await;
        let remaining: Value = serde_json::from_slice(&std::fs::read(&panes).unwrap()).unwrap();
        if failure || listing_failure || replaced_child || (identityless && stored_pane == 76) {
            let message = &response
                .error()
                .expect("unsafe or failed removal refuses")
                .message;
            if failure {
                let reason = if substrate == Some("thread") {
                    "typing"
                } else {
                    "pane server unreachable"
                };
                assert!(message.contains(reason), "{message}");
            }
            if listing_failure {
                assert!(message.contains("pane listing unavailable"), "{message}");
            }
            if replaced_child {
                assert!(message.contains("pane child changed"), "{message}");
            }
            assert_eq!(remaining.as_array().unwrap().len(), 2);
            assert_eq!(
                state::load_registry(&home.registry_json())
                    .unwrap()
                    .entries
                    .len(),
                1
            );
        } else {
            assert!(response.error().is_none(), "{response:?}");
            assert_eq!(
                remaining,
                json!([successor]),
                "rm left the dead child's pane behind"
            );
            assert_eq!(response.result().unwrap()["pane_removed"], true);
            assert!(state::load_registry(&home.registry_json())
                .unwrap()
                .entries
                .is_empty());
        }
        std::fs::remove_dir_all(home.root()).ok();
    }
}

#[tokio::test]
async fn rm_never_deletes_a_successors_re_armed_manifest() {
    let _env = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // The vacated row is removed with a manifest on disk naming a
    // DIFFERENT session: a successor teamed over the scope after this
    // row went terminal re-armed it. Deleting that file would disarm the
    // live successor's stop gate.
    let home = short_home("rmcrownsucc");
    let project = home.root().join("project");
    let spaces = home.root().join("spaces");
    std::env::set_var("FNO_SPACES_DIR", &spaces);
    let manifest = crate::paths::space_dir_opt(&project)
        .unwrap()
        .join("leads")
        .join("alpha.md");
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    std::fs::write(
        &manifest,
        "---\nscope: alpha\nharness_session_id: bbbb9999-9999-4999-8999-999999999999\n---\n",
    )
    .unwrap();
    let mut row = claude_rm_row(
        "stopped-worker",
        "aaaa3333",
        "aaaa3333-1111-2222-3333-444444444444",
    );
    row.cwd = project.to_string_lossy().into_owned();
    row.role_level = Some(1);
    row.role_scope = Some("alpha".into());
    row.role_grantor = Some("human".into());
    state::update_registry(&home.registry_json(), |registry| registry.entries.push(row)).unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agent-worker"));
    let request = Request::new(1, "agent.rm", json!({"name": "stopped-worker"}));
    let snapshots = claude_row_then_absent("aaaa3333", "stopped");

    let response = handle_rm_with(
        &ctx,
        &request,
        &snapshots,
        &|_| Ok(()),
        &|_, _| crate::daemon::rm_teardown::StopLeg::Asked,
        &|_, _| Ok(true),
        &|_, _| PaneProbe::Unknown,
    )
    .await;

    assert!(response.error().is_none(), "{response:?}");
    assert!(
        manifest.exists(),
        "rm deleted a manifest naming a different session: the live successor's gate"
    );
    std::env::remove_var("FNO_SPACES_DIR");
    std::fs::remove_dir_all(home.root()).ok();
}

/// rm of a codex thread row stamps a tombstone beside the registry, so the
/// harness-store healer refuses to adopt the same session back under a
/// fresh short-id name (the adopted duplicate that blocked resume). The
/// row here carries the dead-actor shape the gone-row test uses; the
/// tombstone is the point, not the teardown.
#[tokio::test]
async fn rm_stamps_a_tombstone_for_the_removed_codex_session() {
    let _guard = crate::path_test_guard();
    let home = short_home("rmthreadtomb");
    let cwd = tempfile::tempdir().unwrap();
    let mut row = thread_entry("t-rm-tomb", AgentStatus::Live, None);
    row.cwd = cwd.path().to_string_lossy().into_owned();
    row.project_root = row.cwd.clone();
    row.harness_session_id = Some("01a0aba4-0934-7f10-aac3-67c76bfd244c".into());
    row.codex_session_id = Some("01a0aba4-0934-7f10-aac3-67c76bfd244c".into());
    state::update_registry(&home.registry_json(), |registry| registry.entries.push(row)).unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
    ctx.codex_threads.lock().await.insert(
        "t-rm-tomb".into(),
        std::sync::Arc::new(crate::codex_thread::CodexThreadActor::with_dead_actor()),
    );
    let request = Request::new(1, "agent.rm", json!({"name": "t-rm-tomb"}));

    let response = handle_rm_with(
        &ctx,
        &request,
        &|| panic!("a codex row must not read the claude roster"),
        &|_| panic!("rm must not reach claude rm"),
        &|_, _| panic!("no claude stop may run for a codex row"),
        &|_, _| panic!("a thread row has no mux ref to kill"),
        &|_, _| PaneProbe::Unknown,
    )
    .await;

    assert!(
        response.error().is_none(),
        "{:?}",
        response.error().map(|e| e.message.clone())
    );
    let raw = std::fs::read_to_string(home.root().join("rm_tombstones.json"))
        .expect("rm wrote the tombstone file");
    let tombstones: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
    assert_eq!(tombstones.len(), 1);
    assert_eq!(tombstones[0]["harness"], "codex");
    assert_eq!(
        tombstones[0]["session_id"],
        "01a0aba4-0934-7f10-aac3-67c76bfd244c"
    );
    assert!(tombstones[0]["removed_at"].is_u64());
    std::fs::remove_dir_all(home.root()).ok();
}
