//! The feed's pickers: one popup of choices under the column header. Sort
//! and group list every column, hidden ones included, so a narrow panel
//! still sorts and groups by any of the eight; the active choice wears its
//! direction arrow, and picking it again flips that direction. The filter
//! picker is two steps: a column, then that column's values with their row
//! counts, where typing narrows the list and space ticks a value.

use super::table::{Col, Group, TableView};
use crate::feed_overlay::FeedItem;
use crate::popup::{Anchor, Popup, PopupRow};
use std::collections::BTreeSet;

/// The most values one list paints; typing narrows past it.
const MAX_VALUES: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickerKind {
    Sort,
    Group,
    /// Step one of a filter: which column.
    FilterCol,
    /// Step two: the values of one column.
    Values(Col),
}

pub(crate) struct FeedPicker {
    pub kind: PickerKind,
    pub popup: Popup,
    anchor: Anchor,
    /// Every value of the column with its count, for `Values`.
    values: Vec<(String, usize)>,
    /// The values the list shows under `find`, in order.
    shown: Vec<String>,
    /// The typed narrowing text, matched case-blind anywhere in a value.
    find: String,
    /// The ticked values; Enter makes them the column's filter.
    picked: BTreeSet<String>,
    /// True once a tick or untick happened: Enter then applies the ticks
    /// exactly, so unticking every value clears the column.
    touched: bool,
}

/// What an Enter on a picker asks for next.
#[derive(Debug, PartialEq)]
pub(crate) enum Picked {
    /// The view changed, or nothing was selected.
    Done,
    /// The filter's first step chose a column: open its values.
    Values(Col),
}

fn entry(glyph: &str, label: &str, hint: String) -> PopupRow {
    PopupRow::Entry {
        glyph: glyph.into(),
        label: label.into(),
        hint,
        enabled: true,
    }
}

impl FeedPicker {
    fn bare(kind: PickerKind, popup: Popup, anchor: Anchor) -> Self {
        FeedPicker {
            kind,
            popup,
            anchor,
            values: Vec::new(),
            shown: Vec::new(),
            find: String::new(),
            picked: BTreeSet::new(),
            touched: false,
        }
    }

    /// The sort, group or filter-column picker over `view`, its cursor on
    /// the active choice.
    pub(crate) fn open(kind: PickerKind, view: &TableView, anchor: Anchor) -> Self {
        let arrow = |active: bool, desc: bool| match (active, desc) {
            (false, _) => " ",
            (true, true) => "↓",
            (true, false) => "↑",
        };
        let (title, footer, rows, active) = match kind {
            PickerKind::Sort => (
                "sort by",
                "enter pick · again flips · esc close",
                Col::ALL
                    .iter()
                    .map(|c| {
                        entry(
                            arrow(view.sort == *c, view.sort_desc),
                            c.name(),
                            String::new(),
                        )
                    })
                    .collect::<Vec<_>>(),
                Col::ALL.iter().position(|c| *c == view.sort),
            ),
            PickerKind::Group => (
                "group by",
                "enter pick · again flips · esc close",
                Group::ALL
                    .iter()
                    .map(|g| {
                        entry(
                            arrow(view.group == *g, view.group_desc),
                            g.name(),
                            String::new(),
                        )
                    })
                    .collect(),
                Group::ALL.iter().position(|g| *g == view.group),
            ),
            PickerKind::FilterCol | PickerKind::Values(_) => (
                "filter by",
                "enter values · esc close",
                Col::ALL
                    .iter()
                    .map(|c| {
                        let n = view.filters.get(c).map_or(0, |s| s.len());
                        let (glyph, hint) = if n > 0 {
                            ("●", format!("{n} picked"))
                        } else {
                            (" ", String::new())
                        };
                        entry(glyph, c.name(), hint)
                    })
                    .collect(),
                None,
            ),
        };
        let kind = match kind {
            PickerKind::Values(_) => PickerKind::FilterCol,
            k => k,
        };
        let mut popup = Popup::new(rows, anchor)
            .title(title)
            .footer(footer)
            .width_cap(60);
        popup.select(active.unwrap_or(0));
        Self::bare(kind, popup, anchor)
    }

    /// The value list of `col`: every value the other filters and `keep`
    /// leave, with its row count, the column's current filter ticked.
    pub(crate) fn open_values(
        col: Col,
        items: &[FeedItem],
        view: &TableView,
        keep: impl Fn(&FeedItem) -> bool,
        anchor: Anchor,
    ) -> Self {
        Self::open_with_values(
            col,
            super::table::values(items, view, col, keep),
            view,
            anchor,
        )
    }

    /// Open the value list from counts already in hand: the held reader's
    /// answer for the live panel.
    pub(crate) fn open_with_values(
        col: Col,
        values: Vec<(String, usize)>,
        view: &TableView,
        anchor: Anchor,
    ) -> Self {
        let mut p = Self::bare(
            PickerKind::Values(col),
            Popup::new(Vec::new(), anchor),
            anchor,
        );
        p.values = values;
        p.picked = view.filters.get(&col).cloned().unwrap_or_default();
        p.rebuild(0);
        p
    }

    /// Repaint the value list under the current find text and ticks, the
    /// cursor on the `sel`-th shown value.
    fn rebuild(&mut self, sel: usize) {
        let PickerKind::Values(col) = self.kind else {
            return;
        };
        let needle = self.find.to_lowercase();
        let empty_label = col.empty_label();
        let hits: Vec<&(String, usize)> = self
            .values
            .iter()
            .filter(|(v, _)| needle.is_empty() || v.to_lowercase().contains(&needle))
            .collect();
        self.shown = hits
            .iter()
            .take(MAX_VALUES)
            .map(|(v, _)| v.clone())
            .collect();
        let mut rows: Vec<PopupRow> = hits
            .iter()
            .take(MAX_VALUES)
            .map(|(v, n)| {
                let glyph = if self.picked.contains(v) { "●" } else { " " };
                let label = if v.is_empty() {
                    empty_label.as_str()
                } else {
                    v.as_str()
                };
                entry(glyph, label, n.to_string())
            })
            .collect();
        if hits.len() > MAX_VALUES {
            rows.push(PopupRow::Text(format!(
                "{} more - type to narrow",
                hits.len() - MAX_VALUES
            )));
        }
        if hits.is_empty() {
            rows.push(PopupRow::Text("no value matches".into()));
        }
        let title = if self.find.is_empty() {
            format!("filter {}", col.name())
        } else {
            format!("filter {} · {}", col.name(), self.find)
        };
        self.popup = Popup::new(rows, self.anchor)
            .title(title)
            .footer("type find · space tick · enter apply · esc close")
            .width_cap(60);
        self.popup
            .select(sel.min(self.shown.len().saturating_sub(1)));
    }

    /// The value under the cursor, for `Values`.
    fn selected_value(&self) -> Option<&String> {
        let (row, _) = self.popup.selected()?;
        self.shown.get(row)
    }

    /// Space on a value list: tick or untick the value under the cursor.
    pub(crate) fn toggle_selected(&mut self) {
        let Some(v) = self.selected_value().cloned() else {
            return;
        };
        if !self.picked.remove(&v) {
            self.picked.insert(v);
        }
        self.touched = true;
        let at = self.popup.selected().map_or(0, |(row, _)| row);
        self.rebuild(at);
    }

    /// A typed character narrows a value list; other pickers ignore it.
    pub(crate) fn type_char(&mut self, c: char) {
        if matches!(self.kind, PickerKind::Values(_)) {
            self.find.push(c);
            self.rebuild(0);
        }
    }

    /// Backspace on a value list's find text.
    pub(crate) fn backspace(&mut self) {
        if matches!(self.kind, PickerKind::Values(_)) && self.find.pop().is_some() {
            self.rebuild(0);
        }
    }

    /// Apply the selected choice to `view`. A value list's Enter makes the
    /// ticked values the column's filter. With no tick ever changed and none
    /// held, it takes the value under the cursor; an empty pick clears the
    /// column, so unticking every value removes that column's filter.
    pub(crate) fn pick(&self, view: &mut TableView) -> Picked {
        let row = self.popup.selected().map(|(row, _)| row);
        match self.kind {
            PickerKind::Sort => {
                if let Some(col) = row.and_then(|r| Col::ALL.get(r)) {
                    view.pick_sort(*col);
                }
            }
            PickerKind::Group => {
                if let Some(group) = row.and_then(|r| Group::ALL.get(r)) {
                    view.pick_group(*group);
                }
            }
            PickerKind::FilterCol => {
                if let Some(col) = row.and_then(|r| Col::ALL.get(r)) {
                    return Picked::Values(*col);
                }
            }
            PickerKind::Values(col) => {
                let mut set = self.picked.clone();
                if set.is_empty() && !self.touched {
                    set.extend(self.selected_value().cloned());
                }
                if set.is_empty() {
                    view.filters.remove(&col);
                } else {
                    view.filters.insert(col, set);
                }
            }
        }
        Picked::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(node: &str, harness: Option<&str>) -> FeedItem {
        FeedItem {
            ts: "2026-09-01T00:00:00Z".into(),
            kind: "node_created".into(),
            node: Some(node.into()),
            harness: harness.map(Into::into),
            ..Default::default()
        }
    }

    /// The filter picker's two steps: a column, then its values with counts;
    /// typing narrows, space ticks, Enter makes the ticks the filter, and
    /// an Enter with nothing ticked and no match clears the column.
    #[test]
    fn filter_picker_narrows_ticks_and_applies() {
        let items = vec![
            item("x-aaaa", Some("claude")),
            item("x-aaaa", None),
            item("x-bbbb", Some("codex")),
        ];
        let mut view = TableView::default();
        let at = Anchor::Center;
        let cols = FeedPicker::open(PickerKind::FilterCol, &view, at);
        let mut cols = cols;
        cols.popup.select(
            Col::ALL
                .iter()
                .position(|c| *c == Col::Harness)
                .unwrap_or(0),
        );
        assert_eq!(cols.pick(&mut view), Picked::Values(Col::Harness));

        let mut vals = FeedPicker::open_values(Col::Harness, &items, &view, |_| true, at);
        assert_eq!(
            vals.shown,
            ["", "claude", "codex"],
            "every value, empty too"
        );
        vals.type_char('c');
        vals.type_char('o');
        assert_eq!(vals.shown, ["codex"], "typing narrows case-blind");
        vals.toggle_selected();
        vals.backspace();
        vals.backspace();
        assert_eq!(vals.shown.len(), 3, "the find clears back to every value");
        assert_eq!(vals.pick(&mut view), Picked::Done);
        assert_eq!(
            view.filters.get(&Col::Harness),
            Some(&BTreeSet::from(["codex".to_string()])),
            "the tick survives the find and becomes the filter"
        );

        let mut none = FeedPicker::open_values(Col::Harness, &items, &view, |_| true, at);
        none.type_char('x');
        none.toggle_selected();
        none.backspace();
        assert_eq!(none.pick(&mut view), Picked::Done);
        assert!(
            !view.filters.contains_key(&Col::Harness),
            "unticking the only value clears the column"
        );
    }
}
