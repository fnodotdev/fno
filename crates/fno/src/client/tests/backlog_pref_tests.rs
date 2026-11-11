//! The sideline menu's backlog and share rows, the org board gate, and the
//! Settings card-layout toggle.

use super::*;

#[test]
fn sideline_menu_lists_backlog_and_leaves_experimental_to_settings() {
    let menu = build_sideline_menu(Anchor::Center, None, experimental::Flags::default());
    let labels: Vec<(&str, &str)> = menu
        .popup
        .rows
        .iter()
        .filter_map(|row| match row {
            PopupRow::Entry { glyph, label, .. } => Some((glyph.as_str(), label.as_str())),
            _ => None,
        })
        .collect();
    // The experimental toggles live on the settings modal's own tab: a
    // header here read as if every row below it were experimental.
    assert!(!labels
        .iter()
        .any(|(_, l)| *l == "fleet org board" || *l == "pane share"));
    assert!(!labels.iter().any(|(_, l)| l.contains("backlog view")));
    assert!(!menu
        .popup
        .rows
        .iter()
        .any(|row| matches!(row, PopupRow::Header(h) if h == "experimental")));
    let backlog_hint = menu
        .popup
        .rows
        .iter()
        .find_map(|row| match row {
            PopupRow::Entry { label, hint, .. } if label == "backlog" => Some(hint.clone()),
            _ => None,
        })
        .expect("the backlog row is always shown");
    let want = crate::keys::key_for("open-backlog-board")
        .map(|k| format!("prefix {k}"))
        .unwrap_or_default();
    assert_eq!(backlog_hint, want, "the row hint is the live chord");
    // Rows and actions pair by entry index; a swapped pair would run the
    // wrong action.
    let at = |label: &str| labels.iter().position(|(_, l)| *l == label).expect(label);
    assert_eq!(menu.actions[at("backlog")], AuxAction::OpenBacklogView);
    assert_eq!(menu.actions[at("keybindings")], AuxAction::OpenKeybinds);
    let on = build_sideline_menu(
        Anchor::Center,
        None,
        experimental::Flags {
            org_board: true,
            pane_share: true,
            ..Default::default()
        },
    );
    // With pane share on, the menu carries the share row, still paired.
    let at = on
        .popup
        .rows
        .iter()
        .filter(|row| matches!(row, PopupRow::Entry { .. }))
        .position(
            |row| matches!(row, PopupRow::Entry { label, .. } if label == "share focused pane"),
        )
        .expect("the share row shows while pane share is on");
    assert_eq!(on.actions[at], AuxAction::Share(share_flow::Act::Start));
}

#[test]
fn org_board_opens_only_while_its_experimental_flag_is_on() {
    let mut v = two_pane_view();
    v.experimental_org_board = false;
    org_board::open(&mut v);
    assert!(v.org_board.is_none(), "off: nothing opens");
    assert!(
        v.notice
            .as_ref()
            .is_some_and(|(notice, _)| notice.contains("fleet org board is off")),
        "off: the notice names where to enable it"
    );
    v.experimental_org_board = true;
    org_board::open(&mut v);
    assert!(v.org_board.is_some(), "on: the board opens");
}

// The first-run show arms the board for frame one and lands the sideline on
// Org, but never takes the keyboard: a user booted into the command center
// still types into their pane. It saves the seen marker, so the show happens
// once, and raises no notice: the caller already read the flag.
#[test]
fn open_first_run_paints_the_board_without_taking_the_keyboard() {
    let prefs = tempfile::tempdir().expect("tempdir");
    crate::view_store::set_test_path(prefs.path());
    let mut v = two_pane_view();
    v.experimental_org_board = true;
    let owner_before = v.region_owner;
    org_board::open_first_run(&mut v);
    assert!(
        v.org_board.is_some(),
        "the board is armed for the first paint"
    );
    assert_eq!(v.sideline_view, crate::view_store::SidelineView::Org);
    assert_eq!(v.region_owner, owner_before, "input stays on the pane");
    assert!(
        v.notice.is_none(),
        "no flag-off notice on the first-run path"
    );
    assert!(v.backlog_board.is_none(), "no backlog board rides along");
    assert!(
        crate::view_store::load_experimental(crate::view_store::EXPERIMENTAL_ORG_BOARD_SEEN),
        "the seen marker persists"
    );
}

// The boot landing is once-only: a store with the flag on and no marker gets
// the first-run show, a store whose operator turned the flag off gets Agents
// with no marker written, and a store that already saw the show gets Agents
// with the marker left alone.
#[test]
fn boot_sideline_shows_once_then_lands_on_agents() {
    let prefs = tempfile::tempdir().expect("tempdir");
    crate::view_store::set_test_path(prefs.path());
    // Fresh store: the show fires and writes the marker.
    let mut v = two_pane_view();
    v.experimental_org_board = true;
    org_board::boot_sideline(&mut v);
    assert!(v.org_board.is_some(), "first boot: the show fires");
    assert_eq!(v.sideline_view, crate::view_store::SidelineView::Org);
    // Seen store: no second show, and the marker survives untouched.
    let mut v = two_pane_view();
    v.experimental_org_board = true;
    org_board::boot_sideline(&mut v);
    assert!(v.org_board.is_none(), "second boot: no second show");
    assert_eq!(v.sideline_view, crate::view_store::SidelineView::Agents);
    // Flag off: Agents, and the marker stays unwritten so a later explicit
    // turn-on still earns the show.
    let prefs2 = tempfile::tempdir().expect("tempdir");
    crate::view_store::set_test_path(prefs2.path());
    let mut v = two_pane_view();
    v.experimental_org_board = false;
    org_board::boot_sideline(&mut v);
    assert!(v.org_board.is_none(), "flag off: nothing opens");
    assert_eq!(v.sideline_view, crate::view_store::SidelineView::Agents);
    assert!(
        !crate::view_store::load_experimental(crate::view_store::EXPERIMENTAL_ORG_BOARD_SEEN),
        "flag off writes no marker"
    );
}
