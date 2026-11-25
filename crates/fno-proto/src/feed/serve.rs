//! The held reader's pipe protocol: one JSON request per line in, one JSON
//! answer per line out. The mux client sends the view; `fno-agent fleet
//! feed --serve` holds the history, evaluates the view with this module's
//! grammar and table, and answers the slot list plus the rows the panel
//! paints. The client never holds the history, so its memory stays flat.

use super::row::FeedItem;
use super::table::{Col, Group, Slot, TableView};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A table view by name, as it crosses the pipe. A name this build does not
/// know drops that one choice back to its default, never the whole view.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewSpec {
    pub sort: String,
    pub sort_desc: bool,
    pub group: String,
    pub group_desc: bool,
    #[serde(default)]
    pub filters: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub collapsed: Vec<String>,
}

impl ViewSpec {
    /// The view by name: every choice, empty filters left out.
    pub fn of(view: &TableView) -> ViewSpec {
        ViewSpec {
            sort: view.sort.name().to_string(),
            sort_desc: view.sort_desc,
            group: view.group.name().to_string(),
            group_desc: view.group_desc,
            filters: view
                .filters
                .iter()
                .filter(|(_, set)| !set.is_empty())
                .map(|(c, set)| (c.name().to_string(), set.iter().cloned().collect()))
                .collect(),
            collapsed: view.collapsed.iter().cloned().collect(),
        }
    }

    /// The table view this names.
    pub fn view(&self) -> TableView {
        let mut view = TableView::default();
        if let Some(col) = Col::parse(&self.sort) {
            view.sort = col;
            view.sort_desc = self.sort_desc;
        }
        if let Some(group) = Group::parse(&self.group) {
            view.group = group;
            view.group_desc = self.group_desc;
        }
        for (name, vals) in &self.filters {
            if let Some(col) = Col::parse(name) {
                view.filters.insert(col, vals.iter().cloned().collect());
            }
        }
        view.collapsed = self.collapsed.iter().cloned().collect();
        view
    }
}

/// One request. With `slots`, the reader evaluates the view and the query
/// and answers the whole slot list; `fetch` names the storage indexes whose
/// rows the panel needs to paint; `values_for` asks one column's filter
/// value list.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub view: ViewSpec,
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub slots: bool,
    #[serde(default)]
    pub fetch: Vec<usize>,
    /// With `slots`: also answer the rows of the `rows` slots from `top`,
    /// so a view change paints after one round trip.
    #[serde(default)]
    pub top: usize,
    #[serde(default)]
    pub rows: usize,
    #[serde(default)]
    pub values_for: Option<String>,
}

/// The widest value of each capped column over the held rows, in display
/// columns, already capped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Widths {
    pub area: usize,
    pub harness: usize,
    pub kind: usize,
    pub node: usize,
    pub session: usize,
    pub lead: usize,
    /// The widest summary, uncapped: the horizontal pan's ceiling.
    pub title: usize,
}

impl Widths {
    /// Measure `items` with `width`, the caller's display-width function.
    pub fn of(items: &[FeedItem], width: fn(&str) -> usize) -> Widths {
        let tail = |s: &str| {
            let n = s.chars().count();
            s.chars().skip(n.saturating_sub(8)).collect::<String>()
        };
        let mut w = Widths::default();
        for it in items {
            w.area = w.area.max(width(&it.area));
            w.harness = w.harness.max(it.harness.as_deref().map_or(0, width));
            w.kind = w.kind.max(width(super::table::display_kind(&it.kind)));
            w.node = w.node.max(it.node.as_deref().map_or(0, width));
            w.session = w
                .session
                .max(it.session_id.as_deref().map_or(0, |s| width(&tail(s))));
            w.lead = w.lead.max(it.lead.as_deref().map_or(0, width));
            w.title = w.title.max(width(&it.title));
        }
        Widths {
            area: w.area.min(8),
            harness: w.harness.min(8),
            kind: w.kind.min(16),
            node: w.node.min(12),
            session: w.session.min(8),
            lead: w.lead.min(12),
            title: w.title,
        }
    }
}

/// One answer, carrying the request's id. `stamp` changes when the reader
/// re-projects, so the client drops rows it cached under an older stamp.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Answer {
    pub id: u64,
    pub stamp: u64,
    /// Every row the reader holds.
    pub total: usize,
    /// Rows the query and the column filters keep.
    #[serde(default)]
    pub matched: usize,
    /// The slot list, when the request asked for it.
    #[serde(default)]
    pub slots: Option<Vec<Slot>>,
    /// The fetched rows, by storage index.
    #[serde(default)]
    pub items: Vec<(usize, FeedItem)>,
    #[serde(default)]
    pub widths: Widths,
    /// The asked column's values with their row counts.
    #[serde(default)]
    pub values: Option<Vec<(String, usize)>>,
    /// The query's refusal, verbatim for the footer. The slots then hold
    /// the view without the query.
    #[serde(default)]
    pub error: Option<String>,
}

/// Answer one request over the held rows. The reader runs this per line; a
/// client holding its own rows (a shot seed, a test) runs the same call, so
/// both sides evaluate one way. `width` is the caller's display width.
pub fn answer(
    items: &[FeedItem],
    stamp: u64,
    req: &Request,
    now: i64,
    width: fn(&str) -> usize,
) -> Answer {
    let view = req.view.view();
    let parsed = (!req.query.trim().is_empty())
        .then(|| super::grammar::parse(&req.query, super::grammar::Surface::Event, now));
    let error = match &parsed {
        Some(Err(e)) => Some(e.to_string()),
        _ => None,
    };
    let query = match &parsed {
        Some(Ok(p)) => Some(p),
        _ => None,
    };
    let ctx = super::row::EventCtx::default();
    let keep = |it: &FeedItem| query.is_none_or(|q| q.keeps(&super::row::event_fields(it, &ctx)));
    let (slots, matched) = if req.slots {
        let (slots, matched) = super::table::evaluate(items, &view, keep);
        (Some(slots), matched)
    } else {
        (None, 0)
    };
    let window = slots
        .as_deref()
        .unwrap_or_default()
        .iter()
        .skip(req.top)
        .take(req.rows)
        .filter_map(|s| match s {
            Slot::Item(i) => Some(*i),
            Slot::Header { .. } => None,
        })
        .collect::<Vec<usize>>()
        .into_iter();
    let values = req
        .values_for
        .as_deref()
        .and_then(Col::parse)
        .map(|col| super::table::values(items, &view, col, keep));
    Answer {
        id: req.id,
        stamp,
        total: items.len(),
        matched,
        slots,
        items: window
            .chain(req.fetch.iter().copied())
            .filter_map(|i| Some((i, items.get(i)?.clone())))
            .collect(),
        widths: if req.slots {
            Widths::of(items, width)
        } else {
            Widths::default()
        },
        values,
        error,
    }
}
