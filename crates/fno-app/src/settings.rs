//! The Settings panel (⌘,): the D3442 grammar - searchable label/value
//! rows, Enter toggles or cycles in place, a `›` row opens a numbered
//! choice page, a locked row dims with its reason - over the app's five
//! curated sections: Look, Alerts, Machines, Keys and Updates.
//!
//! The model is pure and mirrors the mux's `setting_list.rs` grammar, not
//! its code: that model is `pub(crate)`, byte-fed, and popup-typed, and
//! the `crates/fno` freeze keeps the two from being one shared function
//! until v0.1.0 ships (the same trade `goto.rs` makes for its ranker).
//! What IS shared: the schema `edit` fields, the row grammar, and the
//! write door - the in-process `fno config` set and unset calls, never a
//! raw TOML write. The write lives in `window.rs` where the gpui tasks live.

use gpui::{
    div, prelude::*, px, rgb, rgba, AnyElement, Context, Div, ElementId, FontWeight, ScrollHandle,
    Window,
};
use std::cell::Cell;
use std::path::Path;

use serde_json::Value;

use super::keys::{Key, SHEET};
use super::palette::Palette;
use super::ui::{self, Edit, LineField, Role};
use super::window::Root;
use fno::app_api::client::org_seats::Machine;
use fno::ui::theme::THEME_NAMES;

/// The five sections, in panel order.
pub(super) const SECTIONS: [&str; 5] = ["Look", "Alerts", "Machines", "Keys", "Updates"];

/// One selectable value on a choice page.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Choice {
    pub value: String,
    pub doc: &'static str,
}

/// How a row answers Enter. The app-side twin of the mux model's
/// vocabulary; the caller resolves the outcome against the real config.
#[derive(Debug, PartialEq)]
pub(crate) enum Mode {
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
    Locked {
        reason: String,
    },
}

/// One row of the panel: the dotted key, the painted label, the current
/// value text, and how Enter treats it.
#[derive(Debug, PartialEq)]
pub(crate) struct Row {
    pub id: &'static str,
    pub label: String,
    pub value: String,
    pub mode: Mode,
}

/// What Enter resolved to: the caller writes it through `fno config set`.
#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
    /// Flip a bool key (a toggle row) or step a cycle row.
    Set { id: &'static str, value: String },
    /// Confirm one option of an open page.
    Page { index: usize, options: Vec<Choice> },
    /// `fno config unset`, the Follow macOS theme pseudo-choice.
    Unset { id: &'static str },
    /// Nothing to write; repaint only.
    None,
}

/// The panel's live state: the query field, the cursor over the visible
/// rows, an open choice page, and the last refusal under its row.
pub(crate) struct SettingsState {
    pub query: LineField,
    pub selected: usize,
    pub page: Option<(String, Vec<Choice>, usize)>,
    pub note: Option<String>,
    pub rows: Vec<SectionRows>,
    /// The open-time monospaced family list, so the Font page never
    /// re-probes the text system per frame.
    pub fonts: Vec<String>,
    /// The row list's scroll position; a jump or a cursor move asks it to
    /// bring its row (and the section head above it) into view.
    pub scroll: ScrollHandle,
    pub follow: Cell<bool>,
}

impl SettingsState {
    /// Move the cursor to the first visible row of section `si`, if any.
    pub(crate) fn jump_to_section(&mut self, si: usize) {
        let vis = visible(&self.rows, self.query.text());
        if let Some(i) = vis.iter().position(|(s, _)| *s == si) {
            self.selected = i;
            self.follow.set(true);
        }
    }
}

/// One section's rows, keyed by its SECTIONS name.
pub(crate) type SectionRows = (&'static str, Vec<Row>);

/// The five sections' rows, from the raw global config, the theme cwd,
/// the fleet read, and the open-time font list. Pure: no gpui types.
pub(crate) fn build_rows(
    config: Option<&Value>,
    cwd: &Path,
    machines: &[Machine],
    fonts: &[String],
) -> Vec<SectionRows> {
    vec![
        ("Look", look_rows(config, cwd, fonts)),
        ("Alerts", alerts_rows(config)),
        ("Machines", machine_rows(machines)),
        ("Keys", key_rows()),
        ("Updates", updates_rows()),
    ]
}

/// Whether a Look row's decider is the project-local file: the app writes
/// the global file, so a project pin outranks it and the row dims.
fn project_pin(id: &str, cwd: &Path) -> Option<String> {
    let (decider, _) = fno::config_cli::resolve_source(id, cwd)?;
    let global = fno::config_cli::global_settings_path();
    let global_toml = global.with_file_name("config.toml");
    let pinned = decider != global && decider != global_toml;
    pinned.then(|| decider.display().to_string())
}

/// One dotted key's raw value out of the settings dict. The RAW read: the
/// schema default never reads as a written key (the launch ladder's rule).
fn raw_value(config: Option<&Value>, key: &str) -> Option<Value> {
    config.and_then(|raw| fno::config_cli::raw_get(raw, key).cloned())
}

/// The Look section: the theme pin, the font, its size, the first view.
fn look_rows(config: Option<&Value>, cwd: &Path, fonts: &[String]) -> Vec<Row> {
    let theme = raw_value(config, "mux.theme").and_then(|v| v.as_str().map(str::to_string));
    let theme_mode = match project_pin("mux.theme", cwd) {
        Some(path) => Mode::Locked {
            reason: format!("set in {path}, which outranks the global file"),
        },
        None => Mode::Page {
            options: theme_choices(),
            current: theme_current(&theme),
        },
    };
    let theme_value = theme.clone().unwrap_or_else(|| "Follow macOS".to_string());
    let font_raw = raw_value(config, "app.font").and_then(|v| v.as_str().map(str::to_string));
    let size_raw = raw_value(config, "app.font_size").and_then(|v| v.as_f64());
    let font_display = font_raw.clone().unwrap_or_else(|| "SF Mono".to_string());
    let size_list: Vec<String> = ["11", "12", "13", "14", "15", "16", "18"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let size_current = size_raw
        .map(|f| {
            let txt = format!("{f}");
            size_list.iter().position(|s| **s == txt).unwrap_or(2)
        })
        .unwrap_or(2);
    let launch_raw =
        raw_value(config, "mux.launch_screen").and_then(|v| v.as_str().map(str::to_string));
    let launch_list = ["last view", "agents", "hq"];
    let launch_current = launch_raw
        .as_deref()
        .and_then(|w| launch_list.iter().position(|s| *s == w))
        .unwrap_or(0);
    vec![
        Row {
            id: "mux.theme",
            label: "Theme".into(),
            value: theme_value,
            mode: theme_mode,
        },
        Row {
            id: "app.font",
            label: "Terminal font".into(),
            value: font_display,
            mode: Mode::Page {
                options: fonts
                    .iter()
                    .map(|f| Choice {
                        value: f.clone(),
                        doc: "",
                    })
                    .collect(),
                current: fonts
                    .iter()
                    .position(|f| *f == font_raw.as_deref().unwrap_or("SF Mono"))
                    .unwrap_or(0),
            },
        },
        Row {
            id: "app.font_size",
            label: "Terminal font size".into(),
            value: size_raw
                .map(|f| format!("{f}"))
                .unwrap_or_else(|| "13 (default)".to_string()),
            mode: Mode::Cycle {
                options: size_list,
                current: size_current,
            },
        },
        Row {
            id: "mux.launch_screen",
            label: "First view".to_string(),
            value: launch_raw.unwrap_or_else(|| "last view".to_string()),
            mode: Mode::Cycle {
                options: launch_list.iter().map(|s| s.to_string()).collect(),
                current: launch_current,
            },
        },
    ]
}

fn theme_choices() -> Vec<Choice> {
    let mut out = vec![Choice {
        value: String::new(),
        doc: "follow the system appearance",
    }];
    for name in THEME_NAMES.iter() {
        out.push(Choice {
            value: name.to_string(),
            doc: "",
        });
    }
    out
}

/// The page's selection lands on the current theme; Follow macOS (empty)
/// is index 0, so the unset key opens with its own choice picked.
fn theme_current(theme: &Option<String>) -> usize {
    match theme.as_deref() {
        None => 0,
        Some(name) => 1 + THEME_NAMES.iter().position(|n| *n == name).unwrap_or(0),
    }
}

/// The Alerts section: the app's own three attention switches.
fn alerts_rows(config: Option<&Value>) -> Vec<Row> {
    let on = |key: &str, default: bool| -> bool {
        raw_value(config, key)
            .and_then(|v| v.as_bool())
            .unwrap_or(default)
    };
    let mk = |id, label: &'static str, on: bool| -> Row {
        Row {
            id,
            label: label.into(),
            value: if on { "on".into() } else { "off".into() },
            mode: Mode::Toggle { on },
        }
    };
    vec![
        mk(
            "app.alert_sound",
            "Play the attention sound",
            on("app.alert_sound", true),
        ),
        mk(
            "app.alert_notification",
            "Post the alert notification",
            on("app.alert_notification", true),
        ),
        mk(
            "app.alert_badge",
            "Badge blocked agents on the Dock",
            on("app.alert_badge", true),
        ),
    ]
}

/// The Machines section: one locked row per fleet machine.
fn machine_rows(machines: &[Machine]) -> Vec<Row> {
    machines
        .iter()
        .map(|m| {
            let seats: usize = m.leads.iter().map(|l| l.seats.len()).sum();
            let name = m.display_name.clone().unwrap_or_else(|| m.host.clone());
            let fresh = if m.fresh {
                "live".to_string()
            } else {
                format!("seen {}s ago", m.age_s)
            };
            Row {
                id: "machine",
                label: name,
                value: format!("{seats} seat{} {fresh}", if seats == 1 { "" } else { "s" }),
                mode: Mode::Locked {
                    reason: "add or connect a host from the mux".into(),
                },
            }
        })
        .collect()
}

/// The Keys section: the key sheet's table, read-only in place.
fn key_rows() -> Vec<Row> {
    SHEET
        .iter()
        .flat_map(|(_, rows)| rows.iter())
        .map(|(keys, label)| Row {
            id: "key",
            label: format!("{keys}  {label}"),
            value: String::new(),
            mode: Mode::Locked {
                reason: "keys are fixed".into(),
            },
        })
        .collect()
}

/// The Updates section: the plan's Deletable disposition ships it as one
/// version row.
fn updates_rows() -> Vec<Row> {
    vec![Row {
        id: "version",
        label: "fno-app".into(),
        value: env!("CARGO_PKG_VERSION").to_string(),
        mode: Mode::Locked {
            reason: "updates arrive with the release".into(),
        },
    }]
}

/// The rows the query keeps: a label, key or section name that contains
/// the query (lowercased) survives, locked rows included.
pub(crate) fn visible<'a>(rows: &'a [SectionRows], query: &str) -> Vec<(usize, &'a Row)> {
    let q = query.to_lowercase();
    rows.iter()
        .enumerate()
        .flat_map(|(si, (_, srows))| srows.iter().map(move |r| (si, r)))
        .filter(|(si, r)| {
            q.is_empty()
                || r.label.to_lowercase().contains(&q)
                || r.id.contains(&q)
                || SECTIONS
                    .get(*si)
                    .is_some_and(|s| s.to_lowercase().contains(&q))
        })
        .collect()
}

/// What Enter on the selected visible row resolves to. Pure. A cycle's
/// next value resolves here too; the unset pseudo-choices (an empty page
/// choice, `last view`) resolve to Unset, the Follow-macOS door.
pub(crate) fn resolve_enter(rows: &[SectionRows], q: &str, sel: usize) -> Outcome {
    let vis = visible(rows, q);
    let Some((_, row)) = vis.get(sel) else {
        return Outcome::None;
    };
    match &row.mode {
        Mode::Toggle { on } => Outcome::Set {
            id: row.id,
            value: if *on {
                "false".to_string()
            } else {
                "true".to_string()
            },
        },
        Mode::Cycle { options, current } => {
            if options.len() < 2 {
                return Outcome::None;
            }
            let Some(next) = options.get((current + 1) % options.len()).cloned() else {
                return Outcome::None;
            };
            if next == "last view" {
                Outcome::Unset { id: row.id }
            } else {
                Outcome::Set {
                    id: row.id,
                    value: next,
                }
            }
        }
        Mode::Page { options, current } => Outcome::Page {
            index: *current,
            options: options.clone(),
        },
        Mode::Locked { .. } => Outcome::None,
    }
}

/// What a key on the open panel handed back to the window: a write to
/// resolve through the CLI door, or a close.
#[derive(Debug, PartialEq)]
pub(crate) enum Event {
    Close,
    Set { id: String, value: String },
    Unset { id: String },
}

/// What Enter resolves on the panel's current selection, as an Event.
fn enter_out(state: &mut SettingsState) -> Option<Event> {
    match resolve_enter(&state.rows, state.query.text(), state.selected) {
        Outcome::Set { id, value } => Some(Event::Set {
            id: id.to_string(),
            value,
        }),
        Outcome::Unset { id } => Some(Event::Unset { id: id.to_string() }),
        Outcome::Page { index, options } => {
            let id = visible(&state.rows, state.query.text())
                .get(state.selected)
                .map(|(_, r)| r.id.to_string());
            state.page = id.map(|id| (id, options, index));
            None
        }
        Outcome::None => None,
    }
}

/// One key on the open panel. `None` = consumed, repaint; `Some` = the
/// window resolves it. An open page folds its own keys; otherwise the
/// query, cursor and Enter resolve per the grammar.
pub(crate) fn key(state: &mut SettingsState, k: &Key) -> Option<Event> {
    if state.page.is_some() {
        match k.key {
            "enter" => {
                let (id, options, sel) = state.page.take().expect("page open");
                return match options.into_iter().nth(sel) {
                    Some(c) if c.value.is_empty() => Some(Event::Unset { id }),
                    Some(c) => Some(Event::Set { id, value: c.value }),
                    None => None,
                };
            }
            "escape" => {
                state.page = None;
                return None;
            }
            "up" | "down" => {
                if let Some((_, options, sel)) = state.page.as_mut() {
                    if k.key == "up" {
                        *sel = sel.saturating_sub(1);
                    } else if *sel + 1 < options.len() {
                        *sel += 1;
                    }
                }
                return None;
            }
            _ => {
                if let Some((_, options, sel)) = state.page.as_mut() {
                    if let Some(d) = k
                        .key_char
                        .and_then(|c| c.chars().next())
                        .and_then(|c| c.to_digit(10))
                    {
                        let idx = d.wrapping_sub(1) as usize;
                        if idx < options.len() {
                            *sel = idx;
                        }
                    }
                }
                return None;
            }
        }
    }
    match k.key {
        "up" => {
            state.selected = state.selected.saturating_sub(1);
            state.follow.set(true);
            None
        }
        "down" => {
            let n = visible(&state.rows, state.query.text()).len();
            if state.selected + 1 < n {
                state.selected += 1;
            }
            state.follow.set(true);
            None
        }
        "tab" => {
            let vis = visible(&state.rows, state.query.text());
            let here = vis.get(state.selected).map(|(si, _)| *si).unwrap_or(0);
            let next = vis
                .iter()
                .map(|(si, _)| *si)
                .find(|si| *si > here)
                .or_else(|| vis.first().map(|(si, _)| *si));
            if let Some(si) = next {
                state.jump_to_section(si);
            }
            None
        }
        "enter" => enter_out(state),
        "escape" => {
            if state.query.text().is_empty() {
                Some(Event::Close)
            } else {
                state.query.clear();
                None
            }
        }
        _ => match state.query.edit(k.key, k.key_char) {
            Edit::Typed => {
                state.selected = 0;
                state.follow.set(true);
                None
            }
            Edit::Close => Some(Event::Close),
            Edit::Submit => enter_out(state),
        },
    }
}

/// The Settings overlay: a dimmed backdrop, the centered panel in the key
/// sheet's look, search on top, sections left and rows right, the footer
/// grammar line. A click outside closes; keys arrive through `key`.
pub(super) fn render(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let Some(state) = root.settings.as_ref() else {
        return div();
    };
    let footer = if state.page.is_some() {
        "Enter to confirm · Esc to cancel"
    } else {
        "Type to filter · Enter to select · Esc to clear"
    };
    let panel = div()
        .id("settings-panel")
        .flex()
        .flex_col()
        .w(px(720.0))
        .max_h(px(560.0))
        .px_4()
        .py_3()
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(p.border))
        .bg(rgb(p.panel))
        .font_family(ui::MONO)
        .text_size(px(ui::BODY))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(body(root, cx))
        .child(ui::role(div(), Role::Hint, &p).pt(px(10.0)).child(footer));

    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x0000_0066))
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|root, _, _, cx| root.close_settings(cx)),
        )
        .child(panel)
}

/// The panel's middle: the title and search row over the section list or
/// the open page.
fn body(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let Some(state) = root.settings.as_ref() else {
        return div();
    };
    let mut panel = div().flex().flex_col().min_h_0();
    panel = panel.child(
        div()
            .flex()
            .flex_row()
            .items_baseline()
            .child(
                div()
                    .flex_1()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Settings"),
            )
            .child(ui::kbd("esc", &p)),
    );
    if let Some((id, options, sel)) = state.page.as_ref() {
        panel = panel.child(page_body(id, options, *sel, &state.rows, &p));
        return panel;
    }
    let q = state.query.text();
    let search = if q.is_empty() {
        ui::role(div(), Role::Hint, &p).child("Type to filter")
    } else {
        ui::role(div(), Role::Body, &p).child(q.to_string())
    };
    panel = panel.child(search.pb(px(6.0)));
    panel = panel.child(rows_body(root, cx));
    panel
}

/// The section rail and the filtered rows: each group under its heading,
/// the rail jumping to a group when clicked.
fn rows_body(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let Some(state) = root.settings.as_ref() else {
        return div();
    };
    let vis = visible(&state.rows, state.query.text());
    let filtering = !state.query.text().is_empty();
    let here = vis.get(state.selected).map(|(si, _)| *si);
    let mut wrap = div().flex().flex_row().min_h_0();
    if !filtering {
        let rail = div().flex_none().w(px(96.0)).flex().flex_col().children(
            SECTIONS.iter().enumerate().map(|(si, name)| {
                let on = here == Some(si);
                ui::clickable(
                    ui::row(
                        ElementId::NamedInteger("settings-rail".into(), si as u64),
                        on,
                        &p,
                    )
                    .px_0()
                    .child(ui::role(div(), Role::Label, &p).child(*name)),
                    &p,
                )
                .on_click(cx.listener(move |root, _, _, cx| {
                    if let Some(st) = root.settings.as_mut() {
                        st.jump_to_section(si);
                    }
                    cx.notify();
                }))
            }),
        );
        wrap = wrap.child(rail);
    }
    let mut children: Vec<AnyElement> = Vec::new();
    let mut scroll_to: Vec<usize> = Vec::new();
    let mut last = None;
    for (i, (si, row)) in vis.iter().enumerate() {
        if last != Some(*si) {
            last = Some(*si);
            let name = SECTIONS.get(*si).copied().unwrap_or("");
            let mut head = div()
                .flex_none()
                .pt(px(8.0))
                .child(ui::role(div(), Role::Heading, &p).child(name));
            if let Some(reason) = state
                .rows
                .get(*si)
                .and_then(|(_, rows)| section_reason(rows))
            {
                head = head.child(ui::role(div(), Role::Hint, &p).child(reason));
            }
            children.push(head.into_any_element());
        }
        // The cursor row scrolls into view with its group head above it
        // when it is the group's first row.
        let first_of_group = i == 0 || vis.get(i - 1).map(|(s, _)| *s) != Some(*si);
        scroll_to.push(if first_of_group {
            children.len() - 1
        } else {
            children.len()
        });
        children.push(row_line(row, i == state.selected, &p, cx).into_any_element());
    }
    if state.follow.replace(false) {
        if let Some(ix) = scroll_to.get(state.selected) {
            state.scroll.scroll_to_item(*ix);
        }
    }
    let list = div()
        .id("settings-rows")
        .flex_1()
        .min_w_0()
        .min_h_0()
        .overflow_x_hidden()
        .overflow_y_scroll()
        .track_scroll(&state.scroll)
        .children(children);
    wrap.child(list)
}

/// The one reason a whole section shares, when every row is locked for
/// it: the heading says why once, so the rows stay quiet.
fn section_reason(rows: &[Row]) -> Option<&str> {
    let mut reasons = rows.iter().map(|r| match &r.mode {
        Mode::Locked { reason } => Some(reason.as_str()),
        _ => None,
    });
    let first = reasons.next()??;
    reasons.all(|r| r == Some(first)).then_some(first)
}

/// One row: label left, value right, the mode glyph leading, a locked
/// row's reason dimmed under it while the row is selected.
fn row_line(row: &Row, selected: bool, p: &Palette, cx: &mut Context<Root>) -> Div {
    let _ = cx;
    let locked = matches!(row.mode, Mode::Locked { .. });
    let (glyph, value) = match &row.mode {
        Mode::Toggle { .. } | Mode::Locked { .. } => (" ".to_string(), row.value.clone()),
        Mode::Cycle { .. } => (" ".to_string(), format!("< {} >", row.value)),
        Mode::Page { .. } => ("›".to_string(), row.value.clone()),
    };
    let label_role = if locked { Role::Hint } else { Role::Name };
    let line = ui::row(ElementId::Name(row.label.clone().into()), selected, p)
        .child(ui::text(glyph, Role::Meta, p))
        .child(ui::role(div(), label_role, p).child(row.label.clone()))
        .child(div().flex_1())
        .child(ui::role(div(), Role::Meta, p).child(value));
    match &row.mode {
        Mode::Locked { reason } if selected => div()
            .flex_none()
            .min_w_0()
            .child(line)
            .child(ui::role(div(), Role::Hint, p).px_3().child(reason.clone())),
        _ => div().flex_none().min_w_0().child(line),
    }
}

/// The open choice page: the row's label as the heading, one numbered
/// line per option, the check on the current selection.
fn page_body(id: &str, options: &[Choice], sel: usize, rows: &[SectionRows], p: &Palette) -> Div {
    let heading = rows
        .iter()
        .flat_map(|(_, srows)| srows.iter())
        .find(|r| r.id == id)
        .map(|r| r.label.clone())
        .unwrap_or_else(|| id.to_string());
    let mut page = div().flex().flex_col().min_h_0();
    page = page.child(
        ui::role(div(), Role::Heading, p)
            .pt(px(4.0))
            .pb(px(6.0))
            .child(heading),
    );
    for (i, c) in options.iter().enumerate() {
        let mark = if i == sel { "✓" } else { " " };
        let n = format!("{}", i + 1);
        let (name, doc) = if c.value.is_empty() {
            (c.doc.to_string(), String::new())
        } else if c.doc.is_empty() {
            (c.value.clone(), String::new())
        } else {
            (c.value.clone(), format!("  {}", c.doc))
        };
        page = page.child(
            ui::row(
                ElementId::NamedInteger("page-row".into(), i as u64),
                i == sel,
                p,
            )
            .child(ui::text(mark, Role::Meta, p))
            .child(ui::text(n, Role::Fact, p))
            .child(ui::text(name, Role::Body, p))
            .child(ui::text(doc, Role::Hint, p)),
        );
    }
    page
}

/// The installed monospaced families: `advance('i') == advance('m')`
/// keeps only fixed-pitch faces (the plan's Unknown-3 probe).
pub(crate) fn mono_fonts(window: &Window) -> Vec<String> {
    let ts = window.text_system();
    ts.all_font_names()
        .into_iter()
        .filter(|f| {
            let id = ts.resolve_font(&gpui::font(f));
            let i = ts.advance(id, px(12.0), 'i').map(|s| s.width);
            let m = ts.advance(id, px(12.0), 'm').map(|s| s.width);
            matches!((i, m), (Ok(a), Ok(b)) if a == b)
        })
        .collect()
}

/// Why a settings write failed: the CLI's first line, painted under the row.
#[derive(Debug)]
pub(crate) struct Refusal(pub String);

/// One settings write through the in-process config door: the same
/// `set_values` / `unset_value` the `fno config` command runs, on the
/// global file, with a 3 s bound on the file lock. `Err` carries the
/// refusal's message for the row to paint.
pub(crate) fn bounded_write(set: bool, id: &str, value: Option<&str>) -> Result<(), Refusal> {
    use fno::config_cli::write;
    let out = match (set, value) {
        (true, Some(v)) => write::set::set_values(
            &[(id.to_string(), v.to_string())],
            "global",
            Some(std::time::Duration::from_secs(3)),
        )
        .map(|_| ()),
        _ => write::unset::unset_value(id, "global").map(|_| ()),
    };
    out.map_err(|r| Refusal(r.message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(pairs: &[(&str, serde_json::Value)]) -> Value {
        let mut mux = serde_json::Map::new();
        let mut app = serde_json::Map::new();
        for (k, v) in pairs {
            match k.strip_prefix("mux.").or_else(|| k.strip_prefix("app.")) {
                Some(rest) => {
                    let target = if k.starts_with("mux.") {
                        &mut mux
                    } else {
                        &mut app
                    };
                    target.insert(rest.into(), v.clone());
                }
                None => {}
            }
        }
        serde_json::json!({"mux": mux, "app": app})
    }

    /// The five sections build in order, and a locked Machines row names
    /// its reason.
    #[test]
    fn five_sections_and_a_locked_reason() {
        let rows = build_rows(Some(&cfg(&[])), Path::new("/"), &[], &[]);
        let names: Vec<&str> = rows.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ["Look", "Alerts", "Machines", "Keys", "Updates"]);
        let machines = &rows.get(2).expect("machines section").1;
        assert_eq!(machines.len(), 0, "no fleet, no rows");
        let m = Machine {
            host: "m2".into(),
            local: false,
            fresh: false,
            age_s: 42,
            leads: Vec::new(),
            ..Machine::default()
        };
        let rows = build_rows(Some(&cfg(&[])), Path::new("/"), &[m], &[]);
        let machines = &rows.get(2).expect("machines section").1;
        assert_eq!(machines.len(), 1);
        assert_eq!(
            machines.first().expect("one machine").mode,
            Mode::Locked {
                reason: "add or connect a host from the mux".into()
            }
        );
        assert_eq!(
            machines.first().expect("one machine").value,
            "0 seats seen 42s ago"
        );
    }

    /// The filter matches a label, the dotted key, and the section name.
    #[test]
    fn filter_matches_label_key_and_section() {
        let rows = build_rows(Some(&cfg(&[])), Path::new("/"), &[], &[]);
        let hits = visible(&rows, "notification").len();
        assert_eq!(hits, 1, "notification matches the notification row");
        assert_eq!(visible(&rows, "sound").len(), 1);
        assert_eq!(visible(&rows, "app.alert_sound").len(), 1);
        assert_eq!(
            visible(&rows, "alerts").len(),
            3,
            "the section name matches"
        );
        assert_eq!(visible(&rows, "theme").len(), 1);
    }

    /// Enter on a toggle writes the other state; a cycle steps; a page
    /// opens on the current choice; a locked row stays inert.
    #[test]
    fn enter_resolves_per_mode() {
        let rows = build_rows(
            Some(&cfg(&[("app.alert_sound", true.into())])),
            Path::new("/"),
            &[],
            &[],
        );
        let out = resolve_enter(&rows, "", 4);
        assert_eq!(
            out,
            Outcome::Set {
                id: "app.alert_sound",
                value: "false".to_string()
            }
        );
        let out = resolve_enter(&rows, "theme", 0);
        assert!(matches!(out, Outcome::Page { .. }), "theme opens its page");
        assert_eq!(resolve_enter(&rows, "machine", 0), Outcome::None);
    }

    /// Esc clears a query first, then closes; a typed query narrows and
    /// Enter resolves within it; a page opens and confirms through its
    /// numbered options.
    #[test]
    fn the_panel_key_flow() {
        let rows = build_rows(Some(&cfg(&[])), Path::new("/"), &[], &[]);
        let mut st = SettingsState {
            query: ui::LineField::new(64),
            selected: 0,
            page: None,
            note: None,
            rows,
            fonts: vec!["SF Mono".into(), "Menlo".into()],
            scroll: ScrollHandle::new(),
            follow: Cell::new(false),
        };
        st.query.edit("t", Some("t"));
        assert_eq!(
            key(
                &mut st,
                &Key {
                    key: "escape",
                    ..Key::default()
                }
            ),
            None
        );
        assert_eq!(st.query.text(), "", "esc clears the query first");
        assert_eq!(
            key(
                &mut st,
                &Key {
                    key: "escape",
                    ..Key::default()
                }
            ),
            Some(Event::Close)
        );
        // The theme row opens its page on the Follow-macOS choice.
        st.query.edit("theme", Some("theme"));
        assert_eq!(
            key(
                &mut st,
                &Key {
                    key: "enter",
                    ..Key::default()
                }
            ),
            None
        );
        assert!(st.page.is_some(), "the page opened");
        // Confirm the current choice: Follow macOS = unset.
        assert_eq!(
            key(
                &mut st,
                &Key {
                    key: "enter",
                    ..Key::default()
                }
            ),
            Some(Event::Unset {
                id: "mux.theme".into()
            })
        );
        assert!(st.page.is_none());
    }
}

/// The --shot-settings script: the panel, a filtered query, or the theme
/// page open. Each drives the real paths, as the find bar's shot does.
pub(crate) fn apply_shot(state: &mut SettingsState, shot: crate::SettingsShot) {
    match shot {
        crate::SettingsShot::Panel => {}
        crate::SettingsShot::Filter => {
            state.query.edit("", Some("alert"));
        }
        crate::SettingsShot::Page => {
            state.selected = 0;
            let _ = key(
                state,
                &Key {
                    key: "enter",
                    ..Key::default()
                },
            );
        }
    }
}
