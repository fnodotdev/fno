use super::*;

#[test]
fn store_findings_read_error_is_named_not_zero() {
    // AC5: a store whose JSON leg is unparseable (and no db to answer
    // instead) reads as an ERROR, never as a clean node.
    let tmp = tempfile::tempdir().unwrap();
    let graph = tmp.path().join("graph.json");
    std::fs::write(&graph, "{ not json at all").unwrap();
    let (open, error) = open_findings_from_store(&graph, "x-1");
    assert!(open.is_empty());
    assert!(error.is_some(), "could-not-read must not read as zero");
}
