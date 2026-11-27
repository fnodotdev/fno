//! The activity feed's table lives in `fno_proto::feed::table`, shared with
//! the fno-agent reader. This module adds the one client-only part: the view
//! as the store keeps it.

#[cfg(test)]
use crate::feed_overlay::FeedItem;
pub(crate) use fno_proto::feed::table::*;
#[cfg(test)]
use std::collections::BTreeSet;

/// The view as the store keeps it: every choice by name.
pub(crate) fn to_pref(view: &TableView) -> crate::view_store::FeedViewPref {
    crate::view_store::FeedViewPref {
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

/// A saved view. A name this build does not know drops that one choice
/// back to its default, never the whole view.
pub(crate) fn from_pref(pref: &crate::view_store::FeedViewPref) -> TableView {
    let mut view = TableView::default();
    if let Some(col) = Col::parse(&pref.sort) {
        view.sort = col;
        view.sort_desc = pref.sort_desc;
    }
    if let Some(group) = Group::parse(&pref.group) {
        view.group = group;
        view.group_desc = pref.group_desc;
    }
    for (name, vals) in &pref.filters {
        if let Some(col) = Col::parse(name) {
            view.filters.insert(col, vals.iter().cloned().collect());
        }
    }
    view.collapsed = pref.collapsed.iter().cloned().collect();
    view
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture with an empty value in every column but time, so each
    /// column's empty-last rule and empty group are exercised.
    fn rows() -> Vec<FeedItem> {
        let mk = |i: usize, area: &str, h: Option<&str>, kind: &str, node: Option<&str>| FeedItem {
            ts: format!("2026-09-0{}T1{}:00:00Z", 1 + i % 3, i),
            kind: kind.into(),
            area: area.into(),
            harness: h.map(Into::into),
            node: node.map(Into::into),
            session_id: (i % 2 == 0).then(|| format!("s-{i:04}").into()),
            lead: (i % 3 != 0).then(|| format!("lead-{}", i % 3).into()),
            title: if i == 4 {
                String::new()
            } else {
                format!("Event {}", 9 - i)
            },
            cursor: format!("c{i}"),
            ..Default::default()
        };
        vec![
            mk(0, "mail", Some("claude"), "question_asked", Some("x-aaaa")),
            mk(1, "agents", Some("codex"), "session_spawned", None),
            mk(2, "backlog", None, "node_created", Some("x-bbbb")),
            mk(3, "", Some("Claude"), "node_started", Some("x-aaaa")),
            mk(4, "ship", Some("opencode"), "node_shipped", Some("x-cccc")),
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
                        Some(Slot::Header { label, .. }) if *label == col.empty_label()),
                    "{col:?}: the empty group sorts last"
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
            BTreeSet::from(["x-aaaa".to_string(), "x-dddd".to_string()]),
        );
        view.collapsed.clear();
        let (slots, _) = evaluate(&items, &view, |_| true);
        let mut kept = items_of(&slots);
        kept.sort();
        assert_eq!(kept, [0, 3], "harness claude (case-blind) AND node x-aaaa");
        let vals = values(&items, &view, Col::Node, |_| true);
        assert_eq!(vals.first(), Some(&("x-aaaa".to_string(), 2)));
        assert!(vals.contains(&("x-dddd".to_string(), 0)), "{vals:?}");

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

        // The saved view round-trips by name; an unknown name drops only
        // its own choice back to the default.
        view.filters
            .insert(Col::Area, BTreeSet::from(["ci".to_string()]));
        view.collapsed.insert("kind=pr_created".into());
        let pref = to_pref(&view);
        assert_eq!(from_pref(&pref), view, "the view round-trips");
        let mut odd = pref.clone();
        odd.sort = "colour".into();
        odd.filters.insert("colour".into(), vec!["red".into()]);
        let back = from_pref(&odd);
        assert_eq!(back.sort, Col::Time, "an unknown sort reads as the default");
        assert_eq!(back.group, view.group, "the known group survives");
        assert_eq!(back.filters, view.filters, "an unknown filter column drops");
    }
}
