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

// The composer opened from HQ is drawn by HQ and HQ stays the screen; the keys
// that mean something in HQ (`n`, `L`) are plain text inside it.
#[test]
fn n_and_l_typed_in_the_hq_composer_are_text_and_hq_stays_behind() {
    let mut v = two_pane_view();
    org_board::open(&mut v);
    org_board::open_composer(&mut v, "");
    assert!(v.launcher.is_some(), "the composer opened");
    assert_eq!(
        v.sideline_view,
        crate::view_store::SidelineView::Org,
        "HQ stays the screen"
    );
    let mut sock: Vec<u8> = Vec::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"nLn", &mut sock).await;
    });
    let draft = &v.launcher.as_ref().expect("still open").draft;
    assert_eq!(draft.message, "nLn", "n and L type text");
    assert!(v.org_board.is_some(), "HQ is still there");
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b", &mut sock).await;
    });
    assert!(v.launcher.is_none(), "esc closes the composer");
    assert_eq!(
        v.sideline_view,
        crate::view_store::SidelineView::Org,
        "and HQ is back"
    );
}

// HQ is always reachable: `open` takes no flag. The launch screen only picks
// what a boot shows first, and a boot into HQ paints the board without taking
// the keyboard, so a user still types into their pane.
#[test]
fn boot_sideline_follows_the_launch_screen_and_keeps_the_pane_keyboard() {
    let prefs = tempfile::tempdir().expect("tempdir");
    crate::view_store::set_test_path(prefs.path());
    let mut v = two_pane_view();
    org_board::open(&mut v);
    assert!(v.org_board.is_some(), "HQ opens with no flag set");

    let mut v = two_pane_view();
    v.launch_screen = crate::view_store::LaunchScreen::Hq;
    let owner_before = v.region_owner;
    org_board::boot_sideline(&mut v);
    assert!(
        v.org_board.is_some(),
        "hq: the board is armed for frame one"
    );
    assert_eq!(v.sideline_view, crate::view_store::SidelineView::Org);
    assert_eq!(v.region_owner, owner_before, "input stays on the pane");
    assert!(v.backlog_board.is_none(), "no backlog board rides along");

    let mut v = two_pane_view();
    v.launch_screen = crate::view_store::LaunchScreen::Agents;
    org_board::boot_sideline(&mut v);
    assert!(v.org_board.is_none(), "agents: nothing opens");
    assert_eq!(v.sideline_view, crate::view_store::SidelineView::Agents);
}
