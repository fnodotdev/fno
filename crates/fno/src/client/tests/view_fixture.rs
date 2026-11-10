//! The client tests' shared view fixtures: the squad fixture, a text frame,
//! and the two-pane view most board tests compose from. Lived in
//! client_tests.rs until the file-budget gate made that file shrink-only.

use super::*;

pub(crate) fn meta(id: u64, name: &str, tabs: usize, active_tab: usize) -> SquadMeta {
    SquadMeta {
        id,
        name: name.into(),
        canonical_cwd: format!("/code/{name}"),
        tabs: (1..=tabs)
            .map(|i| TabMeta {
                id: (i - 1) as u64,
                name: i.to_string(),
                named: false,
                panes: Vec::new(),
            })
            .collect(),
        active_tab,
        // One pane per tab is the test fixture's shape (each tab is a leaf).
        panes: tabs,
    }
}

pub(crate) fn text_frame(rows: u16, cols: u16, ch: char) -> Frame {
    Frame {
        rows,
        cols,
        cells: vec![
            Cell {
                c: ch,
                fg: Color::Default,
                bg: Color::Default,
                flags: 0,
            };
            rows as usize * cols as usize
        ],
        cursor_visible: true,
        ..Default::default()
    }
}

pub(crate) fn two_pane_view() -> View {
    // 30x100 terminal, panel visible (100 >= 28+40). Content = 28x72
    // (tab bar + status row). Two panes split H: 35 + divider + 36 cols.
    let mut view = View::new(
        (30, 100),
        "main".into(),
        LayoutView {
            squads: vec![meta(1, "footnote", 2, 1), meta(2, "notes", 1, 0)],
            active_squad: 1,
            panes: vec![
                (
                    10,
                    Rect {
                        x: 0,
                        y: 0,
                        rows: 29,
                        cols: 35,
                    },
                ),
                (
                    11,
                    Rect {
                        x: 36,
                        y: 0,
                        rows: 29,
                        cols: 36,
                    },
                ),
            ],
            focus: 11,
            area: (29, 72),
            agents: vec![],
            focus_node: None,
        },
    );
    view.frames.insert(10, text_frame(29, 35, 'a'));
    view.frames.insert(11, text_frame(29, 36, 'b'));
    // Pin the row shape: these helpers assert display-row geometry, and the
    // card default (ambient config or no config at all) inserts a detail
    // line per agent and moves every row index.
    view.sideline_layout = sideline_color::SidelineLayout::List;
    // Pin the no-op theme: these tests assert the terminal theme's literal
    // accent (`Indexed(3)`), which is the theme whose byte-identity the
    // lattice structure protects.
    view.theme = crate::theme::Theme::from_name("terminal").0;
    view.server_proto = Some(crate::proto::PROTO_VERSION);
    view
}
