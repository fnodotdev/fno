//! The rule book tests: book mode paints sections and topic faces with no
//! ids, and `[b]` toggles back to the flat table.

use super::*;

fn book_row(
    id: &str,
    slug: &str,
    lane: &str,
    lifecycle: &str,
    ts: &str,
    decision: &str,
) -> crate::decisions_overlay::DecisionRow {
    crate::decisions_overlay::DecisionRow {
        decision_id: id.into(),
        decision: decision.into(),
        subject: slug.into(),
        ts: ts.into(),
        lifecycle: lifecycle.into(),
        lane: lane.into(),
        subject_node: String::new(),
        subject_pr: String::new(),
        subject_text: slug.into(),
        ..Default::default()
    }
}

#[test]
fn the_book_paints_sections_and_faces_with_no_ids() {
    let mut v = two_pane_view();
    let rows = vec![
        book_row(
            "d-1",
            "merge-guard-purpose",
            "law",
            "live",
            "2026-11-05T00:00:00Z",
            "A merge guard stops a PR only when it does not solve the plan's problems.",
        ),
        book_row(
            "d-2",
            "look-rows-forward",
            "coord",
            "unscoped",
            "2026-11-05T01:00:00Z",
            "A look approval rolls forward to every later head until merge.",
        ),
    ];
    crate::client::decisions_view::open_rows(&mut v, rows);
    if let Some(b) = v.decisions_board.as_mut() {
        b.book = true;
    }
    let lines = book_lines_of(&v);
    let joined = lines.join("\n");
    assert!(
        joined.contains("MERGING AND REVIEW"),
        "section heading paints: {joined}"
    );
    assert!(
        joined.contains("merge guard purpose"),
        "plain-words title paints: {joined}"
    );
    assert!(
        joined
            .contains("A merge guard stops a PR only when it does not solve the plan's problems."),
        "face first sentence paints: {joined}"
    );
    assert!(
        !joined.contains("d-1") && !joined.contains("d-2"),
        "no decision id paints in book mode: {joined}"
    );
}

fn book_lines_of(v: &View) -> Vec<String> {
    crate::vt::frame_text(&v.compose())
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn the_book_toggle_returns_to_the_flat_table() {
    let mut v = two_pane_view();
    let rows = vec![book_row(
        "d-3",
        "ask-length",
        "law",
        "live",
        "2026-11-05T02:00:00Z",
        "Keep the ask short. The flat table paints the raw slug.",
    )];
    crate::client::decisions_view::open_rows(&mut v, rows);
    crate::client::decisions_view::keys(&mut v, b"b");
    let joined = book_lines_of(&v).join("\n");
    assert!(
        joined.contains("the rule book"),
        "book header paints after the toggle: {joined}"
    );
    crate::client::decisions_view::keys(&mut v, b"b");
    let flat = book_lines_of(&v).join("\n");
    assert!(
        flat.contains("ask-length"),
        "the flat table paints the raw slug: {flat}"
    );
    assert!(
        !flat.contains("the rule book"),
        "no book header in flat mode: {flat}"
    );
}

#[test]
fn retract_takes_its_reason_inline_and_runs_the_door_with_no_pane() {
    let mut v = two_pane_view();
    let row = book_row(
        "d-7",
        "merge-guard",
        "law",
        "live",
        "2026-11-05T00:00:00Z",
        "x",
    );
    crate::client::decisions_view::open_rows(&mut v, vec![row]);
    crate::client::decisions_view::keys(&mut v, b"r");
    for b in "stale -now".bytes() {
        crate::client::decisions_view::keys(&mut v, &[b]);
    }
    crate::client::decisions_view::keys(&mut v, b"\x7f");
    let painted = book_lines_of(&v).join("\n");
    assert!(
        painted.contains("reason: stale -no"),
        "entry paints: {painted}"
    );
    crate::client::decisions_view::keys(&mut v, b"\r");
    let want: Vec<String> = ["inbox", "law", "retract", "d-7", "--reason", "stale -no"]
        .map(String::from)
        .to_vec();
    assert_eq!(crate::client::decisions_view::last_door_argv(), Some(want));
    let notice = v
        .notice
        .as_ref()
        .map(|(t, _)| t.clone())
        .unwrap_or_default();
    assert!(!notice.contains("pane"), "no pane opens: {notice}");
    // Esc leaves an entry without running anything; the tab stays open.
    crate::client::decisions_view::keys(&mut v, b"o");
    crate::client::decisions_view::keys(&mut v, b"\x1b");
    assert!(v.decisions_board.is_some(), "Esc cancels the entry only");
    assert_eq!(crate::client::decisions_view::last_door_argv(), None);
}
