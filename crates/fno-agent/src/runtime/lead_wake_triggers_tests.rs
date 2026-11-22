//! Tests for the refill-respawn leg's session-id targeting: the
//! wake routes on the holder's session id (laws d-e952ed19, d-8b72097f),
//! the name stays a label, and a legacy row with no id still wakes by
//! name.

use super::lead_wake_triggers::{build_walk_argv, select_targets, Target};
use crate::state::RegistryEntry;
use serde_json::{json, Value};
use std::path::Path;

fn row(name: &str, sid: Option<&str>, status: &str, scope: Option<&str>) -> Value {
    json!({
        "name": name,
        "cwd": "/tmp",
        "status": status,
        "created_at": "2026-11-21T00:00:00Z",
        "role_level": scope.map(|_| 2),
        "role_scope": scope,
        "role_grantor": scope.map(|_| "human"),
        "harness_session_id": sid,
    })
}

fn rows(values: &[Value]) -> Vec<RegistryEntry> {
    serde_json::from_value::<Vec<RegistryEntry>>(json!(values)).expect("rows deserialize")
}

fn write_manifest(cwd: &Path, scope: &str) {
    let leads = crate::paths::space_dir(cwd).join("leads");
    std::fs::create_dir_all(&leads).unwrap();
    std::fs::write(
        leads.join(format!("{scope}.md")),
        format!("---\nfno_id: {scope}-1\nscope: {scope}\n---\n"),
    )
    .unwrap();
}

#[test]
fn two_sessions_sharing_one_name_wake_by_session_id() {
    // The predecessor's row is terminal, the successor's live, and both are
    // named wren. Only the session ids tell them apart, so the target's
    // routing identity is the live row's session id - a name-keyed wake
    // would resolve two matches and read the holder gone.
    let _root = crate::paths::DeclaredRoot::declare("waketwosessions");
    let cwd = _root.path().join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    write_manifest(&cwd, "epic-x");
    let rows = rows(&[
        row(
            "wren",
            Some("aaaaaaaa-1111-4aaa-8aaa-aaaaaaaaaaaa"),
            "exited",
            Some("epic-x"),
        ),
        row(
            "wren",
            Some("bbbbbbbb-2222-4bbb-8bbb-bbbbbbbbbbbb"),
            "busy",
            Some("epic-x"),
        ),
    ]);
    let selection = select_targets(&rows);
    assert!(
        selection.drops.is_empty(),
        "a terminal predecessor never conflicts: {:?}",
        selection.drops
    );
    assert_eq!(selection.ok.len(), 1, "one live holder");
    let target = &selection.ok[0];
    assert_eq!(target.holder, "wren");
    assert_eq!(
        target.holder_session,
        "bbbbbbbb-2222-4bbb-8bbb-bbbbbbbbbbbb"
    );
    assert_eq!(target.handle(), "bbbbbbbb-2222-4bbb-8bbb-bbbbbbbbbbbb");
    let argv = build_walk_argv(
        target,
        "test",
        Path::new("/usr/bin/true"),
        None,
        None,
        false,
        "test-model",
    );
    let holder_at = argv
        .iter()
        .position(|a| a == "--wake-holder")
        .expect("--wake-holder present");
    assert_eq!(
        argv[holder_at + 1],
        "bbbbbbbb-2222-4bbb-8bbb-bbbbbbbbbbbb",
        "the wake address is the session id, never the shared name"
    );
    std::fs::remove_dir_all(crate::paths::space_dir(&cwd)).ok();
}

#[test]
fn a_row_with_no_session_id_still_wakes_by_name() {
    // Legacy shape: the row predates the id axis. handle() falls back to
    // the name so the legacy wake keeps working.
    let _root = crate::paths::DeclaredRoot::declare("wakelegacyname");
    let cwd = _root.path().join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    write_manifest(&cwd, "epic-x");
    let rows = rows(&[row("briar", None, "busy", Some("epic-x"))]);
    let selection = select_targets(&rows);
    assert!(selection.drops.is_empty(), "{:?}", selection.drops);
    let target: &Target = &selection.ok[0];
    assert_eq!(target.holder_session, "");
    assert_eq!(target.handle(), "briar");
    std::fs::remove_dir_all(crate::paths::space_dir(&cwd)).ok();
}
