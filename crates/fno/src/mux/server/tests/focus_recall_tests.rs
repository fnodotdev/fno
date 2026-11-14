use super::*;

// The focus_recall command family (v124): zoom, next pane, last pane, last
// tab and swap pane - the tmux parity commands from docs/mux-keys.md. The
// family lives here because server_tests.rs is shrink-only past the file
// budget; every other family follows the same server/tests/ convention.

/// A one-squad core whose tab `5` holds the given leaves in a flat
/// horizontal branch (one leaf passes through as a bare leaf tab), focus on
/// the named pane, with a 24x80 area. The leaves need no live panes: every
/// command under test touches only the tree, the recall maps and notices.
fn panes_core(leaves: &[u64], focus: u64) -> Core {
    let mut core = empty_core();
    let root = if leaves.len() == 1 {
        Node::Leaf(*leaves.first().expect("one leaf"))
    } else {
        Node::Branch {
            axis: Axis::Horizontal,
            children: leaves
                .iter()
                .map(|&id| (1.0 / leaves.len() as f32, Node::Leaf(id)))
                .collect(),
        }
    };
    core.session.add_squad(
        1,
        vec!["/a".into()],
        None,
        Tab {
            name: None,
            id: 5,
            root,
            focus,
        },
    );
    core.sizing.areas.insert(5, (24, 80));
    core
}

/// A client whose notice stream stays readable: the shared `client` helper
/// drops the receiver, and a notice into a dead channel would still pass.
fn client_with_rx(id: u64, tab: TabId) -> (Client, mpsc::Receiver<ServerMsg>) {
    let (tx, rx) = mpsc::channel(16);
    let mut c = client(id, tab, (24, 80), false);
    c.reliable_tx = tx;
    (c, rx)
}

fn notices(rx: &mut mpsc::Receiver<ServerMsg>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(ServerMsg::Notice { text }) = rx.try_recv() {
        out.push(text);
    }
    out
}

fn viewed_tab(core: &Core) -> &Tab {
    core.viewed_tab((1, 5)).expect("tab 5 live")
}

fn rects(core: &mut Core) -> Vec<(u64, Rect)> {
    let tab = core.viewed_tab((1, 5)).expect("tab 5 live").clone();
    core.recall.tab_rects(
        &tab,
        Rect {
            x: 0,
            y: 0,
            rows: 24,
            cols: 80,
        },
    )
}

/// AC1-HP: `prefix ;` toggles between the two most recent panes of the tab.
#[test]
fn last_pane_toggles_between_the_two_most_recent_panes() {
    let mut core = panes_core(&[1, 2, 3], 1);
    core.clients.push(client(1, 5, (24, 80), false));
    // Prime the pass bookkeeping the way a lived-in tab already is: the
    // level-triggered record seeds at a tab's first layout pass, so a
    // change seen on that same pass would have no previous to rotate in.
    // On the real path attach has already run passes before any gesture.
    core.command(1, Command::FocusPane(1));
    // Focus 1 -> 2 records 1 as the previous pane (level-triggered in
    // tab_rects, run by the focus arm's push_layout).
    core.command(1, Command::FocusPane(2));
    assert_eq!(viewed_tab(&core).focus, 2);

    core.command(1, Command::LastPane);
    assert_eq!(viewed_tab(&core).focus, 1, "first `;` returns to pane 1");
    if let Some(c) = core.clients.first_mut() {
        *c = client(1, 5, (24, 80), false);
    }

    core.command(1, Command::LastPane);
    assert_eq!(viewed_tab(&core).focus, 2, "second `;` returns to pane 2");
}

/// AC2-HP: `prefix z` sends one whole-tab rect for the zoomed pane, the
/// second press restores the real rects, and a focus change ends the zoom.
#[test]
fn zoom_pane_covers_the_tab_until_a_focus_change_or_second_press() {
    let mut core = panes_core(&[1, 2], 1);
    core.clients.push(client(1, 5, (24, 80), false));

    core.command(1, Command::ZoomPane);
    let tab_rects = rects(&mut core);
    assert_eq!(tab_rects.len(), 1, "zoom paints one rect");
    let Some(&(zoomed, rect)) = tab_rects.first() else {
        panic!("zoom paints one rect");
    };
    assert_eq!(zoomed, 1, "the zoomed pane is the focused one");
    assert_eq!(
        (rect.rows, rect.cols),
        (24, 80),
        "the zoom rect covers the whole tab area"
    );

    // A focus change ends the zoom before any explicit unzoom.
    core.command(1, Command::FocusPane(2));
    assert!(core.recall.zoomed.get(&5).is_none(), "focus change unzooms");
    assert_eq!(rects(&mut core).len(), 2);

    // Re-zoom on pane 2, then the second press restores.
    core.command(1, Command::ZoomPane);
    assert_eq!(rects(&mut core).len(), 1);
    core.command(1, Command::ZoomPane);
    assert!(core.recall.zoomed.get(&5).is_none(), "second z unzooms");
    assert_eq!(rects(&mut core).len(), 2, "both rects are back");
}

/// AC3-HP: `prefix o` walks `tree::leaves` order with wrap.
#[test]
fn next_pane_wraps_in_tree_order() {
    let mut core = panes_core(&[1, 2, 3], 3);
    core.clients.push(client(1, 5, (24, 80), false));

    core.command(1, Command::NextPane);
    assert_eq!(viewed_tab(&core).focus, 1, "3 wraps to 1");
    if let Some(c) = core.clients.first_mut() {
        *c = client(1, 5, (24, 80), false);
    }
    core.command(1, Command::NextPane);
    assert_eq!(viewed_tab(&core).focus, 2);
}

/// AC4-HP: `prefix l` returns this client to its previous view, and no
/// other client's view moves. A second `l` toggles back.
#[test]
fn last_tab_returns_this_client_to_its_previous_view_only() {
    let mut core = panes_core(&[1, 2], 1);
    core.session.squad_mut(1).unwrap().tabs.push(Tab {
        name: Some("bee".into()),
        id: 6,
        root: Node::Leaf(2),
        focus: 2,
    });
    core.sizing.areas.insert(6, (24, 80));
    let (c1, _rx1) = client_with_rx(1, 5);
    let (c2, _rx2) = client_with_rx(2, 6);
    core.clients.push(c1);
    core.clients.push(c2);

    // Client 1 moves 5 -> 6 through the one view gateway.
    core.set_view(1, 1, 6);
    assert_eq!(core.client_view(1), Some((1, 6)));
    assert_eq!(
        core.client_view(2),
        Some((1, 6)),
        "sanity: c2 already there"
    );

    core.command(1, Command::LastTab);
    assert_eq!(core.client_view(1), Some((1, 5)), "c1 returns to tab 5");
    assert_eq!(core.client_view(2), Some((1, 6)), "c2 never moves");

    if let Some(c) = core.clients.first_mut() {
        *c = client(1, 5, (24, 80), false);
    }
    core.command(1, Command::LastTab);
    assert_eq!(core.client_view(1), Some((1, 6)), "second l toggles back");
}

/// AC5-HP: `prefix }` exchanges the focused pane with its next neighbour in
/// tree order, keeps focus on the same pane, and `prefix {` puts them back.
/// The tree-level unit rows for [`tree::swap_leaves`] ride here with the
/// command rows: one swap contract, two altitudes, no other owner to extend.
#[test]
fn swap_pane_exchanges_neighbours_and_keeps_focus() {
    let mut core = panes_core(&[1, 2, 3], 2);
    core.clients.push(client(1, 5, (24, 80), false));

    core.command(1, Command::SwapPane { forward: true });
    let leaves = tree::leaves(&viewed_tab(&core).root);
    assert_eq!(leaves, vec![1, 3, 2], "2 swaps with its next neighbour");
    assert_eq!(viewed_tab(&core).focus, 2, "focus stays on pane 2");

    if let Some(c) = core.clients.first_mut() {
        *c = client(1, 5, (24, 80), false);
    }
    core.command(1, Command::SwapPane { forward: false });
    let leaves = tree::leaves(&viewed_tab(&core).root);
    assert_eq!(leaves, vec![1, 2, 3], "prev swap puts them back");

    // Tree-level unit rows: swap_leaves exchanges ids in place, keeps sizes
    // and ratios alone, and refuses a missing id or a == b untouched.
    let mut tab = Tab {
        name: None,
        id: 0,
        root: Node::Branch {
            axis: Axis::Horizontal,
            children: vec![
                (0.2, Node::Leaf(1)),
                (0.3, Node::Leaf(2)),
                (0.5, Node::Leaf(3)),
            ],
        },
        focus: 2,
    };
    let ratios_before = match &tab.root {
        Node::Branch { children, .. } => children.iter().map(|(r, _)| *r).collect::<Vec<_>>(),
        _ => unreachable!(),
    };
    assert!(tree::swap_leaves(&mut tab, 1, 3));
    assert_eq!(tree::leaves(&tab.root), vec![3, 2, 1]);
    assert_eq!(
        tab.focus, 2,
        "the focused pane id survives a swap of other ids"
    );
    assert!(
        tree::check_invariants(&tab).is_ok(),
        "invariants hold after a swap"
    );
    let ratios_after = match &tab.root {
        Node::Branch { children, .. } => children.iter().map(|(r, _)| *r).collect::<Vec<_>>(),
        _ => unreachable!(),
    };
    assert_eq!(ratios_after, ratios_before, "swap keeps ratios alone");
    assert!(!tree::swap_leaves(&mut tab, 1, 1), "a == b refuses");
    assert!(!tree::swap_leaves(&mut tab, 1, 999), "missing id refuses");
    assert_eq!(
        tree::leaves(&tab.root),
        vec![3, 2, 1],
        "refusals leave the tree"
    );
}

/// AC6-EDGE: a one-pane tab refuses zoom, next pane, last pane and swap with
/// a notice and an unchanged tree; `l` with no earlier view notices and
/// leaves the view alone.
#[test]
fn one_pane_and_missing_recall_notice_and_change_nothing() {
    let mut core = panes_core(&[9], 9);
    let (c, mut rx) = client_with_rx(1, 5);
    core.clients.push(c);
    let root_before = viewed_tab(&core).root.clone();

    for (name, cmd) in [
        ("zoom", Command::ZoomPane),
        ("next", Command::NextPane),
        ("last pane", Command::LastPane),
        ("swap", Command::SwapPane { forward: true }),
    ] {
        core.command(1, cmd);
        // No client refresh here: this client's receiver stays retained, and
        // replacing it would send the later notices into a dropped channel.
        let got = notices(&mut rx);
        assert!(
            got.iter().any(|t| t.contains("only one pane")),
            "{name} on a one-pane tab notices, got {got:?}"
        );
        assert_eq!(
            viewed_tab(&core).root,
            root_before,
            "{name} mutates nothing"
        );
    }

    // No earlier view: `l` refuses without moving.
    core.command(1, Command::LastTab);
    if let Some(c) = core.clients.first_mut() {
        *c = client(1, 5, (24, 80), false);
    }
    let got = notices(&mut rx);
    assert!(
        got.iter().any(|t| t.contains("no last tab")),
        "no earlier view notices, got {got:?}"
    );
    assert_eq!(core.client_view(1), Some((1, 5)));
}
