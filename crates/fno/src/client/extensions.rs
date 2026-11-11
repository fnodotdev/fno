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
use serde::Deserialize;
use std::time::{Duration, Instant};

const LS: [&str; 4] = ["config", "plugins", "ls", "--json"];
const GALLERY: [&str; 4] = ["config", "plugins", "gallery", "--json"];
const MAX_URL: usize = 512;
const PLACEHOLDER: &str = "https://github.com/owner/repo";
const REREAD: Duration = Duration::from_secs(3);

/// One mod or layout row of `ls --json`, with its pack's install commit.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct Component {
    pub(crate) id: String,
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) summary: String,
    #[serde(default)]
    pub(crate) preview: String,
    #[serde(default)]
    pub(crate) recipe: String,
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) runs_code: bool,
    /// `run`, `feed` or `native`; a layout carries none.
    #[serde(default)]
    pub(crate) kind: String,
    /// A command mod's argv.
    #[serde(default)]
    pub(crate) run: Option<Vec<String>>,
    /// The commit a URL install recorded; empty for a local pack.
    #[serde(skip)]
    pub(crate) commit: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SourceRow {
    commit: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PackRow {
    pack_id: String,
    #[serde(default)]
    source: Option<SourceRow>,
    #[serde(default)]
    mods: Vec<Component>,
    #[serde(default)]
    layouts: Vec<Component>,
}

#[derive(Debug, Deserialize)]
struct Listing {
    packs: Vec<PackRow>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct Published {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) stars: u64,
    pub(crate) install: String,
    #[serde(default)]
    pub(crate) reviewed: bool,
}

#[derive(Debug, Deserialize)]
struct GalleryRead {
    repos: Vec<Published>,
}

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

/// A failed `fno config plugins` read, as the one line the page paints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadFailed(pub(crate) String);

impl std::fmt::Display for ReadFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A read that landed off the input loop.
#[derive(Debug, Clone)]
pub(crate) enum Landed {
    Installed(Result<Vec<u8>, ReadFailed>),
    Gallery(Result<Vec<Published>, ReadFailed>),
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

/// One argv shown so a reader can paste it: plain words bare, anything
/// else single-quoted.
pub(crate) fn shell_words(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            let plain = !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:@,+".contains(c));
            if plain {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn fno_json(args: &[&str]) -> Result<Vec<u8>, ReadFailed> {
    let shown = format!("fno {}", args.join(" "));
    let mut cmd = crate::process_admission::std_command(crate::server::fno_bin());
    cmd.args(args).stdin(std::process::Stdio::null());
    let out = crate::process_admission::std_output(&mut cmd)
        .map_err(|e| ReadFailed(format!("`{shown}` did not start: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(ReadFailed(format!(
            "`{shown}` exited {}: {}",
            out.status.code().unwrap_or(-1),
            first.trim()
        )));
    }
    Ok(out.stdout)
}

/// Parse `ls --json` into page rows, mods before layouts per pack.
pub(crate) fn parse_installed(
    read: Result<Vec<u8>, ReadFailed>,
) -> Result<Vec<(String, Component)>, ReadFailed> {
    let bytes = read?;
    let listing: Listing = serde_json::from_slice(&bytes)
        .map_err(|e| ReadFailed(format!("`fno {}` printed no listing: {e}", LS.join(" "))))?;
    Ok(listing
        .packs
        .into_iter()
        .flat_map(|p| {
            let id = p.pack_id;
            let commit = p.source.map(|s| s.commit).unwrap_or_default();
            p.mods.into_iter().chain(p.layouts).map(move |mut c| {
                c.commit = commit.clone();
                (id.clone(), c)
            })
        })
        .collect())
}

/// Read the installed rows now (the tab switch; a local, fast read).
pub(super) fn refresh(view: &mut View) {
    view.extensions.installed = Some(parse_installed(fno_json(&LS)));
    view.extensions.last_read = Some(Instant::now());
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
            let read = tokio::task::spawn_blocking(|| fno_json(&LS))
                .await
                .unwrap_or_else(|e| Err(ReadFailed(e.to_string())));
            let _ = tx.send((0, OrgMsg::Extensions(Landed::Installed(read))));
        });
    }
    if page.gallery == GalleryState::Wanted {
        page.gallery = GalleryState::Reading;
        let tx = tx.clone();
        tokio::spawn(async move {
            let read = tokio::task::spawn_blocking(|| {
                fno_json(&GALLERY).and_then(|bytes| {
                    serde_json::from_slice::<GalleryRead>(&bytes)
                        .map(|g| g.repos)
                        .map_err(|e| {
                            ReadFailed(format!(
                                "`fno {}` printed no gallery: {e}",
                                GALLERY.join(" ")
                            ))
                        })
                })
            })
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

/// Close the URL field or the armed confirm. True when one was open.
pub(super) fn cancel_url(view: &mut View) -> bool {
    let page = &mut view.extensions;
    page.url.take().is_some() | page.confirm.take().is_some()
}

/// Submit the URL field: run the install in its own pane, where the consent
/// card prints and the prompt reads a real terminal. The pane holds its
/// result until Enter, and the list re-reads on its own.
pub(super) fn submit(view: &mut View, url: String) -> bool {
    let url = url.trim().to_string();
    if url.is_empty() || url.starts_with('-') {
        view.set_notice("type a git URL".into());
        return false;
    }
    view.extensions.url = None;
    let notice = in_pane(&["install", url.as_str()], &format!("install {url}"));
    view.set_notice(notice);
    true
}

/// Run `fno config plugins <args>` in a new pane on the right through
/// `sh`, which keeps the pane open on the result until Enter. The argv rides
/// as positional parameters, so no word of it is shell-parsed.
fn in_pane(args: &[&str], what: &str) -> String {
    let fno = crate::server::fno_bin();
    let fno_text = fno.to_string_lossy().into_owned();
    let script = r#"fno="$1"; shift; "$fno" config plugins "$@"; printf '\n[exit %s] press enter to close\n' "$?"; read -r _"#;
    let mut cmd = crate::process_admission::std_command(&fno);
    cmd.args([
        "mux", "pane", "run", "--split", "right", "--", "sh", "-c", script, "sh",
    ])
    .arg(&fno_text)
    .args(args)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null());
    match crate::process_admission::std_spawn(&mut cmd) {
        Ok(mut child) => {
            // `pane run` returns once the pane exists; reap it off the loop.
            std::thread::spawn(move || child.wait());
            format!("{what}: opened in a pane on the right")
        }
        Err(e) => format!("{what}: cannot open a pane: {e}"),
    }
}

/// Open a pack file (a preview or a recipe) in the file viewer.
pub(super) fn load(path: String) -> file_viewer::Loaded {
    file_viewer::load(PathBuf::from(path), None, Default::default())
}

fn switch(view: &mut View, target: &str, on: bool) {
    let command = if on { "enable" } else { "disable" };
    let notice = match fno_json(&["config", "plugins", command, target]) {
        Ok(_) => format!("{target} {}", if on { "on" } else { "off" }),
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
            let notice = in_pane(&["open", target.as_str()], &target);
            view.set_notice(notice);
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
