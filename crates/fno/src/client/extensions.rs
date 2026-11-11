//! The settings modal's Extensions tab: the mods and layouts installed
//! packs declare, each with its switch, its summary, its preview and its
//! recipe, plus an install row and the published gallery.
//!
//! The rows come from `fno config plugins ls --json` and the gallery from
//! `fno config plugins gallery --json`, the same feeds the GPUI app reads,
//! so both hosts show one list. Both reads run off the input loop
//! (`kick`) and land through the org channel (`apply`). While the tab is
//! open the installed list re-reads every few seconds, so an install that
//! finishes in its pane shows up without a key. A failed read paints one
//! notice row naming the command and its first stderr line.

use super::input_field::{back_row, InputField};
use super::org_board::{OrgMsg, OrgTx};
use super::*;
use crate::plugins_feed::{self as feed, run_in_pane};
pub(crate) use crate::plugins_feed::{
    parse_installed, shell_words, Component, Published, ReadFailed,
};
use std::time::{Duration, Instant};

const MAX_URL: usize = 512;
const PLACEHOLDER: &str = "https://github.com/owner/repo";
const REREAD: Duration = Duration::from_secs(3);

/// A row action on the Extensions page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Act {
    /// Switch `pack/id` to the carried state.
    Switch(String, bool),
    /// The second press that turns on a mod that runs code.
    Confirm(String),
    /// Open a pack file (a preview image or a recipe) in the file viewer.
    Open(String),
    /// Open the URL field.
    Install,
    /// Read the published gallery.
    Browse,
    /// A gallery row: open the URL field holding its URL.
    Pick(String),
    /// Show `pack/mod` in a pane on the right through `fno config plugins open`.
    Show(String),
}

/// A read that landed off the input loop.
#[derive(Debug, Clone)]
pub(crate) enum Landed {
    Installed(Result<Vec<u8>, ReadFailed>),
    Gallery(Result<Vec<Published>, ReadFailed>),
    /// A pane launch answered: the notice's subject and the launcher's result.
    Paned(String, Result<(), ReadFailed>),
}

/// An install or open waiting for `kick` to run it off the loop.
#[derive(Debug, Clone)]
pub(crate) struct PaneAsk {
    args: Vec<String>,
    what: String,
    anchor: u64,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) enum GalleryState {
    #[default]
    Idle,
    Wanted,
    Reading,
    Done(Result<Vec<Published>, ReadFailed>),
}

/// The page state: the last reads, the open URL field and an armed confirm.
#[derive(Debug, Default)]
pub(crate) struct Extensions {
    /// `(pack id, component)` pairs, or the failed read's notice.
    pub(crate) installed: Option<Result<Vec<(String, Component)>, ReadFailed>>,
    pub(crate) gallery: GalleryState,
    pub(crate) url: Option<InputField>,
    /// The code mod a first press armed; the second press turns it on.
    pub(crate) confirm: Option<String>,
    pane: Option<PaneAsk>,
    reading: bool,
    last_read: Option<Instant>,
}

/// The snapshot's page: the first-party pack, a URL pack with a code mod
/// and a layout, and optionally the gallery or the armed confirm. No read
/// runs, so the shot is the same on every machine.
pub(super) fn snapshot_seam(view: &mut View, gallery: bool, confirm: bool) {
    let listing = br#"{"packs": [
        {"pack_id": "fno-panels", "mods": [
            {"id": "backlog-board", "title": "Backlog board", "kind": "native", "enabled": true,
             "summary": "The board view, shipped with fno", "preview": "/demo/fno-panels/backlog-board.png"},
            {"id": "review-ledger", "title": "Review ledger", "kind": "feed", "enabled": true,
             "summary": "Review rounds per branch", "preview": "/demo/fno-panels/review-ledger.png",
             "recipe": "/demo/fno-panels/RECIPE.md"},
            {"id": "decisions", "title": "Decisions", "kind": "native", "enabled": true}]},
        {"pack_id": "demo-clock", "source": {"commit": "4f2a9c1e7b3d5a6f8e0c"}, "mods": [
            {"id": "clock", "title": "Clock", "kind": "run", "enabled": false, "runs_code": true,
             "summary": "A clock in a side pane", "run": ["sh", "-c", "while :; do date; sleep 1; done"]},
            {"id": "ci-feed", "title": "CI feed", "kind": "feed", "enabled": true,
             "summary": "Open PR checks"}],
         "layouts": [{"id": "focus", "title": "Focus", "enabled": false,
             "summary": "Board left, one agent right"}]}]}"#;
    let page = &mut view.extensions;
    page.installed = Some(parse_installed(Ok(listing.to_vec())));
    page.last_read = Some(Instant::now());
    page.gallery = if gallery {
        GalleryState::Done(Ok(vec![
            Published {
                name: "demo-org/fno-weather".into(),
                description: "Weather for the cwd's city in a side pane".into(),
                stars: 42,
                install: "fno config plugins install https://github.com/demo-org/fno-weather.git"
                    .into(),
                reviewed: false,
            },
            Published {
                name: "demo-org/fno-zen-layout".into(),
                description: "One agent, full width, no sideline".into(),
                stars: 7,
                install:
                    "fno config plugins install https://github.com/demo-org/fno-zen-layout.git"
                        .into(),
                reviewed: false,
            },
        ]))
    } else {
        GalleryState::Idle
    };
    page.confirm = confirm.then(|| "demo-clock/clock".to_string());
    view.settings_tab = SettingsTab::Extensions;
}

/// The tab came into focus: the next tick reads the installed rows off the
/// loop, and the last rows stay painted until that read lands.
pub(super) fn refresh(view: &mut View) {
    view.extensions.last_read = None;
}

fn tab_open(view: &View) -> bool {
    settings_modal::is_settings(view) && view.settings_tab == SettingsTab::Extensions
}

/// Start the reads the page owes, off the loop: the installed list while
/// the tab is open, and the gallery once it is asked for.
pub(super) fn kick(view: &mut View, tx: &OrgTx) {
    let open = tab_open(view);
    let page = &mut view.extensions;
    let due = page.last_read.is_none_or(|at| at.elapsed() >= REREAD);
    if open && due && !page.reading {
        page.reading = true;
        page.last_read = Some(Instant::now());
        let tx = tx.clone();
        tokio::spawn(async move {
            let read = tokio::task::spawn_blocking(|| feed::fno_json(&feed::LS))
                .await
                .unwrap_or_else(|e| Err(ReadFailed(e.to_string())));
            let _ = tx.send((0, OrgMsg::Extensions(Landed::Installed(read))));
        });
    }
    if let Some(ask) = page.pane.take() {
        let tx = tx.clone();
        tokio::spawn(async move {
            let what = ask.what.clone();
            let done = tokio::task::spawn_blocking(move || {
                let words: Vec<&str> = ask.args.iter().map(String::as_str).collect();
                run_in_pane(&words, &ask.what, ask.anchor, None)
            })
            .await
            .unwrap_or_else(|e| Err(ReadFailed(e.to_string())));
            let _ = tx.send((0, OrgMsg::Extensions(Landed::Paned(what, done))));
        });
    }
    if page.gallery == GalleryState::Wanted {
        page.gallery = GalleryState::Reading;
        let tx = tx.clone();
        tokio::spawn(async move {
            let read = tokio::task::spawn_blocking(feed::read_gallery)
                .await
                .unwrap_or_else(|e| Err(ReadFailed(e.to_string())));
            let _ = tx.send((0, OrgMsg::Extensions(Landed::Gallery(read))));
        });
    }
}

/// A read landed: store it and repaint the open tab.
pub(super) fn apply(view: &mut View, landed: Landed) {
    match landed {
        Landed::Installed(read) => {
            view.extensions.reading = false;
            view.extensions.installed = Some(parse_installed(read));
        }
        Landed::Gallery(read) => view.extensions.gallery = GalleryState::Done(read),
        Landed::Paned(what, done) => view.set_notice(match done {
            Ok(()) => format!("{what} opened in a pane"),
            Err(e) => e.0,
        }),
    }
    if tab_open(view) {
        view.reopen_settings_keeping_sel();
    }
}

fn entry(glyph: &str, label: String, hint: &str, enabled: bool) -> PopupRow {
    PopupRow::Entry {
        glyph: glyph.into(),
        label,
        hint: hint.into(),
        enabled,
    }
}

/// The page rows and their actions. Only enabled entries take an action:
/// the popup selects among targets, so inert rows carry none.
pub(super) fn rows(view: &View) -> (Vec<PopupRow>, Vec<AuxAction>) {
    let page = &view.extensions;
    let mut rows = Vec::new();
    let mut actions = Vec::new();
    let inert = |label: String| entry(" ", label, "", false);
    if let Some(field) = &page.url {
        rows.push(back_row());
        actions.push(AuxAction::SettingsBack);
        rows.push(field.row("url"));
        rows.push(inert(
            "A git URL. Enter opens the install in a pane.".into(),
        ));
        rows.push(inert(
            "The pane shows what the pack runs before it asks.".into(),
        ));
        return (rows, actions);
    }
    let listed = match &page.installed {
        Some(Ok(list)) => list.as_slice(),
        _ => &[],
    };
    if let Some(target) = &page.confirm {
        if let Some((_, c)) = listed
            .iter()
            .find(|(p, c)| format!("{p}/{}", c.id) == *target)
        {
            rows.push(back_row());
            actions.push(AuxAction::SettingsBack);
            rows.push(PopupRow::Header(format!("turn on {target}?")));
            let argv = c.run.as_deref().map(shell_words).unwrap_or_default();
            rows.push(inert(format!("Runs code: {argv}")));
            if !c.commit.is_empty() {
                let short = c.commit.get(..12).unwrap_or(&c.commit);
                rows.push(inert(format!("From commit {short}")));
            }
            rows.push(entry("!", "Turn it on and let it run".into(), "", true));
            actions.push(AuxAction::Extension(Act::Confirm(target.clone())));
            return (rows, actions);
        }
    }
    rows.push(PopupRow::Header("installed".into()));
    match &page.installed {
        None => rows.push(inert("Reading installed packs".into())),
        Some(Err(notice)) => rows.push(inert(notice.0.clone())),
        Some(Ok(list)) if list.is_empty() => {
            rows.push(inert("No mods or layouts installed".into()))
        }
        Some(Ok(list)) => {
            for (pack, c) in list {
                let target = format!("{pack}/{}", c.id);
                let hint = if c.runs_code { "Runs code" } else { "" };
                if c.kind == "native" {
                    rows.push(entry(
                        "■",
                        format!("{} · {target}", c.title),
                        "Built in",
                        false,
                    ));
                } else {
                    rows.push(entry(
                        if c.enabled { "☑" } else { "☐" },
                        format!("{} · {target}", c.title),
                        hint,
                        true,
                    ));
                    actions.push(AuxAction::Extension(Act::Switch(
                        target.clone(),
                        !c.enabled,
                    )));
                }
                if !c.summary.is_empty() {
                    rows.push(inert(format!("  {}", c.summary)));
                }
                if c.enabled && (c.kind == "feed" || c.kind == "run") {
                    rows.push(entry(" ", "  Open in a pane".into(), "", true));
                    actions.push(AuxAction::Extension(Act::Show(target)));
                }
                for (word, path) in [("Preview", &c.preview), ("Recipe", &c.recipe)] {
                    if !path.is_empty() {
                        rows.push(entry(" ", format!("  {word}"), "", true));
                        actions.push(AuxAction::Extension(Act::Open(path.clone())));
                    }
                }
            }
        }
    }
    rows.push(entry("+", "Install from URL".into(), "", true));
    actions.push(AuxAction::Extension(Act::Install));
    rows.push(PopupRow::Header("gallery".into()));
    match &page.gallery {
        GalleryState::Idle => {
            rows.push(entry("›", "Browse published packs".into(), "", true));
            actions.push(AuxAction::Extension(Act::Browse));
        }
        GalleryState::Wanted | GalleryState::Reading => {
            rows.push(inert("Reading the gallery".into()))
        }
        GalleryState::Done(Err(notice)) => rows.push(inert(notice.0.clone())),
        GalleryState::Done(Ok(list)) if list.is_empty() => {
            rows.push(inert("No published packs yet".into()))
        }
        GalleryState::Done(Ok(list)) => {
            for p in list {
                let hint = if p.reviewed {
                    ""
                } else {
                    "Not reviewed by fno"
                };
                rows.push(entry(
                    "·",
                    format!("{} · {} stars", p.name, p.stars),
                    hint,
                    true,
                ));
                actions.push(AuxAction::Extension(Act::Pick(p.install.clone())));
                if !p.description.is_empty() {
                    rows.push(inert(format!("  {}", p.description)));
                }
            }
        }
    }
    (rows, actions)
}

/// Escape: close the URL field first. An armed confirm stays until a second
/// Escape. True when one was open.
pub(super) fn cancel_url(view: &mut View) -> bool {
    let page = &mut view.extensions;
    page.url.take().is_some() || page.confirm.take().is_some()
}

/// Leaving the tab drops the URL field and the armed confirm together.
pub(super) fn reset(view: &mut View) {
    let page = &mut view.extensions;
    page.url = None;
    page.confirm = None;
}

/// Queue a pane beside the focused one; `kick` runs it off the loop.
fn queue_pane(view: &mut View, args: Vec<String>, what: String) {
    view.extensions.pane = Some(PaneAsk {
        args,
        what,
        anchor: view.layout.focus,
    });
}

/// Submit the URL field: run the install in its own pane, where the consent
/// card prints and the prompt reads a real terminal. The pane holds its
/// result until Enter, and the list re-reads on its own.
pub(super) fn submit(view: &mut View, url: String) -> bool {
    let url = url.trim().to_string();
    if url.is_empty() || url.starts_with('-') {
        view.set_notice("Type a git URL".into());
        return false;
    }
    view.extensions.url = None;
    queue_pane(
        view,
        vec!["install".into(), url.clone()],
        format!("Install {url}"),
    );
    true
}

/// Open a pack file (a preview or a recipe) in the file viewer.
pub(super) fn load(path: String) -> file_viewer::Loaded {
    file_viewer::load(PathBuf::from(path), None, Default::default())
}

fn switch(view: &mut View, target: &str, on: bool) {
    let notice = match feed::switch(target, on) {
        Ok(()) => format!("{target} {}", if on { "on" } else { "off" }),
        Err(e) => e.0,
    };
    view.set_notice(notice);
    refresh(view);
}

/// Every row action but `Open`, which the modal runs because it writes to the
/// server socket.
pub(crate) fn act(view: &mut View, act: Act) {
    match act {
        Act::Switch(target, on) => {
            let code = matches!(&view.extensions.installed, Some(Ok(list))
                if list.iter().any(|(p, c)| format!("{p}/{}", c.id) == target && c.runs_code));
            if on && code {
                view.extensions.confirm = Some(target);
            } else {
                switch(view, &target, on);
            }
        }
        Act::Confirm(target) => {
            view.extensions.confirm = None;
            switch(view, &target, true);
        }
        Act::Pick(install) => {
            let url = install.rsplit(' ').next().unwrap_or_default();
            view.extensions.url = Some(InputField::new(PLACEHOLDER, MAX_URL).with_text(url));
        }
        Act::Open(_) => {}
        Act::Show(target) => {
            queue_pane(view, vec!["open".into(), target.clone()], target);
        }
        Act::Install => {
            if view.extensions.url.is_none() {
                view.extensions.url = Some(InputField::new(PLACEHOLDER, MAX_URL));
            }
        }
        Act::Browse => view.extensions.gallery = GalleryState::Wanted,
    }
    view.reopen_settings_keeping_sel();
}
