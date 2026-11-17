//! The persona overlay: Enter on an audience chip opens who the record
//! serves. The page renders the persona file (`~/.fno/profile/customers/
//! <project>/<persona>.md`): who they are, their jobs, and the frontmatter
//! status and confidence. Nothing is cut: every job paragraph wraps with a
//! hanging indent under its slug, and a page longer than the viewport
//! scrolls (j/k). The serving-now section lists the open records naming
//! this persona; j/k select one and Enter opens it. A multi-persona
//! audience opens a picker first; the bell's Personas tab lists every
//! persona under the project's profile directory. A missing file renders a
//! notice naming the exact path.
//!
//! The path resolution mirrors `fno-agent`'s `persona_customers_dir` (the
//! `FNO_CUSTOMERS_DIR` override, then `FNO_HOME`, then `HOME/.fno`), because
//! the backlog command menu sits in shrink-only menu-caps debt and takes no
//! new read door. The files are static user content, so a client-side read
//! cannot drift from a listing the way a graph projection can.
//!
//! A child module of `client`, so `View`'s private fields stay local.

use super::*;
use crate::text_style::TSeg;

#[derive(Debug)]
pub(crate) struct PersonaOverlay {
    /// The chrome title: the persona's display name, or the picker header.
    title: String,
    lines: Vec<TLine>,
    /// Picker mode: the highlighted row index. `None` on a persona page or
    /// a notice, where Esc, `q` and `x` close.
    cursor: Option<usize>,
    /// The picker's targets, so Enter drills into the highlighted persona.
    personas: Vec<String>,
    /// The project the personas resolve against.
    project: String,
    /// The page's own file, the one `a` stamps approved in place.
    path: std::path::PathBuf,
    /// A draft persona (the status frontmatter says so): the page shows
    /// the Approve key.
    draft: bool,
    /// The slug this page serves; the serving-now read matches it against
    /// the records' audiences. Empty on a picker or a notice.
    slug: String,
    /// The parsed page file, kept so the paint can wrap at the real body
    /// width. `None` on a picker (its `lines` are the rows) or a notice.
    doc: Option<PersonaDoc>,
    /// The plain-scroll row a page without serving rows keeps visible.
    /// Unbounded here; the paint clamps to the body's real height.
    scroll: usize,
    /// The selected serving row, if any.
    sel: Option<usize>,
    /// A missing-file notice: short, so any key closes (the digest rule).
    notice: bool,
}

/// The personas a stored audience value names: the `customer:<slug>` items
/// of the comma list. Mirrors `audience::personas_in_scope` on the agent
/// side; this crate cannot call it, so a test pins the two together.
pub(crate) fn personas_in(audience: Option<&str>) -> Vec<String> {
    audience
        .map(str::trim)
        .unwrap_or("")
        .split(',')
        .filter_map(|v| v.trim().strip_prefix("customer:").map(str::to_string))
        .collect()
}

/// The persona file for one slug, resolved the way the filing doors
/// validate it (`fno-agent` `paths::persona_customers_dir`).
fn persona_path(project: &str, persona: &str) -> std::path::PathBuf {
    customers_root(project).join(format!("{persona}.md"))
}

fn customers_root(project: &str) -> std::path::PathBuf {
    let root = std::env::var_os("FNO_CUSTOMERS_DIR")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("FNO_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".fno"))
                })
                .unwrap_or_else(|| std::path::PathBuf::from(".fno"))
                .join("profile")
                .join("customers")
        });
    root.join(project)
}

/// The persona file's renderable fields: the frontmatter facts, the whole
/// who paragraph, and one job slug with its whole paragraph. Nothing is
/// cut here; the paint wraps at the real body width.
#[derive(Debug)]
struct PersonaDoc {
    name: String,
    status: Option<String>,
    confidence: Option<String>,
    who: Vec<String>,
    jobs: Vec<(String, String)>,
}

fn parse_persona(text: &str) -> PersonaDoc {
    let mut doc = PersonaDoc {
        name: String::new(),
        status: None,
        confidence: None,
        who: Vec::new(),
        jobs: Vec::new(),
    };
    let mut lines = text.lines().peekable();
    // Frontmatter: the `key: value` block between the leading --- pair.
    if lines.peek().is_some_and(|l| l.trim() == "---") {
        lines.next();
        for l in lines.by_ref() {
            let l = l.trim();
            if l == "---" {
                break;
            }
            if let Some(v) = l.strip_prefix("persona:") {
                doc.name = v.trim().to_string();
            } else if let Some(v) = l.strip_prefix("status:") {
                doc.status = Some(v.trim().to_string());
            } else if let Some(v) = l.strip_prefix("confidence:") {
                doc.confidence = Some(v.trim().to_string());
            }
        }
    }
    // Body sections. `## Who they are` keeps every line; `## Jobs` keeps
    // one `### slug` row with its whole paragraph (the lines join with
    // spaces). The `###` check runs before any `#` skip, or the job slugs
    // never reach the Jobs arm.
    let mut section = String::new();
    for l in lines {
        let t = l.trim();
        if t.starts_with("## ") {
            section = t.trim_start_matches('#').trim().to_string();
            continue;
        }
        if t.is_empty() {
            continue;
        }
        match section.as_str() {
            "Jobs" if t.starts_with("### ") => {
                let slug = t.trim_start_matches('#').trim();
                doc.jobs.push((slug.to_string(), String::new()));
            }
            "Jobs" if !t.starts_with('#') => {
                if let Some(last) = doc.jobs.last_mut() {
                    if last.1.is_empty() {
                        last.1 = t.to_string();
                    } else {
                        last.1.push(' ');
                        last.1.push_str(t);
                    }
                }
            }
            "Who they are" if !t.starts_with('#') => doc.who.push(t.to_string()),
            _ => {}
        }
    }
    doc
}

/// Greedy word wrap at `width`; a word longer than the width takes its
/// own line whole, so no word is ever cut.
fn wrap_para(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let long = cur.chars().count() + 1 + word.chars().count() > width;
        if !cur.is_empty() && long {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn page_lines(doc: &PersonaDoc, path: &std::path::Path, width: usize) -> Vec<TLine> {
    let mut lines = Vec::new();
    lines.push(TLine::of(&[
        TSeg {
            text: "file: ".into(),
            role: TextStyle::Meta,
        },
        TSeg {
            text: path.display().to_string(),
            role: TextStyle::Hint,
        },
    ]));
    if let Some(s) = &doc.status {
        lines.push(TLine::of(&[
            TSeg {
                text: "status: ".into(),
                role: TextStyle::Meta,
            },
            TSeg {
                text: s.clone(),
                role: TextStyle::Item,
            },
        ]));
    }
    if let Some(c) = &doc.confidence {
        lines.push(TLine::of(&[
            TSeg {
                text: "confidence: ".into(),
                role: TextStyle::Meta,
            },
            TSeg {
                text: c.clone(),
                role: TextStyle::Item,
            },
        ]));
    }
    if !doc.who.is_empty() {
        lines.push(TLine::plain(String::new()));
        lines.push(TLine::line(TextStyle::Section, "who they are"));
        for w in &doc.who {
            for chunk in wrap_para(w, width.saturating_sub(2).max(8)) {
                lines.push(TLine::meta(format!("  {chunk}")));
            }
        }
    }
    if !doc.jobs.is_empty() {
        lines.push(TLine::plain(String::new()));
        lines.push(TLine::line(TextStyle::Section, "jobs"));
        for (slug, desc) in &doc.jobs {
            lines.push(TLine::line(TextStyle::Badge, format!("  {slug}")));
            for chunk in wrap_para(desc, width.saturating_sub(4).max(8)) {
                // The paragraph hangs indented under its slug, so the
                // slug's column stays the paragraph's left edge.
                lines.push(TLine::meta(format!("    {chunk}")));
            }
        }
    }
    lines
}

fn notice_lines(path: &std::path::Path) -> Vec<TLine> {
    vec![
        TLine::of(&[
            TSeg {
                text: "no persona file at ".into(),
                role: TextStyle::Item,
            },
            TSeg {
                text: path.display().to_string(),
                role: TextStyle::Value,
            },
        ]),
        TLine::meta("  the chip names a persona the profile folder does not hold"),
    ]
}

fn open_page(project: &str, persona: &str) -> PersonaOverlay {
    let path = persona_path(project, persona);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let doc = parse_persona(&text);
            let title = if doc.name.is_empty() {
                persona.to_string()
            } else {
                doc.name.clone()
            };
            let draft = is_draft(&doc.status);
            PersonaOverlay {
                title,
                lines: Vec::new(),
                cursor: None,
                personas: Vec::new(),
                project: project.to_string(),
                path,
                draft,
                slug: persona.to_string(),
                doc: Some(doc),
                scroll: 0,
                sel: None,
                notice: false,
            }
        }
        Err(_) => PersonaOverlay {
            title: persona.to_string(),
            lines: notice_lines(&path),
            cursor: None,
            personas: Vec::new(),
            project: project.to_string(),
            path,
            draft: false,
            slug: String::new(),
            doc: None,
            scroll: 0,
            sel: None,
            notice: true,
        },
    }
}

/// The persona page the bell's Personas tab opens on Enter.
pub(crate) fn open_page_for(project: &str, persona: &str) -> PersonaOverlay {
    open_page(project, persona)
}

/// The project's profile directory, for the bell's Personas tab.
pub(crate) fn customers_root_pub(project: &str) -> std::path::PathBuf {
    customers_root(project)
}

/// The project this client's persona reads resolve against: the open
/// board's rows carry the graph project; before a read lands, the
/// fallback matches the filing doors' default.
pub(crate) fn session_project(view: &View) -> String {
    view.backlog_board
        .as_ref()
        .and_then(|b| b.inputs.as_ref())
        .and_then(|i| i.rows.iter().find_map(|r| r.project.clone()))
        .unwrap_or_else(|| "fno".into())
}

/// One open record that names the persona in its audience.
#[derive(Debug, Clone)]
struct Serving {
    kind: ServeKind,
    id: String,
    label: String,
}

/// Where the record lives, so Enter opens the right surface.
#[derive(Debug, Clone, Copy)]
enum ServeKind {
    Node,
    Question,
    Decision,
}

impl ServeKind {
    fn word(self) -> &'static str {
        match self {
            ServeKind::Node => "node",
            ServeKind::Question => "question",
            ServeKind::Decision => "decision",
        }
    }
}

/// The open records whose audience names `slug`: the board's rows, the
/// questions fold and the decisions table, in that order. Reads the view
/// at call time, so a record opened elsewhere re-resolves each frame.
fn serving_for(view: &View, slug: &str) -> Vec<Serving> {
    let mut out = Vec::new();
    if let Some(rows) = view
        .backlog_board
        .as_ref()
        .and_then(|b| b.inputs.as_ref())
        .map(|i| &i.rows)
    {
        for r in rows {
            if personas_in(r.audience.as_deref()).iter().any(|p| p == slug) {
                out.push(Serving {
                    kind: ServeKind::Node,
                    id: r.id.clone().unwrap_or_default(),
                    label: r.title.clone().unwrap_or_default(),
                });
            }
        }
    }
    if let Some(qf) = view.questions_fold.as_ref() {
        for item in &qf.items {
            if personas_in(item.audience.as_deref())
                .iter()
                .any(|p| p == slug)
            {
                out.push(Serving {
                    kind: ServeKind::Question,
                    id: item.id.clone(),
                    label: item.title.clone(),
                });
            }
        }
    }
    for r in super::decisions_view::rows(view) {
        if personas_in(r.audience.as_deref()).iter().any(|p| p == slug) {
            out.push(Serving {
                kind: ServeKind::Decision,
                id: r.decision_id.clone(),
                label: r.subject.clone(),
            });
        }
    }
    out
}

/// The serving-now rows: a `> ` marker on the selected row.
fn serving_lines(serving: &[Serving], sel: Option<usize>) -> Vec<TLine> {
    if serving.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![
        TLine::plain(String::new()),
        TLine::line(TextStyle::Section, "serving now"),
    ];
    for (i, s) in serving.iter().enumerate() {
        lines.push(TLine::of(&[
            TSeg {
                text: if sel == Some(i) {
                    "> ".into()
                } else {
                    "  ".into()
                },
                role: TextStyle::Meta,
            },
            TSeg {
                text: format!("{}: ", s.kind.word()),
                role: TextStyle::Meta,
            },
            TSeg {
                text: s.label.clone(),
                role: TextStyle::Item,
            },
        ]));
    }
    lines
}

/// Enter on a selected serving row: the record opens, the persona page
/// closes, so the surface under it shows the record.
fn open_serving(view: &mut View, s: &Serving) {
    match s.kind {
        ServeKind::Node => {
            if let Some(b) = view.backlog_board.as_mut() {
                b.detail = Some(super::node_detail::NodeDetailOverlay {
                    node_id: s.id.clone(),
                    trail: Vec::new(),
                    sel: 0,
                    scroll: 0,
                });
            }
        }
        ServeKind::Question => view.open_detail_on(&s.id),
        ServeKind::Decision => {
            super::decisions_view::open(view);
            super::decisions_view::select_decision(view, &s.id);
        }
    }
}

impl PersonaOverlay {
    /// The status frontmatter awaits the user's approval.
    pub(crate) fn is_draft(&self) -> bool {
        self.draft
    }

    /// The chrome title, for the approve toast.
    pub(crate) fn title(&self) -> &str {
        &self.title
    }
}

/// How many persona files the project's profile directory holds, for the
/// bell tab count. A missing directory reads zero.
pub(crate) fn count_personas(project: &str) -> String {
    let n = std::fs::read_dir(customers_root(project))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
                .count()
        })
        .unwrap_or(0);
    n.to_string()
}

/// A draft persona: the status frontmatter awaits the user's approval.
fn is_draft(status: &Option<String>) -> bool {
    status
        .as_deref()
        .map(|s| {
            let s = s.to_ascii_lowercase();
            s.contains("draft") || s.contains("awaiting approval")
        })
        .unwrap_or(false)
}

/// Stamp the frontmatter status line approved with today's date, in
/// place, every other byte kept. `None` approved; `Some` names the
/// failure for the toast. The clock is the render clock, so the demo's
/// frozen epoch stamps a fixed date.
fn approve_in_place(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let secs = crate::ui::digest_overlay::now_secs();
    let date = chrono::DateTime::from_timestamp(secs as i64, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    let stamp = format!("status: approved {date}");
    let mut lines = text.lines();
    if !lines.next().is_some_and(|l| l.trim() == "---") {
        return Some("the file has no frontmatter".into());
    }
    let mut out = String::from("---\n");
    let mut stamped = false;
    let mut closed = false;
    for l in lines {
        if !closed && l.trim() == "---" {
            closed = true;
        } else if !closed && !stamped && l.trim().starts_with("status:") {
            out.push_str(&stamp);
            stamped = true;
            out.push('\n');
            continue;
        }
        out.push_str(l);
        out.push('\n');
    }
    if !stamped {
        return Some("the frontmatter holds no status line".into());
    }
    std::fs::write(path, out).ok()?;
    None
}

/// The picker's rows: a `> ` marker on the cursor row, the slug in the
/// accent slot. Rebuilt whole on every cursor move.
fn picker_lines(personas: &[String], cur: usize) -> Vec<TLine> {
    personas
        .iter()
        .enumerate()
        .map(|(i, p)| {
            TLine::of(&[
                TSeg {
                    text: if i == cur { "> ".into() } else { "  ".into() },
                    role: TextStyle::Meta,
                },
                TSeg {
                    text: p.clone(),
                    role: TextStyle::Badge,
                },
            ])
        })
        .collect()
}

fn open_list(title: &str, project: &str, personas: Vec<String>) -> PersonaOverlay {
    PersonaOverlay {
        title: title.to_string(),
        lines: picker_lines(&personas, 0),
        cursor: (!personas.is_empty()).then_some(0),
        personas,
        project: project.to_string(),
        path: std::path::PathBuf::new(),
        draft: false,
        slug: String::new(),
        doc: None,
        scroll: 0,
        sel: None,
        notice: false,
    }
}

/// The overlay a chip opens, if its audience names any persona: one persona
/// opens its page, several open the picker. `internal`, `customers` and the
/// unset `audience?` name no persona file and return `None`, so the caller
/// keeps its existing behavior.
pub(crate) fn open_from(
    audience: Option<&String>,
    project: Option<&str>,
) -> Option<PersonaOverlay> {
    let personas = personas_in(audience.map(|s| s.as_str()));
    let project = project.unwrap_or("fno");
    match personas.as_slice() {
        [] => None,
        [only] => Some(open_page(project, only)),
        // The picker carries the chip's own audience in its title, so a
        // shot shows which chip opened it.
        _ => Some(open_list(
            &format!("audience: {}", audience.map(|s| s.trim()).unwrap_or("")),
            project,
            personas,
        )),
    }
}

/// The overlay's own keys. A picker moves its cursor and drills in on
/// Enter. A notice closes on any key (the digest rule). A page scrolls
/// and selects with j/k (the serving rows when there are any, the body
/// lines when not), opens the selected serving record on Enter, takes `a`
/// on a draft for the approve-in-place stamp, and Esc, `q` and `x` close.
/// Infallible, so it returns the flow directly (the R6 ratchet, rust-style
/// R6, holds string errors shrink-only).
pub(crate) fn keys(view: &mut View, bytes: &[u8]) -> StdinFlow {
    if bytes.is_empty() {
        // A flush is not a key: leave the overlay up.
        return StdinFlow::Continue;
    }
    if view.persona.is_none() {
        return StdinFlow::Continue;
    }
    if view.persona.as_ref().is_some_and(|o| o.notice) {
        view.persona = None;
        return StdinFlow::Continue;
    }
    if view.persona.as_ref().is_some_and(|o| o.cursor.is_some()) {
        // The flow lands after the overlay borrow ends, or the &mut view
        // argument would race the borrow the call's second argument holds.
        let flow = apply_keys(
            view.persona
                .as_mut()
                .expect("the picker branch checked the cursor"),
            bytes,
        );
        return finish_flow(view, flow);
    }
    // A page. The serving read runs before the overlay borrow: it walks
    // the board, the questions fold and the decisions table, which share
    // the view with the persona field.
    let slug = view
        .persona
        .as_ref()
        .map(|o| o.slug.clone())
        .unwrap_or_default();
    let serving = serving_for(view, &slug);
    if let Some(i) = view
        .persona
        .as_ref()
        .and_then(|o| o.sel)
        .filter(|_| bytes.contains(&b'\r') || bytes.contains(&b'\n'))
    {
        if let Some(s) = serving.get(i).cloned() {
            view.persona = None;
            open_serving(view, &s);
            return StdinFlow::Continue;
        }
    }
    let Some(o) = view.persona.as_mut() else {
        return StdinFlow::Continue;
    };
    for &k in bytes {
        match k {
            0x1b | b'q' | b'x' => {
                view.persona = None;
                return StdinFlow::Continue;
            }
            b'j' => {
                if serving.is_empty() {
                    o.scroll += 1;
                } else {
                    let last = serving.len().saturating_sub(1);
                    o.sel = Some(o.sel.map_or(0, |s| (s + 1).min(last)));
                }
            }
            b'k' => {
                if serving.is_empty() {
                    o.scroll = o.scroll.saturating_sub(1);
                } else {
                    o.sel = Some(o.sel.map_or(0, |s| s.saturating_sub(1)));
                }
            }
            _ => {}
        }
    }
    if o.draft && bytes == b"a" {
        let path = o.path.clone();
        return finish_flow(view, Flow::Approve(path));
    }
    StdinFlow::Continue
}

/// Land one picker flow on the view: stay, close, drill in, or the
/// draft's approve stamp.
fn finish_flow(view: &mut View, flow: Flow) -> StdinFlow {
    match flow {
        Flow::Stay => StdinFlow::Continue,
        Flow::Close => {
            view.persona = None;
            StdinFlow::Continue
        }
        Flow::Drill(page) => {
            view.persona = Some(page);
            StdinFlow::Continue
        }
        Flow::Approve(path) => {
            // The mux keeps the user: `a` stamps the frontmatter approved
            // in place, the page re-renders un-drafted, and a toast
            // confirms. A failed stamp keeps the overlay up to retry.
            let project = view
                .persona
                .as_ref()
                .map(|o| o.project.clone())
                .unwrap_or_default();
            match approve_in_place(&path) {
                Some(fail) => view.set_notice(format!("persona approve failed: {fail}")),
                None => {
                    let slug = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let fresh = open_page(&project, &slug);
                    view.set_notice(format!("persona approved: {}", fresh.title));
                    view.persona = Some(fresh);
                }
            }
            StdinFlow::Continue
        }
    }
}

/// What one chunk of keys does to the open overlay.
#[derive(Debug)]
enum Flow {
    Stay,
    Close,
    Drill(PersonaOverlay),
    /// `a` on a draft persona: stamp the status line approved in place,
    /// the mux keeping the user (the open-out law).
    Approve(std::path::PathBuf),
}

/// The picker's keys against the overlay alone, so the contract is
/// testable without a `View`. A page's keys live in [`keys`]: j/k and
/// Enter read the view's serving rows, so they cannot run here.
fn apply_keys(o: &mut PersonaOverlay, bytes: &[u8]) -> Flow {
    let Some(start) = o.cursor else {
        // A page or notice: Esc, `q` and `x` close; nothing else acts.
        for &k in bytes {
            if k == 0x1b || k == b'q' || k == b'x' {
                return Flow::Close;
            }
        }
        return Flow::Stay;
    };
    let mut cur = start;
    for &k in bytes {
        match k {
            0x1b | b'q' => return Flow::Close,
            b'j' => cur = (cur + 1).min(o.personas.len().saturating_sub(1)),
            b'k' => cur = cur.saturating_sub(1),
            b'\r' | b'\n' => {
                return match o.personas.get(cur) {
                    Some(p) => Flow::Drill(open_page(&o.project, p)),
                    None => Flow::Stay,
                };
            }
            _ => {}
        }
    }
    o.cursor = Some(cur);
    o.lines = picker_lines(&o.personas, cur);
    Flow::Stay
}

/// The overlay a card's chip opens, if any.
pub(crate) fn open_from_card(card: &crate::backlog_model::Card) -> Option<PersonaOverlay> {
    open_from(card.audience.as_ref(), card.project.as_deref())
}

/// The compose-chain arm: draws the open persona overlay and answers
/// whether it painted.
pub(crate) fn draw_detail(
    view: &View,
    cells: &mut [Cell],
    (rows, cols): (usize, usize),
    content_origin: (usize, usize),
    content_dims: (usize, usize),
) -> bool {
    if view.persona.is_none() {
        return false;
    }
    // The overlay floats over the surface that opened it: the node detail
    // over the full board, the question page, or the decisions table.
    // Paint that host first, so the chip that opened the overlay shows
    // behind it; this arm sits ahead of the hosts in the draw chain.
    if view.backlog_board.is_some() {
        view.draw_board(cells, rows, cols);
    } else if view.decisions_board.is_some() {
        super::decisions_view::paint_full(view, cells, rows, cols);
    } else if view.question_detail.is_some() {
        super::questions::draw_detail(view, cells, (rows, cols), content_origin, content_dims);
    }
    paint_over(view, cells, rows, cols, content_origin, content_dims);
    true
}

/// The overlay alone, over an already-painted host. The question page's
/// draw arm sits ahead of this module in the compose chain, so it paints
/// its own surface and calls this after, the same chip-over-page stack a
/// live Enter leaves on screen.
pub(crate) fn paint_over(
    view: &View,
    cells: &mut [Cell],
    rows: usize,
    cols: usize,
    content_origin: (usize, usize),
    content_dims: (usize, usize),
) {
    let Some(o) = view.persona.as_ref() else {
        return;
    };
    let page = o.cursor.is_none() && !o.notice;
    let serving = if page {
        serving_for(view, &o.slug)
    } else {
        Vec::new()
    };
    // The chrome's esc chip is the one closer; the footer names the page
    // keys, so nothing is silent.
    let footer = overlay_footer(o, serving.is_empty());
    let chrome = chrome::Chrome::new(&o.title, Anchor::Center);
    let chrome = if footer.is_empty() {
        chrome
    } else {
        chrome.footer(&footer)
    };
    let cap = content_dims
        .1
        .saturating_sub(chrome::Chrome::FRAME_COLS)
        .max(12);
    let (lines, follow, paint_cursor) = if page {
        let mut l = o
            .doc
            .as_ref()
            .map(|d| page_lines(d, &o.path, cap.saturating_sub(4).min(108)))
            .unwrap_or_else(|| o.lines.clone());
        let serving_start = l.len();
        l.extend(serving_lines(&serving, o.sel));
        let total = l.len();
        let follow = match o.sel {
            // +2 skips the blank and section lines serving_lines leads
            // with, so the follow row is the selected record itself.
            Some(i) if i < serving.len() => Some(serving_start + i + 2),
            _ => Some(o.scroll.min(total.saturating_sub(1))),
        };
        (l, follow, o.sel.is_some())
    } else {
        (o.lines.clone(), None, false)
    };
    super::overlay_paint::draw_lines_overlay_follow(
        cells,
        rows,
        cols,
        content_origin,
        content_dims,
        &chrome,
        &lines,
        follow,
        paint_cursor,
        &view.theme,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The env-pinned tests share one process; the mutex keeps their
    /// FNO_CUSTOMERS_DIR windows from interleaving.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn pin_customers_dir(dir: &std::path::Path) {
        std::env::set_var("FNO_CUSTOMERS_DIR", dir);
    }

    fn unpin_customers_dir() {
        std::env::remove_var("FNO_CUSTOMERS_DIR");
    }

    fn fixture_doc(persona: &str) -> String {
        format!(
            "---\npersona: {persona}\nproject: fno\nstatus: hypothesis, untested\nconfidence: inferred, no interview yet\n---\n\n# {persona}\n\n## Who they are\nOne person building a product, with no engineering team.\n\n## Jobs\n\n### ship-first-feature\nShip the first feature tonight.\n\n### keep-spend-low\nKeep spend predictable.\n"
        )
    }

    #[test]
    fn personas_in_splits_the_customer_items_only() {
        assert_eq!(
            personas_in(Some("customer:a,customer:b")),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(personas_in(Some("internal")).is_empty());
        assert!(personas_in(Some("customers")).is_empty());
        assert!(personas_in(None).is_empty());
        assert!(personas_in(Some("audience?")).is_empty());
    }

    #[test]
    fn the_page_renders_the_frontmatter_who_and_jobs() {
        let doc = parse_persona(&fixture_doc("solo-founder"));
        assert_eq!(doc.name, "solo-founder");
        assert_eq!(doc.status.as_deref(), Some("hypothesis, untested"));
        assert_eq!(
            doc.who,
            vec!["One person building a product, with no engineering team.".to_string()]
        );
        assert_eq!(doc.jobs.len(), 2);
        assert_eq!(
            doc.jobs.first(),
            Some(&(
                "ship-first-feature".to_string(),
                "Ship the first feature tonight.".to_string()
            ))
        );
        let path = std::path::Path::new("/p/solo-founder.md");
        let text = page_lines(&doc, path, 96)
            .iter()
            .map(|l| l.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("status: hypothesis, untested"));
        assert!(text.contains("confidence: inferred, no interview yet"));
        assert!(text.contains("ship-first-feature"));
        assert!(text.contains("/p/solo-founder.md"));
    }

    #[test]
    fn a_missing_persona_file_names_its_path() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("persona-walk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        pin_customers_dir(&dir);
        let o = open_page("fno", "absent-persona");
        unpin_customers_dir();
        std::fs::remove_dir_all(&dir).ok();
        let text = o
            .lines
            .iter()
            .map(|l| l.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("no persona file at"));
        assert!(text.contains("absent-persona.md"));
    }

    #[test]
    fn approve_stamps_the_status_line_and_keeps_every_other_byte() {
        let dir = std::env::temp_dir().join(format!("persona-approve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("solo.md");
        std::fs::write(
            &path,
            "---\npersona: solo\nstatus: hypothesis, draft awaiting user approval\nconfidence: inferred\n---\n\n# Solo\n",
        )
        .unwrap();
        let stamped = approve_in_place(&path);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(stamped, None, "the stamp succeeds");
        assert!(
            text.contains("status: approved 20"),
            "the status line carries the date: {text}"
        );
        assert!(!is_draft(&Some(
            text.lines()
                .find(|l| l.starts_with("status:"))
                .map(|l| l["status:".len()..].trim().to_string())
                .unwrap_or_default()
        )));
        for keep in ["persona: solo", "confidence: inferred", "# Solo"] {
            assert!(text.contains(keep), "{keep} survives the stamp: {text}");
        }
        let bare = dir.join("bare.md");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&bare, "# no frontmatter\n").unwrap();
        let refused = approve_in_place(&bare);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(
            refused.as_deref(),
            Some("the file has no frontmatter"),
            "a file without frontmatter refuses"
        );
    }

    #[test]
    fn open_from_picks_page_picker_or_none() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("persona-walk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        pin_customers_dir(&dir);
        let single = open_from(Some(&"customer:solo".to_string()), Some("fno"));
        let multi = open_from(
            Some(&"customer:solo,customer:vibe".to_string()),
            Some("fno"),
        );
        unpin_customers_dir();
        std::fs::remove_dir_all(&dir).ok();
        assert!(single.is_some_and(|o| o.cursor.is_none()));
        let m = multi.unwrap();
        assert_eq!(m.cursor, Some(0));
        assert_eq!(m.personas, vec!["solo".to_string(), "vibe".to_string()]);
        assert!(open_from(Some(&"internal".to_string()), Some("fno")).is_none());
    }

    #[test]
    fn the_picker_moves_and_drills_into_a_real_page() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("persona-walk-{}", std::process::id()));
        let root = dir.join("fno");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("solo.md"), fixture_doc("solo")).unwrap();
        std::fs::write(root.join("vibe.md"), fixture_doc("vibe")).unwrap();
        pin_customers_dir(&dir);
        let mut o = open_list("fno personas", "fno", vec!["solo".into(), "vibe".into()]);
        assert!(matches!(apply_keys(&mut o, b"j"), Flow::Stay));
        assert_eq!(o.cursor, Some(1));
        match apply_keys(&mut o, b"\r") {
            Flow::Drill(page) => assert_eq!(page.title, "vibe"),
            other => panic!("expected a drill, got {other:?}"),
        }
        let mut page = open_page("fno", "solo");
        assert!(matches!(apply_keys(&mut page, b"x"), Flow::Close));
        unpin_customers_dir();
        std::fs::remove_dir_all(&dir).ok();
    }
    #[test]
    fn the_job_paragraph_wraps_with_a_hanging_indent() {
        let long = "Ship the first feature tonight. The demo cuts scope to one screen: the board, one pane, and the feed beside it. Anything past that waits for the first paying user.";
        let text = format!("---\npersona: solo\nstatus: hypothesis, untested\nconfidence: inferred\n---\n\n# solo\n\n## Who they are\nOne person building a product, with no engineering team.\n\n## Jobs\n\n### ship-first-feature\n{long}\n");
        let doc = parse_persona(&text);
        assert_eq!(doc.jobs.len(), 1);
        assert_eq!(doc.jobs.first().map(|j| j.1.as_str()), Some(long));
        let lines = page_lines(&doc, std::path::Path::new("/p/solo.md"), 40);
        let texts: Vec<String> = lines.iter().map(|l| l.text.clone()).collect();
        let job: String = texts
            .iter()
            .filter(|t| t.starts_with("    ") && !t.trim().is_empty())
            .map(|t| t.trim().to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(job.contains("Ship the first feature tonight."));
        assert!(job.contains("waits for the first paying user."));
        assert!(
            texts.iter().any(|t| t == "  ship-first-feature"),
            "the slug takes its own line: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|t| t == "  One person building a product, with"),
            "the who wraps at the narrow width: {texts:?}"
        );
    }

    #[test]
    fn a_page_without_serving_rows_scrolls_with_j_and_k() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("persona-scroll-{}", std::process::id()));
        let root = dir.join("fno");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("solo.md"), fixture_doc("solo")).unwrap();
        pin_customers_dir(&dir);
        let mut v = crate::client::tests::two_pane_view();
        v.persona = Some(open_page("fno", "solo"));
        assert!(v.persona.as_ref().is_some_and(|o| !o.notice));
        crate::client::persona_overlay::keys(&mut v, b"j");
        assert_eq!(v.persona.as_ref().unwrap().scroll, 1);
        crate::client::persona_overlay::keys(&mut v, b"j");
        assert_eq!(v.persona.as_ref().unwrap().scroll, 2);
        crate::client::persona_overlay::keys(&mut v, b"k");
        assert_eq!(v.persona.as_ref().unwrap().scroll, 1);
        crate::client::persona_overlay::keys(&mut v, b"x");
        assert!(v.persona.is_none());
        unpin_customers_dir();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn serving_rows_list_and_enter_dispatches_each_kind() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("persona-serve-{}", std::process::id()));
        let root = dir.join("fno");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("solo.md"), fixture_doc("solo")).unwrap();
        pin_customers_dir(&dir);
        // The boards are mutually exclusive full-surface views (opening
        // one closes the others), so each kind's read runs in its own view.
        let mut v = crate::client::tests::two_pane_view();
        let mut b = crate::client::backlog_board::BoardView::new(0);
        b.inputs = Some(crate::backlog_model::Inputs {
            rows: vec![crate::node_row::NodeRow::from_value(serde_json::json!({
                "id": "demo-1", "title": "Ship the persona page", "audience": "customer:solo"
            }))],
            ..Default::default()
        });
        v.backlog_board = Some(b);
        v.persona = Some(open_page("fno", "solo"));
        let entries = serving_for(&v, "solo");
        assert_eq!(entries.len(), 1);
        assert!(entries
            .first()
            .is_some_and(|e| matches!(e.kind, ServeKind::Node)));
        crate::client::persona_overlay::keys(&mut v, b"j");
        assert_eq!(v.persona.as_ref().unwrap().sel, Some(0));
        crate::client::persona_overlay::keys(&mut v, b"\r");
        assert!(v.persona.is_none());
        let detail = v.backlog_board.as_ref().unwrap().detail.as_ref().unwrap();
        assert_eq!(detail.node_id, "demo-1");
        let mut v2 = crate::client::tests::two_pane_view();
        v2.questions_fold = Some(crate::needs_overlay::QuestionsFold {
            items: vec![crate::needs_overlay::QuestionItem {
                id: "q-2".into(),
                title: "Second".into(),
                audience: Some("customer:solo".into()),
                ..Default::default()
            }],
            ..Default::default()
        });
        v2.persona = Some(open_page("fno", "solo"));
        assert_eq!(serving_for(&v2, "solo").len(), 1);
        crate::client::persona_overlay::keys(&mut v2, b"j");
        crate::client::persona_overlay::keys(&mut v2, b"\r");
        assert!(v2.question_detail.is_some());
        let mut v3 = crate::client::tests::two_pane_view();
        crate::client::decisions_view::open_rows(
            &mut v3,
            vec![crate::decisions_overlay::DecisionRow {
                decision_id: "d-2".into(),
                subject: "Alone".into(),
                audience: Some("customer:solo".into()),
                ..Default::default()
            }],
        );
        v3.persona = Some(open_page("fno", "solo"));
        assert_eq!(serving_for(&v3, "solo").len(), 1);
        crate::client::persona_overlay::keys(&mut v3, b"j");
        crate::client::persona_overlay::keys(&mut v3, b"\r");
        let picked = v3
            .decisions_board
            .as_ref()
            .map(|b| b.selected_row().map(|r| r.decision_id.clone()))
            .unwrap_or(None);
        assert_eq!(picked.as_deref(), Some("d-2"));
        assert!(v3.persona.is_none());
        unpin_customers_dir();
        std::fs::remove_dir_all(&dir).ok();
    }
}

/// The page's key hint: the same string the box footer carries and the
/// board's bottom strip shows while the overlay owns the keyboard.
fn overlay_footer(o: &PersonaOverlay, serving_empty: bool) -> String {
    match (o.cursor, o.notice, o.draft, serving_empty) {
        (Some(_), _, _, _) => "enter opens".into(),
        (_, true, _, _) => String::new(),
        (None, false, _, false) => "j/k move \u{b7} enter open \u{b7} esc close".into(),
        (None, false, true, true) => "a approve \u{b7} j/k scroll \u{b7} esc close".into(),
        (None, false, false, true) => "j/k scroll \u{b7} esc close".into(),
    }
}

/// The strip hint the board shows while the persona overlay owns the
/// keyboard: the overlay's own keys, never the board's.
pub(crate) fn strip_hint(view: &View) -> Option<String> {
    let o = view.persona.as_ref()?;
    let serving_empty = if o.cursor.is_none() && !o.notice {
        serving_for(view, &o.slug).is_empty()
    } else {
        true
    };
    let footer = overlay_footer(o, serving_empty);
    Some(if footer.is_empty() {
        "esc close".into()
    } else {
        footer
    })
}
