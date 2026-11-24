//! The session panel at the window's right edge: the focused session's
//! repo, branch and host over a list of cards (Pull request, Changes,
//! Context, Outputs, Questions, and any installed view mod the user adds).
//! Each card is keyed by its id, so a later card joins the list without a
//! layout change. Chat and Terminal mount it; ⌥⌘B folds it.

use fno::process_admission::std_output;
use std::collections::{HashMap, HashSet};
use std::process::Command as Proc;
use std::time::{Duration, Instant};

use gpui::{div, prelude::*, px, relative, rgb, Context, Div, ElementId, Stateful, Task};
use serde::{Deserialize, Serialize};

use super::link::Layout;
use super::palette::Palette;
use super::ui::{self, Role};
use super::window::{Action, MainView, Root};
use fno::proto::AgentRow;

/// The panel's one width, fixed until the pane edges learn to drag.
pub const WIDTH: f32 = 320.0;
/// How often an open panel re-reads the focused session's diff.
const REREAD: Duration = Duration::from_secs(10);
/// How often the panel checks whether its subject moved.
const TICK: Duration = Duration::from_secs(2);
/// The rows a list card shows before its "N more" line.
const TOP: usize = 3;

/// One card, by the id the list persists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Card {
    PullRequest,
    Changes,
    Context,
    Outputs,
    Questions,
    /// An installed view mod, by its `pack/id` target.
    View(String),
}

impl Card {
    fn from_id(id: &str) -> Card {
        match id {
            "fno.pr" => Card::PullRequest,
            "fno.changes" => Card::Changes,
            "fno.context" => Card::Context,
            "fno.outputs" => Card::Outputs,
            "fno.questions" => Card::Questions,
            other => Card::View(other.to_string()),
        }
    }
}

/// The default order. A Mail card joins once the app reads mail.
const DEFAULT_CARDS: [&str; 5] = [
    "fno.pr",
    "fno.changes",
    "fno.context",
    "fno.outputs",
    "fno.questions",
];

/// What survives a restart: open or folded, the card order, and which
/// cards sit folded to their header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    pub open: bool,
    pub cards: Vec<String>,
    pub folded: HashSet<String>,
}

impl Default for Saved {
    fn default() -> Self {
        Saved {
            open: true,
            cards: DEFAULT_CARDS.iter().map(|s| s.to_string()).collect(),
            folded: HashSet::new(),
        }
    }
}

/// One `git diff --numstat` read: the totals and each file's counts,
/// biggest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diffstat {
    pub added: u64,
    pub deleted: u64,
    pub files: Vec<(String, u64, u64)>,
}

/// The panel's live state: the saved part plus the last diff read.
#[derive(Debug, Default)]
pub struct Panel {
    pub saved: Saved,
    /// The cwd the last read ran in and what it found.
    changes: Option<(String, Result<Diffstat, DiffFailed>)>,
    read_at: Option<Instant>,
    reading: bool,
    /// The "+ panel" list is open.
    adding: bool,
}

/// What a panel click, or ⌥⌘B, does.
#[derive(Debug, Clone, PartialEq)]
pub enum PanelAct {
    /// Show or hide the whole panel.
    Toggle,
    Fold(String),
    Close(String),
    Add(String),
    ToggleAdd,
    /// Open a URL or a file with the system handler.
    Open(String),
}

impl PanelAct {
    /// The stable control id the usage events arm records.
    pub fn click_id(&self) -> &'static str {
        match self {
            PanelAct::Toggle => "toggle_right_pane",
            PanelAct::Fold(_) => "session_panel_fold",
            PanelAct::Close(_) => "session_panel_close",
            PanelAct::Add(_) => "session_panel_add",
            PanelAct::ToggleAdd => "session_panel_add_list",
            PanelAct::Open(_) => "session_panel_open",
        }
    }
}

/// Whether the panel takes a column in this view.
pub fn mounted(root: &Root) -> bool {
    root.right.saved.open && matches!(root.main, MainView::Chat | MainView::Terminal)
}

/// The focused row's pane: its live directory and the machine its pty runs
/// on. A remote row or a row with no pane has neither.
fn pane_facts<'a>(layout: &'a Layout, a: &AgentRow) -> (Option<&'a str>, Option<&'a str>) {
    if a.remote.is_some() {
        return (None, None);
    }
    let Some(id) = a.pane_id else {
        return (None, None);
    };
    let pane = layout
        .squads
        .iter()
        .flat_map(|s| s.tabs.iter())
        .flat_map(|t| t.panes.iter())
        .find(|p| p.id == id);
    (
        pane.and_then(|p| p.cwd.as_deref()),
        pane.and_then(|p| p.host.as_deref()),
    )
}

fn subject(root: &Root) -> Option<&AgentRow> {
    let chat = root.chat.key.as_ref().map(|k| k.session_id.as_str());
    super::chat::focused_agent(&root.layout, root.main, chat)
}

/// Totals and per-file counts from `git diff --numstat` output. A binary
/// file's `-` counts read as zero.
pub fn parse_numstat(text: &str) -> Diffstat {
    let mut out = Diffstat::default();
    for line in text.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(a), Some(d), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let a = a.parse().unwrap_or(0);
        let d = d.parse().unwrap_or(0);
        out.added += a;
        out.deleted += d;
        out.files.push((path.to_string(), a, d));
    }
    out.files.sort_by(|x, y| (y.1 + y.2).cmp(&(x.1 + x.2)));
    out
}

/// Why the Changes card has no diff to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffFailed {
    /// The pane's directory is outside any git work tree: a fact, not an error.
    NotARepo,
    /// git ran and failed: its first stderr line.
    Git(String),
}

/// The diff of `cwd` against its merge base with the remote's default
/// branch, working tree included; against HEAD when no remote base reads.
fn read_changes(cwd: &str) -> Result<Diffstat, DiffFailed> {
    let git = |args: &[&str]| {
        std_output(Proc::new("git").arg("-C").arg(cwd).args(args))
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    if git(&["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return Err(DiffFailed::NotARepo);
    }
    let base = git(&["rev-parse", "--abbrev-ref", "origin/HEAD"])
        .and_then(|up| git(&["merge-base", "HEAD", &up]))
        .unwrap_or_else(|| "HEAD".to_string());
    let out = std_output(
        Proc::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["diff", "--numstat", &base]),
    )
    .map_err(|e| DiffFailed::Git(e.to_string()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().next().unwrap_or("git diff failed");
        return Err(DiffFailed::Git(line.to_string()));
    }
    Ok(parse_numstat(&String::from_utf8_lossy(&out.stdout)))
}

/// The panel's one task: every tick it asks whether the subject's directory
/// moved or the last read aged out, and reads the diff off the UI thread.
/// It also asks the plugins thread for any view mod card not read yet.
pub fn start(cx: &mut Context<Root>) -> Task<()> {
    cx.spawn(async move |this, cx| loop {
        let want = this.update(cx, |root, _| {
            let views: Vec<String> = root
                .right
                .saved
                .cards
                .iter()
                .filter(|id| matches!(Card::from_id(id), Card::View(_)))
                .cloned()
                .collect();
            for target in views {
                root.ext.want_panel_view(&target);
            }
            if !mounted(root) || root.right.reading {
                return None;
            }
            let cwd = subject(root)
                .and_then(|a| pane_facts(&root.layout, a).0)
                .map(str::to_string)?;
            let same = root.right.changes.as_ref().is_some_and(|(c, _)| *c == cwd);
            let fresh = root.right.read_at.is_some_and(|t| t.elapsed() < REREAD);
            if same && fresh {
                return None;
            }
            root.right.reading = true;
            Some(cwd)
        });
        let Ok(want) = want else { break };
        if let Some(cwd) = want {
            let read = {
                let cwd = cwd.clone();
                cx.background_executor()
                    .spawn(async move { read_changes(&cwd) })
                    .await
            };
            let landed = this.update(cx, |root, cx| {
                root.right.changes = Some((cwd, read));
                root.right.read_at = Some(Instant::now());
                root.right.reading = false;
                cx.notify();
            });
            if landed.is_err() {
                break;
            }
            continue;
        }
        cx.background_executor().timer(TICK).await;
    })
}

/// Apply one panel click.
pub fn act(root: &mut Root, act: PanelAct) {
    let saved = &mut root.right.saved;
    match act {
        PanelAct::Toggle => saved.open = !saved.open,
        PanelAct::Fold(id) => {
            if !saved.folded.remove(&id) {
                saved.folded.insert(id);
            }
        }
        PanelAct::Close(id) => saved.cards.retain(|c| *c != id),
        PanelAct::Add(id) => {
            if !saved.cards.contains(&id) {
                saved.cards.push(id);
            }
            root.right.adding = false;
        }
        PanelAct::ToggleAdd => root.right.adding = !root.right.adding,
        PanelAct::Open(target) => {
            // A short-lived `open`; waited on its own thread, so no zombie.
            std::thread::spawn(move || {
                if let Err(e) = Proc::new("open").arg(&target).status() {
                    eprintln!("fno-app: cannot open {target}: {e}");
                }
            });
        }
    }
}

/// The files the session wrote with its Write tool outside `cwd`, newest
/// first, each once. With no cwd known, every written path counts.
pub fn outputs(written: &[&str], cwd: Option<&str>) -> Vec<String> {
    let inside = |p: &str| {
        cwd.is_some_and(|c| {
            let c = c.trim_end_matches('/');
            p == c || p.starts_with(&format!("{c}/"))
        })
    };
    let mut seen = HashSet::new();
    written
        .iter()
        .rev()
        .filter(|p| !inside(**p))
        .filter(|p| seen.insert(p.to_string()))
        .map(|p| p.to_string())
        .collect()
}

/// The panel column.
pub(super) fn render(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    // The cards scroll inside a body under the title bar's fixed band, as
    // the left pane's rows do.
    let frame = div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(WIDTH))
        .h_full()
        .pt(px(ui::TITLE_H))
        .bg(rgb(p.panel))
        .border_l_1()
        .border_color(rgb(p.border))
        .font_family(ui::MONO);
    frame.child(body(root, cx))
}

fn body(root: &Root, cx: &mut Context<Root>) -> Stateful<Div> {
    let p = root.palette;
    let mut col = div()
        .id("right-pane-body")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px_3()
        .pb_3()
        .gap_2();
    let Some(a) = subject(root) else {
        return col.child(ui::quiet("no agent in this pane", &p));
    };
    let (cwd, pane_host) = pane_facts(&root.layout, a);
    col = col.child(environment(a, pane_host, &p));
    for (i, id) in root.right.saved.cards.iter().enumerate() {
        let card = Card::from_id(id);
        let folded = root.right.saved.folded.contains(id);
        let (title, count, body) = match &card {
            Card::PullRequest => pr_card(a, &p, cx),
            Card::Changes => changes_card(root, a, cwd, &p),
            Card::Context => context_card(a, &p),
            Card::Outputs => outputs_card(root, a, cwd, &p, cx),
            Card::Questions => questions_card(root, &p, cx),
            Card::View(target) => view_card(root, target, &p),
        };
        col = col.child(card_frame(i, id, &title, count, folded, body, &p, cx));
    }
    col.child(add_row(root, &p, cx))
}

/// Repo, branch and host, one quiet line each.
fn environment(a: &AgentRow, pane_host: Option<&str>, p: &Palette) -> Div {
    let repo = a
        .project_root
        .as_deref()
        .and_then(|r| r.trim_end_matches('/').rsplit('/').next())
        .or(a.cwd_base.as_deref())
        .unwrap_or("no repo");
    let base = a.cwd_base.as_deref().unwrap_or("");
    let branch = super::start::branch_of(a, base).unwrap_or("no branch");
    let host = a
        .remote
        .as_ref()
        .map(|r| r.host.as_str())
        .or(pane_host)
        .unwrap_or("this machine");
    div()
        .flex()
        .flex_col()
        .pt_3()
        .child(ui::text(a.name.clone(), Role::Heading, p))
        // A repo named like its agent would print the same text twice.
        .when(repo != a.name, |d| {
            d.child(ui::text(repo.to_string(), Role::Meta, p))
        })
        .child(ui::text(branch.to_string(), Role::Meta, p))
        .child(ui::text(host.to_string(), Role::Fact, p))
}

/// One card: a header row (title, count, id, fold chevron, close) over its
/// body. A hairline and the raised fill only, never a colored edge.
#[allow(clippy::too_many_arguments)]
fn card_frame(
    i: usize,
    id: &str,
    title: &str,
    count: Option<usize>,
    folded: bool,
    body: Div,
    p: &Palette,
    cx: &mut Context<Root>,
) -> Div {
    let fold = PanelAct::Fold(id.to_string());
    let close = PanelAct::Close(id.to_string());
    let header = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .child(ui::clamp(title.to_string(), Role::Name, p))
        .children(count.map(|n| ui::text(n.to_string(), Role::Fact, p)))
        .child(
            div()
                .id(ElementId::Name(format!("right-fold-{i}").into()))
                .cursor_pointer()
                .child(ui::text(if folded { "▸" } else { "▾" }, Role::Meta, p))
                .on_click(cx.listener(move |root, _, _, cx| root.act_panel(fold.clone(), cx))),
        )
        .child(
            div()
                .id(ElementId::Name(format!("right-close-{i}").into()))
                .cursor_pointer()
                .child(ui::text("×", Role::Meta, p))
                .on_click(cx.listener(move |root, _, _, cx| root.act_panel(close.clone(), cx))),
        );
    let frame = div()
        .flex()
        .flex_col()
        .min_w_0()
        .gap_1()
        .p_2()
        .bg(rgb(p.raised))
        .border_1()
        .border_color(rgb(p.border))
        .child(header);
    // The body takes the card's width, so a row's clamp truncates against
    // that width rather than collapsing to its own min-content of zero.
    if folded {
        frame
    } else {
        frame.child(body.w_full().min_w_0())
    }
}

/// A line that opens `target` on click.
fn link_line(
    id: String,
    text: String,
    target: String,
    p: &Palette,
    cx: &mut Context<Root>,
) -> Stateful<Div> {
    let action = PanelAct::Open(target);
    let p = *p;
    div()
        .id(ElementId::Name(id.into()))
        .flex()
        .w_full()
        .cursor_pointer()
        .hover(move |s| s.text_color(rgb(p.fg)))
        .child(ui::clamp(text, Role::Body, &p))
        .on_click(cx.listener(move |root, _, _, cx| root.act_panel(action.clone(), cx)))
}

fn pr_card(a: &AgentRow, p: &Palette, cx: &mut Context<Root>) -> (String, Option<usize>, Div) {
    let binding = a.bindings.iter().rev().find(|b| b.pr.is_some());
    let count = binding.map(|b| b.pr_count as usize).filter(|n| *n > 1);
    // The number and its link come from one source, so they never name two
    // PRs: the newest binding with a PR, else the row's own number unlinked.
    let shown = match binding {
        Some(b) => (b.pr, b.pr_url.clone()),
        None => (a.pr, None),
    };
    let body = match shown {
        (Some(n), Some(url)) => {
            div().child(link_line("right-pr".into(), format!("#{n}"), url, p, cx))
        }
        (Some(n), None) => div().child(ui::text(format!("#{n}"), Role::Body, p)),
        (None, _) => div().child(ui::quiet("no PR yet", p)),
    };
    ("Pull request".into(), count, body)
}

fn changes_card(
    root: &Root,
    a: &AgentRow,
    cwd: Option<&str>,
    p: &Palette,
) -> (String, Option<usize>, Div) {
    let title = "Changes".to_string();
    if let Some(r) = &a.remote {
        return (
            title,
            None,
            div().child(ui::quiet(format!("on {}, not read from here", r.host), p)),
        );
    }
    let Some(cwd) = cwd else {
        return (
            title,
            None,
            div().child(ui::quiet("no pane, so no working directory", p)),
        );
    };
    let read = root
        .right
        .changes
        .as_ref()
        .filter(|(c, _)| c == cwd)
        .map(|(_, r)| r);
    let body = match read {
        None => div().child(ui::quiet("reading…", p)),
        Some(Err(DiffFailed::NotARepo)) => div().child(ui::quiet("not a git repository", p)),
        Some(Err(DiffFailed::Git(e))) => {
            div().child(ui::text(e.clone(), Role::Fact, p).text_color(rgb(p.error)))
        }
        Some(Ok(d)) if d.files.is_empty() => div().child(ui::quiet("no changes", p)),
        Some(Ok(d)) => {
            let n = d.files.len();
            let mut out = div().flex().flex_col().child(ui::text(
                format!(
                    "+{} -{} · {n} file{}",
                    d.added,
                    d.deleted,
                    if n == 1 { "" } else { "s" }
                ),
                Role::Body,
                p,
            ));
            for (path, add, del) in d.files.iter().take(TOP) {
                out = out.child(
                    div()
                        .flex()
                        .flex_row()
                        .w_full()
                        .gap(px(6.0))
                        .child(ui::clamp(path.clone(), Role::Meta, p))
                        .child(ui::text(format!("+{add} -{del}"), Role::Fact, p)),
                );
            }
            if n > TOP {
                out = out.child(ui::text(format!("{} more", n - TOP), Role::Hint, p));
            }
            return (title, Some(n), out);
        }
    };
    (title, None, body)
}

fn context_card(a: &AgentRow, p: &Palette) -> (String, Option<usize>, Div) {
    let body = match a.context_used_pct {
        None => div().child(ui::quiet("not measured", p)),
        Some(pct) => {
            let mut out = div().flex().flex_col().gap_1().child(ui::text(
                format!("{pct}% of context"),
                Role::Body,
                p,
            ));
            if let Some((used, total)) = a.context_tokens {
                out = out.child(ui::text(
                    format!("{} / {}", tokens(used), tokens(total)),
                    Role::Fact,
                    p,
                ));
            }
            out.child(
                div().h(px(4.0)).w_full().bg(rgb(p.border)).child(
                    div()
                        .h_full()
                        .w(relative(f32::from(pct.min(100)) / 100.0))
                        .bg(rgb(p.accent)),
                ),
            )
        }
    };
    ("Context".into(), None, body)
}

/// A token count the way the title bar reads them: 620k, 1M.
fn tokens(n: u64) -> String {
    if n >= 1_000_000 && n % 1_000_000 == 0 {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

fn outputs_card(
    root: &Root,
    a: &AgentRow,
    cwd: Option<&str>,
    p: &Palette,
    cx: &mut Context<Root>,
) -> (String, Option<usize>, Div) {
    let title = "Outputs".to_string();
    if let Some(r) = &a.remote {
        return (
            title,
            None,
            div().child(ui::quiet(format!("on {}, not read from here", r.host), p)),
        );
    }
    if a.harness.as_deref() != Some("claude") {
        return (
            title,
            None,
            div().child(ui::quiet("no outputs: this pane runs no claude session", p)),
        );
    }
    let streamed = root.chat.key.as_ref().map(|k| k.session_id.as_str());
    if streamed.is_none() || streamed != a.harness_session_id.as_deref() {
        return (
            title,
            None,
            div().child(ui::quiet("open its chat to list outputs", p)),
        );
    }
    let files = outputs(&root.chat.conv.written(), cwd);
    if files.is_empty() {
        return (
            title,
            None,
            div().child(ui::quiet("no files written outside the repo", p)),
        );
    }
    let n = files.len();
    let mut out = div().flex().flex_col();
    for (i, path) in files.into_iter().take(TOP).enumerate() {
        out = out.child(link_line(
            format!("right-out-{i}"),
            path.clone(),
            path,
            p,
            cx,
        ));
    }
    if n > TOP {
        out = out.child(ui::text(format!("{} more", n - TOP), Role::Hint, p));
    }
    (title, Some(n), out)
}

fn questions_card(
    root: &Root,
    p: &Palette,
    cx: &mut Context<Root>,
) -> (String, Option<usize>, Div) {
    let title = "Questions".to_string();
    if !root.board.read_yet() {
        return (title, None, div().child(ui::quiet("reading…", p)));
    }
    let qs = root.board.questions_newest_first();
    if qs.is_empty() {
        return (
            title,
            Some(0),
            div().child(ui::quiet("nothing waits on you", p)),
        );
    }
    let n = qs.len();
    // min_w_0 down the chain lets each title truncate to an ellipsis
    // inside the card padding instead of clipping at the card edge.
    let mut out = div().flex().flex_col().min_w_0();
    for (i, q) in qs.iter().take(TOP).enumerate() {
        let action = Action::OpenAnswer(q.item.id.clone());
        let pp = *p;
        out = out.child(
            div()
                .id(ElementId::Name(format!("right-q-{i}").into()))
                .flex()
                .flex_row()
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .gap(px(6.0))
                .cursor_pointer()
                .hover(move |s| s.bg(rgb(pp.hover)))
                .child(div().flex_none().text_color(rgb(p.warn)).child("◆"))
                .child(ui::clamp(q.item.title.clone(), Role::Body, p))
                .on_click(cx.listener(move |root, _, _, cx| root.act(action.clone(), cx))),
        );
    }
    if n > TOP {
        out = out.child(ui::text(format!("+{} more", n - TOP), Role::Hint, p));
    }
    (title, Some(n), out)
}

fn view_card(root: &Root, target: &str, p: &Palette) -> (String, Option<usize>, Div) {
    let (title, view) = root.ext.panel_view(target);
    let body = match view {
        None => div().child(ui::quiet("reading…", p)),
        Some(Err(e)) => div().child(ui::text(e.0, Role::Fact, p).text_color(rgb(p.error))),
        Some(Ok(view)) => {
            let mut out = div().flex().flex_col().gap_1();
            for block in &view.blocks {
                out = out.child(super::mods::view_block(block, p));
            }
            out
        }
    };
    (title.unwrap_or_else(|| target.to_string()), None, body)
}

/// The list's last row: "+ panel", which lists installed view mods not
/// shown yet.
fn add_row(root: &Root, p: &Palette, cx: &mut Context<Root>) -> Div {
    let toggle = PanelAct::ToggleAdd;
    let mut out = div().flex().flex_col().child(
        div()
            .id("right-add")
            .cursor_pointer()
            .child(ui::text("+ panel", Role::Meta, p))
            .on_click(cx.listener(move |root, _, _, cx| root.act_panel(toggle.clone(), cx))),
    );
    if !root.right.adding {
        return out;
    }
    let shown: HashSet<&str> = root.right.saved.cards.iter().map(String::as_str).collect();
    let mut offered: HashMap<String, String> = root
        .ext
        .view_mods()
        .into_iter()
        .filter(|(target, _)| !shown.contains(target.as_str()))
        .collect();
    let builtin = [
        ("fno.pr", "Pull request"),
        ("fno.changes", "Changes"),
        ("fno.context", "Context"),
        ("fno.outputs", "Outputs"),
        ("fno.questions", "Questions"),
    ];
    for (id, title) in builtin {
        if !shown.contains(id) {
            offered.insert(id.to_string(), title.to_string());
        }
    }
    if offered.is_empty() {
        return out.child(ui::quiet("every panel is shown", p));
    }
    let mut offered: Vec<(String, String)> = offered.into_iter().collect();
    offered.sort();
    for (i, (target, title)) in offered.into_iter().enumerate() {
        let add = PanelAct::Add(target);
        out = out.child(
            div()
                .id(ElementId::Name(format!("right-add-{i}").into()))
                .cursor_pointer()
                .pl_3()
                .child(ui::text(title, Role::Body, p))
                .on_click(cx.listener(move |root, _, _, cx| root.act_panel(add.clone(), cx))),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_totals_and_sorts_biggest_first_with_binary_as_zero() {
        let d = parse_numstat("3\t1\tsrc/a.rs\n-\t-\tlogo.png\n40\t2\tsrc/b.rs\n\n");
        assert_eq!((d.added, d.deleted), (43, 3));
        let order: Vec<&str> = d.files.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(order, ["src/b.rs", "src/a.rs", "logo.png"]);
    }

    #[test]
    fn outputs_keep_files_outside_the_repo_newest_first_once() {
        let written = [
            "/repo/src/a.rs",
            "/tmp/report.md",
            "/repo-other/x.md",
            "/tmp/report.md",
            "/Users/me/notes.md",
        ];
        assert_eq!(
            outputs(&written, Some("/repo/")),
            ["/Users/me/notes.md", "/tmp/report.md", "/repo-other/x.md"]
        );
        assert_eq!(outputs(&written, None).len(), 4);
    }
}
