use super::super::tests::empty_core;
use super::super::*;

fn client(id: u64, passive: bool) -> Client {
    Client {
        id,
        reliable_tx: mpsc::channel(1).0,
        dirty: Arc::default(),
        notify: Arc::new(Notify::new()),
        synced_modes: Modes::default(),
        view: (1, 1),
        visible: HashSet::new(),
        dims: (24, 80),
        passive,
        last_press: None,
    }
}

#[test]
fn a_pane_pty_loses_the_largest_inset_among_the_clients_viewing_it() {
    let mut core = empty_core();
    core.shells = vec!["/bin/cat".into()];
    let p1 = core.spawn_pane(24, 40, "/tmp").expect("pane 1");
    let p2 = core.spawn_pane(24, 40, "/tmp").expect("pane 2");
    core.session.add_squad(
        1,
        vec!["/tmp/inset".into()],
        None,
        Tab {
            name: None,
            id: 1,
            root: Node::Branch {
                axis: Axis::Horizontal,
                children: vec![(0.5, Node::Leaf(p1)), (0.5, Node::Leaf(p2))],
            },
            focus: p1,
        },
    );
    let size = |core: &Core| {
        let of = |p: u64| core.panes.get(&p).unwrap().requested_size;
        (of(p1), of(p2))
    };
    // A terminal client alone: no inset. Both rects are framed, so each PTY is
    // its rect minus the border ring.
    core.clients.push(client(1, false));
    core.push_layout(true);
    assert_eq!(size(&core), ((22, 37), (22, 38)));
    // The app joins and asks for one row and one column of padding: the
    // terminal client's panes shrink with it. (A push drops clients whose
    // receiver is gone, so both are registered again.)
    core.clients.push(client(1, false));
    core.clients.push(client(2, false));
    core.handle(CoreMsg::PaneInset {
        id: 2,
        rows: 1,
        cols: 1,
    });
    assert_eq!(size(&core), ((21, 36), (21, 37)));
    // An observer never insets anything.
    core.clients.push(client(3, true));
    core.handle(CoreMsg::PaneInset {
        id: 3,
        rows: 5,
        cols: 5,
    });
    assert_eq!(core.tab_inset(1), (1, 1));
    assert!(!core.sizing.insets.contains_key(&3));
}
