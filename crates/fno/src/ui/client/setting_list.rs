//! The reusable settings list: a query filter, rows of label and value that
//! each declare an edit mode, and a drilled choice page. Pure model plus a
//! row painter - it knows no config and no client state, so every mux form
//! can adopt it; the Config tab (`config_page.rs`) is its first user.
//!
//! Selection lives in the caller's [`Popup`](crate::popup::Popup): this
//! component paints rows whose enabled entries line up with the popup's
//! target order, and `feed` takes the popup's selected target index. Locked
//! rows are disabled entries (zero targets), so arrows skip them and Enter
//! can never land on one.

use super::agent_launcher::{LKey, LauncherEsc};
use super::input_field::{FieldEvent, InputField};
use crate::popup::PopupRow;

/// One selectable value on a choice page. An empty `value` is the unset
/// pseudo-choice: confirming it clears the key instead of writing a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Choice {
    pub value: String,
    pub doc: String,
}

/// How a row answers Enter. The caller resolves an outcome against the real
/// config; the component only names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowMode {
    Toggle {
        on: bool,
    },
    Cycle {
        options: Vec<String>,
        current: usize,
    },
    Page {
        options: Vec<Choice>,
        current: usize,
    },
    /// A folded group: Enter opens the children as their own view (the
    /// Config tab's `paths.*` leaves fold into one "Paths" row). The
    /// children are full rows; Enter on one runs its own mode.
    Group {
        rows: Vec<SettingRow>,
    },
    Editor,
    Locked {
        reason: String,
    },
}

/// One row of the list: a setting's key, its current value text, and how
/// Enter treats it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettingRow {
    /// The dotted config key (the greppable identity).
    pub id: String,
    /// The painted label (the same dotted key for the Config tab).
    pub label: String,
    /// The current value, painted right.
    pub value: String,
    /// The value as the row paints it. It differs from `value` when the
    /// paint names a state instead of the text: `default` for an unset
    /// override. Enter seeds from `value`, never from `display`.
    pub display: String,
    /// One line of doc for the selected row.
    pub detail: String,
    pub mode: RowMode,
}

/// What one folded key asked the caller to do. `None` events only fire when
/// the query text actually changed, so the caller rebuilds once per real
/// change, never per inert key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ListEvent {
    /// Query text changed (or a page closed): repaint.
    None,
    /// Move the popup selection up/down.
    Up,
    Down,
    /// Switch the settings modal's section (Tab).
    Tab,
    Set {
        id: String,
        value: String,
    },
    Unset {
        id: String,
    },
    /// Open `$EDITOR` on the key; `seed` is the current value text.
    Edit {
        id: String,
        seed: String,
    },
    /// Esc on an empty query at the top of the list: close the modal.
    Close,
    /// A digit on an open page: put the selection on that option.
    Select(usize),
}

/// The state of one open choice page.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PageState {
    id: String,
    options: Vec<Choice>,
    /// The option index the current value paints a check on.
    current: usize,
}

pub(crate) struct SettingList {
    query: InputField,
    rows: Vec<SettingRow>,
    page: Option<PageState>,
    /// `(row id, message)` of the last refused write, painted under its row.
    note: Option<(String, String)>,
    /// The page fold: active only while a page is open, so the query's own
    /// carry never sees bytes the page consumed.
    page_esc: LauncherEsc,
    /// The id of the open group row, if any. By id, not index: a reload
    /// (the write path re-reads every row) rebuilds the group's children
    /// and the open group survives showing the fresh values.
    group_open: Option<String>,
    /// The painted height to hold: the row count at construction, so a
    /// narrowed filter or an open page never shrinks the modal.
    held: usize,
}

impl Default for SettingList {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl SettingList {
    pub(crate) fn new(rows: Vec<SettingRow>) -> Self {
        let mut list = SettingList {
            query: InputField::new("", super::MAX_SEARCH_QUERY),
            rows,
            page: None,
            note: None,
            page_esc: LauncherEsc::default(),
            group_open: None,
            held: 0,
        };
        list.held = list.popup_rows().len();
        list
    }

    /// Replace the rows after a write or a reload. Query, page and note
    /// survive: a reload re-reads the same keys the user is looking at.
    pub(crate) fn reload(&mut self, rows: Vec<SettingRow>) {
        self.rows = rows;
    }

    /// Record a refused write under its row. Any later successful write
    /// clears it.
    pub(crate) fn set_note(&mut self, id: &str, message: String) {
        self.note = Some((id.to_string(), message));
    }

    pub(crate) fn clear_note(&mut self) {
        self.note = None;
    }

    #[cfg(test)]
    pub(crate) fn query_text(&self) -> &str {
        self.query.text()
    }

    /// Whether a choice page sits over the list.
    #[cfg(test)]
    pub(crate) fn page_open(&self) -> bool {
        self.page.is_some()
    }

    /// The rows the query keeps, in paint order, locked rows included. A
    /// group row stays visible when the query matches any folded child: the
    /// filter must be able to reach a path root the fold hides.
    pub(crate) fn filtered(&self) -> Vec<&SettingRow> {
        let q = self.query.text().to_lowercase();
        self.rows
            .iter()
            .filter(|row| row_matches(row, &q))
            .collect()
    }

    /// The filtered rows a disabled row can never occupy: the popup's target
    /// order. `feed`'s `sel` indexes into this list.
    fn selectable(&self) -> Vec<&SettingRow> {
        self.filtered()
            .into_iter()
            .filter(|row| !matches!(row.mode, RowMode::Locked { .. }))
            .collect()
    }

    /// Fold one raw chunk. The query field folds list bytes; an open page
    /// folds them itself (digits jump, Enter confirms, Esc cancels), so a
    /// digit never lands in a query the page is hiding. An open group
    /// folds the same way (Esc backs out, Enter edits the selected child,
    /// arrows move the selection); a text key is inert there.
    pub(crate) fn feed(&mut self, bytes: &[u8], sel: Option<usize>) -> Vec<ListEvent> {
        if self.page.is_some() {
            let (page_id, option_count) = {
                let page = self.page.as_ref().expect("page open");
                (page.id.clone(), page.options.len())
            };
            return self.feed_page(page_id, option_count, bytes, sel);
        }
        if self.group_open.is_some() {
            return self.feed_group(bytes, sel);
        }
        let mut events = Vec::new();
        for ev in self.query.feed(bytes) {
            match ev {
                FieldEvent::Up => events.push(ListEvent::Up),
                FieldEvent::Down => events.push(ListEvent::Down),
                FieldEvent::Tab => events.push(ListEvent::Tab),
                FieldEvent::Cancel => {
                    // Esc steps back one level at a time: query first, then
                    // the modal itself (the page has its own fold branch).
                    if self.query.is_empty() {
                        events.push(ListEvent::Close);
                    } else {
                        self.query.clear();
                        events.push(ListEvent::None);
                    }
                }
                FieldEvent::Submit(_) => events.push(self.enter(sel)),
            }
        }
        // The query mutated for every text key the fold folded; say so once
        // unless a stronger event already fired.
        if events.is_empty() {
            events.push(ListEvent::None);
        }
        events
    }

    /// The page branch of `feed`. The page closes only through the events
    /// it returns, never by a caller's assumption.
    fn feed_page(
        &mut self,
        page_id: String,
        option_count: usize,
        bytes: &[u8],
        sel: Option<usize>,
    ) -> Vec<ListEvent> {
        let mut events = Vec::new();
        let flush = bytes.is_empty();
        for key in self.page_esc.fold_carry(bytes, flush) {
            match key {
                LKey::Esc if flush => {
                    // The page closes and the value stays whatever it was.
                    self.page = None;
                    events.push(ListEvent::None);
                }
                LKey::Enter => {
                    let Some(idx) = sel.filter(|i| *i < option_count) else {
                        continue;
                    };
                    let page = self.page.as_ref().expect("page open");
                    let Some(choice) = page.options.get(idx).cloned() else {
                        continue;
                    };
                    self.page = None;
                    if choice.value.is_empty() {
                        events.push(ListEvent::Unset {
                            id: page_id.clone(),
                        });
                    } else {
                        events.push(ListEvent::Set {
                            id: page_id.clone(),
                            value: choice.value,
                        });
                    }
                }
                LKey::Char(c) if ('1'..='9').contains(&c) => {
                    // `1` is the first option, so the jump target is the
                    // digit minus one; `0` and past-the-list are inert.
                    let idx = (c as u8 - b'1') as usize;
                    if idx < option_count {
                        events.push(ListEvent::Select(idx));
                    }
                }
                _ => {}
            }
        }
        if events.is_empty() {
            events.push(ListEvent::None);
        }
        events
    }

    /// The group branch of `feed`: Esc backs out to the list, Enter runs
    /// the selected child's own mode (every folded child edits), arrows
    /// move the selection, a text key is inert. The same esc-carry
    /// discipline as the page: a lone ESC held until the flush releases it.
    fn feed_group(&mut self, bytes: &[u8], sel: Option<usize>) -> Vec<ListEvent> {
        let mut events = Vec::new();
        let flush = bytes.is_empty();
        for key in self.page_esc.fold_carry(bytes, flush) {
            match key {
                LKey::Esc if flush => {
                    self.group_open = None;
                    events.push(ListEvent::None);
                }
                LKey::Enter => {
                    let picked = self.group_children().and_then(|rows| {
                        sel.filter(|i| *i < rows.len())
                            .and_then(|i| rows.get(i).cloned())
                    });
                    let Some(child) = picked else {
                        continue;
                    };
                    match child.mode {
                        // The child answers through its own mode; every
                        // folded child today is an editor row.
                        RowMode::Editor => {
                            events.push(ListEvent::Edit {
                                id: child.id,
                                seed: child.value,
                            });
                        }
                        RowMode::Toggle { on } => {
                            events.push(ListEvent::Set {
                                id: child.id,
                                value: if on { "false".into() } else { "true".into() },
                            });
                        }
                        _ => {}
                    }
                }
                LKey::Up => events.push(ListEvent::Up),
                LKey::Down => events.push(ListEvent::Down),
                _ => {}
            }
        }
        if events.is_empty() {
            events.push(ListEvent::None);
        }
        events
    }

    /// The open group's children, or None when no group is open or its row
    /// is gone. A read: the paint falls back to the list without mutating.
    fn group_children(&self) -> Option<Vec<SettingRow>> {
        let gid = self.group_open.as_ref()?;
        match self
            .rows
            .iter()
            .find(|r| &r.id == gid)
            .map(|r| r.mode.clone())
        {
            Some(RowMode::Group { rows }) => Some(rows),
            _ => None,
        }
    }

    /// Enter on the list: one outcome per row mode. A page row opens its
    /// page here; a group row opens the group.
    fn enter(&mut self, sel: Option<usize>) -> ListEvent {
        // Clone the picked row's facts first: opening a page mutates
        // `self.page`, which cannot happen while a row borrow is live.
        let picked = sel.and_then(|i| {
            self.selectable()
                .get(i)
                .map(|row| (row.id.clone(), row.value.clone(), row.mode.clone()))
        });
        let Some((id, value, mode)) = picked else {
            return ListEvent::None;
        };
        match mode {
            RowMode::Toggle { on } => ListEvent::Set {
                id,
                value: if on { "false".into() } else { "true".into() },
            },
            RowMode::Cycle { options, current } => {
                // A malformed cycle row (under two options) is inert rather
                // than a modulo panic; the schema test refuses such a leaf.
                if options.len() < 2 {
                    return ListEvent::None;
                }
                let Some(value) = options.get((current + 1) % options.len()).cloned() else {
                    return ListEvent::None;
                };
                ListEvent::Set { id, value }
            }
            RowMode::Page { options, current } => {
                self.page = Some(PageState {
                    id: id.clone(),
                    options,
                    current,
                });
                // The selection lands on the current value, so Enter right
                // after the page opens confirms what is already set, the
                // reference /config behavior.
                ListEvent::Select(current)
            }
            RowMode::Editor => ListEvent::Edit { id, seed: value },
            RowMode::Group { .. } => {
                self.group_open = Some(id);
                // The selection lands on the first child, the shape the
                // page arm uses (it lands on the current value).
                ListEvent::Select(0)
            }
            // Unreachable through the popup (a locked row carries no
            // target), kept honest for a direct call.
            RowMode::Locked { .. } => ListEvent::None,
        }
    }

    /// The footer line: the page's grammar replaces the list's while a page
    /// is open, the group's while a group is.
    pub(crate) fn footer(&self) -> &'static str {
        if self.page.is_some() {
            "Enter to confirm · Esc to cancel"
        } else if self.group_open.is_some() {
            "Enter to edit · Esc to go back"
        } else {
            "Type to filter · Enter to select · Esc to clear"
        }
    }

    /// The popup rows: the open page (a header over its numbered options,
    /// compact like the reference choice page), or the search box over the
    /// list. Each list row carries its key as a dim line under it; the
    /// selected row's doc paints as one line at the end; the list holds its
    /// opening height under a filter. `sel` is the popup's selected target
    /// index. The popup's cursor band marks the selected row; the selected
    /// row's doc answers through `selected_detail` instead.
    pub(crate) fn popup_rows(&self) -> Vec<PopupRow> {
        let mut out = Vec::new();
        if let Some(page) = &self.page {
            // The page replaces the filter box with a header: the key in the
            // dim column, its current value beside it. No query here to
            // drop; the page folds its own keys.
            let current = page
                .options
                .get(page.current)
                .map(|c| c.value.clone())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "(unset)".into());
            out.push(PopupRow::Info {
                label: page.id.clone(),
                value: current,
            });
            for (i, choice) in page.options.iter().enumerate() {
                let label = if choice.value.is_empty() {
                    "(unset)".to_string()
                } else {
                    choice.value.clone()
                };
                out.push(PopupRow::Entry {
                    glyph: (i + 1).to_string(),
                    label,
                    hint: if i == page.current {
                        "✓".into()
                    } else {
                        String::new()
                    },
                    enabled: true,
                });
                if !choice.doc.is_empty() {
                    out.push(PopupRow::Sub(format!("  {}", choice.doc)));
                }
            }
            // A page paints compact, the reference choice-page shape; the
            // height hold below is the list's contract only.
            return out;
        }
        if let Some(children) = self.group_children() {
            // The group replaces the list: a header (the group's label and
            // its summary hint), then the folded children. No filter here;
            // Esc backs out, Enter edits the selected child. A missing
            // group row (a reload dropped the fold) paints the list; no
            // mutation on a `&self` read.
            if let Some(row) = self
                .rows
                .iter()
                .find(|r| Some(&r.id) == self.group_open.as_ref())
            {
                out.push(PopupRow::Info {
                    label: row.label.clone(),
                    value: row.display.clone(),
                });
                for child in &children {
                    let (glyph, enabled, hint) = entry_parts(&child.mode, &child.display);
                    out.push(PopupRow::Entry {
                        glyph: glyph.into(),
                        label: child.label.clone(),
                        hint,
                        enabled,
                    });
                    // A refused write lands under its row here too, the same
                    // paint the list gives it: the folded child never paints
                    // at the top level, so a note keyed to it would be
                    // invisible if only the list branch rendered one.
                    if let Some((id, message)) = &self.note {
                        if id == &child.id {
                            out.push(PopupRow::Error(message.clone()));
                        }
                    }
                    out.push(PopupRow::Sub(format!("  {}", child.id)));
                }
                // The group paints compact like the page; the height hold
                // is the list's contract only.
                return out;
            }
        }
        out.push(self.query.row("filter"));
        for row in self.filtered() {
            let (glyph, enabled, hint) = entry_parts(&row.mode, &row.display);
            out.push(PopupRow::Entry {
                glyph: glyph.into(),
                label: row.label.clone(),
                hint,
                enabled,
            });
            if let Some((id, message)) = &self.note {
                if id == &row.id {
                    out.push(PopupRow::Error(message.clone()));
                }
            }
            if let RowMode::Locked { reason } = &row.mode {
                out.push(PopupRow::Text(reason.clone()));
            }
            out.push(PopupRow::Sub(format!("  {}", row.id)));
        }
        // Hold the height: pad a narrowed paint back out to the row count
        // the list opened with, so the modal never shrinks under a filter.
        // An empty Text is the documented spacer line. The selected row's
        // doc is gone from the tail on purpose: it answers through
        // `selected_detail`, which the popup pins above its footer (a tall
        // list scrolled the tail below the fold, where no reader reached).
        while out.len() < self.held {
            out.push(PopupRow::Text(String::new()));
        }
        out
    }

    /// The doc line the popup pins above its footer: the selected row's
    /// detail on the list, the selected child's inside a group, nothing on
    /// an open page (its options already carry their docs inline). `None`
    /// is the width-measure pass (no selection) or a doc-less row.
    pub(crate) fn selected_detail(&self, sel: Option<usize>) -> Option<String> {
        if self.page.is_some() {
            return None;
        }
        let targets: Vec<SettingRow> = if self.group_open.is_some() {
            self.group_children()?
        } else {
            self.filtered().into_iter().cloned().collect()
        };
        let selectable: Vec<&SettingRow> = targets
            .iter()
            .filter(|r| !matches!(r.mode, RowMode::Locked { .. }))
            .collect();
        sel.and_then(|i| selectable.get(i))
            .map(|r| r.detail.clone())
            .filter(|d| !d.is_empty())
    }
}

/// One row's entry cells: the glyph and enabled flag from its mode, the
/// right-column hint from its display.
fn entry_parts(mode: &RowMode, display: &str) -> (&'static str, bool, String) {
    match mode {
        RowMode::Toggle { on } => (if *on { "☑" } else { "☐" }, true, display.to_string()),
        RowMode::Cycle { .. } => ("↻", true, display.to_string()),
        RowMode::Page { .. } => ("›", true, display.to_string()),
        RowMode::Group { .. } => ("›", true, display.to_string()),
        RowMode::Editor => ("✎", true, display.to_string()),
        RowMode::Locked { .. } => ("×", false, String::new()),
    }
}

/// Whether one row answers the filter query (already lowercased). A group
/// answers through its folded children too, so a query naming a hidden
/// path root keeps the group row visible.
fn row_matches(row: &SettingRow, q: &str) -> bool {
    q.is_empty()
        || row.label.to_lowercase().contains(q)
        || row.id.to_lowercase().contains(q)
        || row.detail.to_lowercase().contains(q)
        || matches!(&row.mode, RowMode::Group { rows } if rows.iter().any(|c| {
            c.label.to_lowercase().contains(q)
                || c.id.to_lowercase().contains(q)
                || c.display.to_lowercase().contains(q)
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toggle(on: bool) -> SettingRow {
        SettingRow {
            id: "mux.hover_focus".into(),
            label: "mux.hover_focus".into(),
            value: if on { "on".into() } else { "off".into() },
            display: if on { "on".into() } else { "off".into() },
            detail: "focus follows the mouse".into(),
            mode: RowMode::Toggle { on },
        }
    }

    fn cycle() -> SettingRow {
        SettingRow {
            id: "mux.viewer".into(),
            label: "mux.viewer".into(),
            value: "panel".into(),
            display: "panel".into(),
            detail: "how a transcript opens".into(),
            mode: RowMode::Cycle {
                options: vec!["panel".into(), "modal".into()],
                current: 0,
            },
        }
    }

    fn page(current: usize) -> SettingRow {
        SettingRow {
            id: "autonomy.mode".into(),
            label: "autonomy.mode".into(),
            value: "balanced".into(),
            display: "balanced".into(),
            detail: "how far a worker may act alone".into(),
            mode: RowMode::Page {
                options: vec![
                    Choice {
                        value: "ask".into(),
                        doc: "rule before every write".into(),
                    },
                    Choice {
                        value: "balanced".into(),
                        doc: "act, report after".into(),
                    },
                    Choice {
                        value: "yolo".into(),
                        doc: "act, report never".into(),
                    },
                ],
                current,
            },
        }
    }

    fn paths_group() -> SettingRow {
        SettingRow {
            id: "paths".into(),
            label: "Paths".into(),
            value: String::new(),
            display: "2 · all default".into(),
            detail: "One row per path root; Enter opens the group.".into(),
            mode: RowMode::Group {
                rows: vec![
                    SettingRow {
                        id: "paths.ledger_json".into(),
                        label: "Ledger Json".into(),
                        value: String::new(),
                        display: "<state>/db/history/ledger.json".into(),
                        detail: "run history and cost".into(),
                        mode: RowMode::Editor,
                    },
                    SettingRow {
                        id: "paths.briefs_dir".into(),
                        label: "Briefs Dir".into(),
                        value: "/tmp/briefs".into(),
                        display: "/tmp/briefs".into(),
                        detail: "sidecar briefs".into(),
                        mode: RowMode::Editor,
                    },
                ],
            },
        }
    }

    fn locked() -> SettingRow {
        SettingRow {
            id: "context.artifacts".into(),
            label: "context.artifacts".into(),
            value: String::new(),
            display: String::new(),
            detail: String::new(),
            mode: RowMode::Locked {
                reason: "set in config.local.toml, which outranks the mux; edit there".into(),
            },
        }
    }

    /// AC3: filter to one row, then Enter cycles it in place.
    #[test]
    fn filter_then_enter_cycles() {
        let mut list = SettingList::new(vec![toggle(true), cycle()]);
        list.feed(b"view", None);
        assert_eq!(list.query_text(), "view");
        assert_eq!(list.filtered().len(), 1);
        assert_eq!(list.filtered().first().expect("one row").id, "mux.viewer");
        let events = list.feed(b"\r", Some(0));
        assert_eq!(
            events,
            vec![ListEvent::Set {
                id: "mux.viewer".into(),
                value: "modal".into()
            }]
        );
    }

    /// Enter on a toggle flips the written text.
    #[test]
    fn enter_on_toggle_writes_the_other_state() {
        let mut list = SettingList::new(vec![toggle(true), cycle()]);
        assert_eq!(
            list.feed(b"\r", Some(0)),
            vec![ListEvent::Set {
                id: "mux.hover_focus".into(),
                value: "false".into()
            }]
        );
    }

    /// AC4: Esc on an open page closes it and changes nothing. The lone ESC
    /// of a real read is held until the quiet-window flush, the field rule.
    #[test]
    fn esc_closes_the_page_and_keeps_the_value() {
        let mut list = SettingList::new(vec![page(1)]);
        list.feed(b"\r", Some(0));
        assert!(list.page_open(), "Enter on a page row opens the page");
        assert_eq!(
            list.feed(b"\x1b", Some(0)),
            vec![ListEvent::None],
            "a lone ESC is held, not a cancel"
        );
        let events = list.feed(b"", Some(0));
        assert_eq!(events, vec![ListEvent::None]);
        assert!(!list.page_open());
        assert_eq!(list.filtered().first().expect("one row").value, "balanced");
    }

    /// AC5: a locked row paints dim with its reason, carries no target, and
    /// Enter lands past it on the next selectable row.
    #[test]
    fn locked_row_is_inert_and_paints_its_reason() {
        let mut list = SettingList::new(vec![toggle(false), locked(), cycle()]);
        let rows = list.popup_rows();
        // Row layout: filter, toggle, its key, locked entry, reason text,
        // the locked key, cycle, its key.
        let locked_entry = rows.get(3).expect("locked row fourth");
        let PopupRow::Entry { enabled, .. } = locked_entry else {
            panic!("expected the locked entry");
        };
        assert!(!enabled, "a locked row is a disabled entry");
        assert_eq!(
            rows.get(4).expect("reason text fifth"),
            &PopupRow::Text("set in config.local.toml, which outranks the mux; edit there".into())
        );
        // Enter with the selection on the first selectable row hits the
        // toggle; the locked row never takes a target.
        assert_eq!(
            list.feed(b"\r", Some(0)),
            vec![ListEvent::Set {
                id: "mux.hover_focus".into(),
                value: "true".into()
            }]
        );
    }

    /// Esc clears a non-empty query first, then closes on the next press.
    #[test]
    fn esc_clears_the_query_then_closes() {
        let mut list = SettingList::new(vec![toggle(false), cycle()]);
        list.feed(b"view", None);
        assert_eq!(list.query_text(), "view");
        // A trailing lone ESC on a real read is held (a split-arrow
        // candidate); the flush releases it, and the query clears first.
        assert_eq!(list.feed(b"\x1b", Some(0)), vec![ListEvent::None]);
        assert_eq!(list.feed(b"", Some(0)), vec![ListEvent::None]);
        assert_eq!(list.query_text(), "");
        // With the query empty, an Esc + flush closes the list.
        assert_eq!(list.feed(b"\x1b", Some(0)), vec![ListEvent::None]);
        assert_eq!(list.feed(b"", Some(0)), vec![ListEvent::Close]);
    }

    /// A digit on an open page jumps the selection; Enter confirms the
    /// jumped option; the unset pseudo-choice yields Unset.
    #[test]
    fn page_digits_jump_and_enter_confirms() {
        let mut list = SettingList::new(vec![page(1)]);
        list.feed(b"\r", Some(0));
        assert_eq!(list.feed(b"3", Some(0)), vec![ListEvent::Select(2)]);
        assert_eq!(
            list.feed(b"\r", Some(2)),
            vec![ListEvent::Set {
                id: "autonomy.mode".into(),
                value: "yolo".into()
            }]
        );
        assert!(!list.page_open());

        // An unset choice (empty value) clears the key instead.
        let unset_row = SettingRow {
            id: "mux.restore.policy".into(),
            label: "mux.restore.policy".into(),
            value: "idle".into(),
            display: "idle".into(),
            detail: String::new(),
            mode: RowMode::Page {
                options: vec![
                    Choice {
                        value: "hold".into(),
                        doc: String::new(),
                    },
                    Choice {
                        value: String::new(),
                        doc: String::new(),
                    },
                ],
                current: 0,
            },
        };
        let mut list = SettingList::new(vec![unset_row]);
        list.feed(b"\r", Some(0));
        assert_eq!(
            list.feed(b"\r", Some(1)),
            vec![ListEvent::Unset {
                id: "mux.restore.policy".into()
            }]
        );
    }

    /// A refused write paints an Error line under its row; a clean reload
    /// clears nothing until the caller says so.
    #[test]
    fn a_refused_write_paints_under_its_row() {
        let mut list = SettingList::new(vec![toggle(false), cycle()]);
        list.set_note(
            "mux.hover_focus",
            "fno config set mux.hover_focus banana failed".into(),
        );
        let rows = list.popup_rows();
        assert_eq!(
            rows.get(2).expect("note row third"),
            &PopupRow::Error("fno config set mux.hover_focus banana failed".into())
        );
    }

    /// The page paints a header (key and current value, no filter box),
    /// numbered rows with a check on the current value, and each option's
    /// doc indented under the option; the list paints the key as a dim
    /// line under each row and the selected row's doc at the end.
    #[test]
    fn paint_shape_page_and_selected_doc() {
        let list = SettingList::new(vec![page(1)]);
        // Enter would open the page; construct it directly through the same
        // path a real press takes.
        let mut list = list;
        list.feed(b"\r", Some(0));
        let rows = list.popup_rows();
        assert_eq!(
            rows.first(),
            Some(&PopupRow::Info {
                label: "autonomy.mode".into(),
                value: "balanced".into(),
            }),
            "the header names the key and its current value; no filter box"
        );
        assert_eq!(
            rows.get(1).expect("first option row"),
            &PopupRow::Entry {
                glyph: "1".into(),
                label: "ask".into(),
                hint: String::new(),
                enabled: true,
            }
        );
        assert_eq!(
            rows.get(2).expect("rule row"),
            &PopupRow::Sub("  rule before every write".into()),
            "the option doc is an indented secondary line"
        );
        let Some(PopupRow::Entry { hint, .. }) = rows.get(3) else {
            panic!("expected option 2");
        };
        assert_eq!(hint, "✓", "the current value carries the check");
        assert_eq!(
            list.footer(),
            "Enter to confirm · Esc to cancel",
            "the page footer replaces the list footer"
        );

        // Back on the list: the key paints dim under each row, and the
        // selected row's doc answers through `selected_detail`, which the
        // popup pins above its footer (never at the list's end, where a
        // tall list scrolls it below the fold).
        let list = SettingList::new(vec![toggle(false), cycle()]);
        let rows = list.popup_rows();
        assert_eq!(
            rows.get(2).expect("key line under the toggle"),
            &PopupRow::Sub("  mux.hover_focus".into())
        );
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r, PopupRow::Text(t) if t == "how a transcript opens")),
            "the doc no longer paints at the list's tail"
        );
        assert_eq!(
            list.selected_detail(Some(1)),
            Some("how a transcript opens".into()),
            "the selected row's doc goes to the pin slot"
        );
        assert_eq!(
            list.selected_detail(Some(0)),
            Some("focus follows the mouse".into())
        );
        assert_eq!(
            list.footer(),
            "Type to filter · Enter to select · Esc to clear"
        );
    }

    /// A folded group row: Enter opens it (header over the children, each
    /// child painting its dotted key dim), Esc+flush backs out to the list,
    /// and the footer names the group's grammar while open.
    #[test]
    fn group_opens_paints_and_esc_backs_out() {
        let mut list = SettingList::new(vec![toggle(false), paths_group()]);
        // The group row is selectable: Enter opens it. The selection lands
        // on the first child.
        assert_eq!(list.feed(b"\r", Some(1)), vec![ListEvent::Select(0)]);
        assert_eq!(list.footer(), "Enter to edit · Esc to go back");
        let rows = list.popup_rows();
        assert_eq!(
            rows.first(),
            Some(&PopupRow::Info {
                label: "Paths".into(),
                value: "2 · all default".into(),
            }),
            "the header names the group and its summary"
        );
        assert_eq!(
            rows.get(1),
            Some(&PopupRow::Entry {
                glyph: "✎".into(),
                label: "Ledger Json".into(),
                hint: "<state>/db/history/ledger.json".into(),
                enabled: true,
            }),
            "an unset child paints its default sketch in the dim column"
        );
        assert_eq!(
            rows.get(2),
            Some(&PopupRow::Sub("  paths.ledger_json".into()))
        );
        assert_eq!(rows.len(), 5, "header + 2 children + 2 key lines");
        // Esc (held, then flushed) backs out to the list.
        assert_eq!(list.feed(b"\x1b", Some(0)), vec![ListEvent::None]);
        assert_eq!(list.feed(b"", Some(0)), vec![ListEvent::None]);
        assert_eq!(
            list.footer(),
            "Type to filter · Enter to select · Esc to clear"
        );
    }

    /// A refused write keyed to a folded child paints under that child
    /// inside the open group: the child never paints at the top level, so
    /// the list branch's note render can never reach it.
    #[test]
    fn group_paints_a_refused_write_under_its_child() {
        let mut list = SettingList::new(vec![paths_group()]);
        list.feed(b"\r", Some(0));
        list.set_note(
            "paths.ledger_json",
            "fno config set paths.ledger_json 'a*b' failed".into(),
        );
        let rows = list.popup_rows();
        // Layout: header, ledger Entry, its Error note, its key Sub.
        assert_eq!(
            rows.get(2),
            Some(&PopupRow::Error(
                "fno config set paths.ledger_json 'a*b' failed".into()
            )),
            "the refusal paints under the folded child"
        );
        assert_eq!(
            rows.get(3),
            Some(&PopupRow::Sub("  paths.ledger_json".into()))
        );
        assert_eq!(
            rows.get(4),
            Some(&PopupRow::Entry {
                glyph: "\u{270e}".into(),
                label: "Briefs Dir".into(),
                hint: "/tmp/briefs".into(),
                enabled: true,
            })
        );
        assert_eq!(
            rows.get(5),
            Some(&PopupRow::Sub("  paths.briefs_dir".into()))
        );
    }

    /// Enter inside an open group edits the selected child through its own
    /// mode; the event carries the child's key and its raw value as seed.
    #[test]
    fn group_enter_edits_the_selected_child() {
        let mut list = SettingList::new(vec![paths_group()]);
        list.feed(b"\r", Some(0));
        assert_eq!(
            list.feed(b"\r", Some(0)),
            vec![ListEvent::Edit {
                id: "paths.ledger_json".into(),
                seed: "".into(),
            }]
        );
    }

    /// A query naming a folded child keeps the group row in the filtered
    /// list, so the filter reaches a path root the fold hides.
    #[test]
    fn filter_naming_a_child_keeps_the_group_row() {
        let mut list = SettingList::new(vec![toggle(false), paths_group()]);
        list.feed(b"ledger", None);
        let kept: Vec<&str> = list.filtered().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(kept, vec!["paths"], "the group answers for its child");
        // And a query matching nothing stays empty.
        list.feed(b"zzz", None);
        assert!(list.filtered().is_empty());
    }

    /// A narrowed filter holds the paint height the list opened with, so
    /// the modal never shrinks under a query.
    #[test]
    fn filtering_holds_the_paint_height() {
        let list = SettingList::new(vec![toggle(false), cycle(), page(0)]);
        let open = list.popup_rows().len();
        let mut list = list;
        list.feed(b"view", None);
        let rows = list.popup_rows();
        assert_eq!(list.filtered().len(), 1, "the filter narrowed to one row");
        assert!(
            rows.len() >= open,
            "the filtered paint still fills the opening height"
        );
        assert!(
            rows.iter()
                .any(|r| matches!(r, PopupRow::Sub(s) if s == "  mux.viewer")),
            "the key line rides the filtered row"
        );
    }
}
