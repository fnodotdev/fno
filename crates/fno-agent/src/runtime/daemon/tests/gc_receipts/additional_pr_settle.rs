//! The additional-PR settle families (tasks 1.1 and 1.2): the three graph
//! rules, the one-GitHub-read stamp, and the pass that applies it.

use super::*;
use super::{
    no_agents, open_do_row, quiet_transcript, spawn_row, stage_graph, staged_ages_at,
    staged_graph_home,
};
use crate::gc_sweep::{self, GcSummary};

/// The settle over a staged reader, then the row pass - the real branch of
/// `settle_then_run` with the reader the test owns.
fn settle_staged_then_run(
    home: &AgentsHome,
    emitter: &EventEmitter,
    transcripts: &dyn Fn(&state::RegistryEntry) -> Option<Vec<std::path::PathBuf>>,
    read: &mut dyn FnMut(&str, &str) -> Option<gc_sweep::PrState>,
) -> GcSummary {
    let (settled, refused) = gc_sweep::settle_stale_do_rows_with(home, read);
    let mut summary = gc_sweep::run(
        home,
        emitter,
        0,
        false,
        7,
        &gc_sweep::read_graph_entries,
        transcripts,
        &staged_ages_at(crate::daemon::now_epoch_secs(), transcripts),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (None, None),
        &|_| None,
    );
    summary.settled_do_rows = settled
        .into_iter()
        .map(|row| (row.node, row.harness, row.session_id))
        .collect();
    summary.settle_refused = refused;
    summary
}

/// The AC2/AC3 specimen: a done+merged node with a `cwd`, an open do row,
/// and one extra that no graph rule can settle. `completed_at` rides so the
/// store's status derivation (any write recomputes) keeps the node `done`
/// across the stamp write.
fn held_extra_node(id: &str) -> Value {
    json!({
        "id": id, "title": "Stamp me", "slug": id, "type": "feature",
        "status": "done", "priority": "p2",
        "created_at": "2026-09-11T00:00:00+00:00",
        "completed_at": "2026-09-11T02:00:00+00:00",
        "merge_status": "merged",
        "cwd": "/repo/wt",
        "sessions": [open_do_row("claude", "sess-stamp")],
        "additional_prs": [{"number": 1523}]
    })
}

fn stamp_test_setup(
    id: &str,
) -> (
    tempfile::TempDir,
    AgentsHome,
    EventEmitter,
    std::path::PathBuf,
    tempfile::TempDir,
) {
    let (dir, home) = staged_graph_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "b.jsonl", 2 * 3600);
    stage_graph(dir.path(), json!([held_extra_node(id)]));
    state::update_registry(&home.registry_json(), |r| {
        spawn_row(r, "row-stamp", "sess-stamp");
    })
    .unwrap();
    (dir, home, emitter, quiet, transcripts)
}

#[test]
fn ac2_a_merged_answer_stamps_and_settles() {
    let (dir, home, emitter, quiet, _t) = stamp_test_setup("x-stamp");
    let mut read = |path: &str, _cwd: &str| {
        assert_eq!(path, "repos/{owner}/{repo}/pulls/1523");
        Some(gc_sweep::PrState::Merged)
    };
    let summary = settle_staged_then_run(
        &home,
        &emitter,
        &move |_| Some(vec![quiet.clone()]),
        &mut read,
    );

    assert!(
        summary.settle_refused.is_empty(),
        "refused: {:?}",
        summary.settle_refused
    );
    let rows = crate::graph_store::read_rows(&dir.path().join("graph.json")).unwrap();
    let extras = &rows[0]["additional_prs"];
    assert_eq!(extras[0]["merge_status"], json!("merged"));
    assert_eq!(
        summary.settled_do_rows,
        vec![("x-stamp".into(), "claude".into(), "sess-stamp".into())]
    );
    assert!(summary.retired.iter().any(|(id, _)| id == "row-stamp"));
}

#[test]
fn ac2_edge_an_open_or_unreadable_answer_holds() {
    for answer in [Some(gc_sweep::PrState::Open), None] {
        let (_dir, home, emitter, _quiet, t) = stamp_test_setup("x-stamp");
        // Comfortably inside the 30-minute done-node window (law
        // d-b01ffede): the fixture mtime is stamped before the sweep
        // captures `now`, so a 1800s stage flips keep to retire the moment
        // a second ticks between stamping and the age read.
        let recent = quiet_transcript(t.path(), "recent.jsonl", 900);
        let mut read = |_path: &str, _cwd: &str| answer;
        let summary = settle_staged_then_run(
            &home,
            &emitter,
            &move |_| Some(vec![recent.clone()]),
            &mut read,
        );

        assert!(summary.settled_do_rows.is_empty());
        assert!(!summary.retired.iter().any(|(id, _)| id == "row-stamp"));
        assert!(
            summary.kept_active.iter().any(|(id, _)| id == "row-stamp"),
            "kept_active: {:?}",
            summary.kept_active
        );
    }
}

// The primary-stamp family: the sweep records a done node's out-of-band
// merged primary PR, so its dead do rows settle, and the dry run rehearses
// the same outcome.

/// The AC1 specimen: a done node whose own merge is unrecorded. Its PR
/// merged out of band, so the Python writers left `merge_status` null and
/// the settle's gate skips the node - the defect this family pins.
fn held_primary_node(id: &str) -> Value {
    json!({
        "id": id, "title": "Stamp my primary", "slug": id, "type": "feature",
        "status": "done", "priority": "p2",
        "created_at": "2026-09-11T00:00:00+00:00",
        "completed_at": "2026-09-11T02:00:00+00:00",
        "merge_status": json!(null),
        "pr_number": 2180,
        "pr_url": "https://github.com/o/r/pull/2180",
        "cwd": "/repo/wt",
        "sessions": [open_do_row("claude", "sess-prim")],
    })
}

/// A home staged with one graph fixture set; the caller adds registry rows.
fn primary_home(
    entries: Vec<Value>,
) -> (
    tempfile::TempDir,
    AgentsHome,
    EventEmitter,
    tempfile::TempDir,
) {
    let (dir, home) = staged_graph_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    stage_graph(dir.path(), json!(entries));
    (dir, home, emitter, transcripts)
}

#[test]
fn ac1_err_an_open_or_unreadable_primary_stamps_nothing() {
    for answer in [Some(gc_sweep::PrState::Open), None] {
        let (dir, home, _emitter, _t) = primary_home(vec![held_primary_node("x-prim")]);
        let mut read = |_path: &str, _cwd: &str| answer;
        let (settled, refused) = gc_sweep::settle_stale_do_rows_with(&home, &mut read);
        assert!(settled.is_empty());
        assert!(refused.is_empty(), "{:?}", refused);
        let rows = crate::graph_store::read_rows(&dir.path().join("graph.json")).unwrap();
        assert_eq!(rows[0]["merge_status"], json!(null));
        assert!(rows[0]["sessions"][0]
            .as_object()
            .unwrap()
            .get("ended_at")
            .is_none());
    }
}
