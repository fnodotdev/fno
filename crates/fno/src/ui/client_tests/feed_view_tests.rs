//! The feed panel's client-side tests (x-4433, x-f089): render order, the
//! hover marker, the click mapping, the focus mode, and the provenance view.
//! One `Reach` resolves a row, and the footer and the action both read it, so
//! a joined row yields exactly what `agent_hit` yields for that sideline row,
//! an unjoined row with a session id attaches on portal 0, and a row with no
//! session id offers nothing. The click resolver and the painter must invert
//! each other exactly.

use super::tests::view_with_agents;
use super::*;
use crate::client::feed_detail::{self, destination, Destination};
use crate::client::feed_view::table::Group;
use crate::client::feed_view::{feed_panel_lines, feed_row_item, FeedOverlay};
use crate::proto::Reach;

const W: usize = 40;
const ROWS: usize = 10;

fn feed_item(node: Option<&str>, sid: Option<&str>) -> crate::feed_overlay::FeedItem {
    crate::feed_overlay::FeedItem {
        ts: "2026-09-02T18:27:06Z".into(),
        kind: "pr_created".into(),
        node: node.map(Into::into),
        cwd: None,
        session_id: sid.map(Into::into),
        harness: None,
        title: "PR 1395".into(),
        r#ref: Some("1395".into()),
        name: None,
        actor: None,
        model: None,
        effort: None,
        phase: None,
        detail: None,
        reason: None,
        team: None,
        holder: None,
        owner: None,
        parent: None,
        url: None,
        lead: None,
        lead_current: false,
        area: Default::default(),
        cursor: String::new(),
    }
}

fn reaped_item(sid: &str, resume: &str) -> crate::feed_overlay::FeedItem {
    crate::feed_overlay::FeedItem {
        ts: "2026-09-06T10:00:00Z".into(),
        kind: "session_reaped".into(),
        node: None,
        cwd: None,
        session_id: Some(sid.into()),
        harness: Some("claude".into()),
        title: "t-d145 removed by reap".into(),
        r#ref: None,
        name: None,
        actor: None,
        model: None,
        effort: None,
        phase: None,
        detail: Some(resume.into()),
        reason: None,
        team: None,
        holder: None,
        owner: None,
        parent: None,
        url: None,
        lead: None,
        lead_current: false,
        area: Default::default(),
        cursor: String::new(),
    }
}

fn overlay(items: Vec<crate::feed_overlay::FeedItem>) -> FeedOverlay {
    overlay_in(items, Group::LeadOrEpic)
}

/// A settled panel holding `items` under one grouping, the cursor on the
/// first row: what a landed fold leaves.
fn overlay_in(items: Vec<crate::feed_overlay::FeedItem>, group: Group) -> FeedOverlay {
    let mut f = feed_view::open_overlay(None, 0);
    f.view.group = group;
    f.win.items = items;
    f.want = false;
    feed_view::reslot(&mut f);
    f.sel = feed_view::first_item_slot(&f.slots);
    f
}

/// A pane-hosted row, so the joined case exercises agent_hit's FocusPane arm.
fn joined_row(name: &str, cwd_base: Option<&str>, pane: Option<u64>) -> AgentRow {
    AgentRow {
        spawned_by_name: None,
        lineage_reason: None,
        harness: None,
        model: None,
        route: None,
        reach: Reach::Locate,
        spawned_by_session: None,
        lineage_kind: None,
        harness_session_id: None,
        squad: None,
        name: name.into(),
        pane_id: pane,
        portal: None,
        badge: None,
        reason: None,
        exited: false,
        dnd: false,
        unmeasured: false,
        answerable: None,
        attach_id: None,
        external: false,
        seen: false,
        cwd_base: cwd_base.map(str::to_string),
        tombstone: false,
        subline: None,
        tab: None,
        account: None,
        updated_at: None,
        pr: None,
        pr_session_short: None,
        tail: None,
        role_level: None,
        role_scope: None,
        role_title: None,
        basis: None,
        last_activity_age_s: None,
        resumable: false,
        no_pane_reason: None,
        pane_activity: None,
        ..Default::default()
    }
}

fn view_with_rows(rows: Vec<AgentRow>) -> View {
    view_with_agents(rows)
}

#[test]
fn render_rows() {
    let o = {
        let mut a = feed_item(Some("x-aaaa"), Some("s-1"));
        a.ts = "2026-09-02T16:27:06Z".into();
        let mut b = feed_item(Some("x-bbbb"), Some("s-2"));
        b.ts = "2026-09-02T17:27:06Z".into();
        let mut c = feed_item(Some("x-cccc"), Some("s-3"));
        c.ts = "2026-09-02T18:27:06Z".into();
        overlay(vec![a, b, c])
    };
    let lines = feed_panel_lines(&o, false, W, ROWS, 0);
    // The viewport is exact: header + ROWS-2 item rows + footer, so the
    // painter can blit 1:1 and short lists render blank below their last row.
    assert_eq!(lines.len(), ROWS);
    // Slot 0 is the `other` group header (no owners in this fixture); the
    // top ITEM row shows the newest event.
    assert!(
        lines[1].contains("time"),
        "column header second: {}",
        lines[1]
    );
    assert!(
        lines[2].contains("other"),
        "group header first: {}",
        lines[2]
    );
    assert!(
        lines.get(3).is_some_and(|l| l.contains("x-cccc")),
        "top row shows the newest: {lines:?}"
    );
    assert!(!lines[3].contains('▸'), "the fill is the cursor");
    assert!(lines.get(4).is_some_and(|l| l.contains("x-bbbb")));
    assert!(lines.get(5).is_some_and(|l| l.contains("x-aaaa")));
    assert!(lines[6].trim().is_empty(), "below the last item: blank");
    assert!(
        lines
            .first()
            .is_some_and(|l| l.contains("activity feed · 3")),
        "the title counts the rows: {lines:?}"
    );

    let o = {
        let mut a = feed_item(Some("x-aaaa"), Some("s-1"));
        a.ts = "2026-09-02T16:27:06Z".into();
        let mut b = feed_item(Some("x-bbbb"), Some("s-2"));
        b.ts = "2026-09-02T17:27:06Z".into();
        let mut c = feed_item(Some("x-cccc"), Some("s-3"));
        c.ts = "2026-09-02T18:27:06Z".into();
        overlay(vec![a, b, c])
    };
    // Slot offset 1 skips the group header, so the newest row (x-c) leads
    // the window now.
    let lines = feed_panel_lines(&o, false, W, ROWS, 1);
    assert!(lines.get(2).is_some_and(|l| l.contains("x-cccc")));
    assert!(lines.get(3).is_some_and(|l| l.contains("x-bbbb")));
    assert!(lines.get(4).is_some_and(|l| l.contains("x-aaaa")));
    assert!(lines[5].trim().is_empty());

    // x-d15a: the failure line names the cause, never the old generic
    // "feed unavailable" sentence. Timeout names its budget; a malformed
    // body carries the projection's stderr so the cause leads the line.
    let mut o = overlay(vec![]);
    o.error = Some(crate::feed_overlay::FeedError::Timeout);
    let lines = feed_panel_lines(&o, false, W, ROWS, 0);
    assert!(lines.iter().any(|l| l.contains("timed out after 10s")));

    let mut o = overlay(vec![]);
    o.error = Some(crate::feed_overlay::FeedError::Exit(
        "unreadable store: graph.json".into(),
    ));
    let lines = feed_panel_lines(&o, false, W, ROWS, 0);
    // At the panel's 40 columns the panel clips the tail; the CAUSE still
    // leads the line (the x-d15a contract).
    assert!(lines.iter().any(|l| l.contains("feed exited non-zero")));

    // AC15-HP: a landed page under a query names the query in the footer,
    // and every row's node cell reads the filtered node.
    let mut o = overlay(vec![feed_item(Some("x-1234"), None)]);
    o.query_text = "x-1234".into();
    let lines = feed_panel_lines(&o, false, W, ROWS, 0);
    assert!(
        lines.iter().filter(|l| l.contains("x-1234")).count() >= 1,
        "the filtered rows paint: {lines:?}"
    );
    assert!(
        lines.last().unwrap().contains("x-1234"),
        "the footer shows the query: {}",
        lines.last().unwrap()
    );
    // AC15-EDGE: a query with zero hits over held rows reads as a filter,
    // never as an empty feed.
    let mut o = overlay(vec![feed_item(Some("x-1"), None)]);
    o.query_text = "k:pane".into();
    feed_view::search::apply_query(&mut o);
    let lines = feed_panel_lines(&o, false, W, ROWS, 0);
    assert!(
        lines.iter().any(|l| l.contains("no rows match")),
        "{lines:?}"
    );
}

/// Header and data draw through ONE column grid: every title's display
/// offset equals its column's value offset, a value at the cap fills its
/// cell with a one-space gutter, and a value past the cap ellipsizes
/// without pushing the next column sideways.
#[test]
fn column_grid_rows() {
    fn display_col(line: &str, needle: &str) -> Option<usize> {
        let byte = line.find(needle)?;
        Some(
            line[..byte]
                .chars()
                .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(1))
                .sum(),
        )
    }
    // The rendered ts spells differently per zone (short_ts is local), so the
    // time cell is read back out of the row: display columns 3..14 are the
    // marker block plus the 11-wide date cell.
    fn display_slice(line: &str, start: usize, len: usize) -> String {
        let mut out = String::new();
        let mut col = 0usize;
        for ch in line.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
            if col >= start + len {
                break;
            }
            if col + cw > start {
                out.push(ch);
            }
            col += cw;
        }
        out
    }
    let mut long = feed_item(Some("x-a1b2c3d4e5f6"), Some("s-1234567890"));
    long.kind = "daemon_restarted".into();
    long.area = "mux".into();
    long.harness = Some("claude".into());
    long.lead = Some("team-alpha".into());
    long.title = "restarted the daemon".into();
    let mut over = feed_item(Some("x-b"), Some("s-2"));
    // A kind past the 16-column cap: the cell ellipsizes at the cap.
    over.kind = "prompt_parked_by_gatekeeper".into();
    let o = overlay_in(vec![long, over], Group::None);
    let lines = feed_panel_lines(&o, false, 120, ROWS, 0);
    let header = &lines[1];
    // Recent order renders NEWEST first, so display row 0 is `over` and
    // display row 1 (lines[3]) is `long`, whose cells carry the wide values.
    let row = &lines[3];
    let ts_cell = display_slice(row, 3, 11);
    assert!(
        !ts_cell.trim().is_empty(),
        "the row carries a time cell: {row}"
    );
    for (title, value) in [
        ("time", ts_cell.as_str()),
        ("area", "mux"),
        ("harness", "claude"),
        ("kind", "daemon_restarted"),
        ("node", "x-a1b2c"),
        ("session", "34567890"),
        ("lead", "team-alpha"),
    ] {
        assert_eq!(
            display_col(header, title),
            display_col(row, value),
            "column `{title}`: header and data sit at different x\nheader: {header}\nrow:    {row}"
        );
    }
    // A kind past the cap ellipsizes to exactly the cap and the node column
    // keeps its grid position (over is display row 0, lines[2]).
    let over_row = &lines[2];
    assert!(
        over_row.contains("prompt_parked_b\u{2026} "),
        "the over-cap kind ellipsizes at 16 columns: {over_row}"
    );
    assert_eq!(
        display_col(header, "node"),
        display_col(over_row, "x-b"),
        "node keeps its offset under an ellipsized kind\nheader: {header}\nrow:    {over_row}"
    );
}
#[tokio::test]
async fn a_created_row_without_node_offers_no_deep_link_or_blueprint_composer() {
    let v = view_with_rows(vec![]);
    let mut item = feed_item(None, None);
    item.kind = "node_created".into();
    let (_, actions, _) = feed_detail::build(&v.layout.agents, 0, &item);
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, feed_detail::FeedAction::Node(_))),
        "a node-less created row offers no node action"
    );

    let mut v = v;
    v.feed_detail = Some(feed_detail::modal(&v, item));
    let (mut writer, _reader) = tokio::io::duplex(4096);
    feed_view::feed_keys(&mut v, b"b", &mut writer)
        .await
        .unwrap();
    assert!(
        v.launcher.is_none(),
        "missing node id cannot prefill the composer"
    );
    assert!(v.feed_detail.is_some(), "an ineligible detail stays open");
}
#[test]
fn click_rows() {
    // Row 0 is the panel header, the last row is the footer; painted row 1 is
    // the group HEADER (no detail), row 2 the first item. The resolver reads
    // the same slot list the painter drew.
    // Distinct timestamps, so the slot order is total: newest first.
    let items: Vec<_> = (0..3)
        .map(|i| {
            let mut it = feed_item(Some("x-n"), Some("s-n"));
            it.title = format!("event {i}");
            it.ts = format!("2026-09-02T1{i}:27:06Z");
            it
        })
        .collect();
    let o = overlay(items);
    let g = &o.slots;
    assert_eq!(feed_row_item(g, 0, ROWS, 0), None, "panel header");
    assert_eq!(feed_row_item(g, ROWS - 1, ROWS, 0), None, "footer");
    assert_eq!(feed_row_item(g, 1, ROWS, 0), None, "column header");
    assert_eq!(
        feed_row_item(g, 2, ROWS, 0),
        None,
        "a group header never opens a detail"
    );
    assert_eq!(feed_row_item(g, 3, ROWS, 0), Some(2));
    assert_eq!(
        feed_row_item(g, 6, ROWS, 0),
        None,
        "past the last item: blank"
    );
    // Offset 2 scrolls the header and the newest row off: painted row 3 is
    // now the OLDEST row (slot 3 = storage 0).
    assert_eq!(feed_row_item(g, 3, ROWS, 2), Some(0), "offset applies");

    // The full client path: feed open, click in the panel's item rows at the
    // geometry the painter draws, hit = that row's provenance view. The deep
    // link moved to that view's own action; a click inspects, never attaches.
    // Real-width node ids: the give order at this width is read on the ids
    // the board actually mints, never on short stand-ins.
    let mut v = view_with_rows(vec![]);
    let mut a = feed_item(Some("x-aaaa"), Some("s-1"));
    a.ts = "2026-09-02T16:27:06Z".into();
    a.title = "PR 1391".into();
    let mut b = feed_item(Some("x-bbbb"), Some("s-2"));
    b.ts = "2026-09-02T17:27:06Z".into();
    b.title = "PR 1392".into();
    let mut c = feed_item(Some("x-cccc"), Some("s-3"));
    c.ts = "2026-09-02T18:27:06Z".into();
    c.title = "PR 1393".into();
    v.feed = Some(overlay(vec![a, b, c]));
    let w = v.feed_panel_w() as u16;
    assert!(w > 0, "the panel must render at the 100-col view");
    let col = v.term.1 - w + 2; // inside the panel text area
                                // The TOP item row displays the newest event (x-c) and clicking that
                                // same row deep-links THAT event's session: painter and resolver must
                                // name one event, never two.
    let f = v.feed.as_ref().unwrap();
    let lines = feed_panel_lines(f, false, w as usize - 1, v.term.0 as usize, 0);
    assert!(
        lines.get(3).is_some_and(|l| l.contains("PR 1393")),
        "top ITEM row is the newest: {lines:?}"
    );
    // At this 31-wide panel a 6-wide node needs 33 columns at kind 6 and
    // summary 10, so node goes last in the give order and the summary stays.
    assert!(
        !lines.iter().any(|l| l.contains("x-cccc"))
            && lines.get(1).is_some_and(|h| !h.contains("node")),
        "node drops at 31 columns with real-width ids: {lines:?}"
    );
    // The header rows never deep-link; the item row under them does.
    // The column header's marker gutter is inert; a title sorts by its
    // column; the group header folds its group. None of them deep-links.
    assert!(v.chrome_hit(1, col).is_none(), "the marker gutter is inert");
    let kind_x = lines
        .get(1)
        .and_then(|l| l.get(..l.find("kind")?))
        .map(unicode_width::UnicodeWidthStr::width)
        .expect("the column header names kind");
    let title_col = v.term.1 - w + 1 + kind_x as u16;
    assert!(
        matches!(
            v.chrome_hit(1, title_col),
            Some(ChromeHit::Feed(feed_view::FeedHit::Sort(
                feed_view::table::Col::Kind
            )))
        ),
        "a title click sorts by its column"
    );
    assert!(
        matches!(
            v.chrome_hit(2, col),
            Some(ChromeHit::Feed(feed_view::FeedHit::Fold(key))) if key == "lead or epic=other"
        ),
        "the group header folds its group"
    );
    feed_view::apply_hit(
        &mut v,
        feed_view::FeedHit::Sort(feed_view::table::Col::Kind),
    );
    assert_eq!(
        v.feed.as_ref().map(|f| f.view.sort),
        Some(feed_view::table::Col::Kind),
        "the click sorts through the view change the keys make"
    );
    feed_view::apply_hit(
        &mut v,
        feed_view::FeedHit::Sort(feed_view::table::Col::Time),
    );
    let hit = v.chrome_hit(3, col).unwrap();
    assert!(
        matches!(&hit, ChromeHit::OpenFeedDetail(item) if item.session_id.as_deref() == Some("s-3")),
        "the click names the event the top row painted"
    );
    // And THAT modal's session row offers the adopt command the session id
    // answers: the feed id is a session handle the registry lacks, so the
    // shared open-session action spells the adopt line, and `y` copies it.
    v.feed_detail = Some(feed_detail::modal(&v, feed_item(Some("x-c"), Some("s-3"))));
    let m = v.feed_detail.as_ref().unwrap();
    assert!(m.actions.iter().any(
        |a| matches!(a, feed_detail::FeedAction::Resume { line, name: None }
            if line == "fno agent registry adopt s-3")
    ));
    assert!(
        m.values.iter().any(|v| v == "fno agent registry adopt s-3"),
        "y copies the adopt command: {:?}",
        m.values
    );
    // Header and footer rows are chrome, not rows: they never deep-link.
    assert!(v.chrome_hit(0, col).is_none());
    assert!(v.chrome_hit((v.term.0 - 1) as u16, col).is_none());
}
// (AC4) The narrowed invariant, both halves. An unfocused panel takes no
// keys, so the header says how to focus and the marker does not move; a
// focused panel takes the arrows and says so.
#[tokio::test]
async fn header_rows() {
    let unfocused = overlay(vec![feed_item(Some("x-a"), Some("s-1"))]);
    let lines = feed_panel_lines(&unfocused, false, W, ROWS, 0);
    assert!(
        lines[0].contains("activity feed"),
        "unfocused header is a title: {}",
        lines[0]
    );
    assert!(
        lines.last().unwrap().contains("E focus"),
        "the focus hint lives on the footer: {}",
        lines.last().unwrap()
    );

    let focused = overlay(vec![feed_item(Some("x-a"), Some("s-1"))]);
    let lines = feed_panel_lines(&focused, true, W, ROWS, 0);
    assert!(lines[0].contains("FOCUSED"), "focused header: {}", lines[0]);
    assert!(
        lines.last().is_some_and(|l| l.contains("o sort")),
        "the focused footer names the table keys: {lines:?}"
    );
    assert!(
        !lines[0].contains("esc"),
        "no hint text sits on the header row: {}",
        lines[0]
    );

    // The focus key is advertised nowhere else, so it survives every width
    // the border can be dragged to rather than being clipped off the end.
    for w in 30..90usize {
        let o = overlay(vec![feed_item(Some("x-a"), Some("s-1"))]);
        let rows = feed_view::feed_panel_rows(&o, false, w, ROWS, 0);
        let footer = rows
            .last()
            .unwrap()
            .iter()
            .map(|s| s.text.clone())
            .collect::<String>();
        assert!(
            footer.contains("E focus"),
            "the focus key vanished from the footer at width {w}: {footer}"
        );
    }

    let mut v = view_with_rows(vec![]);
    v.feed = Some(overlay(vec![feed_item(Some("x-a"), Some("s-1"))]));
    v.region_owner = crate::client::region_focus::RegionOwner::Feed;
    // The cursor inversion rides keyboard ownership (AC3-HP/AC4-HP): the
    // driven row wears the theme's focus pair while the feed owns typing,
    // and it is gone the moment the keyboard returns to the pane.
    let frame = v.compose();
    let cols = frame.cols as usize;
    let feed_w = v.feed_panel_w() as usize;
    assert!(feed_w > 0, "the panel is painted with a width");
    let x0 = cols - feed_w;
    let sel = v.feed.as_ref().unwrap().sel;
    let band_cell = frame.cells[(2 + sel) * cols + x0 + 2];
    let (_, focus_bg, focus_flags) = crate::cursor::cursor_style(&v.theme);
    assert_eq!(
        (band_cell.bg, band_cell.flags & focus_flags),
        (focus_bg, focus_flags),
        "the driven row inverts while the feed owns the keyboard"
    );
    // The keyboard returns to the pane when the panel CLOSES, not through a
    // release that leaves it open: the release arm is gone.
    v.region_owner = crate::client::region_focus::RegionOwner::Pane;
    let frame = v.compose();
    let cols = frame.cols as usize;
    let x0 = cols - v.feed_panel_w() as usize;
    let band_cell = frame.cells[(2 + sel) * cols + x0 + 2];
    assert_eq!(
        band_cell.bg,
        Color::Default,
        "the band is gone once typing returns to the pane"
    );
    // AC13-HP: `?` opens the keys overlay; the frame carries the title and
    // the shared keys; Esc unwinds the overlay and the feed stays open.
    let mut v = view_with_rows(vec![]);
    v.feed = Some(overlay(vec![feed_item(Some("x-a"), Some("s-1"))]));
    v.region_owner = crate::client::region_focus::RegionOwner::Feed;
    let (mut writer, _reader) = tokio::io::duplex(4096);
    feed_view::feed_keys(&mut v, b"?", &mut writer)
        .await
        .unwrap();
    assert!(v.feed.as_ref().unwrap().keys_open, "? opened the overlay");
    let text = crate::vt::frame_text(&v.compose());
    assert!(text.contains("feed keys"), "{text}");
    assert!(text.contains("id:"), "{text}");
    assert!(text.contains("sid:"), "{text}");
    assert!(text.contains("k:"), "{text}");
    assert!(text.contains("h:"), "{text}");
    feed_view::feed_keys(&mut v, &[0x1b], &mut writer)
        .await
        .unwrap();
    feed_view::feed_keys(&mut v, &[], &mut writer)
        .await
        .unwrap();
    assert!(
        !v.feed.as_ref().unwrap().keys_open,
        "esc closed the overlay"
    );
    assert!(v.feed.is_some(), "the feed stayed open");

    // AC14-HP: `/` opens the bar; typing + Tab completes from the held rows;
    // a settled query re-slots them and reads nothing.
    let mut codex = feed_item(None, Some("s-codex"));
    codex.harness = Some("codex".into());
    codex.ts = "2026-09-28T16:00:00Z".into();
    let mut claude = feed_item(None, Some("s-claude"));
    claude.harness = Some("claude".into());
    claude.ts = "2026-09-28T17:00:00Z".into();
    v.feed = Some(overlay(vec![codex, claude]));
    feed_view::feed_keys(&mut v, b"/", &mut writer)
        .await
        .unwrap();
    assert!(v.feed.as_ref().unwrap().bar_open, "/ opened the bar");
    feed_view::feed_keys(&mut v, b"h:co", &mut writer)
        .await
        .unwrap();
    feed_view::feed_keys(&mut v, &[0x09], &mut writer)
        .await
        .unwrap();
    assert_eq!(
        v.feed.as_ref().unwrap().query_text,
        "h:codex",
        "Tab completed the value"
    );
    crate::client::feed_view::search::apply_query(v.feed.as_mut().unwrap());
    let f = v.feed.as_ref().unwrap();
    assert_eq!(f.matched, 1, "h:codex keeps the codex row alone");
    assert!(!f.want, "a query never reads: the panel holds every row");
    // AC14-EDGE: Esc in the bar clears the query, keeps every row again, and
    // the panel stays open.
    feed_view::feed_keys(&mut v, b"/", &mut writer)
        .await
        .unwrap();
    feed_view::feed_keys(&mut v, &[0x1b], &mut writer)
        .await
        .unwrap();
    feed_view::feed_keys(&mut v, &[], &mut writer)
        .await
        .unwrap();
    let f = v.feed.as_ref().unwrap();
    assert!(!f.bar_open, "esc closed the bar");
    assert!(f.query_text.is_empty(), "esc cleared the query");
    assert_eq!(f.matched, 2, "the cleared query keeps every row");
    assert!(v.feed.is_some(), "the panel stayed open");

    // AC10-EDGE (keys half): g on a scrolled-away window puts the newest
    // row on top and clears the marker, with no read.
    v.feed.as_mut().unwrap().win.new_count = 2;
    v.feed_offset = 1;
    feed_view::feed_keys(&mut v, b"g", &mut writer)
        .await
        .unwrap();
    let f = v.feed.as_ref().unwrap();
    assert_eq!(f.win.new_count, 0, "the marker is gone");
    assert_eq!(v.feed_offset, 0, "the newest row is on top");
    assert!(!f.want, "home reads nothing");

    // Esc and e close the focused panel outright; there is no key-less open
    // state left behind.
    v.region_owner = crate::client::region_focus::RegionOwner::Feed;
    feed_view::feed_keys(&mut v, b"e", &mut writer)
        .await
        .unwrap();
    assert!(v.feed.is_none(), "e closes the focused panel");
    assert_eq!(
        v.input_owner(),
        crate::client::region_focus::RegionOwner::Pane,
        "closing returns typing to the pane"
    );
    // A lone Esc through the same fold closes too: the fold carries a lone
    // ESC, and the quiet-window flush (the empty read) releases it.
    let mut v = view_with_rows(vec![]);
    v.feed = Some(overlay(vec![feed_item(Some("x-a"), Some("s-1"))]));
    v.region_owner = crate::client::region_focus::RegionOwner::Feed;
    feed_view::feed_keys(&mut v, &[0x1b], &mut writer)
        .await
        .unwrap();
    assert!(v.feed.is_some(), "the carried Esc has not fired yet");
    feed_view::feed_keys(&mut v, &[], &mut writer)
        .await
        .unwrap();
    assert!(v.feed.is_none(), "esc closes the focused panel");

    assert_eq!(feed_view::pan_by("abcdef", 0), "abcdef");
    assert_eq!(feed_view::pan_by("abcdef", 2), "cdef");
    // A two-column glyph straddling the cut is dropped whole, never halved.
    assert_eq!(feed_view::pan_by("漢字ab", 1), "字ab");
    assert_eq!(feed_view::pan_by("abc", 99), "");
    assert_eq!(
        feed_view::widest_title(&[feed_item(None, None), reaped_item("s", "r")]),
        "t-d145 removed by reap".len()
    );

    use crate::client::feed_view::header_line;
    let tv = feed_view::table::TableView::default();
    for w in 0..=80usize {
        let unfocused = header_line(false, &tv, 1, 1, w);
        assert!(
            unicode_width::UnicodeWidthStr::width(unfocused.as_str()) <= w || w < 6,
            "w={w} picked {unfocused:?}"
        );
        let focused = header_line(true, &tv, 1, 1, w);
        assert!(
            unicode_width::UnicodeWidthStr::width(focused.as_str()) <= w || w < 6,
            "w={w} picked {focused:?}"
        );
    }
    // The header is a title at every width; the keys live on the footer.
    assert_eq!(header_line(false, &tv, 1, 1, 8), " feed");
    assert_eq!(header_line(true, &tv, 1, 1, 8), " FOCUSED");
}

// Esc and e close the focused panel; a close returns typing to the pane,
// and the reopen starts unfocused.

// The pan moves the TITLE only, in display columns, and never splits a wide
// glyph: the stamp, kind and node stay anchored so a panned row is still the
// row that was selected.

// (AC5-HP) A removal reads as a normal outcome carrying its recovery line,
// never as a bare attach the server refuses. (x-1b90) Its pane field is a
// measurement question, not an applicability question: NOT RECORDED names
// that the removal did not measure the pane - whatever the recovery line
// says, since no removal record carries the measurement on a field yet.
#[test]
fn detail_field_rows() {
    let item = reaped_item(
        "00847995-e0db-47c2-ab5b-24468ba1a4f5",
        "resume: claude --resume x",
    );
    let (popup, actions, values) = feed_detail::build(&[], 0, &item);
    // A removal measures no pane: the row hides rather than printing a
    // NOT RECORDED stand-in for a measurement no record carries.
    let labels: Vec<String> = popup_rows(&popup)
        .iter()
        .filter_map(|(l, _)| l.clone())
        .collect();
    assert!(!labels.iter().any(|l| l == "pane"), "{labels:?}");
    assert!(
        !popup_lines(&popup)
            .iter()
            .any(|l| l.contains("NOT RECORDED")),
        "an absent field prints nothing: {labels:?}"
    );
    // The recovery line is its own row: with no worker name (the row is
    // gone from the roster), Enter hands the revival line over as a notice,
    // never an attach of a session that is gone.
    let i = actions
        .iter()
        .position(|a| {
            matches!(a, feed_detail::FeedAction::Resume { line, name: None }
                if line == "resume: claude --resume x")
        })
        .expect("the resume row exists");
    assert_eq!(values[i], "resume: claude --resume x");
    assert!(labels.iter().any(|l| l == "resume"), "{labels:?}");

    // A removal whose worker name the roster still holds resumes through
    // fno: the row reads the fno command (and `y` copies it), Enter sends
    // ResumeAgent for that name, and the receipt's revival line stays on
    // the card as an inert field. A name the roster lost keeps the revival
    // line as the row: the fno handle has nothing left to address.
    let mut named = reaped_item("sid-1", "resume: claude --resume sid-1");
    named.name = Some("t-d145".into());
    let holder = joined_row("t-d145", None, Some(0));
    let rows = [holder];
    let (popup, actions, values) = feed_detail::build(&rows, 0, &named);
    let i = actions
        .iter()
        .position(|a| {
            matches!(a, feed_detail::FeedAction::Resume { line, name: Some(n) }
                if line == "fno agent resume t-d145" && n == "t-d145")
        })
        .expect("the named resume row exists");
    assert_eq!(values[i], "fno agent resume t-d145");
    let revival_rows = popup_rows(&popup)
        .into_iter()
        .filter(|(l, _)| l.as_deref() == Some("revival"))
        .count();
    assert_eq!(revival_rows, 1, "the receipt line stays on the card");

    let (_popup, actions, values) = feed_detail::build(&[], 0, &named);
    assert!(
        actions.iter().any(|a| matches!(a,
            feed_detail::FeedAction::Resume { line, name: None }
                if line == "resume: claude --resume sid-1")),
        "a gone row reads its revival form"
    );
    assert!(
        values.iter().all(|v| v != "fno agent resume t-d145"),
        "no fno command when the row is gone: {values:?}"
    );

    let mut row = joined_row("some-other-name", None, Some(0));
    row.harness_session_id = Some("s-9".into());
    row.portal = Some(0);
    row.spawned_by_session = Some("s-parent".into());
    row.role_scope = Some("e-0001".into());
    row.role_level = Some(1);

    let item = feed_item(Some("x-a"), Some("s-9"));
    let rows = [row];
    let d = destination(&rows, &item);
    assert!(
        matches!(d, Destination::Exact(_)),
        "joined on the session id"
    );
    let (popup, _, _) = feed_detail::build(&rows, 0, &item);
    let by = |label: &str| -> String {
        popup_rows(&popup)
            .into_iter()
            .find(|(l, _)| l.as_deref() == Some(label))
            .map(|(_, v)| v)
            .unwrap_or_default()
    };
    assert_eq!(by("pane"), "pane 0 · portal 0");
    assert_eq!(by("parent"), "s-parent");
    assert_eq!(by("lead"), "L1 e-0001");

    // A row whose session id does not match is NOT this event's session,
    // however its name reads: parent and lead stay unrecorded, and the pane
    // says whose seat it actually is.
    let other = feed_item(Some("some-other-name"), Some("s-someone-else"));
    let other_dest = destination(&rows, &other);
    assert!(matches!(other_dest, Destination::NameOnly(_)));
    let (popup2, _, _) = feed_detail::build(&rows, 0, &other);
    let pane2 = popup_rows(&popup2)
        .into_iter()
        .find(|(l, _)| l.as_deref() == Some("pane"))
        .map(|(_, v)| v)
        .unwrap_or_default();
    // A name join reaches the node's CURRENT worker, and the pane says so;
    // the unjoined facts stay hidden rather than printed as silences.
    assert!(pane2.contains("the node's current worker"), "{pane2}");
    assert!(
        !popup_lines(&popup2)
            .iter()
            .any(|l| l.contains("NOT RECORDED")),
        "absent fields print nothing"
    );

    let mut bare = feed_item(Some("x-a"), Some("s-1"));
    bare.kind = "node_created".into();
    let (popup, _, _) = feed_detail::build(&[], 0, &bare);
    let lines = popup_lines(&popup);
    assert!(
        !lines.iter().any(|l| l.contains("NOT RECORDED")),
        "an absent field prints nothing: {lines:?}"
    );
    let labels: Vec<String> = popup_rows(&popup)
        .into_iter()
        .filter_map(|(l, _)| l)
        .collect();
    assert!(!labels.iter().any(|l| l == "model"), "{labels:?}");
    assert!(!labels.iter().any(|l| l == "parent"), "{labels:?}");
    assert!(!labels.iter().any(|l| l == "team"), "{labels:?}");

    // The birth stamps ride the row: model, effort, parent and team print
    // when the creating session's registry row carried them.
    let mut stamped = feed_item(Some("x-a"), Some("s-1"));
    stamped.kind = "node_created".into();
    stamped.model = Some("glm-5.3-flash".into());
    stamped.effort = Some("high".into());
    stamped.parent = Some("s-parent".into());
    stamped.team = Some("L2 e-0001".into());
    let (popup, _, _) = feed_detail::build(&[], 0, &stamped);
    let by = |label: &str| -> String {
        popup_rows(&popup)
            .into_iter()
            .find(|(l, _)| l.as_deref() == Some(label))
            .map(|(_, v)| v)
            .unwrap_or_default()
    };
    assert_eq!(by("model"), "glm-5.3-flash");
    assert_eq!(by("effort"), "high");
    assert_eq!(by("parent"), "s-parent");
    assert_eq!(by("team"), "L2 e-0001");

    let item = feed_item(Some("x-a"), Some("s-1"));
    let mut child = joined_row("jn-t-x-1", None, None);
    child.harness_session_id = Some("s-1".into());
    child.spawned_by_session = Some("s-lead".into());
    let parent_name = "t-x-lead";
    let by_parent = |row: &AgentRow| -> String {
        let (popup, _, _) = feed_detail::build(&[row.clone()], 0, &item);
        popup_rows(&popup)
            .into_iter()
            .find(|(l, _)| l.as_deref() == Some("parent"))
            .map(|(_, v)| v)
            .unwrap_or_default()
    };

    // Child edge resolving to a row: "<name> (<session>)".
    child.spawned_by_name = Some(parent_name.into());
    child.lineage_kind = Some("child".into());
    assert_eq!(by_parent(&child), "t-x-lead (s-lead)");

    // Peer edge: the same answer, plus the handoff word.
    child.lineage_kind = Some("peer".into());
    assert_eq!(by_parent(&child), "t-x-lead (s-lead) (handoff)");

    // Edge present, name absent (a session no row holds): the bare id, and
    // never a fabricated or borrowed name.
    child.spawned_by_name = None;
    child.lineage_kind = Some("child".into());
    assert_eq!(by_parent(&child), "s-lead");
    child.lineage_kind = Some("peer".into());
    assert_eq!(by_parent(&child), "s-lead (handoff)");

    // No edge: the birth's reason stands in for the parent it could not name.
    // The popup caps its width and ellipsizes, so a long reason may clip in
    // the render; the head of the real string must show.
    child.spawned_by_session = None;
    child.lineage_kind = None;
    child.lineage_reason = Some("daemon mint: spawn request carried no parent edge".into());
    let shown = by_parent(&child);
    assert!(
        shown.starts_with("daemon mint: spawn request carried no parent"),
        "the birth's reason stands in: {shown}"
    );

    // No edge and no reason: the field hides (item.parent rides in its
    // place when the birth stamped one).
    child.lineage_reason = None;
    let (popup, _, _) = feed_detail::build(&[child], 0, &item);
    let labels: Vec<String> = popup_rows(&popup)
        .into_iter()
        .filter_map(|(l, _)| l)
        .collect();
    assert!(!labels.iter().any(|l| l == "parent"), "{labels:?}");

    let mut team = feed_item(None, None);
    team.kind = "team_vacated".into();
    team.ts = "2026-09-28T16:45:58Z".into();
    team.title = "warden left L2 e: succession".into();
    let mut owned_a = feed_item(Some("x-a"), None);
    owned_a.owner = Some("epic x-29a8 the epic".into());
    owned_a.ts = "2026-09-28T17:00:00Z".into();
    let mut owned_b = feed_item(Some("x-b"), None);
    owned_b.owner = Some("epic x-29a8 the epic".into());
    owned_b.ts = "2026-09-28T17:30:00Z".into();
    let mut loose = feed_item(Some("x-c"), None);
    loose.ts = "2026-09-28T18:00:00Z".into();
    let items = vec![loose, owned_a, owned_b, team];
    let grouped = overlay(items.clone());
    let slots = &grouped.slots;
    // Slot shapes: teams header + the team row, the owner header with
    // its rows newest first, then the other header with the loose row.
    let shape: Vec<String> = slots
        .iter()
        .map(|s| match s {
            feed_view::Slot::Header { label, .. } => format!("H:{label}"),
            feed_view::Slot::Item(i) => format!("I:{}", items[*i].node.as_deref().unwrap_or("?")),
        })
        .collect();
    assert_eq!(
        shape,
        [
            "H:teams",
            "I:?",
            "H:epic x-29a8 the epic",
            "I:x-b",
            "I:x-a",
            "H:other",
            "I:x-c",
        ],
        "{shape:?}"
    );
    // A header row never resolves to a detail.
    assert_eq!(feed_row_item(slots, 2, ROWS, 0), None, "teams header");
    // The first item row IS the team row.
    assert_eq!(feed_row_item(slots, 3, ROWS, 0), Some(3));

    let mut vacated = feed_item(Some("x-a"), None);
    vacated.kind = "team_vacated".into();
    vacated.ts = "2026-09-28T16:45:58Z".into();
    vacated.title = "warden left".into();
    let o = overlay(vec![vacated]);
    let rows = feed_view::feed_panel_rows(&o, false, W, ROWS, 0);
    // Row 2 is the group header: every span bold.
    assert!(
        rows[2].iter().all(|s| s.bold),
        "headers render bold: {:?}",
        rows[2]
    );
    // Row 3 is the vacated row: its kind span is bold AND brand.
    assert!(
        rows[3]
            .iter()
            .any(|s| s.bold && s.brand && s.text.contains("team_vacated")),
        "the actionable kind is bold brand: {:?}",
        rows[3]
    );
    assert!(
        rows[3].iter().any(|s| s.bold && s.text.trim_end() == "x-a"),
        "the node id is bold: {:?}",
        rows[3]
    );
    let lines = feed_panel_lines(&o, false, W, ROWS, 0);
    assert!(
        lines[3].contains("team_vacated") && lines[3].contains("warden left"),
        "text is unchanged: {}",
        lines[3]
    );

    let mut item = feed_item(Some("x-a"), None);
    item.owner = Some("lead jolly-finch (lead-4d9b)".into());
    // Live holder: the line stands.
    let (popup, _, _) = feed_detail::build(&[joined_row("lead-4d9b", None, Some(1))], 0, &item);
    let owner = popup_rows(&popup)
        .into_iter()
        .find(|(l, _)| l.as_deref() == Some("owner"))
        .map(|(_, v)| v)
        .unwrap();
    assert_eq!(owner, "lead jolly-finch (lead-4d9b)");
    // Dead holder: the modal says so instead of naming a current lead that
    // is not there.
    let (popup, _, _) = feed_detail::build(&[], 0, &item);
    let owner = popup_rows(&popup)
        .into_iter()
        .find(|(l, _)| l.as_deref() == Some("owner"))
        .map(|(_, v)| v)
        .unwrap();
    assert_eq!(owner, "lead jolly-finch (lead-4d9b) · gone");
    // No holder to resolve: untouched.
    let mut plain = feed_item(Some("x-a"), None);
    plain.owner = Some("epic x-29a8 the epic".into());
    let (popup, _, _) = feed_detail::build(&[], 0, &plain);
    let owner = popup_rows(&popup)
        .into_iter()
        .find(|(l, _)| l.as_deref() == Some("owner"))
        .map(|(_, v)| v)
        .unwrap();
    assert_eq!(owner, "epic x-29a8 the epic");

    let tz = chrono::FixedOffset::east_opt(-7 * 3600).unwrap();
    // An older row carries its date when the panel is wide enough; a
    // today row and a narrow panel read time only.
    assert_eq!(
        feed_view::short_ts_in("2026-09-28T16:48:49Z", &tz, true),
        "09-28 09:48"
    );
    assert_eq!(
        feed_view::short_ts_in("2026-09-28T16:48:49Z", &tz, false),
        "09:48"
    );
    let now = chrono::Utc::now()
        .with_timezone(&tz)
        .format("%H:%M")
        .to_string();
    assert_eq!(
        feed_view::short_ts_in(&chrono::Utc::now().to_rfc3339(), &tz, true),
        now
    );
    assert_eq!(
        feed_view::short_ts_in("not-a-time", &tz, true),
        "not-a-time",
        "unparseable stamps show raw"
    );
    // A day boundary marks its first row at any width: the top row of an
    // older day, the first row of each new day, and the first row under a
    // header. Rows of the same day below stay time-only.
    let marks = feed_view::day_marks_in(
        &[
            Some("2026-09-28T16:48:49Z"),
            Some("2026-09-28T09:00:00Z"),
            Some("2026-09-27T23:00:00Z"),
            None,
            Some("2026-09-27T22:00:00Z"),
            Some("not-a-time"),
        ],
        &tz,
    );
    assert_eq!(marks, [true, false, true, false, true, false]);

    let mut a = feed_item(Some("x-a"), None);
    a.ts = "2026-09-28T17:00:00Z".into();
    let mut b = feed_item(Some("x-b"), None);
    b.ts = "2026-09-28T18:00:00Z".into();
    let items = vec![a, b];
    // Grouped: the unowned rows sit under the `other` header.
    let grouped = overlay(items.clone()).slots;
    assert!(matches!(
        grouped.first(),
        Some(feed_view::Slot::Header { .. })
    ));
    // Ungrouped: no headers at all, newest first.
    let recent = overlay_in(items.clone(), Group::None).slots;
    assert!(recent.iter().all(|s| matches!(s, feed_view::Slot::Item(_))));
    let first = match &recent[0] {
        feed_view::Slot::Item(i) => items[*i].node.clone().unwrap(),
        _ => unreachable!(),
    };
    assert_eq!(first, "x-b", "newest first");
    // The resolver answers the same item the painter drew, ungrouped too.
    // Painted row 2 is the first item row (rows 0 and 1 are the header and
    // the column header).
    assert_eq!(
        feed_row_item(&recent, 2, ROWS, 0),
        Some(1),
        "painted row 2 is storage 1 (the newest)"
    );

    let mut v = view_with_rows(vec![]);
    let mut q = feed_item(None, None);
    q.kind = "question_asked".into();
    q.r#ref = Some("q-1".into());
    q.ts = "2026-09-28T19:00:00Z".into();
    let mut older = feed_item(Some("x-a"), Some("s-1"));
    older.ts = "2026-09-28T18:00:00Z".into();
    v.feed = Some(overlay(vec![q, older]));
    let w = v.feed_panel_w() as u16;
    let col = v.term.1 - w + 2;
    let hit = v.chrome_hit(3, col).expect("the question row deep-links");
    assert!(
        matches!(&hit, ChromeHit::OpenQuestionDetail(id) if id == "q-1"),
        "the question row opens the question, not the provenance: {hit:?}"
    );
}

/// The popup's rendered body lines, for label-level assertions. The wide
/// viewport keeps long values whole: the real modal clips them, `y` copies
/// whole, and these tests read the field content, not the clip.
fn popup_lines(popup: &crate::popup::Popup) -> Vec<String> {
    popup
        .render((80, 200))
        .lines
        .iter()
        .map(|l| l.text.clone())
        .collect()
}

/// The popup's rendered (label, value) field pairs: Entry rows key on the
/// glyph column, Info rows carry label and value. Framed lines lead with
/// the border cell, so the border strips before the label splits off.
fn popup_rows(popup: &crate::popup::Popup) -> Vec<(Option<String>, String)> {
    let mut rows = Vec::new();
    for line in popup.render((80, 200)).lines {
        let framed = line.text.trim();
        let mut body = framed
            .strip_prefix('\u{2502}')
            .or_else(|| framed.strip_prefix('\u{250c}'))
            .or_else(|| framed.strip_prefix('\u{2570}'))
            .unwrap_or(framed)
            .to_string();
        if let Some(stripped) = body.strip_suffix('\u{2502}') {
            body = stripped.to_string();
        }
        let body = body.trim();
        if body.is_empty() {
            continue;
        }
        let mut parts = body.splitn(2, char::is_whitespace);
        let head = parts.next().unwrap_or("").to_string();
        let tail = parts.next().unwrap_or("").trim().to_string();
        rows.push((Some(head), tail));
    }
    rows
}

// (AC5-EDGE) Pane ids allocate from zero, so pane 0 is a real seat. The join
// is on the exact session id, never the row name a later worker can reuse.

// The panel drags narrower than any prose fits, and the caller clips from the
// end. The key must survive that clip: it is the only place it is advertised.

// A node_created modal hides what the source lacks and prints what the
// birth stamped: no NOT RECORDED filler anywhere, ever.

// The whole render path, not just the line builder: open the provenance view
// on a real View and read the COMPOSED frame. A field that never reaches the
// screen is the defect this view exists to prevent, so the assertion is on
// painted text.
#[tokio::test]
async fn the_composed_frame_paints_every_field_and_opens_the_blueprint_composer() {
    let mut v = view_with_rows(vec![]);
    v.term = (44, 120);
    v.feed = Some(overlay(vec![feed_item(Some("x-a"), Some("s-1"))]));
    let mut item = feed_item(Some("x-9223"), Some("s-1"));
    v.feed_detail = Some(feed_detail::modal(&v, item.clone()));
    let ordinary_text = crate::vt::frame_text(&v.compose());
    assert!(
        !ordinary_text.contains("b: blueprint"),
        "non-created rows have no blueprint action"
    );
    item.kind = "node_created".into();
    item.cwd = Some("/workspace/node-project".into());
    item.harness = Some("claude".into());
    item.model = Some("glm-5.3-flash".into());
    v.feed_detail = Some(feed_detail::modal(&v, item));

    let text = crate::vt::frame_text(&v.compose());
    for label in [
        "harness",
        "timestamp",
        "model",
        "node",
        "session-id",
        "pane",
    ] {
        assert!(text.contains(label), "the frame never painted {label}");
    }
    // The values that ARE recorded print; the ones that are not hide.
    assert!(text.contains("glm-5.3-flash"));
    assert!(text.contains("x-9223"));
    assert!(
        !text.contains("NOT RECORDED"),
        "an absent field paints nothing"
    );
    // The footer names the gestures before they are pressed, including the
    // created-node composer key.
    assert!(text.contains("enter open"), "footer missing");
    assert!(text.contains("y copy"), "copy affordance missing");
    assert!(text.contains("b blueprint"), "blueprint key missing");

    let (mut writer, _reader) = tokio::io::duplex(4096);
    feed_view::feed_keys(&mut v, b"b", &mut writer)
        .await
        .unwrap();
    let launch = v.launcher.as_ref().expect("blueprint key opens composer");
    assert_eq!(launch.draft.message, "/fno:blueprint x-9223");
    assert_eq!(launch.draft.node.as_deref(), Some("x-9223"));
    assert_eq!(
        launch
            .draft
            .projects
            .get(launch.draft.project_idx)
            .map(String::as_str),
        Some("/workspace/node-project")
    );
    assert!(v.feed_detail.is_none(), "composer replaces feed detail");
}

// (x-9cbf) The parent field spends the derived NAME when the edge resolves
// to a row in the set, keeps the handoff word on a PEER edge, shows the bare
// session id when the edge names no row, and falls back to the birth's own
// reason (or the honest silence) when the row has no edge at all.

// (AC7-HP) The teams band leads, then one header per owner ordered by
// its newest row, then `other`; a header click resolves to no detail.

// (AC8-HP) The kind span goes bold in the brand colour for kinds that
// need action; the flattened text is what the panel always drew.

// (AC9-HP) Local time: a UTC stamp renders in the zone the test pins.

// (AC10-EDGE) A settled fold goes stale and refolds on its own; the
// operator's selected row survives the refresh.
#[tokio::test]
async fn a_stale_fold_refolds_and_keeps_the_selection() {
    let mut v = view_with_rows(vec![]);
    let mut it = feed_item(Some("x-a"), Some("s-1"));
    it.ts = "2026-09-28T16:00:00Z".into();
    it.cursor = "[\"a\"]".into();
    let mut other = feed_item(Some("x-b"), Some("s-2"));
    other.ts = "2026-09-28T17:00:00Z".into();
    other.cursor = "[\"b\"]".into();
    v.feed = Some(overlay(vec![it.clone(), other.clone()]));
    v.feed.as_mut().unwrap().last_fold =
        Some(Instant::now() - feed_view::FEED_REFRESH_EVERY - std::time::Duration::from_secs(1));
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    feed_view::maybe_kick(&mut v, &tx);
    assert!(
        v.feed.as_ref().unwrap().inflight,
        "a stale fold arms the single-flight"
    );
    // Selection kept across a refresh: select the OLDER row (slot 2),
    // refresh, and the same (ts, kind, title) stays selected.
    v.feed.as_mut().unwrap().sel = 2;
    let gen = v.feed.as_ref().unwrap().gen;
    feed_view::apply_fold(
        &mut v,
        gen,
        Ok(crate::feed_overlay::Land::Rows(vec![it, other])),
    );
    let f = v.feed.as_ref().unwrap();
    assert_eq!(f.sel, 2, "the selected row survived the fold");
    assert!(f.last_fold.is_some(), "the fold stamped its time");
    let _ = rx;

    // AC10-HP: a fold that brings newer rows while the reader sits back keeps
    // the top visible row on top, and the marker counts the arrivals.
    let history = |n: usize| -> Vec<crate::feed_overlay::FeedItem> {
        (0..n)
            .map(|i| {
                let mut it = feed_item(Some("x-p"), Some("s-1"));
                it.ts = format!("2026-08-01T00:{i:02}:00Z");
                it.cursor = format!("c{i}");
                it.title = format!("row {i}");
                it
            })
            .collect()
    };
    let mut v = view_with_rows(vec![]);
    v.feed = Some(overlay_in(vec![], Group::None));
    feed_view::apply_fold(&mut v, 0, Ok(crate::feed_overlay::Land::Rows(history(40))));
    v.feed_offset = 5;
    let top_before = {
        let f = v.feed.as_ref().unwrap();
        match f.slots.get(v.feed_offset_clamped()) {
            Some(feed_view::Slot::Item(i)) => f.win.items.get(*i).map(|it| it.title.clone()),
            _ => None,
        }
        .unwrap_or_default()
    };
    assert_eq!(top_before, "row 34", "newest first: slot 5 is row 34");
    feed_view::apply_fold(&mut v, 0, Ok(crate::feed_overlay::Land::Rows(history(43))));
    let f = v.feed.as_ref().unwrap();
    assert_eq!(f.win.items.len(), 43, "the fold replaced the rows");
    assert_eq!(f.win.new_count, 3, "three rows arrived above the reader");
    let lines = feed_panel_lines(f, false, W, ROWS, v.feed_offset_clamped());
    assert!(
        lines[2].contains(&top_before),
        "the anchored row stayed on top: {lines:?}"
    );
}

// (x-182e) The order toggle: `Recent` is one flat newest-first list with no
// headers; `Grouped` stays the shipped shape. The click resolver inverts the
// painter in BOTH orders, so a row and its detail can never disagree.

// (x-182e) A question row answers from the feed: the click opens the whole
// question on the questions view's own path, never a provenance detour.

// (x-182e) The owner line resolves its holder against the live roster: a
// dead team's handle says it is gone, a live one keeps the line, and a
// line with no parenthesized holder passes through untouched.

/// The shipped panel's reach: the last slot paints above the footer under
/// the cursor (AC1), no cursor move at either edge reads a page, since the
/// panel holds every row (AC2), a hover off the
/// rows leaves the cursor on its row (AC24), and a close then reopen in one
/// client paints the same rows and query at once (AC23).
#[tokio::test]
async fn last_row_paging_edge_hover_and_reopen() {
    let items: Vec<_> = (0..40)
        .map(|i| {
            let mut it = feed_item(Some("x-n"), Some("s-n"));
            it.title = format!("event {i:02}");
            it.ts = format!("2026-09-02T10:{i:02}:00Z");
            it.cursor = format!("c{i:02}");
            it
        })
        .collect();
    let mut v = view_with_rows(vec![]);
    v.feed = Some(overlay_in(items, Group::None));
    v.region_owner = crate::client::region_focus::RegionOwner::Feed;
    let (mut writer, _reader) = tokio::io::duplex(4096);
    let rows = v.term.0 as usize;

    feed_view::feed_keys(&mut v, b"\x1b[A", &mut writer)
        .await
        .unwrap();
    assert!(
        !v.feed.as_ref().unwrap().want,
        "up at the newest row reads nothing"
    );

    feed_view::feed_keys(&mut v, b"G", &mut writer)
        .await
        .unwrap();
    let off = v.feed_offset_clamped();
    let f = v.feed.as_ref().unwrap();
    let lines = feed_panel_lines(f, true, W, rows, off);
    let last_slot = lines.get(rows - 2).cloned().unwrap_or_default();
    assert!(
        last_slot.contains("event 00"),
        "the oldest row paints on the last slot: {last_slot}"
    );
    assert_eq!(
        2 + f.sel - off,
        rows - 2,
        "the cursor sits on it, not the footer"
    );
    assert!(!f.want, "the oldest row is held: no read past it");

    v.region_owner = crate::client::region_focus::RegionOwner::Pane;
    let col = v.term.1 - 2;
    v.hover_feed_marker(0, col);
    assert_eq!(
        v.feed.as_ref().unwrap().sel,
        39,
        "a hover on the title row keeps the cursor on its row"
    );

    v.feed.as_mut().unwrap().query_text = "k:node".into();
    feed_view::toggle(&mut v, &mut writer).await.unwrap();
    assert!(v.feed.is_none(), "the first toggle closes");
    feed_view::toggle(&mut v, &mut writer).await.unwrap();
    let f = v.feed.as_ref().unwrap();
    assert_eq!(f.win.items.len(), 40, "the parked rows paint at once");
    assert_eq!(f.query_text, "k:node", "the query survives the close");
    assert_eq!(f.gen, 1, "a fold from before the close cannot land");
}

/// The live panel's reader answers: rows land in the bounded cache, a
/// slots answer older than the newest slots request drops, and the newest
/// lands its slots and the reader's row count.
#[test]
fn reader_answers_cache_rows_and_drop_stale_slots() {
    let answer = |id: u64, slots: Vec<feed_view::Slot>| {
        let mut it = feed_item(Some("x-aaaa"), Some("s-1"));
        it.title = "PR 1391".into();
        fno_proto::feed::serve::Answer {
            id,
            stamp: 1,
            total: 1,
            matched: slots.len(),
            slots: Some(slots),
            items: vec![(0, it)],
            ..Default::default()
        }
    };
    let mut v = view_with_rows(vec![]);
    v.feed = Some(overlay(vec![]));
    v.feed.as_mut().unwrap().slots_req = 5;
    let stale = answer(4, vec![feed_view::Slot::Item(0)]);
    feed_view::apply_fold(
        &mut v,
        0,
        Ok(crate::feed_overlay::Land::Answer(Box::new(stale))),
    );
    let f = v.feed.as_ref().unwrap();
    assert!(
        f.slots.is_empty(),
        "a stale slots answer drops: {:?}",
        f.slots
    );
    assert_eq!(
        f.item(0).map(|it| it.title.as_str()),
        Some("PR 1391"),
        "its rows still cache"
    );
    let newest = answer(5, vec![feed_view::Slot::Item(0)]);
    feed_view::apply_fold(
        &mut v,
        0,
        Ok(crate::feed_overlay::Land::Answer(Box::new(newest))),
    );
    let f = v.feed.as_ref().unwrap();
    assert_eq!(
        f.slots,
        vec![feed_view::Slot::Item(0)],
        "the newest slots land"
    );
    assert_eq!(f.held(), 1, "the panel counts the reader's rows");
    assert!(f.win.items.is_empty(), "the client holds no history");
}
