//! `fno::node_row` and `fno_agents::backlog` each name the node status
//! words and the board columns (this crate links `fno` as a dev-dependency
//! only). The mux reader classifies rows the keeper wrote, so a word or a
//! column that one side adds alone is a row the two boards read apart.

use fno::node_row::{KanbanColumn, NodeRow, Status as MuxStatus};
use fno_agents::backlog::model::Status;
use fno_agents::backlog::projections::KANBAN_COLUMNS;
use serde_json::Value;

#[test]
fn the_mux_reader_and_the_keeper_share_one_status_and_column_vocabulary() {
    let keeper: Vec<&str> = Status::ALL.iter().map(|s| s.as_str()).collect();
    let mux: Vec<&str> = MuxStatus::KNOWN.iter().map(MuxStatus::as_str).collect();
    let only_keeper: Vec<&&str> = keeper.iter().filter(|w| !mux.contains(w)).collect();
    let only_mux: Vec<&&str> = mux.iter().filter(|w| !keeper.contains(w)).collect();
    assert!(
        only_keeper.is_empty() && only_mux.is_empty(),
        "status words differ: keeper only {only_keeper:?}, mux only {only_mux:?}"
    );
    assert_eq!(mux, keeper, "the status vocabularies list one order");
    // The store's migration: both legs read the retired word the same way.
    assert_eq!(
        MuxStatus::parse("claimed").as_str(),
        Status::parse("claimed").map(Status::as_str).unwrap()
    );

    let mux_columns: Vec<&str> = KanbanColumn::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(mux_columns, KANBAN_COLUMNS, "board columns, left to right");
}

/// The mux reader ships rows back to the keeper (board facts, flow), so a
/// row must leave `NodeRow` as the object it arrived as.
#[test]
fn every_golden_store_row_round_trips_through_the_mux_row() {
    use base64::Engine as _;
    /// A golden: the graph file, base64.
    #[derive(serde::Deserialize)]
    struct Golden {
        file: String,
    }
    /// The graph file: its rows, untyped.
    #[derive(serde::Deserialize)]
    struct Graph {
        #[serde(default)]
        entries: Vec<Value>,
    }
    let goldens = std::fs::read_dir("tests/golden/graph_store").expect("the golden dir");
    let mut rows = 0;
    for path in goldens.map(|e| e.expect("a dir entry").path()) {
        let doc: Golden = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let body = base64::engine::general_purpose::STANDARD
            .decode(doc.file)
            .unwrap();
        let body: Graph = serde_json::from_slice(&body).unwrap();
        for row in &body.entries {
            let typed: NodeRow = serde_json::from_value(row.clone()).expect("the parse is total");
            assert_eq!(typed.to_value(), *row, "{}: {row}", path.display());
            rows += 1;
        }
    }
    assert!(rows > 0, "the goldens hold rows");
}
