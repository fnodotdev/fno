use super::*;

// The link gate (v121): a cross-machine `serve --link` session drives its
// one pane. The family lives here because server_tests.rs is shrink-only
// past the file budget; every other family follows the same server/tests/
// convention.

#[test]
fn link_gate_opens_one_pane_for_a_tagged_passive_client() {
    let (mut core, pane) = template_core();
    let vt = &mut core.panes.get_mut(&pane).unwrap().vt;
    vt.feed("line\r\n".repeat(200).as_bytes());
    core.clients.push(client(2, 5, (0, 0), true));
    let tick = |pane| CoreMsg::PaneScroll {
        id: 2,
        pane,
        delta: 3,
    };
    let hello = |identity: String| crate::proto::LinkHello {
        name: "me".into(),
        pane,
        identity,
        rows: 0,
        cols: 0,
    };
    let offset = |core: &Core| core.panes[&pane].vt.display_offset();
    core.handle(tick(pane));
    assert_eq!(offset(&core), 0, "an untagged observer stays read-only");
    core.handle(CoreMsg::Link(2, hello("pid:0".into())));
    core.handle(tick(pane));
    assert_eq!(offset(&core), 0, "a stale pane reading is refused");
    let child = core.panes[&pane].pty.child_pid().unwrap();
    core.handle(CoreMsg::Link(2, hello(format!("pid:{child}"))));
    core.handle(tick(pane));
    assert_ne!(offset(&core), 0, "the link drives its own pane");
    assert!(!core.links.may_send(2, &tick(pane + 1)), "only that pane");
    let command = CoreMsg::Command {
        id: 2,
        cmd: Command::ReapAgents,
    };
    assert!(!core.links.may_send(2, &command), "and no command");
    core.reap_pane(pane);
}
