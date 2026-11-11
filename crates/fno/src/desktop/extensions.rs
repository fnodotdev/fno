//! Extensions mode (cmd-3): the mods and layouts `fno config plugins ls --json`
//! lists, each with its switch, preview image and rendered recipe, plus an
//! install row and the published gallery. The mux settings tab reads the
//! same feed through `plugins_feed`, so both hosts show one list.
//!
//! Reads, switches and pane launches run on one `fno-desktop-plugins` thread
//! and land through a reply channel, like the board poll. Install and open
//! run in a mux pane split beside the focused pane of the attached session,
//! so the consent card and its prompt stay the CLI's: the app never installs
//! on its own.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};

use gpui::{
    div, img, prelude::*, px, rgb, Context, Div, ElementId, FontWeight, ObjectFit, Stateful,
};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use super::keys::Key;
use super::window::{MainView, Root};
use crate::client::node_detail::backlog_md::md_lines;
use crate::plugins_feed::{self as feed, Component, Published, ReadFailed};
use crate::text_style::TextStyle;

const MAX_URL: usize = 512;
const URL_HINT: &str = "https://github.com/owner/repo";
/// The widest the preview draws, in logical pixels.
const PREVIEW_MAX: f32 = 480.0;

type Rows = Vec<(String, Component)>;

/// Work for the plugins thread.
pub(super) enum Ask {
    Installed,
    Gallery,
    Recipe(String),
    Switch(String, bool),
    /// Run `fno config plugins <args>` in a pane split beside `anchor` on
    /// `server`, and wait for the launcher to answer.
    Pane {
        args: Vec<String>,
        what: String,
        anchor: u64,
        server: String,
    },
}

/// What the plugins thread read or did.
pub(super) enum Reply {
    Installed(Result<Rows, ReadFailed>),
    Gallery(Result<Vec<Published>, ReadFailed>),
    Recipe(String, Result<String, ReadFailed>),
    Switched(String, bool, Result<(), ReadFailed>),
    Paned(String, Result<(), ReadFailed>),
}

/// What a reply asks of the window: a notice, and whether to show the
/// terminal where a new pane opened.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Landed {
    pub(super) notice: Option<String>,
    pub(super) terminal: bool,
}

/// Start the plugins thread. It ends when the page drops its sender.
pub(super) fn start() -> std::io::Result<(Sender<Ask>, UnboundedReceiver<Reply>)> {
    let (asks, ask_rx) = channel::<Ask>();
    let (out, replies) = unbounded_channel();
    std::thread::Builder::new()
        .name("fno-desktop-plugins".into())
        .spawn(move || {
            for ask in ask_rx {
                let reply = match ask {
                    Ask::Installed => {
                        Reply::Installed(feed::parse_installed(feed::fno_json(&feed::LS)))
                    }
                    Ask::Gallery => Reply::Gallery(feed::read_gallery()),
                    Ask::Recipe(path) => {
                        let text =
                            std::fs::read_to_string(&path).map_err(|e| ReadFailed(e.to_string()));
                        Reply::Recipe(path, text)
                    }
                    Ask::Switch(target, on) => {
                        let done = feed::switch(&target, on);
                        Reply::Switched(target, on, done)
                    }
                    Ask::Pane {
                        args,
                        what,
                        anchor,
                        server,
                    } => {
                        let words: Vec<&str> = args.iter().map(String::as_str).collect();
                        let done = feed::run_in_pane(&words, &what, anchor, Some(&server));
                        Reply::Paned(what, done)
                    }
                };
                if out.send(reply).is_err() {
                    return;
                }
            }
        })?;
    Ok((asks, replies))
}

#[derive(Debug, Default, PartialEq)]
enum Gallery {
    #[default]
    Idle,
    Reading,
    Done(Result<Vec<Published>, ReadFailed>),
}

/// What a typed key did to the URL field.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Edit {
    Typed,
    Close,
    Submit,
}

/// A click in the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Act {
    Select(String),
    Switch(String, bool),
    Confirm,
    /// Close the URL field; an armed confirm stays.
    CancelUrl,
    /// Drop the armed confirm; the URL field stays.
    CancelConfirm,
    /// Show `pack/mod` in a pane through `fno config plugins open`.
    Open(String),
    Install,
    Browse,
    /// A gallery row: open the URL field holding its URL.
    Pick(String),
    Submit,
}

/// The page state: the last good rows, the last failed read, the selection
/// and its recipe, the open URL field, an armed code-mod confirm, the
/// switches still out and the install pane being watched.
#[derive(Default)]
pub(super) struct Page {
    rows: Rows,
    error: Option<ReadFailed>,
    read: bool,
    reading: bool,
    gallery: Gallery,
    selected: Option<String>,
    recipe: Option<(String, Result<String, ReadFailed>)>,
    confirm: Option<String>,
    url: Option<String>,
    /// Targets whose switch the plugins thread has not answered yet.
    pending: HashSet<String>,
    watch: Watch,
    asks: Option<Sender<Ask>>,
}

/// The install pane, followed so the list re-reads when it closes.
#[derive(Debug, Default, PartialEq, Eq)]
enum Watch {
    #[default]
    Idle,
    /// An install was asked for; the panes the layout held before it.
    Awaiting(Vec<u64>),
    /// The install pane appeared; the list reads when it leaves the layout.
    Watching(u64),
}

fn target(pack: &str, c: &Component) -> String {
    format!("{pack}/{}", c.id)
}

fn native(c: &Component) -> bool {
    c.kind == "native"
}

/// An enabled feed or command mod has a pane to show.
fn can_open(c: &Component) -> bool {
    c.enabled && (c.kind == "feed" || c.kind == "run")
}

impl Page {
    pub(super) fn new(asks: Sender<Ask>) -> Page {
        Page {
            asks: Some(asks),
            ..Page::default()
        }
    }

    fn ask(&self, ask: Ask) -> bool {
        self.asks.as_ref().is_some_and(|a| a.send(ask).is_ok())
    }

    /// Read the installed rows, unless a read is already out.
    pub(super) fn read(&mut self) {
        if !self.reading && self.ask(Ask::Installed) {
            self.reading = true;
        }
    }

    /// Store what the plugins thread sent.
    pub(super) fn apply(&mut self, reply: Reply) -> Landed {
        let mut landed = Landed::default();
        match reply {
            Reply::Installed(read) => {
                self.reading = false;
                self.read = true;
                match read {
                    Ok(rows) => {
                        self.rows = rows;
                        self.error = None;
                    }
                    // The last good rows stay under the failed read's line.
                    Err(e) => self.error = Some(e),
                }
            }
            Reply::Gallery(read) => self.gallery = Gallery::Done(read),
            Reply::Recipe(path, text) => {
                // A reply for a row the user already left is dropped.
                if self.selected_component().is_some_and(|c| c.recipe == path) {
                    self.recipe = Some((path, text));
                }
            }
            Reply::Switched(target, on, done) => {
                self.pending.remove(&target);
                self.read();
                landed.notice = Some(match done {
                    Ok(()) => format!("{target} is {}", if on { "on" } else { "off" }),
                    Err(e) => e.0,
                });
            }
            Reply::Paned(what, Ok(())) => {
                landed.notice = Some(format!("{what} opened in a pane"));
                landed.terminal = true;
            }
            Reply::Paned(_, Err(e)) => {
                self.watch = Watch::Idle;
                landed.notice = Some(e.0);
            }
        }
        landed
    }

    /// The layout's panes changed: follow an install pane from its first
    /// frame to its exit, and read the list once when it leaves.
    pub(super) fn layout(&mut self, panes: &[u64]) {
        match &self.watch {
            Watch::Awaiting(before) => {
                if let Some(new) = panes.iter().find(|p| !before.contains(p)) {
                    self.watch = Watch::Watching(*new);
                }
            }
            Watch::Watching(id) if !panes.contains(id) => {
                self.watch = Watch::Idle;
                self.read();
            }
            _ => {}
        }
    }

    fn row_target(&self, i: usize) -> Option<String> {
        self.rows.get(i).map(|(p, c)| target(p, c))
    }

    /// The selected row's target and the state a Space press would set, for
    /// a row that has a switch.
    fn selected_switch(&self) -> Option<(String, bool)> {
        let c = self.selected_component().filter(|c| !native(c))?;
        Some((self.selected.clone()?, !c.enabled))
    }

    /// The selected row's target when it can show a pane.
    fn selected_open(&self) -> Option<String> {
        self.selected_component().filter(|c| can_open(c))?;
        self.selected.clone()
    }

    /// Move the selection one row, stopping at either end, and fetch its
    /// recipe.
    fn step(&mut self, down: bool) {
        let at = self
            .selected
            .as_deref()
            .and_then(|want| self.rows.iter().position(|(p, c)| target(p, c) == want));
        let to = match (at, down) {
            (None, _) => 0,
            (Some(i), true) => (i + 1).min(self.rows.len().saturating_sub(1)),
            (Some(i), false) => i.saturating_sub(1),
        };
        if let Some(want) = self.row_target(to) {
            if self.selected.as_deref() != Some(want.as_str()) {
                self.select(want);
            }
        }
    }

    fn selected_component(&self) -> Option<&Component> {
        let want = self.selected.as_deref()?;
        self.rows
            .iter()
            .find(|(p, c)| target(p, c) == want)
            .map(|(_, c)| c)
    }

    fn select(&mut self, want: String) {
        self.recipe = None;
        self.selected = Some(want);
        if let Some(path) = self.selected_component().map(|c| c.recipe.clone()) {
            if !path.is_empty() {
                self.ask(Ask::Recipe(path));
            }
        }
    }

    /// A switch press. A code mod's first switch-on arms a confirm and runs
    /// nothing. A press on a switch whose answer is still out does nothing.
    fn toggle(&mut self, want: String, on: bool) {
        if self.pending.contains(&want) {
            return;
        }
        let code = self
            .rows
            .iter()
            .any(|(p, c)| target(p, c) == want && c.runs_code);
        if on && code {
            self.confirm = Some(want);
        } else {
            self.switch(want, on);
        }
    }

    fn switch(&mut self, want: String, on: bool) {
        if self.ask(Ask::Switch(want.clone(), on)) {
            self.pending.insert(want);
        }
    }

    /// The lines the armed confirm shows: the argv and the short commit.
    fn confirm_lines(&self) -> Option<(String, Vec<String>)> {
        let want = self.confirm.as_deref()?;
        let (pack, c) = self.rows.iter().find(|(p, c)| target(p, c) == want)?;
        let argv = c.run.as_deref().map(feed::shell_words).unwrap_or_default();
        let mut lines = vec![format!("Runs code: {argv}")];
        if !c.commit.is_empty() {
            lines.push(format!(
                "From commit {}",
                c.commit.get(..12).unwrap_or(&c.commit)
            ));
        }
        Some((target(pack, c), lines))
    }

    /// The URL to install: none when the field is empty or starts with `-`.
    fn url_to_install(&self) -> Option<String> {
        let url = self.url.as_deref().unwrap_or_default().trim();
        (!url.is_empty() && !url.starts_with('-')).then(|| url.to_string())
    }

    /// A key while the URL field is open. Text keys never leave this page.
    pub(super) fn edit(&mut self, key: &str, text: Option<&str>) -> Edit {
        let Some(url) = self.url.as_mut() else {
            return Edit::Typed;
        };
        match key {
            "escape" => return Edit::Close,
            "enter" => return Edit::Submit,
            "backspace" => {
                url.pop();
            }
            _ => {
                for ch in text.unwrap_or_default().chars().filter(|c| !c.is_control()) {
                    if url.chars().count() < MAX_URL {
                        url.push(ch);
                    }
                }
            }
        }
        Edit::Typed
    }
}

/// A click: switch, select, ask for a read, or run an install or open in a
/// pane beside the focused one. The window shows that pane once the launcher
/// says it exists.
pub(super) fn act(root: &mut Root, act: Act, cx: &mut Context<Root>) {
    match act {
        Act::Select(t) => root.ext.select(t),
        Act::Switch(t, on) => root.ext.toggle(t, on),
        Act::Confirm => {
            if let Some(t) = root.ext.confirm.take() {
                root.ext.switch(t, true);
            }
        }
        Act::CancelUrl => root.ext.url = None,
        Act::CancelConfirm => root.ext.confirm = None,
        Act::Open(t) => pane(root, vec!["open".into(), t.clone()], t, false),
        Act::Install => root.ext.url.get_or_insert_with(String::new).clear(),
        Act::Browse => {
            if root.ext.ask(Ask::Gallery) {
                root.ext.gallery = Gallery::Reading;
            }
        }
        Act::Pick(install) => {
            let url = install.rsplit(' ').next().unwrap_or_default();
            root.ext.url = Some(url.to_string());
        }
        Act::Submit => match root.ext.url_to_install() {
            Some(url) => {
                root.ext.url = None;
                let what = format!("Install {url}");
                pane(root, vec!["install".into(), url], what, true);
            }
            None => return root.say("Type a git URL".into(), cx),
        },
    }
    cx.notify();
}

/// Ask the plugins thread for a pane beside the focused one. An install
/// also arms the watch that re-reads the list when its pane closes.
fn pane(root: &mut Root, args: Vec<String>, what: String, install: bool) {
    let before: Vec<u64> = root.layout.panes.iter().map(|(id, _)| *id).collect();
    let ask = Ask::Pane {
        args,
        what,
        anchor: root.layout.focus,
        server: root.session.clone(),
    };
    if root.ext.ask(ask) && install {
        root.ext.watch = Watch::Awaiting(before);
    }
}

/// A key in the view. True when the page took it.
pub(super) fn key(root: &mut Root, key: &Key, cx: &mut Context<Root>) -> bool {
    if root.ext.url.is_some() {
        let text = key.key_char.filter(|_| !key.ctrl && !key.cmd);
        match root.ext.edit(key.key, text) {
            Edit::Close => act(root, Act::CancelUrl, cx),
            Edit::Submit => act(root, Act::Submit, cx),
            Edit::Typed => cx.notify(),
        }
        return true;
    }
    if key.ctrl || key.cmd {
        return false;
    }
    let page = &mut root.ext;
    match key.key {
        "escape" if page.confirm.is_some() => act(root, Act::CancelConfirm, cx),
        "enter" if page.confirm.is_some() => act(root, Act::Confirm, cx),
        "up" | "down" => {
            page.step(key.key == "down");
            cx.notify();
        }
        "space" => match page.selected_switch() {
            Some((want, on)) => act(root, Act::Switch(want, on), cx),
            None => return false,
        },
        "o" => match page.selected_open() {
            Some(want) => act(root, Act::Open(want), cx),
            None => return false,
        },
        "i" => act(root, Act::Install, cx),
        _ => return false,
    }
    true
}

/// A paste while the view shows goes to the URL field, or nowhere.
pub(super) fn paste(root: &mut Root, text: &str, cx: &mut Context<Root>) {
    if root.ext.url.is_some() {
        root.ext.edit("", Some(text));
        cx.notify();
    }
}

fn button(
    id: ElementId,
    label: String,
    root: &Root,
    on: Act,
    cx: &mut Context<Root>,
) -> Stateful<Div> {
    let p = root.palette;
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(rgb(p.border))
        .text_color(rgb(p.fg))
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(p.row_fill)))
        .active(move |s| s.bg(rgb(p.pressed)))
        .on_click(cx.listener(move |root, _, _, cx| act(root, on.clone(), cx)))
        .child(label)
}

fn quiet(text: impl Into<String>, root: &Root) -> Div {
    div()
        .py_1()
        .text_color(rgb(root.palette.muted))
        .child(text.into())
}

fn section(title: &'static str, root: &Root) -> Div {
    div()
        .pt_4()
        .pb_2()
        .text_xs()
        .text_color(rgb(root.palette.muted))
        .child(title)
}

fn named(prefix: &'static str, i: usize) -> ElementId {
    ElementId::NamedInteger(prefix.into(), i as u64)
}

/// One mod or layout row: its switch (a built-in mod has none) and its name,
/// which selects it.
fn row(i: usize, pack: &str, c: &Component, root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let want = target(pack, c);
    let lead = if native(c) {
        div()
            .w(px(56.0))
            .flex_none()
            .px_2()
            .pt(px(3.0))
            .text_xs()
            .text_color(rgb(p.muted))
            .child("Built in")
    } else {
        let pending = root.ext.pending.contains(&want);
        let (label, color) = match (pending, c.enabled) {
            (true, _) => ("...", p.muted),
            (false, true) => ("On", p.ok),
            (false, false) => ("Off", p.muted),
        };
        let on = !c.enabled;
        let t = want.clone();
        div().w(px(56.0)).flex_none().child(
            div()
                .id(named("ext-switch", i))
                .px_2()
                .py(px(2.0))
                .rounded_md()
                .border_1()
                .border_color(rgb(p.border))
                .text_color(rgb(color))
                .when(!pending, |d| {
                    d.cursor_pointer()
                        .hover(move |s| s.bg(rgb(p.row_fill)))
                        .active(move |s| s.bg(rgb(p.pressed)))
                })
                .on_click(
                    cx.listener(move |root, _, _, cx| act(root, Act::Switch(t.clone(), on), cx)),
                )
                .child(label),
        )
    };
    let chosen = root.ext.selected.as_deref() == Some(want.as_str());
    let t = want.clone();
    let name = div()
        .id(named("ext-row", i))
        .flex_1()
        .min_w_0()
        .px_2()
        .py(px(3.0))
        .rounded_md()
        .when(chosen, |d| d.bg(rgb(p.row_fill)))
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(p.row_fill)))
        .active(move |s| s.bg(rgb(p.pressed)))
        .on_click(cx.listener(move |root, _, _, cx| act(root, Act::Select(t.clone()), cx)))
        .child(div().font_weight(FontWeight::BOLD).child(c.title.clone()))
        .child(div().text_xs().text_color(rgb(p.muted)).child(want))
        .when(c.runs_code, |d| {
            d.child(div().text_xs().text_color(rgb(p.warn)).child("Runs code"))
        })
        .when(!c.summary.is_empty(), |d| {
            d.child(div().text_color(rgb(p.muted)).child(c.summary.clone()))
        });
    div()
        .flex()
        .flex_row()
        .items_start()
        .gap_2()
        .py_1()
        .child(lead)
        .child(name)
}

fn list(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let page = &root.ext;
    let mut out = div().flex().flex_col().child(section("INSTALLED", root));
    if let Some(e) = &page.error {
        out = out.child(div().py_1().text_color(rgb(p.error)).child(e.0.clone()));
    }
    if let Some((want, lines)) = page.confirm_lines() {
        let mut ask = div()
            .flex()
            .flex_col()
            .gap_1()
            .my_1()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(rgb(p.warn))
            .child(
                div()
                    .text_color(rgb(p.warn))
                    .child(format!("Turn on {want}?")),
            );
        for line in lines {
            ask = ask.child(div().child(line));
        }
        out = out.child(
            ask.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .pt_1()
                    .child(button(
                        "ext-confirm".into(),
                        "Turn it on and let it run".into(),
                        root,
                        Act::Confirm,
                        cx,
                    ))
                    .child(button(
                        "ext-cancel".into(),
                        "Cancel".into(),
                        root,
                        Act::CancelConfirm,
                        cx,
                    )),
            ),
        );
    }
    if !page.read {
        out = out.child(quiet("Reading...", root));
    } else if page.rows.is_empty() && page.error.is_none() {
        out = out.child(quiet("No mods or layouts installed", root));
    }
    for (i, (pack, c)) in page.rows.iter().enumerate() {
        out = out.child(row(i, pack, c, root, cx));
    }
    out = out.child(section("INSTALL", root));
    out = match &page.url {
        None => out.child(button(
            "ext-install".into(),
            "Install from URL".into(),
            root,
            Act::Install,
            cx,
        )),
        Some(url) => out
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(p.focus))
                    .child(if url.is_empty() {
                        format!("\u{258f}{URL_HINT}")
                    } else {
                        format!("{url}\u{258f}")
                    })
                    .text_color(rgb(if url.is_empty() { p.muted } else { p.fg })),
            )
            .child(quiet(
                "Enter opens the install in a pane, where its consent prompt runs.",
                root,
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(button(
                        "ext-submit".into(),
                        "Install".into(),
                        root,
                        Act::Submit,
                        cx,
                    ))
                    .child(button(
                        "ext-cancel-url".into(),
                        "Cancel".into(),
                        root,
                        Act::CancelUrl,
                        cx,
                    )),
            ),
    };
    out = out.child(section("GALLERY", root));
    match &page.gallery {
        Gallery::Idle => out.child(button(
            "ext-browse".into(),
            "Browse published packs".into(),
            root,
            Act::Browse,
            cx,
        )),
        Gallery::Reading => out.child(quiet("Reading the gallery...", root)),
        Gallery::Done(Err(e)) => {
            out.child(div().py_1().text_color(rgb(p.error)).child(e.0.clone()))
        }
        Gallery::Done(Ok(repos)) if repos.is_empty() => {
            out.child(quiet("No published packs yet", root))
        }
        Gallery::Done(Ok(repos)) => {
            for (i, g) in repos.iter().enumerate() {
                let install = g.install.clone();
                let reviewed = g.reviewed;
                out = out.child(
                    div()
                        .id(named("ext-pick", i))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(move |s| s.bg(rgb(p.row_fill)))
                        .active(move |s| s.bg(rgb(p.pressed)))
                        .on_click(cx.listener(move |root, _, _, cx| {
                            act(root, Act::Pick(install.clone()), cx)
                        }))
                        .child(format!("{} · {} stars", g.name, g.stars))
                        .when(!reviewed, |d| {
                            d.child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(p.warn))
                                    .child("Not reviewed by fno"),
                            )
                        })
                        .child(div().text_color(rgb(p.muted)).child(g.description.clone())),
                );
            }
            out
        }
    }
}

/// A recipe split at its fence lines: prose the view wraps and code it draws
/// in the terminal font.
#[derive(Debug, PartialEq, Eq)]
enum Block {
    Prose(String),
    Code(Vec<String>),
}

fn recipe_blocks(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut prose: Vec<&str> = Vec::new();
    let mut code: Option<Vec<String>> = None;
    for line in text.lines() {
        let fence = line.trim_start().starts_with("```");
        match (&mut code, fence) {
            (Some(_), true) => blocks.extend(code.take().map(Block::Code)),
            (Some(lines), false) => lines.push(line.to_string()),
            (None, true) => {
                if !prose.is_empty() {
                    blocks.push(Block::Prose(prose.join("\n")));
                    prose.clear();
                }
                code = Some(Vec::new());
            }
            (None, false) => prose.push(line),
        }
    }
    blocks.extend(code.map(Block::Code));
    if !prose.is_empty() {
        blocks.push(Block::Prose(prose.join("\n")));
    }
    blocks
}

/// The selected row: its preview image, its summary and its recipe.
fn detail(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let page = &root.ext;
    let Some(want) = page.selected.as_deref() else {
        return div().pt_4().child(quiet(
            "Select a mod or layout to see its preview and recipe",
            root,
        ));
    };
    let Some(c) = page.selected_component() else {
        return div()
            .pt_4()
            .child(quiet(format!("{want} is no longer installed"), root));
    };
    let mut out = div()
        .flex()
        .flex_col()
        .gap_2()
        .pt_4()
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::BOLD)
                .child(c.title.clone()),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(p.muted))
                .child(want.to_string()),
        );
    if !c.summary.is_empty() {
        out = out.child(div().child(c.summary.clone()));
    }
    if can_open(c) {
        out = out.child(div().flex().child(button(
            "ext-open".into(),
            "Open in a pane".into(),
            root,
            Act::Open(want.to_string()),
            cx,
        )));
    }
    if !c.preview.is_empty() {
        let (path, error, muted) = (c.preview.clone(), p.error, p.muted);
        out = out.child(
            img(PathBuf::from(&c.preview))
                .w_full()
                .max_w(px(PREVIEW_MAX))
                .object_fit(ObjectFit::Contain)
                .with_loading(move || {
                    div()
                        .text_color(rgb(muted))
                        .child("Loading preview...")
                        .into_any_element()
                })
                .with_fallback(move || {
                    div()
                        .text_color(rgb(error))
                        .child(format!("Cannot draw {path}"))
                        .into_any_element()
                }),
        );
    }
    if !c.recipe.is_empty() {
        out = match &page.recipe {
            Some((_, Ok(text))) => {
                let mut body = div().flex().flex_col().pt_2();
                for block in recipe_blocks(text) {
                    match block {
                        Block::Prose(segment) => {
                            // The column wraps the text; no pre-wrap column.
                            for line in md_lines(&segment, usize::MAX) {
                                let (color, bold) = match line.default_role {
                                    TextStyle::Title => (p.fg, true),
                                    TextStyle::Meta | TextStyle::Hint => (p.muted, false),
                                    _ => (p.fg, false),
                                };
                                body = body.child(
                                    div()
                                        .min_h(px(8.0))
                                        .text_color(rgb(color))
                                        .when(bold, |d| d.font_weight(FontWeight::BOLD))
                                        .child(line.text.clone()),
                                );
                            }
                        }
                        Block::Code(lines) => {
                            for line in lines {
                                body = body.child(
                                    div()
                                        .min_h(px(8.0))
                                        .font_family(root.metrics.font.family.clone())
                                        .text_color(rgb(p.fg))
                                        .child(line),
                                );
                            }
                        }
                    }
                }
                out.child(body)
            }
            Some((path, Err(e))) => out.child(
                div()
                    .text_color(rgb(p.error))
                    .child(format!("Cannot read {path}: {e}")),
            ),
            None => out.child(quiet("Reading the recipe...", root)),
        };
    }
    out
}

/// The list and the selected row's detail, side by side, each scrolling on
/// its own. The bottom pad keeps the last row whole above the window edge.
pub(super) fn render(root: &Root, cx: &mut Context<Root>) -> Div {
    let column = |id: &'static str| -> Stateful<Div> {
        div()
            .id(id)
            .h_full()
            .overflow_x_hidden()
            .overflow_y_scroll()
            .px_4()
            .pb_8()
    };
    div()
        .flex()
        .flex_row()
        .flex_1()
        .min_h_0()
        .w_full()
        .child(
            column("ext-list")
                .w(px(520.0))
                .flex_none()
                .child(list(root, cx)),
        )
        .child(
            column("ext-detail")
                .flex_1()
                .min_w_0()
                .border_l_1()
                .border_color(rgb(root.palette.border))
                .child(detail(root, cx)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fno-panels (two built-in mods and a feed mod) plus a URL pack with a
    /// code mod, a feed mod and a layout.
    const LISTING: &[u8] = br#"{"packs": [
        {"pack_id": "fno-panels", "mods": [
            {"id": "backlog-board", "title": "Backlog board", "kind": "native", "enabled": true},
            {"id": "review-ledger", "title": "Review ledger", "kind": "feed", "enabled": true,
             "recipe": "/demo/RECIPE.md"},
            {"id": "decisions", "title": "Decisions", "kind": "native", "enabled": true}]},
        {"pack_id": "demo-clock", "source": {"commit": "4f2a9c1e7b3d5a6f8e0c"}, "mods": [
            {"id": "clock", "title": "Clock", "kind": "run", "enabled": false, "runs_code": true,
             "run": ["sh", "-c", "while :; do date; done"]},
            {"id": "ci-feed", "title": "CI feed", "kind": "feed", "enabled": true}],
         "layouts": [{"id": "focus", "title": "Focus", "enabled": false}]}]}"#;

    fn page() -> (Page, std::sync::mpsc::Receiver<Ask>) {
        let (tx, rx) = channel();
        let mut page = Page::new(tx);
        page.apply(Reply::Installed(feed::parse_installed(
            Ok(LISTING.to_vec()),
        )));
        (page, rx)
    }

    #[test]
    fn a_code_mod_arms_a_confirm_naming_its_argv_before_any_enable() {
        let (mut page, rx) = page();
        let shown: Vec<_> = page
            .rows
            .iter()
            .map(|(p, c)| (target(p, c), native(c)))
            .collect();
        assert_eq!(shown.len(), 6);
        assert_eq!(shown[0], ("fno-panels/backlog-board".to_string(), true));
        assert_eq!(shown[3].0, "demo-clock/clock");
        page.toggle("demo-clock/clock".into(), true);
        assert!(rx.try_recv().is_err(), "enable ran before the confirm");
        let (want, lines) = page.confirm_lines().unwrap();
        assert_eq!(want, "demo-clock/clock");
        assert_eq!(
            lines,
            [
                "Runs code: sh -c 'while :; do date; done'",
                "From commit 4f2a9c1e7b3d"
            ]
        );
        page.toggle("demo-clock/ci-feed".into(), false);
        assert!(matches!(rx.try_recv(), Ok(Ask::Switch(t, false)) if t == "demo-clock/ci-feed"));
    }

    #[test]
    fn a_pending_switch_takes_no_second_press_and_the_keys_stop_at_the_ends() {
        let (mut page, rx) = page();
        page.toggle("demo-clock/ci-feed".into(), false);
        page.toggle("demo-clock/ci-feed".into(), false);
        assert!(matches!(rx.try_recv(), Ok(Ask::Switch(..))));
        assert!(
            rx.try_recv().is_err(),
            "a second press sent a second switch"
        );
        page.apply(Reply::Switched("demo-clock/ci-feed".into(), false, Ok(())));
        assert!(page.pending.is_empty());

        for _ in 0..9 {
            page.step(true);
        }
        assert_eq!(page.selected.as_deref(), Some("demo-clock/focus"));
        for _ in 0..9 {
            page.step(false);
        }
        assert_eq!(page.selected.as_deref(), Some("fno-panels/backlog-board"));
        assert_eq!(page.selected_switch(), None, "a built-in mod has no switch");
        page.select("demo-clock/clock".into());
        assert_eq!(
            page.selected_switch(),
            Some(("demo-clock/clock".into(), true))
        );
        assert_eq!(page.selected_open(), None, "a disabled mod has no pane");
        page.select("demo-clock/ci-feed".into());
        assert_eq!(page.selected_open().as_deref(), Some("demo-clock/ci-feed"));
    }

    #[test]
    fn an_install_pane_reads_the_list_once_when_it_closes() {
        let (mut page, rx) = page();
        page.layout(&[1, 2]);
        assert!(rx.try_recv().is_err(), "an idle view asked for a read");
        page.watch = Watch::Awaiting(vec![1, 2]);
        page.layout(&[1, 2]);
        page.layout(&[1, 2, 9]);
        assert_eq!(page.watch, Watch::Watching(9));
        assert!(rx.try_recv().is_err());
        page.layout(&[1, 2]);
        assert!(matches!(rx.try_recv(), Ok(Ask::Installed)));
        page.layout(&[1]);
        assert!(rx.try_recv().is_err(), "the read ran twice");

        let landed = page.apply(Reply::Paned(
            "Install x".into(),
            Err(ReadFailed(
                "Install x did not open a pane: exit 2: no anchor".into(),
            )),
        ));
        assert!(!landed.terminal);
        assert!(landed.notice.is_some_and(|n| n.contains("exit 2")));
    }

    #[test]
    fn a_recipe_splits_at_its_fences_and_prose_stays_on_one_line() {
        let text = "Intro one. Intro two.\n```sh\nfno mux layout graft  a  b\n```\nAfter.";
        assert_eq!(
            recipe_blocks(text),
            [
                Block::Prose("Intro one. Intro two.".into()),
                Block::Code(vec!["fno mux layout graft  a  b".into()]),
                Block::Prose("After.".into()),
            ]
        );
        let lines = md_lines("Intro one. Intro two.", usize::MAX);
        assert_eq!(lines.len(), 1, "a paragraph wrapped with no width cap");
    }

    #[test]
    fn a_failed_read_keeps_the_last_good_rows() {
        let (mut page, _rx) = page();
        page.apply(Reply::Installed(Err(ReadFailed(
            "`fno config plugins ls --json` exited 1: boom".into(),
        ))));
        assert_eq!(page.rows.len(), 6);
        assert!(page
            .error
            .as_ref()
            .is_some_and(|e| e.0.contains("exited 1")));
        page.apply(Reply::Installed(feed::parse_installed(
            Ok(LISTING.to_vec()),
        )));
        assert!(page.error.is_none());
    }

    #[test]
    fn a_recipe_reply_for_a_row_the_user_left_is_dropped() {
        let (mut page, rx) = page();
        page.select("fno-panels/review-ledger".into());
        assert!(matches!(rx.try_recv(), Ok(Ask::Recipe(p)) if p == "/demo/RECIPE.md"));
        page.select("demo-clock/clock".into());
        page.apply(Reply::Recipe("/demo/RECIPE.md".into(), Ok("# hi".into())));
        assert!(page.recipe.is_none());
    }

    #[test]
    fn the_url_field_types_edits_and_refuses_an_empty_or_dashed_value() {
        let (mut page, _rx) = page();
        assert!(page.url_to_install().is_none());
        page.url = Some(String::new());
        assert_eq!(page.edit("a", Some("--x")), Edit::Typed);
        assert!(
            page.url_to_install().is_none(),
            "a value starting with - installs nothing"
        );
        page.edit("backspace", None);
        page.edit("backspace", None);
        page.edit("backspace", None);
        page.edit("h", Some("https://github.com/o/r"));
        assert_eq!(
            page.url_to_install().as_deref(),
            Some("https://github.com/o/r")
        );
        assert_eq!(page.edit("enter", None), Edit::Submit);
        assert_eq!(page.edit("escape", None), Edit::Close);
    }
}
