//! Mods mode (cmd-3): the mods and layouts `fno config plugins ls --json`
//! lists, split into what you have (Installed) and what you could add
//! (Available), each with its preview image and rendered recipe, plus an
//! install row and the published gallery. A panel shows state, never a
//! toggle: an installed row carries a check, and the detail pane holds the
//! one Install or Uninstall. The mux settings tab reads the same feed through
//! `plugins_feed`, so both hosts show one list.
//!
//! Reads, switches and pane launches run on one `fno-app-plugins` thread
//! and land through a reply channel, like the board poll. Install and open
//! run in a mux pane split beside the focused pane of the attached session,
//! so the consent card and its prompt stay the CLI's: the app never installs
//! on its own.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};

use gpui::{div, img, prelude::*, px, rgb, Context, Div, ElementId, ObjectFit, Stateful};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use super::keys::Key;
use super::ui::{self, Button, Role};
use super::window::{Action, MainView, Root};
use fno::app_api::client::node_detail::backlog_md::md_lines;
use fno::app_api::plugins_feed::{self as feed, Component, Published, ReadFailed};

const MAX_URL: usize = 512;
const URL_HINT: &str = "https://github.com/owner/repo";
/// The tallest a preview draws, and the height held while it decodes: 16:9 at the widest.
const PREVIEW_RESERVE: f32 = 270.0;
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
    /// The notice reports a failure: it paints in the error color.
    pub(super) failed: bool,
    pub(super) terminal: bool,
}

/// Start the plugins thread. It ends when the page drops its sender.
pub(super) fn start() -> std::io::Result<(Sender<Ask>, UnboundedReceiver<Reply>)> {
    let (asks, ask_rx) = channel::<Ask>();
    let (out, replies) = unbounded_channel();
    std::thread::Builder::new()
        .name("fno-app-plugins".into())
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
    /// Add (`true`) or Remove (`false`) `pack/mod`.
    Set(String, bool),
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

/// The stable control id a click's [`Act`] ships to the usage events arm
/// (`fno::ui_usage`). One constant per control, never a label.
fn click_id(act: &Act) -> &'static str {
    match act {
        Act::Select(_) => "mod_select",
        Act::Set(_, true) => "mod_add",
        Act::Set(_, false) => "mod_remove",
        Act::Confirm => "mod_confirm",
        Act::CancelUrl => "mod_cancel_url",
        Act::CancelConfirm => "mod_cancel_confirm",
        Act::Open(_) => "mod_open",
        Act::Install => "mod_install",
        Act::Browse => "mod_browse",
        Act::Pick(_) => "mod_pick",
        Act::Submit => "mod_submit",
    }
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

/// What you have: on, or built in. Everything else is Available.
fn installed(c: &Component) -> bool {
    native(c) || c.enabled
}

/// The row's kind tag in plain words. "Runs code" is the one that changes
/// what adding the mod does to your machine.
fn kind_words(c: &Component) -> &'static str {
    match c.kind.as_str() {
        "native" => "Built in",
        _ if c.runs_code => "Runs code",
        "feed" => "Table",
        "run" => "Command",
        _ => "Layout",
    }
}

/// What the Open button does for a mod that has a view to show. A native mod
/// opens in the app where the app has the view, and says where else.
fn native_open(target: &str) -> Action {
    match target.rsplit('/').next() {
        Some("backlog-board") => Action::Show(MainView::Orchestration),
        _ => Action::Notice("Open this view from the mux menu".into()),
    }
}

/// An enabled feed or command mod has a pane to show.
fn can_open(c: &Component) -> bool {
    native(c) || c.enabled && (c.kind == "feed" || c.kind == "run")
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
                landed.failed = done.is_err();
                landed.notice = Some(match done {
                    Ok(()) => format!("{target} {}", if on { "added" } else { "removed" }),
                    Err(e) => e.0,
                });
            }
            Reply::Paned(what, Ok(())) => {
                landed.notice = Some(format!("{what} opened in a pane"));
                landed.terminal = true;
            }
            Reply::Paned(_, Err(e)) => {
                self.watch = Watch::Idle;
                landed.failed = true;
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

    /// The rows in the order the list shows them: Installed, then Available.
    fn order(&self) -> Vec<usize> {
        let side = |yes: bool| {
            self.rows
                .iter()
                .enumerate()
                .filter(move |(_, (_, c))| installed(c) == yes)
                .map(|(i, _)| i)
        };
        side(true).chain(side(false)).collect()
    }

    /// The selected row's target and what a Space press does: Add (`true`)
    /// or Remove (`false`). A built-in mod has neither.
    fn selected_action(&self) -> Option<(String, bool)> {
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
        let order = self.order();
        let at = self.selected.as_deref().and_then(|want| {
            order
                .iter()
                .position(|&i| self.rows.get(i).is_some_and(|(p, c)| target(p, c) == want))
        });
        let to = match (at, down) {
            (None, _) => 0,
            (Some(i), true) => (i + 1).min(order.len().saturating_sub(1)),
            (Some(i), false) => i.saturating_sub(1),
        };
        let row = order.get(to).and_then(|&i| self.rows.get(i));
        if let Some(want) = row.map(|(p, c)| target(p, c)) {
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

    /// An Install or Uninstall press. Installing a code mod arms a confirm and runs
    /// nothing. A press on a mod whose answer is still out does nothing.
    fn press(&mut self, want: String, on: bool) {
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
            self.set(want, on);
        }
    }

    fn set(&mut self, want: String, on: bool) {
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
    fno::ui_usage::click("app", click_id(&act));
    match act {
        Act::Select(t) => root.ext.select(t),
        Act::Set(t, on) => root.ext.press(t, on),
        Act::Confirm => {
            if let Some(t) = root.ext.confirm.take() {
                root.ext.set(t, true);
            }
        }
        Act::CancelUrl => root.ext.url = None,
        Act::CancelConfirm => root.ext.confirm = None,
        Act::Open(t)
            if root
                .ext
                .rows
                .iter()
                .any(|(p, c)| target(p, c) == t && native(c)) =>
        {
            root.act(native_open(&t), cx)
        }
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
        "space" => match page.selected_action() {
            Some((want, on)) => act(root, Act::Set(want, on), cx),
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

fn named(prefix: &'static str, i: usize) -> ElementId {
    ElementId::NamedInteger(prefix.into(), i as u64)
}

/// A click handler that runs `on` through [`act`].
fn on(
    on: Act,
    cx: &mut Context<Root>,
) -> Box<dyn Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static> {
    Box::new(cx.listener(move |root, _, _, cx| act(root, on.clone(), cx)))
}

/// The `pack/id` column's width: the longest id shown, held to a bound.
fn id_chars(rows: &Rows) -> usize {
    rows.iter()
        .map(|(p, c)| target(p, c).chars().count())
        .max()
        .unwrap_or(0)
        .min(30)
}

/// One mod or layout on one line: a check when you have it, its title, its
/// `pack/id` and its kind. The whole row selects it, and the band covers
/// the whole row.
fn row(i: usize, pack: &str, c: &Component, root: &Root, cx: &mut Context<Root>) -> Stateful<Div> {
    let p = root.palette;
    let want = target(pack, c);
    let chosen = root.ext.selected.as_deref() == Some(want.as_str());
    let pending = root.ext.pending.contains(&want);
    let kind = kind_words(c);
    ui::clickable(ui::row(named("ext-row", i), chosen, &p), &p)
        .on_click(on(Act::Select(want.clone()), cx))
        .when(pending, |d| d.opacity(0.5))
        .child(
            div()
                .w(root.adv * 2.0)
                .flex_none()
                .text_color(rgb(p.accent))
                .child(if installed(c) { "✓" } else { "" }),
        )
        .child(ui::clamp(c.title.clone(), Role::Name, &p))
        .child(ui::cell(
            want,
            id_chars(&root.ext.rows),
            false,
            Role::Meta,
            &p,
            root.adv,
        ))
        .child(
            div()
                .w(root.adv * 10.0)
                .flex_none()
                .flex()
                .flex_row()
                .justify_end()
                .child(ui::tag(
                    kind,
                    if c.runs_code { p.warn } else { p.muted },
                    &p,
                )),
        )
}

/// The armed code-mod confirm: the one box the view keeps, in the needs-you
/// color, because what it guards runs code on this machine.
fn confirm(root: &Root, cx: &mut Context<Root>) -> Option<Div> {
    let p = root.palette;
    let (want, lines) = root.ext.confirm_lines()?;
    let mut ask = div()
        .flex()
        .flex_col()
        .gap_1()
        .mx_3()
        .my_2()
        .p_3()
        .rounded(px(4.0))
        .border_1()
        .border_color(rgb(p.warn))
        .child(ui::text(format!("Install {want}?"), Role::Heading, &p).text_color(rgb(p.warn)));
    for line in lines {
        ask = ask.child(ui::text(line, Role::Body, &p));
    }
    Some(
        ask.child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .pt_1()
                .child(
                    ui::button("ext-confirm", "Install and let it run", Button::Primary, &p)
                        .on_click(on(Act::Confirm, cx)),
                )
                .child(
                    ui::button("ext-cancel", "Cancel", Button::Quiet, &p)
                        .on_click(on(Act::CancelConfirm, cx)),
                )
                .child(ui::kbd("⏎ esc", &p)),
        ),
    )
}

/// The install field, or the two ways to find more.
fn more(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let page = &root.ext;
    let out = div().flex().flex_col().gap_2().px_3().pt_3();
    match &page.url {
        None => out.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    ui::button("ext-install", "Install from URL", Button::Quiet, &p)
                        .on_click(on(Act::Install, cx)),
                )
                .child(ui::kbd("I", &p))
                .when(page.gallery == Gallery::Idle, |d| {
                    d.child(
                        ui::button("ext-browse", "Browse published packs", Button::Quiet, &p)
                            .on_click(on(Act::Browse, cx)),
                    )
                }),
        ),
        Some(url) => out
            .child(
                div()
                    .h(px(26.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(px(4.0))
                    .bg(rgb(p.raised))
                    .text_color(rgb(if url.is_empty() { p.faint } else { p.fg }))
                    .child(if url.is_empty() {
                        format!("\u{258f}{URL_HINT}")
                    } else {
                        format!("{url}\u{258f}")
                    }),
            )
            .child(ui::text(
                "The install opens in a pane, where its consent prompt runs.",
                Role::Meta,
                &p,
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        ui::button("ext-submit", "Install", Button::Primary, &p)
                            .on_click(on(Act::Submit, cx)),
                    )
                    .child(
                        ui::button("ext-cancel-url", "Cancel", Button::Quiet, &p)
                            .on_click(on(Act::CancelUrl, cx)),
                    ),
            ),
    }
}

/// The published packs, one line each, under Available.
fn gallery(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let out = div().flex().flex_col();
    match &root.ext.gallery {
        Gallery::Idle => out,
        Gallery::Reading => out.child(ui::quiet("Reading the gallery...", &p).px_3()),
        Gallery::Done(Err(e)) => out.child(
            ui::text(e.0.clone(), Role::Meta, &p)
                .text_color(rgb(p.error))
                .px_3(),
        ),
        Gallery::Done(Ok(repos)) if repos.is_empty() => {
            out.child(ui::quiet("No published packs yet", &p).px_3())
        }
        Gallery::Done(Ok(repos)) => out.children(repos.iter().enumerate().map(|(i, g)| {
            let install = g.install.clone();
            ui::clickable(ui::row(named("ext-pick", i), false, &p), &p)
                .on_click(on(Act::Pick(install), cx))
                .child(div().w(root.adv * 2.0).flex_none())
                .child(ui::clamp(g.name.clone(), Role::Name, &p))
                .child(ui::clamp(g.description.clone(), Role::Meta, &p))
                .when(!g.reviewed, |d| {
                    d.child(ui::tag("Not reviewed", p.warn, &p))
                })
                .child(ui::cell(
                    format!("{} stars", g.stars),
                    9,
                    true,
                    Role::Fact,
                    &p,
                    root.adv,
                ))
        })),
    }
}

fn list(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let page = &root.ext;
    let order = page.order();
    let have = page.rows.iter().filter(|(_, c)| installed(c)).count();
    let count = |n: usize| ui::role(div(), Role::Hint, &p).child(n.to_string());
    let mut out = div()
        .flex()
        .flex_col()
        .child(ui::section("Installed", &p).px_3().child(count(have)));
    // A failed read names itself under the header; the last good rows stay.
    if let Some(e) = &page.error {
        out = out.child(
            ui::text(e.0.clone(), Role::Meta, &p)
                .text_color(rgb(p.error))
                .px_3()
                .py_1(),
        );
    }
    out = out.children(confirm(root, cx));
    if !page.read {
        out = out.child(ui::quiet("Reading...", &p).px_3());
    } else if page.rows.is_empty() && page.error.is_none() {
        out = out.child(ui::quiet("No mods or layouts installed", &p).px_3());
    }
    for (n, &i) in order.iter().enumerate() {
        if n == have {
            out = out.child(
                ui::section("Available", &p)
                    .px_3()
                    .child(count(order.len() - have)),
            );
        }
        if let Some((pack, c)) = page.rows.get(i) {
            out = out.child(row(i, pack, c, root, cx));
        }
    }
    if have == order.len() {
        out = out.child(ui::section("Available", &p).px_3());
        if page.read && page.gallery == Gallery::Idle {
            out = out.child(ui::quiet("Everything installed is in use", &p).px_3());
        }
    }
    out.child(gallery(root, cx)).child(more(root, cx))
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

/// The part of a recipe a user reads: the body of its "What it shows"
/// section. The rest of a recipe is for the person who maintains the mod.
fn what_it_shows(text: &str) -> Option<String> {
    let mut body: Vec<&str> = Vec::new();
    let mut on = false;
    for line in text.lines() {
        if let Some(head) = line.strip_prefix("## ") {
            if on {
                break;
            }
            on = head.trim().eq_ignore_ascii_case("What it shows");
        } else if on {
            body.push(line);
        }
    }
    let body = body.join("\n");
    (!body.trim().is_empty()).then(|| body.trim().to_string())
}

/// The selected row: its title and purpose, its state and the one action
/// that changes it, its preview image and its recipe.
fn detail(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let page = &root.ext;
    let Some(want) = page.selected.as_deref() else {
        return div().pt_4().child(ui::quiet(
            "Select a mod or layout to see its preview and recipe",
            &p,
        ));
    };
    let Some(c) = page.selected_component() else {
        return div()
            .pt_4()
            .child(ui::quiet(format!("{want} is no longer installed"), &p));
    };
    let mut out = div()
        .flex()
        .flex_col()
        .gap_2()
        .pt_4()
        .child(ui::text(c.title.clone(), Role::Heading, &p))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(ui::text(want.to_string(), Role::Meta, &p))
                .child(ui::tag(
                    kind_words(c),
                    if c.runs_code { p.warn } else { p.muted },
                    &p,
                )),
        );
    if !c.summary.is_empty() {
        out = out.child(
            ui::text(c.summary.clone(), Role::Body, &p)
                .font_family(ui::PROSE)
                .text_size(px(13.0)),
        );
    }
    // State first, then the one action that changes it. The row keeps its
    // height whether or not Open shows, so Preview never jumps.
    let pending = page.pending.contains(want);
    let mut controls = div()
        .h(px(26.0))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .when(pending, |d| d.opacity(0.5));
    controls = if native(c) {
        controls
    } else if c.enabled {
        controls.child(ui::tag("✓ Installed", p.accent, &p)).child(
            ui::button("ext-remove", "Uninstall", Button::Quiet, &p)
                .on_click(on(Act::Set(want.to_string(), false), cx)),
        )
    } else {
        controls.child(
            ui::button("ext-add", "Install", Button::Primary, &p)
                .on_click(on(Act::Set(want.to_string(), true), cx)),
        )
    };
    if can_open(c) {
        controls = controls
            .child(
                ui::button("ext-open", "Open", Button::Quiet, &p)
                    .on_click(on(Act::Open(want.to_string()), cx)),
            )
            .child(ui::kbd("O", &p));
    }
    out = out.child(controls);
    if !c.preview.is_empty() {
        let (path, error, faint, ground) = (c.preview.clone(), p.error, p.faint, p.bg);
        let caption = if installed(c) {
            "An example with invented rows."
        } else {
            "An example with invented rows, so you can see the shape before you install it."
        };
        out = out
            .child(ui::section("Preview", &p))
            .child(ui::text(caption, Role::Meta, &p));
        // The image takes its aspect height from gpui, capped at the reserve.
        // Until it decodes, the loading and fallback elements hold the reserve
        // so the recipe does not jump up on a failed load.
        out = out.child(
            // The preview sits in a bordered, inset well, so a dark terminal
            // block never meets the paper ground bare.
            div()
                .w(px(PREVIEW_MAX + 10.0))
                .max_w_full()
                .p(px(4.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(rgb(p.border))
                .bg(rgb(p.panel))
                .child(
                    img(PathBuf::from(&c.preview))
                        .w(px(PREVIEW_MAX))
                        .max_w_full()
                        .max_h(px(PREVIEW_RESERVE))
                        .bg(rgb(ground))
                        .object_fit(ObjectFit::Contain)
                        .with_loading(move || {
                            div()
                                .w_full()
                                .h(px(PREVIEW_RESERVE))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(rgb(faint))
                                .child("Loading preview...")
                                .into_any_element()
                        })
                        .with_fallback(move || {
                            div()
                                .w_full()
                                .h(px(PREVIEW_RESERVE))
                                .bg(rgb(ground))
                                .text_color(rgb(error))
                                .child(format!("Cannot draw {path}"))
                                .into_any_element()
                        }),
                ),
        );
    }
    if !c.recipe.is_empty() {
        out = match &page.recipe {
            Some((_, Ok(text))) => match what_it_shows(text) {
                Some(shows) => {
                    let mut body = div().flex().flex_col().gap_1();
                    for block in recipe_blocks(&shows) {
                        match block {
                            Block::Prose(segment) => {
                                // The column wraps the text; no pre-wrap column.
                                for line in md_lines(&segment, usize::MAX) {
                                    body = body.child(
                                        ui::text(line.text.clone(), Role::Body, &p)
                                            .min_h(px(8.0))
                                            .font_family(ui::PROSE)
                                            .text_size(px(13.0)),
                                    );
                                }
                            }
                            Block::Code(lines) => {
                                for line in lines {
                                    body =
                                        body.child(ui::text(line, Role::Body, &p).min_h(px(8.0)));
                                }
                            }
                        }
                    }
                    out.child(ui::section("What it shows", &p)).child(body)
                }
                None => out,
            },
            Some((path, Err(e))) => out.child(
                ui::text(format!("Cannot read {path}: {e}"), Role::Meta, &p)
                    .text_color(rgb(p.error)),
            ),
            None => out.child(ui::quiet("Reading the recipe...", &p)),
        };
    }
    out
}

/// The list and the selected row's detail, side by side, each scrolling on
/// its own and set apart by space alone. The bottom pad keeps the last row
/// whole above the window edge.
pub(super) fn render(root: &Root, cx: &mut Context<Root>) -> Div {
    let column = |id: &'static str| -> Stateful<Div> {
        div()
            .id(id)
            .h_full()
            .overflow_x_hidden()
            .overflow_y_scroll()
            .pb_8()
    };
    div()
        .flex()
        .flex_row()
        .flex_1()
        .min_h_0()
        .w_full()
        .gap_6()
        .child(
            column("ext-list")
                .w(px(560.0))
                .flex_none()
                .child(list(root, cx)),
        )
        .child(
            column("ext-detail")
                .flex_1()
                .min_w_0()
                .pr_6()
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
        // Installed (on or built in) lists first, then Available (off).
        let shown: Vec<_> = page
            .order()
            .into_iter()
            .filter_map(|i| page.rows.get(i))
            .map(|(p, c)| (target(p, c), installed(c)))
            .collect();
        let have = |t: &str, yes: bool| (t.to_string(), yes);
        assert_eq!(
            shown,
            [
                have("fno-panels/backlog-board", true),
                have("fno-panels/review-ledger", true),
                have("fno-panels/decisions", true),
                have("demo-clock/ci-feed", true),
                have("demo-clock/clock", false),
                have("demo-clock/focus", false),
            ]
        );
        // Add on a code mod arms the confirm; only Confirm enables it.
        page.press("demo-clock/clock".into(), true);
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
        page.confirm = None;
        page.press("demo-clock/ci-feed".into(), false);
        assert!(matches!(rx.try_recv(), Ok(Ask::Switch(t, false)) if t == "demo-clock/ci-feed"));
    }

    #[test]
    fn a_pending_switch_takes_no_second_press_and_the_keys_stop_at_the_ends() {
        let (mut page, rx) = page();
        page.press("demo-clock/ci-feed".into(), false);
        page.press("demo-clock/ci-feed".into(), false);
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
        assert_eq!(
            page.selected_action(),
            None,
            "a built-in mod has no Install or Uninstall"
        );
        page.select("demo-clock/clock".into());
        assert_eq!(
            page.selected_action(),
            Some(("demo-clock/clock".into(), true))
        );
        assert_eq!(page.selected_open(), None, "a disabled mod has no pane");
        page.select("demo-clock/ci-feed".into());
        assert_eq!(page.selected_open().as_deref(), Some("demo-clock/ci-feed"));
    }

    #[test]
    fn the_detail_pane_reads_only_what_it_shows_and_a_native_mod_opens() {
        let recipe = "# Review ledger\n\nLead.\n\n## What it shows\n\nOne row per branch.\n\n## What it runs\n\nfno-agents\n";
        assert_eq!(
            what_it_shows(recipe).as_deref(),
            Some("One row per branch.")
        );
        assert_eq!(what_it_shows("# T\n\n## What it runs\n\nx\n"), None);
        assert_eq!(
            native_open("fno-panels/backlog-board"),
            Action::Show(MainView::Orchestration)
        );
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
        assert!(!landed.terminal && landed.failed);
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
