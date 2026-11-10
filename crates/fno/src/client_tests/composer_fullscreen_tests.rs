//! The composer toggle (prefix+i) and full-screen sideline (prefix+F).
//! Extracted under the file-budget ratchet: the tests this feature
//! added answer their own question in a file of their own.

use super::*;

#[allow(clippy::indexing_slicing)]
#[tokio::test]
async fn toggle_composer_opens_closes_and_retains_the_draft() {
    let mut v = two_pane_view();
    let mut buf: Vec<u8> = Vec::new();
    dispatch_event(&mut v, Event::ToggleComposer, &mut buf)
        .await
        .unwrap();
    assert!(v.launcher.is_some(), "prefix+i opens the composer");
    if let Some(l) = v.launcher.as_mut() {
        l.draft.message = "hello".into();
    }
    // The mail composer's draft is fno state on disk: saved per target row,
    // restored on the next open, gone only after an Esc or a send - the
    // sibling contract the launcher retention here already holds in memory.
    super::composer_draft::save("candor", "hello there");
    assert_eq!(super::composer_draft::load("candor"), "hello there");
    super::composer_draft::delete("candor");
    assert_eq!(super::composer_draft::load("candor"), "");
    dispatch_event(&mut v, Event::ToggleComposer, &mut buf)
        .await
        .unwrap();
    assert!(v.launcher.is_none(), "prefix+i closes it");
    assert_eq!(
        v.launcher_closed[&1].draft.message, "hello",
        "draft retained"
    );
    dispatch_event(&mut v, Event::ToggleComposer, &mut buf)
        .await
        .unwrap();
    assert_eq!(
        v.launcher.as_ref().unwrap().draft.message,
        "hello",
        "reopen restores the draft"
    );
}

#[tokio::test]
async fn toggle_composer_reveals_a_hidden_panel_and_resizes_the_panes() {
    // With the sideline hidden, prefix+i reveals it at its normal width and
    // opens the focused workspace's card inside it; the panes get the new
    // content area.
    let mut v = two_pane_view();
    v.panel_on = false;
    let mut buf: Vec<u8> = Vec::new();
    dispatch_event(&mut v, Event::ToggleComposer, &mut buf)
        .await
        .unwrap();
    assert!(v.launcher.is_some(), "the card opens");
    assert!(v.panel_on, "the sideline is revealed");
    let mut cur = std::io::Cursor::new(buf);
    match crate::proto::read_msg_sync::<_, ClientMsg>(&mut cur).unwrap() {
        ClientMsg::Resize { .. } => {}
        other => panic!("expected the reveal Resize, got {other:?}"),
    }
}
#[tokio::test]
async fn full_screen_sideline_toggles_back_resyncs_and_repaints_panes() {
    let mut v = two_pane_view();
    v.term = (40, 120);
    v.frames.insert(10, text_frame(29, 35, 'Q'));
    v.frames.insert(11, text_frame(29, 36, 'Q'));
    let mut buf: Vec<u8> = Vec::new();
    dispatch_event(&mut v, Event::ToggleFullSideline, &mut buf)
        .await
        .unwrap();
    dispatch_event(&mut v, Event::ToggleFullSideline, &mut buf)
        .await
        .unwrap();
    assert!(!v.sideline_full, "leaving clears the flag");
    assert!(
        frame_text(&v.compose()).contains('Q'),
        "the panes paint again at their old size"
    );
    // Leaving re-syncs the server's content area once: a full-screen visit
    // that showed a hidden sideline left the server on panel-free pane
    // rects, and an idempotent re-layout is cheaper than stale geometry.
    let mut cur = std::io::Cursor::new(buf);
    match crate::proto::read_msg_sync::<_, ClientMsg>(&mut cur).unwrap() {
        ClientMsg::Resize { .. } => {}
        other => panic!("expected the leave Resize, got {other:?}"),
    }
}
