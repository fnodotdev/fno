//! The activity feed panel : questions, decisions and node
//! lifecycle, newest first, one deep link per row. The render moved-module
//! idiom matches `needs_view.rs`; the data comes from
//! [`crate::feed_overlay::fetch_page`], which shells the `fno agent fleet feed`
//! projection off the UI loop and renders a typed reason when it fails
//!, never one generic sentence for five causes.
//!
//! The deep link is the sideline's own path, not a new one: a row that joins
//! a live roster row resolves through `agent_hit` exactly as a sideline click
//! does (FocusPane, or AttachAgent on portal 0); an unjoined row carrying a
//! session id attaches that id directly, and the server's existing
//! `no such agent` notice is what a dead session answers.
//!
//! Since the feed is the right-edge PANEL the operator specified, not
//! a centered modal: `e` toggles it, the border drags, and an UNFOCUSED panel
//! consumes no keys at all, so typing reaches the focused pane.
//!
//! That key-free rule is the default, not the whole story. `E` focuses the
//! panel explicitly: while focused it takes the arrows (row select and
//! horizontal pan), Enter, and Esc and e, which close the panel - there is
//! no release that leaves it open and key-less, a state no key could leave.
//! The property the rule protects - typing reaches the pane - holds whenever
//! the operator has not asked for the opposite.
//!
//! A click no longer fires the deep link straight off. It opens the row's
//! PROVENANCE (`feed_detail`), and the deep link is that view's footer
//! action. Inspecting never attaches or resumes anything on its own.

use super::*;
use crate::feed_overlay::{event_fields, FeedItem};

pub(crate) mod page;
pub(crate) mod picker;
pub(crate) mod reader;
pub(crate) mod search;
pub(crate) mod table;
pub(crate) use table::display_kind;

pub(crate) use table::Slot;

/// The panel's open state: the whole history the last fold landed, the one
/// table view over it and the slots that view yields, the cursor (a slot
/// index), and the same generation/single-flight discipline the needs fold
/// runs. `want` arms the run-loop kick; `gen` invalidates a result that
/// lands after a close or a re-open. `sel` follows the pointer while the
/// panel is UNFOCUSED, and the keys while it is focused - the panel consumes
/// keys only then.
pub(crate) struct FeedOverlay {
    pub(crate) win: page::FeedWindow,
    /// The cursor, as an index into `slots`; the cursor fill paints here.
    pub(crate) sel: usize,
    /// The typed fold failure, if the last fold failed. `Some` renders its
    /// reason verbatim; a success fold clears it and the last good rows stay.
    pub(crate) error: Option<crate::feed_overlay::FeedError>,
    pub(crate) inflight: bool,
    pub(crate) want: bool,
    pub(crate) gen: u64,
    /// Horizontal pan into the TITLE, in display columns. The timestamp, kind
    /// and node stay anchored so a panned row is still identifiable.
    pub(crate) hpan: usize,
    /// When the last fold LANDED. Arms the 15s auto-refresh; `None` until the
    /// first fold lands, and the first landing resets to the newest row.
    pub(crate) last_fold: Option<Instant>,
    /// Sort, column filters, group and collapsed groups.
    pub(crate) view: table::TableView,
    /// What `view` yields over `win.items` and the query: the ONE slot list
    /// paint, hit test, hover and keys walk. Rebuilt by [`reslot`] only.
    pub(crate) slots: Vec<Slot>,
    /// Rows that pass the query and every column filter.
    pub(crate) matched: usize,
    /// The widest value of each capped column over the held rows.
    pub(crate) widths: ValueWidths,
    /// The open sort or group picker.
    pub(crate) picker: Option<picker::FeedPicker>,
    /// The active query text; empty means unfiltered.
    pub(crate) query_text: String,
    /// The parsed query (or its refusal, for the footer). `None` when the
    /// bar is empty.
    pub(crate) parsed: Option<Result<crate::search_query::Parsed, String>>,
    /// The `?` keys overlay.
    pub(crate) keys_open: bool,
    /// The search bar is open (its text lives in `query_text`).
    pub(crate) bar_open: bool,
    /// The debounce instant while the bar has text.
    pub(crate) bar_debounce: Option<Instant>,
    /// The held reader, while the live panel is open. `None` in a shot or a
    /// test, which hold their rows in `win` and evaluate in process.
    pub(crate) reader: Option<reader::Reader>,
    /// The reader's rows near the window, by storage index. Bounded by
    /// [`CACHE_SPAN`] slots each side of the top row.
    pub(crate) cache: std::collections::HashMap<usize, FeedItem>,
    /// The reader's projection stamp the cached rows came from.
    pub(crate) stamp: u64,
    /// Every row the reader holds.
    pub(crate) total: usize,
    /// The widest summary the reader holds: the pan ceiling.
    pub(crate) title_w: usize,
    /// The id of the newest slots request; an older slots answer drops.
    pub(crate) slots_req: u64,
    /// A row fetch is out to the reader.
    pub(crate) fetch_inflight: bool,
    /// The top slot and the body rows, as the last run-loop tick saw them.
    pub(crate) window: (usize, usize),
    /// A value picker waiting on the reader's value list.
    pub(crate) pending_values: Option<(table::Col, crate::popup::Anchor)>,
}

/// The slots each side of the top row whose rows the panel keeps: the
/// 600-row window of the paged panel, so the client's memory stays flat.
const CACHE_SPAN: usize = 600;

/// Rows fetched beyond each edge of the window, so a short scroll paints
/// from the cache.
const FETCH_MARGIN: usize = 60;

impl FeedOverlay {
    /// The row at one storage index: the held rows in a shot or a test,
    /// the reader's cache in the live panel.
    pub(crate) fn item(&self, i: usize) -> Option<&FeedItem> {
        self.win.items.get(i).or_else(|| self.cache.get(&i))
    }

    /// Every row the panel's source holds.
    pub(crate) fn held(&self) -> usize {
        if self.win.items.is_empty() {
            self.total
        } else {
            self.win.items.len()
        }
    }

    /// The pan ceiling's widest summary.
    pub(crate) fn widest_title(&self) -> usize {
        if self.win.items.is_empty() {
            self.title_w
        } else {
            widest_title(&self.win.items)
        }
    }
}

/// A closing panel parks without its reader: the reader exits, and a
/// reopen starts a fresh one.
pub(crate) fn park(mut f: FeedOverlay) -> FeedOverlay {
    f.reader = None;
    f.inflight = false;
    f.fetch_inflight = false;
    f.pending_values = None;
    f
}

/// The view a fresh client opens with: the saved one, else the default.
/// With no saved view the old two-state `feed_order` preference reads once
/// as its group (`recent` was the flat list).
fn initial_view() -> table::TableView {
    if let Some(pref) = crate::view_store::load_feed_view() {
        return table::from_pref(&pref);
    }
    let mut view = table::TableView::default();
    if crate::view_store::load_feed_order().as_deref() == Some("recent") {
        view.group = table::Group::None;
    }
    view
}

/// A fresh open: the prior rows, query and view ride over (instant
/// content), but a refold is always armed - history may have moved since
/// the last open, and the fold runs off the UI loop.
pub(crate) fn open_overlay(prior: Option<FeedOverlay>, gen: u64) -> FeedOverlay {
    let (win, query_text, parsed, view, carried) = match prior {
        Some(f) => {
            let carried = (f.win.items.is_empty() && f.total > 0).then(|| Carried {
                slots: f.slots,
                cache: f.cache,
                stamp: f.stamp,
                total: f.total,
                title_w: f.title_w,
                matched: f.matched,
                widths: f.widths,
            });
            (f.win, f.query_text, f.parsed, f.view, carried)
        }
        None => (
            page::FeedWindow::default(),
            String::new(),
            None,
            initial_view(),
            None,
        ),
    };
    let mut f = FeedOverlay {
        win,
        sel: 0,
        error: None,
        inflight: false,
        want: true,
        gen,
        hpan: 0,
        last_fold: None,
        view,
        slots: Vec::new(),
        matched: 0,
        widths: ValueWidths::default(),
        picker: None,
        query_text,
        parsed,
        keys_open: false,
        bar_open: false,
        bar_debounce: None,
        reader: None,
        cache: std::collections::HashMap::new(),
        stamp: 0,
        total: 0,
        title_w: 0,
        slots_req: 0,
        fetch_inflight: false,
        window: (0, 0),
        pending_values: None,
    };
    if let Some(prior) = carried {
        // The reader's last answer paints at once while a fresh reader
        // starts; its slots and rows ride over whole.
        f.slots = prior.slots;
        f.cache = prior.cache;
        f.stamp = prior.stamp;
        f.total = prior.total;
        f.title_w = prior.title_w;
        f.matched = prior.matched;
        f.widths = prior.widths;
    } else {
        reslot(&mut f);
    }
    f.sel = first_item_slot(&f.slots);
    f
}

/// What a reader-fed panel carries over a close and reopen.
struct Carried {
    slots: Vec<Slot>,
    cache: std::collections::HashMap<usize, FeedItem>,
    stamp: u64,
    total: usize,
    title_w: usize,
    matched: usize,
    widths: ValueWidths,
}

/// Rebuild the slot list from the rows, the query and the view. Every
/// change to any of the three calls this, and nothing else writes `slots`.
pub(crate) fn reslot(f: &mut FeedOverlay) {
    if f.reader.is_some() {
        request_slots(f);
        return;
    }
    let started = Instant::now();
    let ctx = crate::feed_overlay::EventCtx::default();
    let query = match &f.parsed {
        Some(Ok(parsed)) => Some(parsed),
        _ => None,
    };
    let (slots, matched) = table::evaluate(&f.win.items, &f.view, |it| {
        query.is_none_or(|q| q.keeps(&event_fields(it, &ctx)))
    });
    f.slots = slots;
    f.matched = matched;
    f.widths = ValueWidths::of(&f.win.items);
    f.sel = f.sel.min(f.slots.len().saturating_sub(1));
    // A re-slot runs on the UI thread over every held row, so its cost is
    // the view-change budget; client.log keeps one line per call.
    crate::mux_log::client(
        "info",
        &format!(
            "feed reslot rows={} slots={} ms={}",
            f.win.items.len(),
            f.slots.len(),
            started.elapsed().as_millis()
        ),
    );
}

/// Ask the reader for the view's slots and the window's rows. The query
/// travels only when it parsed; a refused one evaluates the view alone, as
/// the in-process path does.
fn request_slots(f: &mut FeedOverlay) {
    let query = match &f.parsed {
        Some(Ok(_)) => f.query_text.clone(),
        _ => String::new(),
    };
    let (top, rows) = f.window;
    let req = fno_proto::feed::serve::Request {
        view: fno_proto::feed::serve::ViewSpec::of(&f.view),
        query,
        slots: true,
        top: top.saturating_sub(FETCH_MARGIN),
        rows: rows + 2 * FETCH_MARGIN,
        ..Default::default()
    };
    match f.reader.as_mut().and_then(|r| r.send(req)) {
        Some(id) => {
            f.slots_req = id;
            f.inflight = true;
        }
        None => f.reader = None,
    }
}

/// Ask the reader for the window's rows the cache lacks.
fn fetch_missing(f: &mut FeedOverlay) {
    if f.fetch_inflight || f.reader.is_none() {
        return;
    }
    let (top, rows) = f.window;
    let want: Vec<usize> = f
        .slots
        .iter()
        .skip(top.saturating_sub(FETCH_MARGIN))
        .take(rows + 2 * FETCH_MARGIN)
        .filter_map(|s| match s {
            Slot::Item(i) if !f.cache.contains_key(i) => Some(*i),
            _ => None,
        })
        .collect();
    if want.is_empty() {
        return;
    }
    let req = fno_proto::feed::serve::Request {
        view: fno_proto::feed::serve::ViewSpec::of(&f.view),
        fetch: want,
        ..Default::default()
    };
    match f.reader.as_mut().and_then(|r| r.send(req)) {
        Some(_) => f.fetch_inflight = true,
        None => f.reader = None,
    }
}

/// Drop cached rows whose slots sit more than [`CACHE_SPAN`] from the top.
fn evict(f: &mut FeedOverlay) {
    if f.cache.len() <= 2 * CACHE_SPAN {
        return;
    }
    let top = f.window.0;
    let keep: std::collections::HashSet<usize> = f
        .slots
        .iter()
        .skip(top.saturating_sub(CACHE_SPAN))
        .take(2 * CACHE_SPAN)
        .filter_map(|s| match s {
            Slot::Item(i) => Some(*i),
            Slot::Header { .. } => None,
        })
        .collect();
    f.cache.retain(|i, _| keep.contains(i));
}

/// The narrowest the summary column gets before node and the other
/// optional columns drop.
const SUMMARY_MIN: usize = 12;

/// The summary floor where taking it is the only way to keep node.
const SUMMARY_TIGHT: usize = 10;

/// The narrowest the kind column gets; a longer kind ellipsizes.
const KIND_MIN: usize = 6;

/// The widest value, in display columns, of each column that has a cap,
/// already capped: area and harness 8, kind 16, node 12, session tail 8,
/// lead 12. One pass over the rows per re-slot, so a paint reads them free.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct ValueWidths {
    pub area: usize,
    pub harness: usize,
    pub kind: usize,
    pub node: usize,
    pub session: usize,
    pub lead: usize,
}

impl ValueWidths {
    fn of(items: &[FeedItem]) -> ValueWidths {
        let dw = |s: &str| unicode_width::UnicodeWidthStr::width(s);
        let mut w = ValueWidths::default();
        for it in items {
            w.area = w.area.max(dw(&it.area));
            w.harness = w.harness.max(it.harness.as_deref().map_or(0, dw));
            w.kind = w.kind.max(dw(display_kind(&it.kind)));
            w.node = w.node.max(it.node.as_deref().map_or(0, dw));
            w.session = w
                .session
                .max(it.session_id.as_deref().map_or(0, |s| dw(&session_tail(s))));
            w.lead = w.lead.max(it.lead.as_deref().map_or(0, dw));
        }
        ValueWidths {
            area: w.area.min(8),
            harness: w.harness.min(8),
            kind: w.kind.min(16),
            node: w.node.min(12),
            session: w.session.min(8),
            lead: w.lead.min(12),
        }
    }
}

/// True for kinds whose event NEEDS ACTION, so the kind renders bold in the
/// theme's accent: a question waiting, a team leaving, a teamed session
/// removed.
fn bold_kind(item: &FeedItem) -> bool {
    item.kind == "question_asked"
        || item.kind == "team_vacated"
        || (item.kind == "session_reaped" && item.team.is_some())
}

/// The panel body: one header line, up to `visible_rows - 2` item rows, then
/// the footer pinned to the last row. The projection hands rows OLDEST
/// FIRST, so display index `d` reads storage index `len - 1 - d` and the top
/// row is the newest event - the same view `feed_row_item` inverts for the
/// click resolver, so a row and its deep link always name the same event.
pub(crate) fn feed_panel_rows(
    o: &FeedOverlay,
    focused: bool,
    w: usize,
    visible_rows: usize,
    offset: usize,
) -> Vec<Vec<Span>> {
    // Columns, in the operator's order: time, area, harness, kind, node,
    // session (tail 8), lead, summary. The narrowest panel keeps time, kind
    // and summary; a wider one adds node, then lead, harness, area and
    // session, in that order (AC11).
    //
    // Header and rows draw through ONE grid: each column's width is
    // the widest value the table shows (display columns), capped, and a
    // longer value ellipsizes inside its cell, so a title can never sit at a
    // different x than its data.
    let show_date = w >= 48;
    // The first row of each day carries its date at any width, so a day
    // boundary never reads as time running backward (00:57 above 20:06).
    // A narrow panel shows the date in place of the time on that row, so
    // the time column keeps its width and node keeps its place.
    let window = o.slots.get(offset..).unwrap_or_default();
    let window = window
        .get(..feed_body_rows(visible_rows).min(window.len()))
        .unwrap_or_default();
    let stamps: Vec<Option<&str>> = window
        .iter()
        .map(|s| match s {
            Slot::Item(i) => o.item(*i).map(|it| it.ts.as_str()),
            Slot::Header { .. } => None,
        })
        .collect();
    let day_marks = day_marks_in(&stamps, &chrono::Local);
    let ts_w = if show_date { 11 } else { 5 };
    let dw = |s: &str| unicode_width::UnicodeWidthStr::width(s);
    // Each title carries its marks: an arrow on the sort column, a dot and
    // the value count on a filtered one, so color is never the only signal.
    let [t_time, t_area, t_harness, t_kind, t_node, t_session, t_lead, t_summary] =
        table::Col::ALL.map(|c| column_title(&o.view, c));
    // A cell is as wide as its widest value, capped (measured once per
    // re-slot, never per paint); the title always fits.
    let vw = &o.widths;
    let col_w = |title: &str, value_w: usize| dw(title).max(value_w);
    let ts_w = ts_w.max(dw(&t_time));
    let area_w = col_w(&t_area, vw.area);
    let harness_w = col_w(&t_harness, vw.harness);
    let kind_full = col_w(&t_kind, vw.kind);
    let node_w = col_w(&t_node, vw.node);
    let session_w = col_w(&t_session, vw.session);
    let lead_w = col_w(&t_lead, vw.lead);
    // The rows show only what fits the cap: a longer value ellipsizes to
    // cap-1 columns plus the ellipsis, so the next column never moves.
    let fit_cell = |s: &str, cap: usize| -> String {
        if dw(s) <= cap {
            return s.to_string();
        }
        let mut out = String::new();
        let mut cell_w = 0usize;
        for ch in s.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if cell_w + cw > cap - 1 {
                break;
            }
            out.push(ch);
            cell_w += cw;
        }
        out.push('\u{2026}');
        out
    };

    // A short panel gives width in one order: kind narrows to KIND_MIN,
    // then the summary floor drops to SUMMARY_TIGHT, then node goes. The
    // summary is the column a reader reads, and node is the deep-link key.
    let fixed = 3 + ts_w + 1 + 1;
    let kind_min = kind_full.min(KIND_MIN);
    let kind_room = |floor: usize| w.saturating_sub(fixed + node_w + 1 + floor);
    let (kind_w, summary_floor, show_node) = if kind_room(SUMMARY_MIN) >= kind_min {
        (kind_full.min(kind_room(SUMMARY_MIN)), SUMMARY_MIN, true)
    } else if kind_room(SUMMARY_TIGHT) >= kind_min {
        (kind_full.min(kind_room(SUMMARY_TIGHT)), SUMMARY_TIGHT, true)
    } else {
        let room = w.saturating_sub(fixed + SUMMARY_MIN);
        (kind_full.min(room).max(kind_min), SUMMARY_MIN, false)
    };
    let mut used = fixed + kind_w + summary_floor + if show_node { node_w + 1 } else { 0 };
    let fits = |needed: usize, used: &mut usize| {
        if *used + needed <= w {
            *used += needed;
            true
        } else {
            false
        }
    };
    let show_lead = fits(lead_w + 1, &mut used);
    let show_harness = fits(harness_w + 1, &mut used);
    let show_area = fits(area_w + 1, &mut used);
    let show_session = fits(session_w + 1, &mut used);

    // One cell writer for the column header and every row: a value (or a
    // title) plus padding to the column width and a one-column gutter.
    let push_col = |row: &mut Vec<Span>, text: &str, width: usize, bold: bool, brand: bool| {
        let pad = width.saturating_sub(dw(text));
        row.push(Span {
            text: format!("{text}{}", " ".repeat(pad + 1)),
            bold,
            brand,
        });
    };

    let mut rows: Vec<Vec<Span>> = Vec::new();
    rows.push(vec![Span::plain(pad_to(
        &header_line(focused, &o.view, o.matched, o.held(), w),
        w,
    ))]);
    let mut header = vec![Span::plain("   ")];
    push_col(&mut header, &t_time, ts_w, true, false);
    if show_area {
        push_col(&mut header, &t_area, area_w, true, false);
    }
    if show_harness {
        push_col(&mut header, &t_harness, harness_w, true, false);
    }
    push_col(&mut header, &fit_cell(&t_kind, kind_w), kind_w, true, false);
    if show_node {
        push_col(&mut header, &t_node, node_w, true, false);
    }
    if show_session {
        push_col(&mut header, &t_session, session_w, true, false);
    }
    if show_lead {
        push_col(&mut header, &t_lead, lead_w, true, false);
    }
    header.push(Span {
        text: t_summary,
        bold: true,
        brand: false,
    });
    pad_to_spans(&mut header, w);
    rows.push(header);
    let visible = feed_body_rows(visible_rows);
    for d in offset..offset + visible {
        match o.slots.get(d) {
            Some(Slot::Header {
                label,
                count,
                collapsed,
                ..
            }) => {
                let caret = if *collapsed { '▸' } else { '▾' };
                let count = count.to_string();
                let head = format!(" {caret} {label}");
                let gap = w.saturating_sub(dw(&head) + dw(&count) + 1).max(1);
                rows.push(vec![
                    Span {
                        text: head,
                        bold: true,
                        brand: false,
                    },
                    Span {
                        text: format!("{}{count} ", " ".repeat(gap)),
                        bold: true,
                        brand: false,
                    },
                ]);
            }
            Some(Slot::Item(i)) => {
                let Some(item) = o.item(*i) else {
                    rows.push(vec![Span::plain(pad_to("", w))]);
                    continue;
                };
                let selected = focused && d == o.sel;
                let mut row = Vec::new();
                row.push(Span {
                    text: "   ".to_string(),
                    bold: selected,
                    brand: selected,
                });
                let day_mark = day_marks.get(d - offset).copied().unwrap_or(false);
                let ts = if day_mark {
                    dated_ts(&item.ts, show_date)
                } else {
                    short_ts(&item.ts, show_date)
                };
                push_col(&mut row, &fit_cell(&ts, ts_w), ts_w, selected, selected);
                if show_area {
                    push_col(
                        &mut row,
                        &fit_cell(&item.area, area_w),
                        area_w,
                        false,
                        false,
                    );
                }
                if show_harness {
                    let h = item.harness.as_deref().unwrap_or("-");
                    push_col(&mut row, &fit_cell(h, harness_w), harness_w, false, false);
                }
                let kind = fit_cell(display_kind(&item.kind), kind_w);
                push_col(&mut row, &kind, kind_w, bold_kind(item), bold_kind(item));
                let node = item
                    .node
                    .as_deref()
                    .map(|n| fit_cell(n, node_w))
                    .unwrap_or_else(|| "-".to_string());
                if show_node {
                    push_col(&mut row, &node, node_w, item.node.is_some(), false);
                }
                if show_session {
                    let sid = item
                        .session_id
                        .as_deref()
                        .map(|s| fit_cell(&session_tail(s), session_w))
                        .unwrap_or_else(|| "-".to_string());
                    push_col(&mut row, &sid, session_w, false, false);
                }
                if show_lead {
                    // The lead column names a lead or nothing: the epic
                    // rollup lives in the owner grouping, never here.
                    let lead = item.lead.as_deref().unwrap_or("-");
                    push_col(&mut row, &fit_cell(lead, lead_w), lead_w, false, false);
                }
                let title = pan_by(&item.title, o.hpan);
                row.push(Span::plain(title));
                pad_to_spans(&mut row, w);
                rows.push(row);
            }
            // Short list: the rows below the last slot paint the panel's
            // ground across the full width, so no pane cell shows through.
            None => rows.push(vec![Span::plain(pad_to("", w))]),
        }
    }
    // The empty notices only when the fold has SETTLED: "no activity"
    // beside a still-running fold is a claim the fold has not earned yet,
    // and zero matches over a full history is a filter, never an empty feed.
    if o.slots.is_empty() && o.error.is_none() && !o.inflight && visible > 0 {
        let notice = if o.held() == 0 {
            "   no activity yet".to_string()
        } else {
            "   no rows match".to_string()
        };
        if let Some(line) = rows.get_mut(2) {
            *line = vec![Span::plain(pad_to(&notice, w))];
        }
    }
    // The footer carries the hints: the new-row marker, the active query,
    // then the keys; the status rides the same line.
    let mut status = if let Some(e) = &o.error {
        // The typed reason renders verbatim: a timeout names its budget, an
        // admission refusal its slot count. The last good rows stay above.
        format!("{e} · r retry")
    } else if let Some(Err(refusal)) = &o.parsed {
        // The shared grammar's refusal, verbatim.
        refusal.clone()
    } else if o.inflight && o.held() == 0 {
        "reading history".to_string()
    } else {
        String::new()
    };
    if o.bar_open {
        status = format!("/{}▏ {status}", o.query_text);
    }
    let mut left = String::new();
    if o.win.new_count > 0 {
        left.push_str(&format!("↑ {} new · ", o.win.new_count));
    }
    if !o.bar_open && !o.query_text.is_empty() {
        left.push_str(&format!("{} · ", o.query_text));
    }
    let hints = if focused {
        "o sort · W group · f filter · z fold · / search · ? keys"
    } else {
        "E focus · e close"
    };
    let footer = if status.is_empty() {
        format!(" {left}{hints}")
    } else {
        format!(" {left}{hints} · {status}")
    };
    rows.push(vec![Span::plain(pad_to(&footer, w))]);
    rows
}
/// One styled span of a panel row, in display columns. `brand` names the
/// theme's accent; both flags ride the span, so the paint pass reads them
/// without re-deriving which kind needs action.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Span {
    pub text: String,
    pub bold: bool,
    pub brand: bool,
}

impl Span {
    fn plain(text: impl Into<String>) -> Self {
        Span {
            text: text.into(),
            bold: false,
            brand: false,
        }
    }
}

/// Pad a span row to `w` display columns: pads the JOINED width, appending
/// one trailing plain span when the row falls short.
fn pad_to_spans(row: &mut Vec<Span>, w: usize) {
    let width: usize = row
        .iter()
        .map(|s| unicode_width::UnicodeWidthStr::width(s.text.as_str()))
        .sum();
    if width < w {
        row.push(Span::plain(" ".repeat(w - width)));
    }
}

/// The panel body as text: [`feed_panel_rows`] flattened, the shape the text
/// tests read and `draw_feed_panel` no longer re-derives. Test-only: the
/// paint path reads [`feed_panel_rows`] spans directly.
#[cfg(test)]
pub(crate) fn feed_panel_lines(
    o: &FeedOverlay,
    focused: bool,
    w: usize,
    visible_rows: usize,
    offset: usize,
) -> Vec<String> {
    feed_panel_rows(o, focused, w, visible_rows, offset)
        .iter()
        .map(|row| row.iter().map(|s| s.text.clone()).collect())
        .collect()
}

/// The click resolver, the exact inverse of the painter's slot list: painted
/// row 0 is the title, row 1 the column header, painted row
/// `visible_rows - 1` the footer, and a row in between is the slot at
/// `offset + painted_row - 2`. Chrome resolves to None.
pub(crate) fn feed_slot_at(
    slots: &[Slot],
    painted_row: usize,
    visible_rows: usize,
    offset: usize,
) -> Option<(usize, &Slot)> {
    if painted_row <= 1 || painted_row + 1 >= visible_rows {
        return None;
    }
    let d = offset + painted_row - 2;
    slots.get(d).map(|s| (d, s))
}

/// The STORAGE item under a painted row; a group header or chrome is None,
/// so a header never opens a detail.
pub(crate) fn feed_row_item(
    slots: &[Slot],
    painted_row: usize,
    visible_rows: usize,
    offset: usize,
) -> Option<usize> {
    match feed_slot_at(slots, painted_row, visible_rows, offset) {
        Some((_, Slot::Item(i))) => Some(*i),
        _ => None,
    }
}

/// A column header's title: the name, an arrow when the rows sort by it, and
/// a dot with the value count when it filters.
pub(crate) fn column_title(view: &table::TableView, col: table::Col) -> String {
    let mut t = col.name().to_string();
    if view.sort == col {
        t.push(if view.sort_desc { '↓' } else { '↑' });
    }
    let n = view.filters.get(&col).map_or(0, |s| s.len());
    if n > 0 {
        t.push_str(&format!("·{n}"));
    }
    t
}

/// The panel title for one state, at the widest spelling that fits: the
/// matched and total row counts, and while focused the sort and group, so
/// an active column a narrow panel hides is still named. The last spelling
/// is the narrowest; the caller pads and clips from the end.
pub(crate) fn header_line(
    focused: bool,
    view: &table::TableView,
    matched: usize,
    total: usize,
    w: usize,
) -> String {
    let count = if matched == total {
        format!("{total}")
    } else {
        format!("{matched} of {total}")
    };
    let sort = format!(
        "{}{}",
        view.sort.name(),
        if view.sort_desc { '↓' } else { '↑' }
    );
    let group = view.group.name();
    let filters = match view.filter_count() {
        0 => String::new(),
        1 => " · 1 filter".to_string(),
        n => format!(" · {n} filters"),
    };
    let candidates: Vec<String> = if focused {
        vec![
            format!(" FEED FOCUSED · {count} · sort {sort} · group {group}{filters}"),
            format!(" FOCUSED · {count} · {sort} · {group}{filters}"),
            format!(" FOCUSED · {count}{filters}"),
            format!(" FOCUSED · {count}"),
            " FOCUSED".to_string(),
        ]
    } else {
        vec![
            format!(" activity feed · {count}"),
            format!(" feed · {count}"),
            " activity feed".to_string(),
            " feed".to_string(),
        ]
    };
    let narrowest = " feed";
    candidates
        .into_iter()
        .find(|c| unicode_width::UnicodeWidthStr::width(c.as_str()) <= w)
        .unwrap_or_else(|| narrowest.to_string())
}

/// Drop `cols` DISPLAY columns off the front of `s`, so a pan never splits a
/// wide glyph in half: a two-column glyph straddling the cut is dropped whole.
/// Panning past the end yields the empty string rather than clamping, so the
/// caller's own ceiling is what stops the pan.
pub(crate) fn pan_by(s: &str, cols: usize) -> String {
    if cols == 0 {
        return s.to_string();
    }
    let mut skipped = 0usize;
    let mut out = String::new();
    for ch in s.chars() {
        if skipped >= cols {
            out.push(ch);
            continue;
        }
        skipped += unicode_width::UnicodeWidthChar::width(ch)
            .unwrap_or(0)
            .max(1);
    }
    out
}

/// The widest title in the panel, in display columns. The pan stops one
/// column short of this, so the widest row always keeps something on screen.
pub(crate) fn widest_title(items: &[FeedItem]) -> usize {
    items
        .iter()
        .map(|i| unicode_width::UnicodeWidthStr::width(i.title.as_str()))
        .max()
        .unwrap_or(0)
}

/// Compact local time in the operator's zone. With `with_date`, anything
/// older than today carries its date (`MM-DD HH:MM`) so a scrolling feed
/// never presents last week as this morning; without it, every row reads
/// `HH:MM` (the narrow panel's budget). An unparseable ts shows raw.
pub(crate) fn short_ts_in<Tz: chrono::TimeZone>(ts: &str, tz: &Tz, with_date: bool) -> String
where
    Tz::Offset: std::fmt::Display,
{
    match chrono::DateTime::parse_from_rfc3339(ts) {
        Ok(t) => {
            let local = tz.from_utc_datetime(&t.naive_utc());
            let today = chrono::Utc::now().with_timezone(tz).date_naive();
            let older = local.date_naive() != today;
            match (with_date, older) {
                (true, true) => local.format("%m-%d %H:%M").to_string(),
                _ => local.format("%H:%M").to_string(),
            }
        }
        Err(_) => ts.to_string(),
    }
}

fn short_ts(ts: &str, with_date: bool) -> String {
    short_ts_in(ts, &chrono::Local, with_date)
}

fn dated_ts(ts: &str, with_time: bool) -> String {
    let fmt = if with_time { "%m-%d %H:%M" } else { "%m-%d" };
    match chrono::DateTime::parse_from_rfc3339(ts) {
        Ok(t) => t.with_timezone(&chrono::Local).format(fmt).to_string(),
        Err(_) => ts.to_string(),
    }
}

/// Which painted rows open a day, given each row's stamp in paint order
/// (`None` is a group header). A row opens a day when its local day differs
/// from the row above it, or it is the first row under the top or a header
/// and its day is not today. Such a row shows its date at any width: with
/// its time on a wide panel, in place of it on a narrow one.
pub(crate) fn day_marks_in<Tz: chrono::TimeZone>(stamps: &[Option<&str>], tz: &Tz) -> Vec<bool> {
    let today = chrono::Utc::now().with_timezone(tz).date_naive();
    let mut prev = None;
    stamps
        .iter()
        .map(|s| {
            let Some(s) = s else {
                prev = None;
                return false;
            };
            let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) else {
                return false;
            };
            let day = tz.from_utc_datetime(&t.naive_utc()).date_naive();
            let opens = match prev {
                Some(p) => p != day,
                None => day != today,
            };
            prev = Some(day);
            opens
        })
        .collect()
}

/// The session id's last eight characters, in order. Measured unique across
/// every session the feed names (6,553 ids, 6,553 tails).
pub(crate) fn session_tail(s: &str) -> String {
    let n = s.chars().count();
    s.chars().skip(n.saturating_sub(8)).collect()
}

/// The feed panel's width until the operator drags its border once; persisted
/// thereafter, like the sideline's.
pub(crate) const FEED_DEFAULT_W: u16 = 40;

/// The slot rows a panel of `rows` terminal rows paints: the title, the
/// column header and the footer take three. Paint, hit test, wheel, follow,
/// page keys and the land clamp all read this one value, so the last slot
/// always lands above the footer.
pub(crate) fn feed_body_rows(rows: usize) -> usize {
    rows.saturating_sub(3)
}

/// How long a settled fold stays fresh before the next run-loop tick refolds
/// the panel on its own.
pub(crate) const FEED_REFRESH_EVERY: std::time::Duration = std::time::Duration::from_secs(15);

/// The fold result channel the run loop hands [`maybe_kick`] and reads back in
/// its `feed_rx` arm.
pub(crate) type FoldTx = tokio::sync::mpsc::UnboundedSender<(u64, crate::feed_overlay::FoldResult)>;

// ---- The panel's View integration: geometry, input, paint ----

impl View {
    /// The panel's width in columns, or 0 when it is closed or the terminal
    /// cannot admit it. The sideline is the senior panel: the cap here is the
    /// terminal cap MINUS the sideline's own width, so the feed yields its
    /// columns on a tight terminal and the content floor holds with both
    /// panels open. The clamp is TRANSIENT: `feed_width` is never mutated by
    /// it, so a shrink-then-grow restores the operator's chosen width.
    pub(super) fn feed_panel_w(&self) -> u16 {
        // The file viewer panel holds the right edge while it is open.
        if self.feed.is_none() || self.viewer_panel_w() > 0 {
            return 0;
        }
        let cols = self.term.1;
        let max =
            sideline_max_width(cols).min(cols.saturating_sub(self.panel_w() + MIN_CONTENT_COLS));
        if max < MIN_SLIM_PANEL_W {
            return 0;
        }
        self.feed_width.clamp(MIN_SLIM_PANEL_W, max)
    }

    /// True on the panel's divider column - the grab band for the width drag.
    pub(super) fn on_feed_border(&self, row: u16, col: u16) -> bool {
        let w = self.feed_panel_w();
        w > 0 && row >= TAB_BAR_ROWS && col == self.term.1 - w
    }

    /// Begin the width drag when a press lands on the divider, remembering the
    /// width at grab so a bare Esc can revert. `false` leaves the press to
    /// fall through.
    pub(crate) fn begin_border_drag(&mut self, row: u16, col: u16) -> bool {
        if !self.on_feed_border(row, col) {
            return false;
        }
        self.feed_drag = Some(SidelineDrag {
            start_width: self.feed_width,
            last_at: Instant::now(),
        });
        true
    }

    /// Set the panel to a free width from the dragged border column, clamped
    /// exactly as [`Self::feed_panel_w`] clamps. `false` when no drag is live,
    /// the terminal is too tight, or the width did not change.
    pub(super) fn drag_feed_to(&mut self, col: u16, now: Instant) -> bool {
        if self.feed_drag.is_none() {
            return false;
        }
        let cols = self.term.1;
        let max =
            sideline_max_width(cols).min(cols.saturating_sub(self.panel_w() + MIN_CONTENT_COLS));
        let Some(w) = cols.checked_sub(col) else {
            return false;
        };
        if max < MIN_SLIM_PANEL_W {
            return false;
        }
        let want = w.clamp(MIN_SLIM_PANEL_W, max);
        if let Some(drag) = self.feed_drag.as_mut() {
            drag.last_at = now;
        }
        if want == self.feed_panel_w() {
            return false;
        }
        self.feed_width = want;
        true
    }

    /// End a feed-border drag: persist the width when it moved, then refresh
    /// the hover accent so it never lingers past the gesture.
    pub(super) fn end_feed_drag(&mut self, row: u16, col: u16) {
        if let Some(drag) = self.feed_drag.take() {
            if drag.start_width != self.feed_width {
                view_store::save_feed_width(self.feed_width);
            }
        }
        self.refresh_hover_affordances(row, col);
    }

    /// Revert a feed-border drag to the width at grab; `false` when no drag is
    /// live or the width did not change, so no resize travels at all.
    pub(super) fn revert_feed_drag(&mut self) -> bool {
        let Some(drag) = self.feed_drag.take() else {
            return false;
        };
        let changed = self.feed_width != drag.start_width;
        self.feed_width = drag.start_width;
        changed
    }

    /// Wheel-scroll the slot window by one row. The offset re-clamps against
    /// the CURRENT viewport and slot count first, so terminal growth (or a
    /// shorter fold) can never leave the window parked on blank rows. The
    /// panel holds the whole history, so no gesture reads a page.
    pub(super) fn scroll_feed(&mut self, down: bool) {
        let Some(f) = self.feed.as_ref() else {
            return;
        };
        let visible = feed_body_rows(self.term.0 as usize);
        let max_off = f.slots.len().saturating_sub(visible);
        self.feed_offset = if down {
            (self.feed_offset + 1).min(max_off)
        } else {
            self.feed_offset.min(max_off).saturating_sub(1)
        };
    }

    /// True when a click lands on the footer's `↑ N new` marker: the one
    /// footer span that acts, home through the same path the g key takes.
    pub(super) fn feed_new_marker_hit(&self, row: u16, col: u16) -> bool {
        let Some(f) = &self.feed else {
            return false;
        };
        if f.win.new_count == 0 {
            return false;
        }
        let feed_w = self.feed_panel_w() as usize;
        if feed_w == 0 {
            return false;
        }
        let footer_row = (self.term.0 as usize).saturating_sub(1);
        if row as usize != footer_row || self.bottom_row_is_chrome() {
            return false;
        }
        let x0 = self.term.1 as usize - feed_w;
        let marker = format!("↑ {} new · ", f.win.new_count);
        let start = x0 + 1;
        let end = start + marker.chars().count() + 1;
        let c = col as usize;
        c >= start && c < end
    }

    /// The marker's (and g's) action: the window jumps to the first slot,
    /// the cursor to the first row, and the new-row count clears.
    pub(super) fn feed_home(&mut self) {
        let Some(f) = self.feed.as_mut() else {
            return;
        };
        self.feed_offset = 0;
        f.win.new_count = 0;
        f.sel = first_item_slot(&f.slots);
    }

    /// Where a feed picker opens: under the column header at the panel's
    /// left edge. The popup flips and clamps at the screen edges itself, and
    /// sizes from the terminal, so a thin panel never makes a thin picker.
    pub(super) fn feed_picker_anchor(&self) -> crate::popup::Anchor {
        let w = self.feed_panel_w();
        crate::popup::Anchor::At {
            row: 2,
            col: self.term.1.saturating_sub(w) + 1,
        }
    }

    /// The scroll offset clamped to what the CURRENT slots and viewport can
    /// show, the single value every read path (paint, click, hover) shares.
    pub(super) fn feed_offset_clamped(&self) -> usize {
        let Some(f) = &self.feed else {
            return 0;
        };
        let visible = feed_body_rows(self.term.0 as usize);
        self.feed_offset.min(f.slots.len().saturating_sub(visible))
    }

    /// The right-edge panel, [`View::draw_sideline`] inverted: divider on the
    /// panel's LEFT edge, header at row 0, items below, footer pinned to the
    /// last row. Runs after the pane blit so a stale pane rect mid-resize
    /// cannot bleed through, and before the overlay pass so a modal still
    /// wins its cells.
    pub(super) fn draw_feed_panel(&self, cells: &mut [Cell], rows: usize, cols: usize) {
        let Some(f) = &self.feed else {
            return;
        };
        let w = self.feed_panel_w() as usize;
        if w == 0 {
            return;
        }
        let x0 = cols - w;
        let focused = self.input_owner() == super::region_focus::RegionOwner::Feed;
        let span_rows = feed_panel_rows(f, focused, w - 1, rows, self.feed_offset_clamped());
        // The driven row inverts like the bell's and the sideline's: the row
        // about to take Enter reads on both theme families, and the band
        // pair stays with hover/selection surfaces only.
        let band_row = if focused {
            Some(2 + f.sel.saturating_sub(self.feed_offset_clamped()))
        } else {
            None
        };
        for (r, row) in span_rows.iter().enumerate() {
            if r == 0 && focused {
                // Row 0 is the header: the title-row fill paints it when
                // the panel owns typing.
                continue;
            }
            if r >= rows {
                break;
            }
            // Advance by DISPLAY columns, not char index: a double-width glyph
            // claims two columns and marks its right half a WIDE_SPACER, the
            // sideline's own contract, so the row never desyncs against the
            // terminal. Feed titles are arbitrary text, so the width comes
            // from unicode-width, not the sideline's trigram-only glyph_cols.
            let mut dcol = 0usize;
            for span in row {
                for ch in span.text.chars() {
                    let cw = unicode_width::UnicodeWidthChar::width(ch)
                        .unwrap_or(0)
                        .max(1);
                    if dcol + cw > w - 1 {
                        break;
                    }
                    cells[r * cols + x0 + 1 + dcol] = Cell {
                        c: ch,
                        fg: if span.brand {
                            self.theme.brand
                        } else {
                            Color::Default
                        },
                        bg: Color::Default,
                        flags: if span.bold { cell_flags::BOLD } else { 0 },
                    };
                    if cw == 2 && dcol + 2 < w {
                        cells[r * cols + x0 + 1 + dcol + 1] = Cell {
                            c: ' ',
                            fg: Color::Default,
                            bg: Color::Default,
                            flags: cell_flags::WIDE_SPACER,
                        };
                    }
                    dcol += cw;
                }
            }
            if band_row == Some(r) {
                let (bfg, bbg, bflags) = crate::cursor::cursor_style(&self.theme);
                for c in (x0 + 1)..(x0 + w) {
                    let cell = &mut cells[r * cols + c];
                    cell.fg = bfg;
                    cell.bg = bbg;
                    if cell.flags & cell_flags::WIDE_SPACER == 0 {
                        cell.flags = bflags;
                    }
                }
            }
        }
        // The focused header row 0 carries the accent title fill - the one
        // visible mark that the feed owns typing (the pane seam drops its
        // own mark through input_owner in the same frame).
        if focused {
            backlog_style::paint_title_row(
                cells,
                rows,
                cols,
                0,
                x0 + 1,
                w - 1,
                &header_line(focused, &f.view, f.matched, f.held(), w - 1),
                true,
                &self.theme,
            );
        }
        let border_active = self.hover_feed_border
            || self.feed_drag.is_some()
            || self.input_owner() == super::region_focus::RegionOwner::Feed;
        let (border_fg, border_flags) = if border_active {
            (self.theme.brand, cell_flags::BOLD)
        } else {
            (Color::Default, cell_flags::DIM)
        };
        for r in 0..rows {
            cells[r * cols + x0] = Cell {
                c: '│',
                fg: border_fg,
                bg: Color::Default,
                flags: border_flags,
            };
        }
        // The `?` overlay floats over the panel (AC13), the board's own
        // popup layering.
        if f.keys_open {
            let popup = search::feed_keys_popup();
            draw_popup_overlay(cells, rows, cols, &popup, self.term, &self.theme);
        }
        // The sort or group picker opens under the panel's column header.
        if let Some(p) = &f.picker {
            draw_popup_overlay(cells, rows, cols, &p.popup, self.term, &self.theme);
        }
        // The panel's esc chip, at the header row's right edge. A tap on it
        // presses Esc through the shared chip path: a focused panel reads the
        // Esc itself and closes; an unfocused one owns no keys, so
        // `esc_close::tap` closes it directly.
        let chip = " esc ";
        if rows > 0 && w > chip.len() {
            for (i, ch) in chip.chars().enumerate() {
                cells[x0 + w - chip.len() + i] = Cell {
                    c: ch,
                    fg: self.theme.brand,
                    bg: Color::Default,
                    flags: cell_flags::BOLD,
                };
            }
            crate::chrome::record_close_spans(
                (rows, cols),
                (0, x0 + 1),
                w - 1,
                &[(crate::chrome::ESC_CLOSE_HIT, w - 5, 3)],
            );
        }
    }

    /// The `chrome_hit` branch for the panel's columns: a click opens that
    /// row's PROVENANCE, and the deep link is that view's action. The divider
    /// is the drag band and the header/footer rows are chrome, never rows.
    ///
    /// The click used to fire the deep link straight off. It moved because
    /// "take me there" and "tell me what this was" are different questions,
    /// and the row had only one gesture to answer both with.
    pub(super) fn chrome_hit_feed(&self, row: u16, col: u16) -> Option<ChromeHit> {
        let feed_w = self.feed_panel_w();
        if feed_w == 0 || col < self.term.1 - feed_w {
            return None;
        }
        if col == self.term.1 - feed_w {
            return None;
        }
        if row as usize == (self.term.0 as usize).saturating_sub(1) && self.bottom_row_is_chrome() {
            return None;
        }
        let Some(f) = &self.feed else {
            return None;
        };
        // The panel's own chrome: a column title sorts by its column, a
        // group header folds or unfolds its group.
        let at = usize::from(col - (self.term.1 - feed_w) - 1);
        if row == 1 {
            return column_at(f, usize::from(feed_w) - 1, at)
                .map(|c| ChromeHit::Feed(FeedHit::Sort(c)));
        }
        if let Some((_, Slot::Header { key, .. })) = feed_slot_at(
            &f.slots,
            row as usize,
            self.term.0 as usize,
            self.feed_offset_clamped(),
        ) {
            return Some(ChromeHit::Feed(FeedHit::Fold(key.clone())));
        }
        feed_row_item(
            &f.slots,
            row as usize,
            self.term.0 as usize,
            self.feed_offset_clamped(),
        )
        .and_then(|i| {
            let item = f.item(i)?;
            // A question row answers from the feed: the whole question opens
            // on the same path the questions view uses, never a provenance
            // detour (ruling 2026-09-29).
            if item.kind == "question_asked" {
                // A row whose question id never landed still inspects: the
                // click falls back to the provenance modal rather than
                // resolving to nothing (which would forward to a pane).
                return Some(match item.r#ref.clone() {
                    Some(qid) => ChromeHit::OpenQuestionDetail(qid),
                    None => ChromeHit::OpenFeedDetail(item.clone()),
                });
            }
            Some(ChromeHit::OpenFeedDetail(item.clone()))
        })
    }

    /// Keep the selected row inside the window after a keyboard move. The
    /// offset is re-clamped against the CURRENT viewport first, exactly as
    /// [`Self::scroll_feed`] does, so a shrunken terminal cannot leave the
    /// window parked past the last row.
    pub(super) fn follow_feed_selection(&mut self) {
        let visible = feed_body_rows(self.term.0 as usize);
        let Some(f) = self.feed.as_ref() else {
            return;
        };
        if visible == 0 {
            return;
        }
        let max_off = f.slots.len().saturating_sub(visible);
        let sel = f.sel;
        let mut off = self.feed_offset.min(max_off);
        if sel < off {
            off = sel;
        } else if sel >= off + visible {
            off = sel + 1 - visible;
        }
        self.feed_offset = off.min(max_off);
    }

    /// Open the provenance view for the selected row, copying the item OUT of
    /// the list. A later fold replaces `items` wholesale, so holding an index
    /// would re-point the open view at a different event mid-read.
    pub(super) fn open_feed_detail(&mut self) {
        // `sel` is a SLOT index (headers included); a header has no detail.
        // The clone ends the feed borrow, so the question route below can
        // open the questions detail on the same view.
        let item = {
            let Some(f) = &self.feed else {
                return;
            };
            f.slots
                .get(f.sel)
                .and_then(|s| match s {
                    Slot::Item(i) => f.item(*i),
                    Slot::Header { .. } => None,
                })
                .cloned()
        };
        let Some(item) = item else {
            return;
        };
        // A question row answers from the feed, the same detail view the bell
        // opens.
        if item.kind == "question_asked" {
            if let Some(qid) = item.r#ref.as_deref() {
                self.open_detail_on(qid);
                return;
            }
        }
        self.feed_detail = Some(feed_detail::modal(self, item));
    }

    /// The hover marker follows the pointer onto a row while the panel is
    /// unfocused. Off the rows it stays where it was, and a focused panel's
    /// cursor belongs to the keys alone. Marker only - a hover never
    /// deep-links.
    pub(super) fn hover_feed_marker(&mut self, row: u16, col: u16) {
        let Some(f) = &self.feed else {
            return;
        };
        if self.input_owner() == super::region_focus::RegionOwner::Feed {
            return;
        }
        let feed_w = self.feed_panel_w();
        if feed_w == 0 || col <= self.term.1 - feed_w {
            return;
        }
        let hit = feed_slot_at(
            &f.slots,
            row as usize,
            self.term.0 as usize,
            self.feed_offset_clamped(),
        )
        .map(|(d, _)| d);
        if let (Some(d), Some(f)) = (hit, self.feed.as_mut()) {
            f.sel = d;
        }
    }
}

/// The run-loop fold kick: at most ONE fold in flight, armed by `want` (the
/// needs fold's generation/single-flight discipline). Every fold reads the
/// whole history: a head page already projects every row before it trims,
/// so the full read costs the projection nothing more, and a full replace
/// keeps a renamed lead or session name from going stale.
pub(crate) fn maybe_kick(view: &mut View, tx: &FoldTx) {
    let window = (
        view.feed_offset_clamped(),
        feed_body_rows(view.term.0 as usize),
    );
    let Some(f) = view.feed.as_mut() else {
        return;
    };
    f.window = window;
    // The search bar's 250ms debounce: an expired one re-parses and
    // re-slots the held rows; no read is needed for a new query.
    if f.bar_open {
        if let Some(at) = f.bar_debounce {
            if at.elapsed() >= std::time::Duration::from_millis(250) {
                f.bar_debounce = None;
                search::apply_query(f);
            }
        }
    }
    // Time-based re-arm, the backlog board's pattern: a settled fold goes
    // stale after FEED_REFRESH_EVERY and the next run-loop tick refolds.
    let due = f.want
        || f.last_fold
            .is_none_or(|t| t.elapsed() >= FEED_REFRESH_EVERY);
    // A panel holding its own rows (a shot seed, a test) refreshes by one
    // full read, as before the reader; the live panel holds none.
    if !f.win.items.is_empty() {
        if !due || f.inflight {
            return;
        }
        f.want = false;
        f.inflight = true;
        let tx = tx.clone();
        let gen = f.gen;
        tokio::spawn(async move {
            let result = crate::feed_overlay::fetch_history()
                .await
                .map(crate::feed_overlay::Land::Rows);
            tx.send((gen, result)).ok();
        });
        return;
    }
    if f.reader.is_none() && due && !f.inflight {
        match reader::Reader::spawn(f.gen, tx.clone()) {
            Ok(r) => f.reader = Some(r),
            Err(e) => {
                f.error = Some(e);
                f.want = false;
                f.last_fold = Some(Instant::now());
                return;
            }
        }
    }
    if due && !f.inflight {
        f.want = false;
        request_slots(f);
    } else {
        fetch_missing(f);
    }
}

/// A row's identity across folds: the projection's cursor, else the
/// (ts, kind, title) spelling a row without one still answers to.
fn anchor_of(it: &FeedItem) -> String {
    if it.cursor.is_empty() {
        format!("{}\u{1}{}\u{1}{}", it.ts, it.kind, it.title)
    } else {
        it.cursor.clone()
    }
}

/// A fold landed: apply only to the still-open, same-generation panel. The
/// rows replace the held history, and the row under the cursor and the top
/// row stay where they were, so a refresh never moves the reader. A failed
/// fold keeps the last good rows and names its cause.
pub(crate) fn apply_fold(view: &mut View, gen: u64, outcome: crate::feed_overlay::FoldResult) {
    let top_slot = view.feed_offset_clamped();
    let visible = feed_body_rows(view.term.0 as usize);
    let Some(f) = view.feed.as_mut() else {
        return;
    };
    if gen != f.gen {
        return;
    }
    let rows = match outcome {
        Ok(crate::feed_overlay::Land::Rows(rows)) => rows,
        Ok(crate::feed_overlay::Land::Answer(answer)) => {
            apply_answer(view, top_slot, visible, *answer);
            return;
        }
        Err(e) => {
            f.inflight = false;
            f.fetch_inflight = false;
            f.last_fold = Some(Instant::now());
            f.error = Some(e);
            return;
        }
    };
    f.inflight = false;
    let first = f.last_fold.is_none();
    f.last_fold = Some(Instant::now());
    f.error = None;
    // The anchors, recorded BEFORE the rows move: a slot's identity is its
    // row's anchor, or a header's group key.
    let anchor_at = |f: &FeedOverlay, d: usize| match f.slots.get(d) {
        Some(Slot::Item(i)) => f.item(*i).map(anchor_of),
        Some(Slot::Header { key, .. }) => Some(format!("\u{2}{key}")),
        None => None,
    };
    let anchor_top = anchor_at(f, top_slot);
    let anchor_sel = anchor_at(f, f.sel);
    let detached = top_slot > 0;
    f.win.land(rows, detached);
    reslot(f);
    let find = |f: &FeedOverlay, anchor: &str| {
        (0..f.slots.len()).find(|d| anchor_at(f, *d).as_deref() == Some(anchor))
    };
    if first {
        view.feed_offset = 0;
        f.sel = first_item_slot(&f.slots);
    } else {
        if let Some(d) = anchor_top.as_deref().and_then(|a| find(f, a)) {
            view.feed_offset = d;
        }
        match anchor_sel.as_deref().and_then(|a| find(f, a)) {
            Some(d) => f.sel = d,
            None => f.sel = first_item_slot(&f.slots),
        }
    }
    // A refresh never lets the window park past the last slot.
    let max_off = f.slots.len().saturating_sub(visible.max(1));
    view.feed_offset = view.feed_offset.min(max_off);
}

/// One reader answer: cache its rows, open a waiting value picker, and land
/// a current slot list the way a fold lands rows, keeping the top row and
/// the cursor row where they were.
fn apply_answer(
    view: &mut View,
    top_slot: usize,
    visible: usize,
    answer: fno_proto::feed::serve::Answer,
) {
    let Some(f) = view.feed.as_mut() else {
        return;
    };
    if answer.stamp != f.stamp {
        f.cache.clear();
        f.stamp = answer.stamp;
    }
    f.cache.extend(answer.items);
    if let Some(values) = answer.values {
        if let Some((col, anchor)) = f.pending_values.take() {
            f.picker = Some(picker::FeedPicker::open_with_values(
                col, values, &f.view, anchor,
            ));
        }
    }
    let Some(slots) = answer.slots else {
        f.fetch_inflight = false;
        evict(f);
        return;
    };
    if answer.id < f.slots_req {
        return;
    }
    f.inflight = false;
    let first = f.last_fold.is_none();
    f.last_fold = Some(Instant::now());
    f.error = None;
    let anchor_at = |f: &FeedOverlay, d: usize| match f.slots.get(d) {
        Some(Slot::Item(i)) => f.item(*i).map(anchor_of),
        Some(Slot::Header { key, .. }) => Some(format!("\u{2}{key}")),
        None => None,
    };
    let anchor_top = anchor_at(f, top_slot);
    let anchor_sel = anchor_at(f, f.sel);
    let detached = top_slot > 0;
    f.win.new_count = if detached && !first {
        f.win.new_count + answer.total.saturating_sub(f.total)
    } else {
        0
    };
    f.total = answer.total;
    f.slots = slots;
    f.matched = answer.matched;
    let w = answer.widths;
    f.widths = ValueWidths {
        area: w.area,
        harness: w.harness,
        kind: w.kind,
        node: w.node,
        session: w.session,
        lead: w.lead,
    };
    f.title_w = w.title;
    let find = |f: &FeedOverlay, anchor: &str| {
        (0..f.slots.len()).find(|d| anchor_at(f, *d).as_deref() == Some(anchor))
    };
    if first {
        view.feed_offset = 0;
        f.sel = first_item_slot(&f.slots);
    } else {
        if let Some(d) = anchor_top.as_deref().and_then(|a| find(f, a)) {
            view.feed_offset = d;
        }
        match anchor_sel.as_deref().and_then(|a| find(f, a)) {
            Some(d) => f.sel = d,
            None => f.sel = first_item_slot(&f.slots),
        }
    }
    let max_off = f.slots.len().saturating_sub(visible.max(1));
    view.feed_offset = view.feed_offset.min(max_off);
    evict(f);
}

/// A click on the panel's own chrome.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FeedHit {
    /// A column title: sort by it, or flip it when it already sorts.
    Sort(table::Col),
    /// A group header, by its key: fold or unfold the group.
    Fold(String),
}

/// Run one chrome click, through the same view change the keys make.
pub(crate) fn apply_hit(view: &mut View, hit: FeedHit) {
    match hit {
        FeedHit::Sort(col) => change_view(view, |v| v.pick_sort(col)),
        FeedHit::Fold(key) => change_view(view, |v| v.toggle_collapse(&key)),
    }
}

/// The column whose title spans display column `at` of the column header
/// row the painter draws at width `w`; the marker gutter and the gaps are
/// None.
fn column_at(f: &FeedOverlay, w: usize, at: usize) -> Option<table::Col> {
    let rows = feed_panel_rows(f, false, w, 3, 0);
    let mut x = 0usize;
    for span in rows.get(1)? {
        let width = unicode_width::UnicodeWidthStr::width(span.text.as_str());
        if at < x + width {
            let title = span.text.trim_end();
            return table::Col::ALL
                .into_iter()
                .find(|c| !title.is_empty() && column_title(&f.view, *c) == title);
        }
        x += width;
    }
    None
}

/// Run the open picker's selected choice and close it: a sort, group or
/// value pick changes the view, and the filter's column step opens the
/// values of that column. The Enter key and a click share this.
fn pick_open_picker(view: &mut View) {
    let Some(p) = view.feed.as_mut().and_then(|f| f.picker.take()) else {
        return;
    };
    if p.kind == picker::PickerKind::FilterCol {
        let mut scratch = table::TableView::default();
        if let picker::Picked::Values(col) = p.pick(&mut scratch) {
            open_values_picker(view, col);
        }
    } else {
        change_view(view, |v| {
            p.pick(v);
        });
    }
}

/// One mouse report while a picker is open: the picker owns the pointer. A
/// hover selects, a click on a choice picks it, a click off the popup closes
/// it, and no press reaches the rows or a pane under it.
pub(crate) fn picker_mouse(view: &mut View, rep: crate::mouse::MouseReport) {
    let term = view.term;
    let Some(p) = view.feed.as_mut().and_then(|f| f.picker.as_mut()) else {
        return;
    };
    let rendered = p.popup.render(term);
    match rep.kind {
        MouseKind::Move => {
            if let Some(t) = rendered.row_target_at(rep.row, rep.col) {
                p.popup.select(t);
            }
        }
        MouseKind::Press(MouseButton::Left) => match rendered.row_target_at(rep.row, rep.col) {
            Some(t) => {
                p.popup.select(t);
                pick_open_picker(view);
            }
            None if !rendered.contains(rep.row, rep.col) => {
                if let Some(f) = view.feed.as_mut() {
                    f.picker = None;
                }
            }
            None => {}
        },
        _ => {}
    }
}

/// The filter's second step: the values of `col` the query and the other
/// filters leave, with their row counts.
fn open_values_picker(view: &mut View, col: table::Col) {
    let anchor = view.feed_picker_anchor();
    let Some(f) = view.feed.as_mut() else {
        return;
    };
    let ctx = crate::feed_overlay::EventCtx::default();
    let query = match &f.parsed {
        Some(Ok(parsed)) => Some(parsed),
        _ => None,
    };
    if f.win.items.is_empty() && f.reader.is_some() {
        let req = fno_proto::feed::serve::Request {
            view: fno_proto::feed::serve::ViewSpec::of(&f.view),
            query: match &f.parsed {
                Some(Ok(_)) => f.query_text.clone(),
                _ => String::new(),
            },
            values_for: Some(col.name().to_string()),
            ..Default::default()
        };
        if f.reader.as_mut().and_then(|r| r.send(req)).is_some() {
            f.pending_values = Some((col, anchor));
        }
        return;
    }
    let p = picker::FeedPicker::open_values(
        col,
        &f.win.items,
        &f.view,
        |it| query.is_none_or(|q| q.keeps(&event_fields(it, &ctx))),
        anchor,
    );
    f.picker = Some(p);
}

/// The first ITEM slot: where a fresh cursor parks.
pub(crate) fn first_item_slot(slots: &[Slot]) -> usize {
    slots
        .iter()
        .position(|s| matches!(s, Slot::Item(_)))
        .unwrap_or(0)
}

/// Apply one change to the table view, save it, then re-slot and keep the
/// cursor on the row it was on when that row is still shown.
pub(crate) fn change_view(view: &mut View, change: impl FnOnce(&mut table::TableView)) {
    let Some(f) = view.feed.as_mut() else {
        return;
    };
    let keep = match f.slots.get(f.sel) {
        Some(Slot::Item(i)) => Some(*i),
        _ => None,
    };
    change(&mut f.view);
    crate::view_store::save_feed_view(&table::to_pref(&f.view));
    reslot(f);
    f.sel = keep
        .and_then(|i| f.slots.iter().position(|s| *s == Slot::Item(i)))
        .unwrap_or_else(|| first_item_slot(&f.slots));
    view.follow_feed_selection();
}

/// The `e` toggle. Opening keeps the contract - prior rows render
/// instantly, a fresh fold always arms, a failure degrades loudly - and both
/// transitions re-report the content area: a close that sent no Resize would
/// leave the panes narrowed for good.
pub(crate) async fn toggle(
    view: &mut View,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    if view.feed.is_none() {
        // The parked rows and query paint at once while the refold runs. A
        // new generation drops any fold that was in flight at the close.
        let prior = view.feed_parked.take();
        let gen = prior.as_ref().map_or(0, |f| f.gen.wrapping_add(1));
        view.feed = Some(open_overlay(prior, gen));
    } else {
        // Closing releases the keyboard with the panel, so a later reopen
        // never starts already holding it. The owner field is reset too:
        // `input_owner` normalizes a closed panel to Pane, but the stored
        // choice would silently RE-focus a reopened panel.
        view.feed_parked = view.feed.take().map(park);
        view.feed_detail = None;
        view.region_owner = super::region_focus::RegionOwner::Pane;
        // A half-read escape sequence must not survive the close: carried
        // into the next focus it folds with the fresh bytes into a key
        // nobody pressed.
        view.feed_esc.clear();
    }
    let (r, c) = view.content_dims();
    write_msg(sock_w, &ClientMsg::Resize { rows: r, cols: c })
        .await
        .map_err(|e| format!("resize send failed: {e}"))
}

/// The feed-border drag in flight: drag reports resize per crossed column,
/// release (or any non-left event) ends it. `Ok(true)` = consumed.
pub(crate) async fn drag_mouse(
    view: &mut View,
    row: u16,
    col: u16,
    kind: MouseKind,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<bool, String> {
    if view.feed_drag.is_none() {
        return Ok(false);
    }
    match kind {
        MouseKind::Drag(MouseButton::Left) => {
            if view.drag_feed_to(col, Instant::now()) {
                let (r, c) = view.content_dims();
                write_msg(sock_w, &ClientMsg::Resize { rows: r, cols: c })
                    .await
                    .map_err(|e| format!("feed resize send failed: {e}"))?;
            }
            Ok(true)
        }
        MouseKind::Release(MouseButton::Left) => {
            view.end_feed_drag(row, col);
            Ok(true)
        }
        _ => {
            view.end_feed_drag(row, col);
            Ok(true)
        }
    }
}

/// A bare Esc during a feed-border drag reverts the width to where the drag
/// began; `Ok(true)` = the Esc was the drag's.
pub(crate) async fn esc_revert(
    view: &mut View,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<bool, String> {
    if view.feed_drag.is_none() {
        return Ok(false);
    }
    if view.revert_feed_drag() {
        let (rows, cols) = view.content_dims();
        write_msg(sock_w, &ClientMsg::Resize { rows, cols })
            .await
            .map_err(|e| format!("feed revert resize send failed: {e}"))?;
    }
    Ok(true)
}

/// The `E` focus. Opens the panel when it is closed, then takes the keyboard
/// either way. This is the ONE place the key-free default is set aside, and
/// only ever on the operator's explicit gesture.
pub(crate) async fn focus(
    view: &mut View,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    if view.feed.is_none() {
        toggle(view, sock_w).await?;
    }
    view.region_owner = super::region_focus::RegionOwner::Feed;
    view.feed_esc.clear();
    Ok(())
}

/// The focused feed panel's keys, and the provenance view's.
///
/// Reached only when the operator asked for it: `E` focused the panel, or a
/// click opened a row's provenance. Every other time the panel takes no keys
/// at all and this function is never called, which is the whole point.
///
/// Precedence inside: the provenance view wins while it is open (it is the
/// thing in front), then the focused panel. Esc unwinds one layer at a time -
/// the view first, then the panel - so a reader never loses both at once, and
/// each landing keeps the keyboard with the feed. From the panel itself, Esc
/// and e close it; the old Esc-only release left the panel open but key-less,
/// a state no further key could leave.
pub(crate) async fn feed_keys(
    view: &mut View,
    bytes: &[u8],
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<StdinFlow, String> {
    let mut esc = std::mem::take(&mut view.feed_esc);
    let toks = fold_modal_keys(&mut esc, bytes);
    view.feed_esc = esc;
    for tok in toks {
        if view.feed_detail.is_some() {
            match tok {
                ModalKey::Esc | ModalKey::Byte(b'q') | ModalKey::Byte(b'e') => {
                    view.feed_detail = None;
                }
                ModalKey::Up => {
                    if let Some(m) = view.feed_detail.as_mut() {
                        m.popup.nav(crate::popup::NavDir::Up);
                        m.popup.follow_sel(view.term);
                    }
                }
                // The node_created modal's composer gesture, carried over
                // from the line-built view: b opens the agent composer
                // pre-filled with the node-bound blueprint command.
                ModalKey::Byte(b'b') => {
                    let launch = view.feed_detail.as_ref().and_then(|m| {
                        feed_detail::plan_node(&m.item)
                            .map(|node| (node.to_owned(), crate::feed_overlay::owned(&m.item.cwd)))
                    });
                    if let Some((node, cwd)) = launch {
                        if super::sideline::show_composer(view, sock_w).await? {
                            view.feed_detail = None;
                            if let Err(err) = super::agent_launcher::open_with(
                                view,
                                format!("/fno:blueprint {node}"),
                                cwd.as_deref(),
                                node,
                            ) {
                                view.set_notice(err);
                            }
                        }
                    }
                }
                // The selected row's action, never the modal's dismissal:
                // inspecting stays open so the next field is one arrow away.
                ModalKey::Down => {
                    if let Some(m) = view.feed_detail.as_mut() {
                        m.popup.nav(crate::popup::NavDir::Down);
                        m.popup.follow_sel(view.term);
                    }
                }
                ModalKey::Enter => feed_detail::execute_selected(view, sock_w).await?,
                ModalKey::Byte(b'y') => feed_detail::copy_selected(view),
                _ => {}
            }
            continue;
        }
        // The `?` overlay sits in front of the panel: Esc unwinds it first
        // and the panel stays open (the panel-close layering, AC13).
        if view.feed.as_ref().is_some_and(|f| f.keys_open) {
            match tok {
                ModalKey::Esc => {
                    if let Some(f) = view.feed.as_mut() {
                        f.keys_open = false;
                    }
                }
                ModalKey::Byte(b'?') => {
                    if let Some(f) = view.feed.as_mut() {
                        f.keys_open = false;
                    }
                }
                _ => {}
            }
            continue;
        }
        // The search bar takes the typing keys while it is open; Esc clears
        // the query and refolds unfiltered, and never closes the panel.
        if view.feed.as_ref().is_some_and(|f| f.bar_open) {
            match tok {
                ModalKey::Esc => {
                    let f = view.feed.as_mut().unwrap();
                    f.bar_open = false;
                    f.bar_debounce = None;
                    f.query_text.clear();
                    search::apply_query(f);
                }
                ModalKey::Enter => {
                    let f = view.feed.as_mut().unwrap();
                    f.bar_open = false;
                    f.bar_debounce = None;
                    search::apply_query(f);
                }
                ModalKey::Byte(0x09) => {
                    let f = view.feed.as_mut().unwrap();
                    let next = if f.win.items.is_empty() {
                        let pool: Vec<FeedItem> = f.cache.values().cloned().collect();
                        search::complete(&f.query_text, &pool)
                    } else {
                        search::complete(&f.query_text, &f.win.items)
                    };
                    if let Some(next) = next {
                        f.query_text = next;
                    }
                    f.bar_debounce = Some(Instant::now());
                }
                ModalKey::Byte(b'?') => {
                    if let Some(f) = view.feed.as_mut() {
                        f.keys_open = true;
                    }
                }
                ModalKey::Byte(b) if b == 0x7f || b == 0x08 => {
                    let f = view.feed.as_mut().unwrap();
                    f.query_text.pop();
                    f.bar_debounce = Some(Instant::now());
                }
                ModalKey::Byte(b) if b >= 0x20 => {
                    let f = view.feed.as_mut().unwrap();
                    f.query_text.push(b as char);
                    f.bar_debounce = Some(Instant::now());
                }
                _ => {}
            }
            continue;
        }
        // An open picker sits in front of the panel: the arrows move its
        // cursor, Enter picks (a second sort or group pick flips), Esc closes
        // it and leaves the panel open. A value list also takes typing as
        // its find text and space as a tick. Every other key is swallowed.
        if view.feed.as_ref().is_some_and(|f| f.picker.is_some()) {
            match tok {
                ModalKey::Up | ModalKey::Down => {
                    let term = view.term;
                    if let Some(p) = view.feed.as_mut().and_then(|f| f.picker.as_mut()) {
                        let dir = if tok == ModalKey::Up {
                            crate::popup::NavDir::Up
                        } else {
                            crate::popup::NavDir::Down
                        };
                        p.popup.nav(dir);
                        p.popup.follow_sel(term);
                    }
                }
                ModalKey::Enter => pick_open_picker(view),
                ModalKey::Esc => {
                    if let Some(f) = view.feed.as_mut() {
                        f.picker = None;
                    }
                }
                tok => {
                    if let Some(p) = view.feed.as_mut().and_then(|f| f.picker.as_mut()) {
                        match tok {
                            ModalKey::Byte(b' ') => p.toggle_selected(),
                            ModalKey::Byte(0x7f | 0x08) => p.backspace(),
                            ModalKey::Byte(b) if b > 0x20 && b < 0x7f => p.type_char(b as char),
                            _ => {}
                        }
                    }
                }
            }
            continue;
        }
        if matches!(tok, ModalKey::Esc | ModalKey::Byte(b'e')) {
            // Close once; a second close token in the same chunk is
            // swallowed, never a reopen.
            if view.feed.is_some() {
                toggle(view, sock_w).await?;
            }
            continue;
        }
        let picker_at = view.feed_picker_anchor();
        let Some(f) = view.feed.as_mut() else {
            break; // closed mid-chunk: swallow the rest, never forward
        };
        let len = f.slots.len();
        match tok {
            ModalKey::Esc => {}
            // Rows and group headers are both cursor stops.
            ModalKey::Up => {
                f.sel = f.sel.saturating_sub(1);
                view.follow_feed_selection();
            }
            ModalKey::Down => {
                f.sel = (f.sel + 1).min(len.saturating_sub(1));
                view.follow_feed_selection();
            }
            // Panning moves the TITLE only; the stamp, kind and node stay
            // anchored, so a panned row is still the row you selected.
            ModalKey::Left => f.hpan = f.hpan.saturating_sub(1),
            ModalKey::Right => {
                // One column short of the widest title. AT that width every
                // row is blank, so the pan would strand the operator in an
                // empty panel with nothing on screen to pan back by.
                let ceiling = f.widest_title().saturating_sub(1);
                f.hpan = (f.hpan + 1).min(ceiling);
            }
            // No split cells here: the folded Shift+arrow is inert, the
            // same swallow the raw sequence got before it was folded.
            ModalKey::ShiftArrow(_) => {}
            ModalKey::PageUp => {
                let page = feed_body_rows(view.term.0 as usize).max(1);
                f.sel = f.sel.saturating_sub(page);
                view.follow_feed_selection();
            }
            ModalKey::PageDown => {
                let page = feed_body_rows(view.term.0 as usize).max(1);
                f.sel = (f.sel + page).min(len.saturating_sub(1));
                view.follow_feed_selection();
            }
            // Enter (or Space) on a group header folds it; on a row it opens
            // the row's provenance or question.
            ModalKey::Enter | ModalKey::Byte(b' ') => match f.slots.get(f.sel) {
                Some(Slot::Header { key, .. }) => {
                    let key = key.clone();
                    change_view(view, |v| v.toggle_collapse(&key));
                    if let Some(f) = view.feed.as_mut() {
                        f.sel = f
                            .slots
                            .iter()
                            .position(|s| matches!(s, Slot::Header { key: k, .. } if *k == key))
                            .unwrap_or(f.sel);
                    }
                    view.follow_feed_selection();
                }
                Some(Slot::Item(_)) if tok == ModalKey::Enter => view.open_feed_detail(),
                _ => {}
            },
            // Fold every group, or unfold them all when all are folded.
            ModalKey::Byte(b'z') => {
                let keys: Vec<String> = f
                    .slots
                    .iter()
                    .filter_map(|s| match s {
                        Slot::Header { key, .. } => Some(key.clone()),
                        Slot::Item(_) => None,
                    })
                    .collect();
                let all = !keys.is_empty() && keys.iter().all(|k| f.view.collapsed.contains(k));
                change_view(view, |v| {
                    for k in keys {
                        if all {
                            v.collapsed.remove(&k);
                        } else {
                            v.collapsed.insert(k);
                        }
                    }
                });
            }
            // The first slot, clearing the new-row marker.
            ModalKey::Byte(b'g') => view.feed_home(),
            ModalKey::Byte(b'G') => {
                let visible = feed_body_rows(view.term.0 as usize).max(1);
                view.feed_offset = len.saturating_sub(visible);
                f.sel = len.saturating_sub(1);
            }
            ModalKey::Byte(b'?') => {
                f.keys_open = !f.keys_open;
            }
            ModalKey::Byte(b'/') => {
                f.bar_open = true;
                f.bar_debounce = None;
            }
            ModalKey::Byte(b'r') => {
                f.want = true;
            }
            ModalKey::Byte(b'o') => {
                f.picker = Some(picker::FeedPicker::open(
                    picker::PickerKind::Sort,
                    &f.view,
                    picker_at,
                ));
            }
            ModalKey::Byte(b'W') => {
                f.picker = Some(picker::FeedPicker::open(
                    picker::PickerKind::Group,
                    &f.view,
                    picker_at,
                ));
            }
            ModalKey::Byte(b'f') => {
                f.picker = Some(picker::FeedPicker::open(
                    picker::PickerKind::FilterCol,
                    &f.view,
                    picker_at,
                ));
            }
            // Clear every column filter; the query stays.
            ModalKey::Byte(b'x') => change_view(view, |v| v.filters.clear()),
            ModalKey::Byte(_) => {}
        }
    }
    Ok(StdinFlow::Continue)
}
