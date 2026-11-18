use super::*;

#[test]
fn missing_unopenable_and_pre_epoch_stores_never_read_complete() {
    // No store at all: nothing is proven.
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    let missing = coverage(&live, Some(0), &[]);
    assert_eq!(missing.status, "unreadable", "{missing:?}");
    // A store file that is not a database backs no confident count.
    sync(&live).unwrap();
    let store = store_path(&live);
    std::fs::write(&store, b"not a database").unwrap();
    let garbage = coverage(&live, Some(0), &[]);
    assert_eq!(garbage.status, "unreadable", "{garbage:?}");
}
