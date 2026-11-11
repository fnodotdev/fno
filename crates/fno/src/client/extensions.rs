//! The settings modal's Extensions tab: the mods and layouts installed
//! packs declare, each with its switch, its preview and its recipe, plus an
//! install row and the published gallery.
//!
//! The rows come from `fno config plugins ls --json` and the gallery from
//! `fno config plugins gallery --json`: the same feeds the GPUI app reads,
//! so both hosts show one list. Each read runs once when the tab opens and
//! after a switch, never per frame. A failed read paints one notice row
//! naming the command and no mod rows.

use super::input_field::{back_row, InputField};
use super::*;
use serde::Deserialize;

const LS: [&str; 4] = ["config", "plugins", "ls", "--json"];
const GALLERY: [&str; 4] = ["config", "plugins", "gallery", "--json"];
const MAX_URL: usize = 512;
const PLACEHOLDER: &str = "https://github.com/owner/repo";

/// One mod or layout row of `ls --json`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct Component {
    pub(crate) id: String,
    pub(crate) title: String,
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
}

#[derive(Debug, Clone, Deserialize)]
struct PackRow {
    pack_id: String,
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
}

#[derive(Debug, Deserialize)]
struct Gallery {
    repos: Vec<Published>,
}

/// A row action on the Extensions page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Act {
    /// Switch `pack/id` to the carried state.
    Switch(String, bool),
    /// Open a pack file (a preview image or a recipe) in the file viewer.
    Open(String),
    /// Open the URL field.
    Install,
    /// Read the gallery.
    Browse,
    /// A gallery row: open the URL field holding its install command's URL.
    Pick(String),
    /// Show `pack/mod` in a pane on the right through `fno config plugins open`.
    Show(String),
}

/// The page state: the last reads and the open URL field.
#[derive(Debug, Default)]
pub(crate) struct Extensions {
    /// `(pack id, component)` pairs, or the failed read's notice.
    pub(crate) installed: Option<Result<Vec<(String, Component)>, String>>,
    pub(crate) gallery: Option<Result<Vec<Published>, String>>,
    pub(crate) url: Option<InputField>,
}

fn fno_json(args: &[&str]) -> Result<Vec<u8>, String> {
    let shown = format!("fno {}", args.join(" "));
    let out = std::process::Command::new(crate::server::fno_bin())
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("`{shown}` did not start: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`{shown}` exited {}",
            out.status.code().unwrap_or(-1)
        ));
    }
    Ok(out.stdout)
}

/// Parse `ls --json` into page rows, mods before layouts per pack.
pub(crate) fn parse_installed(
    read: Result<Vec<u8>, String>,
) -> Result<Vec<(String, Component)>, String> {
    let bytes = read?;
    let listing: Listing = serde_json::from_slice(&bytes)
        .map_err(|e| format!("`fno {}` printed no listing: {e}", LS.join(" ")))?;
    Ok(listing
        .packs
        .into_iter()
        .flat_map(|p| {
            let id = p.pack_id;
            p.mods
                .into_iter()
                .chain(p.layouts)
                .map(move |c| (id.clone(), c))
        })
        .collect())
}

/// Read the installed rows now.
pub(super) fn refresh(view: &mut View) {
    view.extensions.installed = Some(parse_installed(fno_json(&LS)));
}

fn entry(glyph: &str, label: String, enabled: bool) -> PopupRow {
    PopupRow::Entry {
        glyph: glyph.into(),
        label,
        hint: String::new(),
        enabled,
    }
}

/// The page rows and their actions. Only enabled entries take an action:
/// the popup selects among targets, so inert rows carry none.
pub(super) fn rows(view: &View) -> (Vec<PopupRow>, Vec<AuxAction>) {
    let page = &view.extensions;
    let mut rows = Vec::new();
    let mut actions = Vec::new();
    let inert = |label: String| entry(" ", label, false);
    if let Some(field) = &page.url {
        rows.push(back_row());
        actions.push(AuxAction::SettingsBack);
        rows.push(field.row("url"));
        rows.push(inert("a git URL; enter opens the install in a pane".into()));
        rows.push(inert(
            "the pane shows what the pack runs before it asks".into(),
        ));
        return (rows, actions);
    }
    rows.push(PopupRow::Header("installed".into()));
    match &page.installed {
        None => rows.push(inert("reading installed packs".into())),
        Some(Err(notice)) => rows.push(inert(notice.clone())),
        Some(Ok(list)) if list.is_empty() => {
            rows.push(inert("no mods or layouts installed".into()))
        }
        Some(Ok(list)) => {
            for (pack, c) in list {
                let target = format!("{pack}/{}", c.id);
                if c.kind == "native" {
                    rows.push(inert(format!("■ {} · {target} · built in", c.title)));
                } else {
                    let code = if c.runs_code { " · runs code" } else { "" };
                    rows.push(entry(
                        if c.enabled { "☑" } else { "☐" },
                        format!("{} · {target}{code}", c.title),
                        true,
                    ));
                    actions.push(AuxAction::Extension(Act::Switch(
                        target.clone(),
                        !c.enabled,
                    )));
                }
                if c.enabled && (c.kind == "feed" || c.kind == "run") {
                    rows.push(entry(" ", "  open in a pane".into(), true));
                    actions.push(AuxAction::Extension(Act::Show(target)));
                }
                for (word, path) in [("preview", &c.preview), ("recipe", &c.recipe)] {
                    if !path.is_empty() {
                        rows.push(entry(" ", format!("  {word}"), true));
                        actions.push(AuxAction::Extension(Act::Open(path.clone())));
                    }
                }
            }
        }
    }
    rows.push(entry("+", "install from URL".into(), true));
    actions.push(AuxAction::Extension(Act::Install));
    rows.push(PopupRow::Header("gallery · not reviewed by fno".into()));
    match &page.gallery {
        None => {
            rows.push(entry("›", "browse published packs".into(), true));
            actions.push(AuxAction::Extension(Act::Browse));
        }
        Some(Err(notice)) => rows.push(inert(notice.clone())),
        Some(Ok(list)) if list.is_empty() => rows.push(inert("no published packs yet".into())),
        Some(Ok(list)) => {
            for p in list {
                rows.push(entry(
                    "·",
                    format!("{} · {} stars · {}", p.name, p.stars, p.description),
                    true,
                ));
                actions.push(AuxAction::Extension(Act::Pick(p.install.clone())));
            }
        }
    }
    (rows, actions)
}

/// Close the URL field. True when one was open.
pub(super) fn cancel_url(view: &mut View) -> bool {
    view.extensions.url.take().is_some()
}

/// Submit the URL field: run the install in its own pane, where the consent
/// card prints and the prompt reads a real terminal.
pub(super) fn submit(view: &mut View, url: String) -> bool {
    let url = url.trim().to_string();
    if url.is_empty() || url.starts_with('-') {
        view.set_notice("type a git URL".into());
        return false;
    }
    view.extensions.url = None;
    let notice = in_pane(&["install", url.as_str()], &format!("install {url}"));
    view.set_notice(notice);
    view.extensions.installed = None;
    true
}

/// Run `fno config plugins <args>` in a new pane on the right, so its output
/// and any prompt reach a real terminal. Returns the notice to show.
fn in_pane(args: &[&str], what: &str) -> String {
    let fno = crate::server::fno_bin();
    let fno_text = fno.to_string_lossy().into_owned();
    let spawned = std::process::Command::new(&fno)
        .args(["mux", "pane", "run", "--split", "right", "--"])
        .args([fno_text.as_str(), "config", "plugins"])
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match spawned {
        Ok(_) => format!("{what}: opened in a pane on the right"),
        Err(e) => format!("{what}: cannot open a pane: {e}"),
    }
}

pub(super) async fn act(
    view: &mut View,
    act: Act,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    match act {
        Act::Switch(target, on) => {
            let verb = if on { "enable" } else { "disable" };
            let notice = match fno_json(&["config", "plugins", verb, &target]) {
                Ok(_) => format!("{target} {}", if on { "on" } else { "off" }),
                Err(e) => e,
            };
            view.set_notice(notice);
            refresh(view);
        }
        Act::Pick(install) => {
            let url = install.rsplit(' ').next().unwrap_or_default();
            view.extensions.url = Some(InputField::new(PLACEHOLDER, MAX_URL).with_text(url));
        }
        Act::Open(path) => {
            let loaded = file_viewer::load(PathBuf::from(path), None, Default::default());
            view.aux = None;
            return file_viewer::open(view, loaded, sock_w).await;
        }
        Act::Show(target) => {
            let notice = in_pane(&["open", target.as_str()], &target);
            view.set_notice(notice);
        }
        Act::Install => {
            if view.extensions.url.is_none() {
                view.extensions.url = Some(InputField::new(PLACEHOLDER, MAX_URL));
            }
        }
        Act::Browse => {
            view.extensions.gallery = Some(fno_json(&GALLERY).and_then(|bytes| {
                serde_json::from_slice::<Gallery>(&bytes)
                    .map(|g| g.repos)
                    .map_err(|e| format!("`fno {}` printed no gallery: {e}", GALLERY.join(" ")))
            }));
        }
    }
    view.reopen_settings_keeping_sel();
    Ok(())
}
