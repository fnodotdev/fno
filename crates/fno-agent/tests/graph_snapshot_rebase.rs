//! A write planned over a snapshot publishes over a store that moved on while
//! it planned, as long as no row it touches moved. Planning on a large graph
//! takes seconds, and other sessions write every few seconds, so a refusal on
//! any intervening write loses every attempt.

use fno_agent::graph_store::{
    self, locked_mutate_snapshot, mutate_rows, read_snapshot, MutateInput, StoreError,
    DEFAULT_LOCK_TIMEOUT,
};
use serde_json::{json, Value};
use std::path::Path;

fn node(id: &str) -> Value {
    json!({
        "id": id,
        "slug": format!("slug-{id}"),
        "title": format!("Node {id}"),
        "type": "feature",
        "status": "ready",
        "priority": "p2",
        "details": "d",
    })
}

fn seeded(dir: &tempfile::TempDir) -> std::path::PathBuf {
    let graph = dir.path().join("graph.json");
    graph_store::seed_rows(&graph, &[node("a-1"), node("b-1")]).unwrap();
    graph
}

fn another_session_retitles(graph: &Path, id: &str) {
    mutate_rows(graph, DEFAULT_LOCK_TIMEOUT, None, None, |rows| {
        for row in rows.iter_mut().filter(|row| row["id"] == json!(id)) {
            row["title"] = json!("moved by another session");
        }
        Ok(true)
    })
    .unwrap();
}

fn publish(graph: &Path, retitle: &str) -> Result<(), StoreError> {
    let (base_version, snapshot) = read_snapshot(graph).unwrap();
    let mut entries = snapshot.rows.clone();
    for row in entries.iter_mut().filter(|row| row["id"] == json!(retitle)) {
        row["title"] = json!("planned over the snapshot");
    }
    entries.push(node("c-1"));
    // The write that lands while this one was planning.
    another_session_retitles(graph, "a-1");
    locked_mutate_snapshot(
        graph,
        MutateInput {
            entries,
            canonical_path: None,
            base_version,
            plan_rungs: None,
        },
        snapshot,
        DEFAULT_LOCK_TIMEOUT,
    )
    .map(|_| ())
}

#[test]
fn a_write_lands_when_only_other_rows_moved_while_it_planned() {
    let dir = tempfile::tempdir().unwrap();
    let graph = seeded(&dir);
    publish(&graph, "b-1").unwrap();
    let rows = graph_store::read_rows(&graph).unwrap();
    let title = |id: &str| {
        rows.iter()
            .find(|row| row["id"] == json!(id))
            .map(|row| row["title"].clone())
    };
    assert_eq!(title("c-1"), Some(json!("Node c-1")));
    assert_eq!(title("b-1"), Some(json!("planned over the snapshot")));
    assert_eq!(title("a-1"), Some(json!("moved by another session")));
}

#[test]
fn a_write_refuses_when_a_row_it_touches_moved() {
    let dir = tempfile::tempdir().unwrap();
    let graph = seeded(&dir);
    let err = publish(&graph, "a-1").unwrap_err();
    assert!(matches!(err, StoreError::Conflict), "got {err}");
    let rows = graph_store::read_rows(&graph).unwrap();
    assert!(rows.iter().all(|row| row["id"] != json!("c-1")));
}
