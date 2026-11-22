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
    /// A folded group: Enter expands it in place, its children painting
    /// indented right under it, and Enter again folds it (the Config tab's
    /// `paths.*` leaves fold into one "Paths" row). The children are full
    /// rows; Enter on one runs its own mode.
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
    /// The id of the expanded group row, if any. By id, not index: a reload
    /// (the write path re-reads every row) rebuilds the group's children
    /// and the expanded group survives showing the fresh values.
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

    /// The list as it paints: the filtered rows, with an expanded group's
    /// children right under it, indented. A live query expands every group
    /// it reaches inside and keeps only the children it names.
    fn visible(&self) -> Vec<SettingRow> {
        let q = self.query.text().to_lowercase();
        let mut out = Vec::new();
        for row in self.filtered() {
            out.push(row.clone());
            if let RowMode::Group { rows } = &row.mode {
                if self.group_expanded(&row.id) {
                    out.extend(
                        rows.iter()
                            .filter(|c| q.is_empty() || child_matches(c, &q))
                            .map(|c| SettingRow {
                                label: format!("  {}", c.label),
                                ..c.clone()
                            }),
                    );
                }
            }
        }
        out
    }

    /// Whether a group row paints its children: Enter expanded it, or a
    /// live query is searching inside it.
    fn group_expanded(&self, id: &str) -> bool {
        self.group_open.as_deref() == Some(id) || !self.query.is_empty()
    }

    /// The visible rows a disabled row can never occupy: the popup's target
    /// order. `feed`'s `sel` indexes into this list.
    fn selectable(&self) -> Vec<SettingRow> {
        self.visible()
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
            match key.plain() {
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

    /// Enter on the list: one outcome per row mode. A page row opens its
    /// page here; a group row expands or folds in place.
    fn enter(&mut self, sel: Option<usize>) -> ListEvent {
        // Clone the picked row's facts first: opening a page mutates
        // `self.page`, which cannot happen while a row borrow is live.
        let picked = sel.and_then(|i| {
            self.selectable()
                .into_iter()
                .nth(i)
                .map(|row| (row.id, row.value, row.mode))
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
                // The selection stays on the group row; its children open
                // or fold under it.
                self.group_open = if self.group_open.as_deref() == Some(id.as_str()) {
                    None
                } else {
                    Some(id)
                };
                ListEvent::None
            }
            // Unreachable through the popup (a locked row carries no
            // target), kept honest for a direct call.
            RowMode::Locked { .. } => ListEvent::None,
        }
    }

    /// The footer line: the page's grammar replaces the list's while a page
    /// is open.
    pub(crate) fn footer(&self) -> &'static str {
        if self.page.is_some() {
            "Enter to confirm · Esc to cancel"
        } else {
            "Type to filter · Enter to select · Esc to clear"
        }
    }

    /// The popup rows: the open page (a header over its numbered options,
    /// compact like the reference choice page), or the search box over the
    /// list. A row is its label and value only; the key and the selected
    /// row's doc ride the pinned line. The list holds its opening height
    /// under a filter. `sel` is the popup's selected target
    /// index. The popup's cursor band marks the selected row; the selected
    /// row's doc answers through `selected_detail` instead.
    pub(crate) fn popup_rows(&self) -> Vec<PopupRow> {
        let mut out = Vec::new();
        if let Some(page) = &self.page {
            // The page replaces the filter box with a heading: the setting's
            // label, like the list row that opened it. A heading, not an
            // Info row: an Info label sets the key column's width, and the
            // options would start past it. No query here to drop; the page
            // folds its own keys (a digit still jumps to its option).
            out.push(PopupRow::Header(
                self.rows
                    .iter()
                    .find(|r| r.id == page.id)
                    .map_or_else(|| page.id.clone(), |r| r.label.clone()),
            ));
            for (i, choice) in page.options.iter().enumerate() {
                let label = if choice.value.is_empty() {
                    "(unset)".to_string()
                } else {
                    choice.value.clone()
                };
                // The check in the key column marks the current value; no
                // number column, which only opened a gap before the words.
                out.push(PopupRow::Entry {
                    glyph: if i == page.current { "✓" } else { " " }.into(),
                    label,
                    hint: String::new(),
                    enabled: true,
                });
            }
            // One row per option, no doc line under it: the focused
            // option's doc rides the pinned line, as on the list. A page
            // paints compact; the height hold below is the list's contract.
            return out;
        }
        out.push(self.query.row("filter"));
        for row in self.visible() {
            let (glyph, enabled, hint) = match &row.mode {
                // The fold mark: right while folded, down while expanded.
                RowMode::Group { .. } => (
                    if self.group_expanded(&row.id) {
                        "▾"
                    } else {
                        "▸"
                    },
                    true,
                    row.display.clone(),
                ),
                mode => entry_parts(mode, &row.display),
            };
            out.push(PopupRow::Entry {
                glyph: glyph.into(),
                label: row.label.clone(),
                hint,
                enabled,
            });
            if let Some((id, message)) = &self.note {
                if id == &row.id {
                    // A blank row parts the refusal from its row, so the
                    // red line never touches the cursor band.
                    out.push(PopupRow::Text(String::new()));
                    out.push(PopupRow::Error(message.clone()));
                }
            }
            if let RowMode::Locked { reason } = &row.mode {
                out.push(PopupRow::Text(reason.clone()));
            }
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
    /// detail on the list (an expanded group's children included), the
    /// focused option's doc on an open page. `None` is the width-measure pass (no
    /// selection) or a doc-less row.
    pub(crate) fn selected_detail(&self, sel: Option<usize>) -> Option<String> {
        if let Some(page) = &self.page {
            // A doc-less option pins an empty line, so the page keeps its
            // height as the cursor moves.
            return sel.and_then(|i| page.options.get(i)).map(|c| c.doc.clone());
        }
        let selectable = self.selectable();
        // The dotted key rides the pinned line, not a dim row under each
        // label: the list reads as words, and the key stays one glance away
        // for the person who types `fno config set`.
        sel.and_then(|i| selectable.get(i)).map(|r| {
            if r.detail.is_empty() {
                r.id.clone()
            } else {
                format!("{} · {}", r.detail, r.id)
            }
        })
    }
}

/// One row's entry cells: the glyph and enabled flag from its mode, the
/// right-column hint from its display.
fn entry_parts(mode: &RowMode, display: &str) -> (&'static str, bool, String) {
    match mode {
        RowMode::Toggle { on } => (if *on { "☑" } else { "☐" }, true, display.to_string()),
        // A cycler reads as its value between arrows: Enter steps it.
        RowMode::Cycle { .. } => (" ", true, format!("< {display} >")),
        RowMode::Page { .. } => ("›", true, display.to_string()),
        RowMode::Group { .. } => ("▸", true, display.to_string()),
        // A blank glyph: a pencil on every free-text row read as noise, and
        // the key column stays for the marks that carry state. One space,
        // not "": the popup refuses an empty glyph (`menu_glyph_is_bmp`).
        RowMode::Editor => (" ", true, display.to_string()),
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
        || matches!(&row.mode, RowMode::Group { rows } if rows.iter().any(|c| child_matches(c, q)))
}

/// Whether a folded child answers the query: its label, key or value.
fn child_matches(c: &SettingRow, q: &str) -> bool {
    c.label.to_lowercase().contains(q)
        || c.id.to_lowercase().contains(q)
        || c.display.to_lowercase().contains(q)
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
            label: "Autonomy mode".into(),
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
        // Row layout: filter, toggle, locked entry, reason text, cycle.
        let locked_entry = rows.get(2).expect("locked row third");
        let PopupRow::Entry { enabled, .. } = locked_entry else {
            panic!("expected the locked entry");
        };
        assert!(!enabled, "a locked row is a disabled entry");
        assert_eq!(
            rows.get(3).expect("reason text fourth"),
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
            rows.get(2).expect("blank row third"),
            &PopupRow::Text(String::new()),
            "a blank row parts the note from its row"
        );
        assert_eq!(
            rows.get(3).expect("note row fourth"),
            &PopupRow::Error("fno config set mux.hover_focus banana failed".into())
        );
    }

    /// The page paints a heading (the label, no filter box) and one row per
    /// option, the check in the key column on the current value and no
    /// number column;
    /// the focused option's doc rides the pinned line. The list paints no
    /// key line under a row and never puts the selected row's doc at the end.
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
            Some(&PopupRow::Header("Autonomy mode".into())),
            "the heading names the setting; no filter box"
        );
        assert_eq!(
            rows.get(1).expect("first option row"),
            &PopupRow::Entry {
                glyph: " ".into(),
                label: "ask".into(),
                hint: String::new(),
                enabled: true,
            }
        );
        assert!(
            !rows.iter().any(|r| matches!(r, PopupRow::Sub(_))),
            "no gray doc line paints under an option"
        );
        assert_eq!(
            list.selected_detail(Some(0)).as_deref(),
            Some("rule before every write"),
            "the focused option's doc rides the pinned line"
        );
        let Some(PopupRow::Entry { glyph, .. }) = rows.get(2) else {
            panic!("expected option 2");
        };
        assert_eq!(glyph, "✓", "the current value carries the check");
        assert_eq!(
            list.footer(),
            "Enter to confirm · Esc to cancel",
            "the page footer replaces the list footer"
        );

        // Back on the list: a row is its label and value, no key line
        // under it. The selected row's doc and key answer through
        // `selected_detail`, which the popup pins above its footer (never
        // at the list's end, where a tall list scrolls it below the fold).
        let list = SettingList::new(vec![toggle(false), cycle()]);
        let rows = list.popup_rows();
        assert!(
            !rows.iter().any(|r| matches!(r, PopupRow::Sub(_))),
            "no dim key line paints under a row"
        );
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r, PopupRow::Text(t) if t == "how a transcript opens")),
            "the doc no longer paints at the list's tail"
        );
        assert_eq!(
            list.selected_detail(Some(1)),
            Some("how a transcript opens · mux.viewer".into()),
            "the selected row's doc and key go to the pin slot"
        );
        assert_eq!(
            list.selected_detail(Some(0)),
            Some("focus follows the mouse · mux.hover_focus".into())
        );
        assert_eq!(
            list.footer(),
            "Type to filter · Enter to select · Esc to clear"
        );
    }

    /// A folded group row: Enter expands it in place (its children paint
    /// indented under it, the fold mark turns down), Enter on a child runs
    /// the child's own mode, and Enter on the group again folds it.
    #[test]
    fn group_expands_in_place_and_folds_again() {
        let mut list = SettingList::new(vec![toggle(false), paths_group()]);
        let labels = |list: &SettingList| -> Vec<(String, String)> {
            list.popup_rows()
                .into_iter()
                .filter_map(|r| match r {
                    PopupRow::Entry { glyph, label, .. } => Some((glyph, label)),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(
            labels(&list).last(),
            Some(&("▸".to_string(), "Paths".to_string())),
            "folded: the children stay hidden"
        );
        assert_eq!(list.feed(b"\r", Some(1)), vec![ListEvent::None]);
        assert_eq!(
            labels(&list).get(1..).map(<[_]>::to_vec),
            Some(vec![
                ("▾".to_string(), "Paths".to_string()),
                (" ".to_string(), "  Ledger Json".to_string()),
                (" ".to_string(), "  Briefs Dir".to_string()),
            ]),
            "expanded: the children paint indented under the group"
        );
        assert_eq!(
            list.feed(b"\r", Some(2)),
            vec![ListEvent::Edit {
                id: "paths.ledger_json".into(),
                seed: "".into(),
            }],
            "a child edits through its own mode"
        );
        assert_eq!(
            list.selected_detail(Some(3)),
            Some("sidecar briefs · paths.briefs_dir".into()),
            "a child's doc rides the pin like any row"
        );
        assert_eq!(list.feed(b"\r", Some(1)), vec![ListEvent::None]);
        assert_eq!(labels(&list).len(), 2, "Enter again folds the group");
    }

    /// A refused write keyed to a folded child paints under that child
    /// once the group is expanded.
    #[test]
    fn group_paints_a_refused_write_under_its_child() {
        let mut list = SettingList::new(vec![paths_group()]);
        list.feed(b"\r", Some(0));
        list.set_note(
            "paths.ledger_json",
            "fno config set paths.ledger_json 'a*b' failed".into(),
        );
        let rows = list.popup_rows();
        // Layout: filter, group, ledger Entry, blank, its Error note,
        // briefs Entry.
        assert_eq!(
            rows.get(4),
            Some(&PopupRow::Error(
                "fno config set paths.ledger_json 'a*b' failed".into()
            )),
            "the refusal paints under the folded child"
        );
    }

    /// A query naming a folded child keeps the group row in the filtered
    /// list and paints the child under it, so the filter reaches a path
    /// root the fold hides.
    #[test]
    fn filter_naming_a_child_keeps_the_group_row() {
        let mut list = SettingList::new(vec![toggle(false), paths_group()]);
        list.feed(b"ledger", None);
        let kept: Vec<&str> = list.filtered().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(kept, vec!["paths"], "the group answers for its child");
        let shown: Vec<String> = list.visible().into_iter().map(|r| r.id).collect();
        assert_eq!(
            shown,
            ["paths", "paths.ledger_json"],
            "only the matching child paints under the group"
        );
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
                .any(|r| matches!(r, PopupRow::Entry { label, .. } if label == "mux.viewer")),
            "the filtered row paints"
        );
    }
}
