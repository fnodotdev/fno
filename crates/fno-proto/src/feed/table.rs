//! The activity feed's table: eight columns, one view (sort, column filters,
//! group, collapsed groups) and the one evaluator every reader walks. The
//! panel holds the whole history, so sort, filter and group read every row,
//! never a loaded page.

use super::row::{FeedItem, IStr};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

/// One feed column, in the panel's left-to-right order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Col {
    Time,
    Area,
    Harness,
    Kind,
    Node,
    Session,
    Lead,
    Summary,
}

impl Col {
    pub const ALL: [Col; 8] = [
        Col::Time,
        Col::Area,
        Col::Harness,
        Col::Kind,
        Col::Node,
        Col::Session,
        Col::Lead,
        Col::Summary,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Col::Time => "time",
            Col::Area => "area",
            Col::Harness => "harness",
            Col::Kind => "kind",
            Col::Node => "node",
            Col::Session => "session",
            Col::Lead => "lead",
            Col::Summary => "summary",
        }
    }

    pub fn parse(s: &str) -> Option<Col> {
        Col::ALL.into_iter().find(|c| c.name() == s)
    }

    /// How a row with no value in this column reads, as a group header and
    /// in the filter's value list: `no area`, `no harness`.
    pub fn empty_label(self) -> String {
        format!("no {}", self.name())
    }

    /// The value a filter selects and a group collects: the local calendar
    /// day for time, the full id for session, the first line for summary,
    /// the stored value otherwise. Empty means the row has none.
    pub fn value(self, it: &FeedItem) -> String {
        match self {
            Col::Time => local_day(&it.ts),
            Col::Area => it.area.to_string(),
            Col::Harness => it
                .harness
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase(),
            Col::Kind => display_kind(&it.kind).to_string(),
            Col::Node => it.node.as_deref().unwrap_or_default().to_string(),
            Col::Session => it.session_id.as_deref().unwrap_or_default().to_string(),
            Col::Lead => it.lead.as_deref().unwrap_or_default().to_string(),
            Col::Summary => it.title.lines().next().unwrap_or_default().to_string(),
        }
    }

    /// The sort key. Time sorts by the parsed instant; summary ignores case.
    pub fn sort_key(self, it: &FeedItem) -> SortKey {
        match self {
            Col::Time => match chrono::DateTime::parse_from_rfc3339(&it.ts) {
                Ok(t) => SortKey::Instant(t.timestamp_millis()),
                Err(_) => SortKey::Empty,
            },
            Col::Summary => text_key(self.value(it).to_lowercase()),
            other => text_key(other.value(it)),
        }
    }

    /// A first pick sorts ascending, except time, which starts newest first.
    pub fn first_desc(self) -> bool {
        self == Col::Time
    }
}

fn local_day(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_default()
}

/// A column's sort key: an instant, text, or empty (always last).
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SortKey {
    Instant(i64),
    Text(String),
    Empty,
}

fn text_key(s: String) -> SortKey {
    if s.is_empty() {
        SortKey::Empty
    } else {
        SortKey::Text(s)
    }
}

/// What the rows group by. `LeadOrEpic` is the shipped rollup: the teams
/// band first, then one group per lead or epic, then `other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    None,
    LeadOrEpic,
    Col(Col),
}

impl Group {
    pub const ALL: [Group; 10] = [
        Group::None,
        Group::LeadOrEpic,
        Group::Col(Col::Time),
        Group::Col(Col::Area),
        Group::Col(Col::Harness),
        Group::Col(Col::Kind),
        Group::Col(Col::Node),
        Group::Col(Col::Session),
        Group::Col(Col::Lead),
        Group::Col(Col::Summary),
    ];

    pub fn name(self) -> &'static str {
        match self {
            Group::None => "none",
            Group::LeadOrEpic => "lead or epic",
            Group::Col(c) => c.name(),
        }
    }

    pub fn parse(s: &str) -> Option<Group> {
        Group::ALL.into_iter().find(|g| g.name() == s)
    }

    /// Groups ordered by their newest row start newest first, as do day
    /// groups; value groups start ascending.
    pub fn first_desc(self) -> bool {
        matches!(self, Group::LeadOrEpic | Group::Col(Col::Time))
    }
}

/// One table view: one sort column, the column filters (OR inside a
/// column, AND across columns), the group and its collapsed groups.
#[derive(Debug, Clone, PartialEq)]
pub struct TableView {
    pub sort: Col,
    pub sort_desc: bool,
    pub group: Group,
    pub group_desc: bool,
    pub filters: BTreeMap<Col, BTreeSet<String>>,
    /// Collapsed group keys, `<group name>=<value>`.
    pub collapsed: BTreeSet<String>,
}

impl Default for TableView {
    fn default() -> Self {
        TableView {
            sort: Col::Time,
            sort_desc: true,
            group: Group::LeadOrEpic,
            group_desc: true,
            filters: BTreeMap::new(),
            collapsed: BTreeSet::new(),
        }
    }
}

impl TableView {
    /// Pick a sort column: a second pick of the active one flips it.
    pub fn pick_sort(&mut self, col: Col) {
        if self.sort == col {
            self.sort_desc = !self.sort_desc;
        } else {
            self.sort = col;
            self.sort_desc = col.first_desc();
        }
    }

    /// Pick a grouping: a second pick of the active one flips group order.
    pub fn pick_group(&mut self, group: Group) {
        if self.group == group {
            self.group_desc = !self.group_desc;
        } else {
            self.group = group;
            self.group_desc = group.first_desc();
        }
    }

    /// Collapse or expand one group by its key.
    pub fn toggle_collapse(&mut self, key: &str) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.to_string());
        }
    }

    /// True when the row passes every column filter but `except`'s.
    fn keeps(&self, it: &FeedItem, except: Option<Col>) -> bool {
        self.filters
            .iter()
            .filter(|(c, set)| Some(**c) != except && !set.is_empty())
            .all(|(c, set)| set.contains(&c.value(it)))
    }

    /// The number of active column filters.
    pub fn filter_count(&self) -> usize {
        self.filters.values().filter(|s| !s.is_empty()).count()
    }
}

/// One display slot: a group header or a row as its STORAGE index into the
/// items (oldest first). The one list paint, hit test, hover and keys walk,
/// so a header never opens a detail.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Slot {
    Header {
        key: String,
        label: String,
        count: usize,
        collapsed: bool,
    },
    Item(usize),
}

/// The view over the whole history: the rows `keep` admits and every column
/// filter passes, sorted, then grouped. The second value is the matched row
/// count, collapsed groups included.
pub fn evaluate(
    items: &[FeedItem],
    view: &TableView,
    keep: impl Fn(&FeedItem) -> bool,
) -> (Vec<Slot>, usize) {
    let mut rows: Vec<(usize, SortKey)> = items
        .iter()
        .enumerate()
        .filter(|(_, it)| view.keeps(it, None) && keep(it))
        .map(|(i, it)| (i, view.sort.sort_key(it)))
        .collect();
    rows.sort_by(|a, b| row_order(a, b, view.sort_desc));
    let matched = rows.len();
    let order: Vec<usize> = rows.into_iter().map(|(i, _)| i).collect();
    if view.group == Group::None {
        return (order.into_iter().map(Slot::Item).collect(), matched);
    }
    // Groups in first-seen order of the sorted rows, so a group's rows keep
    // the sort inside it.
    let mut groups: Vec<(String, String, Vec<usize>)> = Vec::new();
    let mut at: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (i, it) in order.into_iter().filter_map(|i| Some((i, items.get(i)?))) {
        let (key, label) = group_of(it, view.group);
        let slot = *at.entry(key.clone()).or_insert_with(|| {
            groups.push((key, label, Vec::new()));
            groups.len() - 1
        });
        if let Some(g) = groups.get_mut(slot) {
            g.2.push(i);
        }
    }
    sort_groups(&mut groups, items, view);
    let mut slots = Vec::new();
    for (key, label, members) in groups {
        let collapsed = view.collapsed.contains(&key);
        slots.push(Slot::Header {
            key,
            label,
            count: members.len(),
            collapsed,
        });
        if !collapsed {
            slots.extend(members.into_iter().map(Slot::Item));
        }
    }
    (slots, matched)
}

/// Empty sorts last in both directions; ties fall to newest first, then the
/// projection's total order (storage order is that order, ascending).
fn row_order(a: &(usize, SortKey), b: &(usize, SortKey), desc: bool) -> Ordering {
    let empty = |k: &SortKey| *k == SortKey::Empty;
    match (empty(&a.1), empty(&b.1)) {
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        _ => {
            let o = a.1.cmp(&b.1);
            if desc { o.reverse() } else { o }.then_with(|| b.0.cmp(&a.0))
        }
    }
}

const TEAMS: &str = "teams";
const OTHER: &str = "other";

/// A row's group key and label under one grouping.
fn group_of(it: &FeedItem, group: Group) -> (String, String) {
    let label = match group {
        Group::None => String::new(),
        Group::LeadOrEpic if in_teams_band(&it.kind, &it.team) => TEAMS.to_string(),
        Group::LeadOrEpic => it.owner.as_deref().unwrap_or(OTHER).to_string(),
        Group::Col(c) => {
            let v = c.value(it);
            if v.is_empty() {
                c.empty_label()
            } else {
                v
            }
        }
    };
    (format!("{}={label}", group.name()), label)
}

/// Group order. Lead or epic: the teams band first, `other` last, the rest
/// by their newest row. A column: by value, with the empty group last.
fn sort_groups(groups: &mut [(String, String, Vec<usize>)], items: &[FeedItem], view: &TableView) {
    let desc = view.group_desc;
    let newest = |g: &(String, String, Vec<usize>)| {
        g.2.iter()
            .filter_map(|i| Some((ts_key_of(&items.get(*i)?.ts), *i)))
            .max()
    };
    match view.group {
        Group::None => {}
        Group::LeadOrEpic => groups.sort_by(|a, b| {
            let rank = |g: &(String, String, Vec<usize>)| match g.1.as_str() {
                TEAMS => 0,
                OTHER => 2,
                _ => 1,
            };
            rank(a).cmp(&rank(b)).then_with(|| {
                let o = newest(a).cmp(&newest(b));
                if desc {
                    o.reverse()
                } else {
                    o
                }
            })
        }),
        Group::Col(c) => groups.sort_by(|a, b| {
            let empty_label = c.empty_label();
            let empty = |g: &(String, String, Vec<usize>)| g.1 == empty_label;
            empty(a).cmp(&empty(b)).then_with(|| {
                let o = a.1.cmp(&b.1);
                if desc {
                    o.reverse()
                } else {
                    o
                }
            })
        }),
    }
}

/// The value list for one column's filter: each value with its row count
/// under every OTHER filter and `keep`, most rows first. A selected value
/// stays listed at count 0, and the empty string is the empty value.
pub fn values(
    items: &[FeedItem],
    view: &TableView,
    col: Col,
    keep: impl Fn(&FeedItem) -> bool,
) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for it in items
        .iter()
        .filter(|it| view.keeps(it, Some(col)) && keep(it))
    {
        *counts.entry(col.value(it)).or_default() += 1;
    }
    for v in view.filters.get(&col).into_iter().flatten() {
        counts.entry(v.clone()).or_default();
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// True when a row renders in the teams band: a team kind, or a removal
/// that names the team it held.
pub fn in_teams_band(kind: &str, team: &Option<IStr>) -> bool {
    kind == "team_granted" || kind == "team_vacated" || (kind == "session_reaped" && team.is_some())
}

/// ts sort key, the projection's own shape.
pub fn ts_key_of(ts: &str) -> (u8, i64) {
    match chrono::DateTime::parse_from_rfc3339(ts) {
        Ok(t) => (1, t.timestamp_millis()),
        Err(_) => (0, 0),
    }
}

/// The kind as printed. The ship event renamed to `node_shipped` (ruling
/// 2026-09-29); the projection derives rows at read time, so only a string
/// embedded in an older surface spells the old name - this is the one reader.
pub fn display_kind(kind: &str) -> &str {
    if kind == "pr_created" {
        "node_shipped"
    } else {
        kind
    }
}
