//! The activity feed's table: eight columns, one view (sort, column filters,
//! group, collapsed groups) and the one evaluator every reader walks. The
//! panel holds the whole history, so sort, filter and group read every row,
//! never a loaded page.

use crate::feed_overlay::FeedItem;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

/// One feed column, in the panel's left-to-right order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Col {
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
    pub(crate) const ALL: [Col; 8] = [
        Col::Time,
        Col::Area,
        Col::Harness,
        Col::Kind,
        Col::Node,
        Col::Session,
        Col::Lead,
        Col::Summary,
    ];

    pub(crate) fn name(self) -> &'static str {
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

    pub(crate) fn parse(s: &str) -> Option<Col> {
        Col::ALL.into_iter().find(|c| c.name() == s)
    }

    /// The value a filter selects and a group collects: the local calendar
    /// day for time, the full id for session, the first line for summary,
    /// the stored value otherwise. Empty means the row has none.
    pub(crate) fn value(self, it: &FeedItem) -> String {
        match self {
            Col::Time => local_day(&it.ts),
            Col::Area => it.area.clone(),
            Col::Harness => it
                .harness
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase(),
            Col::Kind => super::display_kind(&it.kind).to_string(),
            Col::Node => it.node.clone().unwrap_or_default(),
            Col::Session => it.session_id.clone().unwrap_or_default(),
            Col::Lead => it.lead.clone().unwrap_or_default(),
            Col::Summary => it.title.lines().next().unwrap_or_default().to_string(),
        }
    }

    /// The sort key. Time sorts by the parsed instant; summary ignores case.
    fn sort_key(self, it: &FeedItem) -> SortKey {
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
    pub(crate) fn first_desc(self) -> bool {
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

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SortKey {
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
pub(crate) enum Group {
    None,
    LeadOrEpic,
    Col(Col),
}

impl Group {
    pub(crate) const ALL: [Group; 10] = [
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

    pub(crate) fn name(self) -> &'static str {
        match self {
            Group::None => "none",
            Group::LeadOrEpic => "lead or epic",
            Group::Col(c) => c.name(),
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Group> {
        Group::ALL.into_iter().find(|g| g.name() == s)
    }

    /// Groups ordered by their newest row start newest first, as do day
    /// groups; value groups start ascending.
    pub(crate) fn first_desc(self) -> bool {
        matches!(self, Group::LeadOrEpic | Group::Col(Col::Time))
    }
}

/// One table view: one sort column, the column filters (OR inside a
/// column, AND across columns), the group and its collapsed groups.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TableView {
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
    pub(crate) fn pick_sort(&mut self, col: Col) {
        if self.sort == col {
            self.sort_desc = !self.sort_desc;
        } else {
            self.sort = col;
            self.sort_desc = col.first_desc();
        }
    }

    /// Pick a grouping: a second pick of the active one flips group order.
    pub(crate) fn pick_group(&mut self, group: Group) {
        if self.group == group {
            self.group_desc = !self.group_desc;
        } else {
            self.group = group;
            self.group_desc = group.first_desc();
        }
    }

    /// Collapse or expand one group by its key.
    pub(crate) fn toggle_collapse(&mut self, key: &str) {
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
    pub(crate) fn filter_count(&self) -> usize {
        self.filters.values().filter(|s| !s.is_empty()).count()
    }
}

/// One display slot: a group header or a row as its STORAGE index into the
/// items (oldest first). The one list paint, hit test, hover and keys walk,
/// so a header never opens a detail.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Slot {
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
pub(crate) fn evaluate(
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
const EMPTY_LABEL: &str = "(empty)";

/// A row's group key and label under one grouping.
fn group_of(it: &FeedItem, group: Group) -> (String, String) {
    let label = match group {
        Group::None => String::new(),
        Group::LeadOrEpic if super::in_teams_band(&it.kind, &it.team) => TEAMS.to_string(),
        Group::LeadOrEpic => it.owner.clone().unwrap_or_else(|| OTHER.to_string()),
        Group::Col(c) => {
            let v = c.value(it);
            if v.is_empty() {
                EMPTY_LABEL.to_string()
            } else {
                v
            }
        }
    };
    (format!("{}={label}", group.name()), label)
}

/// Group order. Lead or epic: the teams band first, `other` last, the rest
/// by their newest row. A column: by value, with `(empty)` last.
fn sort_groups(groups: &mut [(String, String, Vec<usize>)], items: &[FeedItem], view: &TableView) {
    let desc = view.group_desc;
    let newest = |g: &(String, String, Vec<usize>)| {
        g.2.iter()
            .filter_map(|i| Some((super::ts_key_of(&items.get(*i)?.ts), *i)))
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
        Group::Col(_) => groups.sort_by(|a, b| {
            let empty = |g: &(String, String, Vec<usize>)| g.1 == EMPTY_LABEL;
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
/// stays listed at count 0, and `(empty)` is the empty value.
pub(crate) fn values(
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture with an empty value in every column but time, so each
    /// column's empty-last rule and `(empty)` group are exercised.
    fn rows() -> Vec<FeedItem> {
        let mk = |i: usize, area: &str, h: Option<&str>, kind: &str, node: Option<&str>| FeedItem {
            ts: format!("2026-09-0{}T1{}:00:00Z", 1 + i % 3, i),
            kind: kind.into(),
            area: area.into(),
            harness: h.map(str::to_string),
            node: node.map(str::to_string),
            session_id: (i % 2 == 0).then(|| format!("s-{i:04}")),
            lead: (i % 3 != 0).then(|| format!("lead-{}", i % 3)),
            title: if i == 4 {
                String::new()
            } else {
                format!("Event {}", 9 - i)
            },
            cursor: format!("c{i}"),
            ..Default::default()
        };
        vec![
            mk(0, "mail", Some("claude"), "question_asked", Some("x-aa01")),
            mk(1, "agents", Some("codex"), "session_spawned", None),
            mk(2, "backlog", None, "node_created", Some("x-aa02")),
            mk(3, "", Some("Claude"), "node_started", Some("x-aa01")),
            mk(4, "ship", Some("opencode"), "node_shipped", Some("x-aa03")),
            mk(5, "mail", None, "question_closed", None),
        ]
    }

    fn items_of(slots: &[Slot]) -> Vec<usize> {
        slots
            .iter()
            .filter_map(|s| match s {
                Slot::Item(i) => Some(*i),
                Slot::Header { .. } => None,
            })
            .collect()
    }

    /// The done contract: all 24 column operations, each of eight columns
    /// sorted both ways, filtered on two values, and grouped, over the whole
    /// history the panel holds.
    #[test]
    fn every_feed_column_sorts_filters_and_groups() {
        let items = rows();
        for col in Col::ALL {
            // Sort: both directions order by the column, empty last.
            for desc in [false, true] {
                let view = TableView {
                    sort: col,
                    sort_desc: desc,
                    group: Group::None,
                    ..Default::default()
                };
                let (slots, matched) = evaluate(&items, &view, |_| true);
                assert_eq!(matched, items.len(), "{col:?}: sort keeps every row");
                let keys: Vec<SortKey> = items_of(&slots)
                    .into_iter()
                    .filter_map(|i| items.get(i).map(|it| col.sort_key(it)))
                    .collect();
                let first_empty = keys.iter().position(|k| *k == SortKey::Empty);
                if let Some(at) = first_empty {
                    assert!(
                        keys.iter().skip(at).all(|k| *k == SortKey::Empty),
                        "{col:?} desc={desc}: empty sorts last: {keys:?}"
                    );
                }
                let filled = keys.iter().take(first_empty.unwrap_or(keys.len()));
                assert!(
                    filled.clone().zip(filled.skip(1)).all(|(a, b)| if desc {
                        a >= b
                    } else {
                        a <= b
                    }),
                    "{col:?} desc={desc}: ordered by the column: {keys:?}"
                );
            }

            // Filter: two values of the column keep exactly their rows.
            let mut distinct: Vec<String> = items.iter().map(|it| col.value(it)).collect();
            distinct.sort();
            distinct.dedup();
            let picked: BTreeSet<String> = distinct.iter().take(2).cloned().collect();
            let mut view = TableView {
                group: Group::None,
                ..Default::default()
            };
            view.filters.insert(col, picked.clone());
            let (slots, _) = evaluate(&items, &view, |_| true);
            let kept = items_of(&slots);
            let want: Vec<usize> = items
                .iter()
                .enumerate()
                .filter(|(_, it)| picked.contains(&col.value(it)))
                .map(|(i, _)| i)
                .collect();
            let mut got = kept.clone();
            got.sort();
            assert_eq!(got, want, "{col:?}: a two-value filter keeps their rows");

            // Group: every row in exactly one group; counts sum to matched.
            let view = TableView {
                group: Group::Col(col),
                group_desc: false,
                ..Default::default()
            };
            let (slots, matched) = evaluate(&items, &view, |_| true);
            let mut seen = items_of(&slots);
            seen.sort();
            assert_eq!(seen, (0..items.len()).collect::<Vec<_>>(), "{col:?}");
            let counts: usize = slots
                .iter()
                .map(|s| match s {
                    Slot::Header { count, .. } => *count,
                    Slot::Item(_) => 0,
                })
                .sum();
            assert_eq!(counts, matched, "{col:?}: header counts sum to matched");
            if distinct.iter().any(String::is_empty) {
                assert!(
                    matches!(slots.iter().rev().find(|s| matches!(s, Slot::Header { .. })),
                        Some(Slot::Header { label, .. }) if label == EMPTY_LABEL),
                    "{col:?}: the (empty) group sorts last"
                );
            }
        }

        // A collapsed group contributes its header alone; a second column's
        // filter ANDs; a value list counts under the other filters and keeps
        // a selected value at count 0.
        let mut view = TableView {
            group: Group::Col(Col::Area),
            group_desc: false,
            ..Default::default()
        };
        view.toggle_collapse("area=mail");
        let (slots, matched) = evaluate(&items, &view, |_| true);
        assert_eq!(matched, items.len(), "a collapsed group still counts");
        assert!(slots.contains(&Slot::Header {
            key: "area=mail".into(),
            label: "mail".into(),
            count: 2,
            collapsed: true,
        }));
        assert!(!items_of(&slots).contains(&0), "a collapsed row is hidden");
        view.filters
            .insert(Col::Harness, BTreeSet::from(["claude".to_string()]));
        view.filters.insert(
            Col::Node,
            BTreeSet::from(["x-aa01".to_string(), "x-zz99".to_string()]),
        );
        view.collapsed.clear();
        let (slots, _) = evaluate(&items, &view, |_| true);
        let mut kept = items_of(&slots);
        kept.sort();
        assert_eq!(kept, [0, 3], "harness claude (case-blind) AND node x-aa01");
        let vals = values(&items, &view, Col::Node, |_| true);
        assert_eq!(vals.first(), Some(&("x-aa01".to_string(), 2)));
        assert!(vals.contains(&("x-zz99".to_string(), 0)), "{vals:?}");

        // A second pick flips; a new column starts at its first direction.
        let mut view = TableView::default();
        view.pick_sort(Col::Time);
        assert!(!view.sort_desc, "a second time pick flips to oldest first");
        view.pick_sort(Col::Kind);
        assert_eq!((view.sort, view.sort_desc), (Col::Kind, false));
        view.pick_group(Group::Col(Col::Kind));
        assert!(!view.group_desc);
        view.pick_group(Group::Col(Col::Kind));
        assert!(view.group_desc, "a second group pick flips group order");
    }
}
