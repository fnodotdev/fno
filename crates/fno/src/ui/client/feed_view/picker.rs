//! The feed's sort and group pickers: one popup of choices under the column
//! header. The active choice wears its direction arrow, and picking it again
//! flips that direction. Every column is listed, hidden ones included, so a
//! narrow panel still sorts and groups by any of the eight.

use super::table::{Col, Group, TableView};
use crate::popup::{Anchor, Popup, PopupRow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickerKind {
    Sort,
    Group,
}

pub(crate) struct FeedPicker {
    pub kind: PickerKind,
    pub popup: Popup,
}

impl FeedPicker {
    /// The picker for `kind` over `view`, its cursor on the active choice.
    pub(crate) fn open(kind: PickerKind, view: &TableView, anchor: Anchor) -> Self {
        let entry = |label: &str, active: bool, desc: bool| PopupRow::Entry {
            glyph: match (active, desc) {
                (false, _) => " ".into(),
                (true, true) => "↓".into(),
                (true, false) => "↑".into(),
            },
            label: label.into(),
            hint: String::new(),
            enabled: true,
        };
        let (title, rows, active) = match kind {
            PickerKind::Sort => (
                "sort by",
                Col::ALL
                    .iter()
                    .map(|c| entry(c.name(), view.sort == *c, view.sort_desc))
                    .collect::<Vec<_>>(),
                Col::ALL.iter().position(|c| *c == view.sort),
            ),
            PickerKind::Group => (
                "group by",
                Group::ALL
                    .iter()
                    .map(|g| entry(g.name(), view.group == *g, view.group_desc))
                    .collect(),
                Group::ALL.iter().position(|g| *g == view.group),
            ),
        };
        let mut popup = Popup::new(rows, anchor)
            .title(title)
            .footer("enter pick · again flips · esc close")
            .width_cap(60);
        popup.select(active.unwrap_or(0));
        FeedPicker { kind, popup }
    }

    /// Apply the selected choice to `view`.
    pub(crate) fn pick(&self, view: &mut TableView) {
        let Some((row, _)) = self.popup.selected() else {
            return;
        };
        match self.kind {
            PickerKind::Sort => {
                if let Some(col) = Col::ALL.get(row) {
                    view.pick_sort(*col);
                }
            }
            PickerKind::Group => {
                if let Some(group) = Group::ALL.get(row) {
                    view.pick_group(*group);
                }
            }
        }
    }
}
