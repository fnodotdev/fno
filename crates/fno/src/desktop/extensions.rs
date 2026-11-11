//! Extensions mode (cmd-3): the mods and layouts `fno config plugins ls --json`
//! lists, each with its switch, preview image and rendered recipe, plus an
//! install row and the published gallery. The mux settings tab reads the
//! same feed through `plugins_feed`, so both hosts show one list.
//!
//! Reads and switches run on one `fno-desktop-plugins` thread and land
//! through a reply channel, like the board poll. Install and open run in a
//! mux pane on the attached session, so the consent card and its prompt stay
//! the CLI's: the app never installs on its own.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::time::Duration;

use gpui::{
    div, img, prelude::*, px, rgb, Context, Div, ElementId, FontWeight, ObjectFit, Stateful,
};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use super::keys::Key;
use super::window::{MainView, Root};
use crate::client::node_detail::backlog_md::md_lines;
use crate::plugins_feed::{self as feed, Component, Published, ReadFailed};
use crate::text_style::TextStyle;

/// How often the open view re-reads, so an install that finishes in its pane
/// shows up without a key.
pub(super) const REREAD: Duration = Duration::from_secs(3);
const MAX_URL: usize = 512;
const RECIPE_COLS: usize = 80;

type Rows = Vec<(String, Component)>;

/// Work for the plugins thread.
pub(super) enum Ask {
    Installed,
    Gallery,
    Recipe(String),
    Switch(String, bool),
}

/// What the plugins thread read or did.
pub(super) enum Reply {
    Installed(Result<Rows, ReadFailed>),
    Gallery(Result<Vec<Published>, ReadFailed>),
    Recipe(String, Result<String, ReadFailed>),
    Switched(String, bool, Result<(), ReadFailed>),
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
    Cancel,
    /// Show `pack/mod` in a pane through `fno config plugins open`.
    Open(String),
    Install,
    Browse,
    /// A gallery row: open the URL field holding its URL.
    Pick(String),
    Submit,
}

/// The page state: the last good rows, the last failed read, the selection
/// and its recipe, the open URL field and an armed code-mod confirm.
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
    asks: Option<Sender<Ask>>,
}

fn target(pack: &str, c: &Component) -> String {
    format!("{pack}/{}", c.id)
}

fn native(c: &Component) -> bool {
    c.kind == "native"
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

    /// Store what the plugins thread sent. The text is the notice to show.
    pub(super) fn apply(&mut self, reply: Reply) -> Option<String> {
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
                None
            }
            Reply::Gallery(read) => {
                self.gallery = Gallery::Done(read);
                None
            }
            Reply::Recipe(path, text) => {
                // A reply for a row the user already left is dropped.
                if self.selected_component().is_some_and(|c| c.recipe == path) {
                    self.recipe = Some((path, text));
                }
                None
            }
            Reply::Switched(target, on, done) => {
                self.read();
                Some(match done {
                    Ok(()) => format!("{target} {}", if on { "on" } else { "off" }),
                    Err(e) => e.0,
                })
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
    /// nothing.
    fn toggle(&mut self, want: String, on: bool) {
        let code = self
            .rows
            .iter()
            .any(|(p, c)| target(p, c) == want && c.runs_code);
        if on && code {
            self.confirm = Some(want);
        } else {
            self.ask(Ask::Switch(want, on));
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
/// pane on the attached session and show that pane.
pub(super) fn act(root: &mut Root, act: Act, cx: &mut Context<Root>) {
    let page = &mut root.ext;
    match act {
        Act::Select(t) => page.select(t),
        Act::Switch(t, on) => page.toggle(t, on),
        Act::Confirm => {
            if let Some(t) = page.confirm.take() {
                page.ask(Ask::Switch(t, true));
            }
        }
        Act::Cancel => {
            page.confirm = None;
            page.url = None;
        }
        Act::Open(t) => {
            let notice = feed::in_pane(&["open", &t], &t, Some(&root.session));
            return in_terminal(root, notice, cx);
        }
        Act::Install => page.url.get_or_insert_with(String::new).clear(),
        Act::Browse => {
            if page.ask(Ask::Gallery) {
                page.gallery = Gallery::Reading;
            }
        }
        Act::Pick(install) => {
            let url = install.rsplit(' ').next().unwrap_or_default();
            page.url = Some(url.to_string());
        }
        Act::Submit => match page.url_to_install() {
            Some(url) => {
                page.url = None;
                let notice = feed::in_pane(
                    &["install", &url],
                    &format!("install {url}"),
                    Some(&root.session),
                );
                return in_terminal(root, notice, cx);
            }
            None => root.say("type a git URL".into(), cx),
        },
    }
    cx.notify();
}

/// The pane opens on the server, so the window flips to the terminal where
/// it is in sight.
fn in_terminal(root: &mut Root, notice: String, cx: &mut Context<Root>) {
    root.main = MainView::Terminal;
    root.say(notice, cx);
    cx.notify();
}

/// A key in the view. True when the page took it.
pub(super) fn key(root: &mut Root, key: &Key, cx: &mut Context<Root>) -> bool {
    if root.ext.url.is_some() {
        let text = key.key_char.filter(|_| !key.ctrl && !key.cmd);
        match root.ext.edit(key.key, text) {
            Edit::Close => act(root, Act::Cancel, cx),
            Edit::Submit => act(root, Act::Submit, cx),
            Edit::Typed => cx.notify(),
        }
        return true;
    }
    if key.key == "escape" && root.ext.confirm.is_some() {
        act(root, Act::Cancel, cx);
        return true;
    }
    false
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
        .hover(move |s| s.bg(rgb(p.border)))
        .active(move |s| s.bg(rgb(p.focus)))
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
            .text_xs()
            .text_color(rgb(p.muted))
            .child("Built in")
    } else {
        let (label, color) = if c.enabled {
            ("On", p.ok)
        } else {
            ("Off", p.muted)
        };
        let on = !c.enabled;
        let t = want.clone();
        div().w(px(56.0)).flex_none().child(
            div()
                .id(named("ext-switch", i))
                .px_2()
                .rounded_md()
                .border_1()
                .border_color(rgb(p.border))
                .text_color(rgb(color))
                .cursor_pointer()
                .hover(move |s| s.bg(rgb(p.border)))
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
        .py_1()
        .rounded_md()
        .when(chosen, |d| d.bg(rgb(p.panel)))
        .cursor_pointer()
        .on_click(cx.listener(move |root, _, _, cx| act(root, Act::Select(t.clone()), cx)))
        .child(div().font_weight(FontWeight::BOLD).child(c.title.clone()))
        .child(div().text_xs().text_color(rgb(p.muted)).child(want))
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
                        Act::Cancel,
                        cx,
                    )),
            ),
        );
    }
    if !page.read {
        out = out.child(quiet("reading…", root));
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
                        "https://github.com/owner/repo".to_string()
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
                        Act::Cancel,
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
        Gallery::Reading => out.child(quiet("Reading the gallery…", root)),
        Gallery::Done(Err(e)) => {
            out.child(div().py_1().text_color(rgb(p.error)).child(e.0.clone()))
        }
        Gallery::Done(Ok(repos)) if repos.is_empty() => {
            out.child(quiet("No published packs yet", root))
        }
        Gallery::Done(Ok(repos)) => {
            for (i, g) in repos.iter().enumerate() {
                let install = g.install.clone();
                let note = if g.reviewed {
                    ""
                } else {
                    " · Not reviewed by fno"
                };
                out = out.child(
                    div()
                        .id(named("ext-pick", i))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(move |s| s.bg(rgb(p.panel)))
                        .on_click(cx.listener(move |root, _, _, cx| {
                            act(root, Act::Pick(install.clone()), cx)
                        }))
                        .child(format!("{} · {} stars{note}", g.name, g.stars))
                        .child(div().text_color(rgb(p.muted)).child(g.description.clone())),
                );
            }
            out
        }
    }
}

/// The selected row: its preview image, its summary and its recipe.
fn detail(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let page = &root.ext;
    let Some(want) = page.selected.as_deref() else {
        return div().child(quiet(
            "Select a mod or layout to see its preview and recipe",
            root,
        ));
    };
    let Some(c) = page.selected_component() else {
        return div().child(quiet(format!("{want} is no longer installed"), root));
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
    if c.enabled && (c.kind == "feed" || c.kind == "run") {
        out = out.child(div().flex().child(button(
            "ext-open".into(),
            "Open in a pane".into(),
            root,
            Act::Open(want.to_string()),
            cx,
        )));
    }
    if !c.preview.is_empty() {
        let (path, error) = (c.preview.clone(), p.error);
        out = out.child(
            img(PathBuf::from(&c.preview))
                .w(px(480.0))
                .h(px(300.0))
                .object_fit(ObjectFit::Contain)
                .with_fallback(move || {
                    div()
                        .text_color(rgb(error))
                        .child(format!("cannot draw {path}"))
                        .into_any_element()
                }),
        );
    }
    if !c.recipe.is_empty() {
        out = match &page.recipe {
            Some((_, Ok(text))) => {
                let mut body = div().flex().flex_col().pt_2();
                for line in md_lines(text, RECIPE_COLS) {
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
                out.child(body)
            }
            Some((path, Err(e))) => out.child(
                div()
                    .text_color(rgb(p.error))
                    .child(format!("cannot read {path}: {e}")),
            ),
            None => out.child(quiet("reading the recipe…", root)),
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
        assert_eq!(page.url_to_install().as_deref(), Some("https://github.com/o/r"));
        assert_eq!(page.edit("enter", None), Edit::Submit);
        assert_eq!(page.edit("escape", None), Edit::Close);
    }
}
