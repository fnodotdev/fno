use super::*;

// The attach-door family: what an attach creates and refuses. Moved
// verbatim from server_tests.rs (file budget); that file is shrink-only.

#[test]
fn observer_attach_never_spawns_a_pane() {
    // Locked Decision 5: an observer (0,0) attach to a session it does not
    // match must register read-only without ever creating a squad/PTY.
    let mut core = empty_core();
    let (tx, _rx) = mpsc::channel::<ServerMsg>(8);
    core.attach(
        1,
        0,
        0,
        "/nowhere".into(),
        "/nowhere".into(),
        tx,
        Arc::default(),
        Arc::new(Notify::new()),
    );
    assert_eq!(
        core.panes.len(),
        0,
        "observer attach must never spawn a PTY"
    );
    assert_eq!(core.clients.len(), 1);
    assert!(
        core.clients.first().is_some_and(|c| c.passive),
        "the (0,0) client is passive"
    );
}
