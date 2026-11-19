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

/// The surface whose rect scopes the docked paint. The dock paints the
/// host's own column and the persona beside it inside the home rect,
/// never a modal over the host.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Home {
    /// No scoped host: the page floats over the whole surface.
    Float,
    /// A mux pane hosts the surface the chip sits on; the dock lives
    /// inside that pane's rect.
    Pane(u64),
    /// The sideline backlog board panel.
    SidelineBoard,
    /// The question detail.
    Question,
    /// The decisions table.
    Decision,
    /// The bell's personas tab.
    Bell,
}

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
    /// The newest lines of notes.md beside the file, oldest first. The
    /// note composer appends; the page re-reads on every reload.
    notes: Vec<String>,
    /// The plain-scroll row a page without serving rows keeps visible.
    /// Unbounded here; the paint clamps to the body's real height.
    scroll: usize,
    /// The selected serving row, if any.
    sel: Option<usize>,
    /// A missing-file notice: short, so any key closes (the digest rule).
    notice: bool,
    /// The persona note composer, while open: the field edits, Enter
    /// saves, Esc stays on the page.
    input: Option<super::input_field::InputField>,
    /// The user edit's arm: the file mtime when `e` opened the editor
    /// pane. The frame tick polls it; a moved mtime reloads the page.
    pub(crate) edit_since: Option<std::time::SystemTime>,
    /// The edit `e` armed: the file the input loop opens in `$EDITOR`,
    /// in a mux pane beside this one. The loop drains it into one PaneRun.
    pub(crate) edit_spawn: Option<std::path::PathBuf>,
    /// The surface this page docks beside: the pane the drill or the `e`
    /// split came from, or the chip host a door recorded. `Float` keeps
    /// the full-surface paint.
    pub(crate) home: Home,
    /// The persona column's own pane: a placeholder split spawned right
    /// of the session panes when the sideline door opened, so the column
    /// tiles real space instead of painting over panes that cannot
    /// reflow under an overlay. The door close kills it; the argv is
    /// the orphan marker the server side can reap.
    pub(crate) pane: Option<u64>,
    /// The column still owes its width claim: the spawn (the persona
    /// column, the appended editor pane, or a record pane) landed, and
    /// the seams that shape the row fire once a layout naming the new
    /// pane arrives. One shot, then cleared.
    pub(crate) widen: bool,
    /// The editor pane `e` spawned: appended right of the session panes,
    /// never folded beside the page. The door close kills it.
    pub(crate) edit_pane: Option<u64>,
    /// The pane that still owes its width claim: the persona column, the
    /// editor pane, or the newest record pane, whichever appended last.
    pub(crate) claim_pane: Option<u64>,
    /// Records drilled past the first: each holds its own appended pane
    /// and the node it renders. The board detail keeps the first record.
    pub(crate) records: Vec<(u64, String)>,
    /// A serving record is drilled open right of the persona column.
    /// Left and Esc pop the record before the column itself.
    pub(crate) drill: bool,
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
/// validate it (`fno-agent` `audience::persona_file`, mirrored here: this
/// crate cannot call it). The D3440 layout is one PERSONA.md per persona
/// under its own directory; the flat `<slug>.md` is the fallback the
/// unmigrated records still read.
fn persona_path(project: &str, persona: &str) -> std::path::PathBuf {
    let root = customers_root(project);
    let nested = root.join(persona).join("PERSONA.md");
    if nested.is_file() {
        nested
    } else {
        root.join(format!("{persona}.md"))
    }
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
    /// The typed approval date (`approved:`), beside `status: approved`.
    approved: Option<String>,
    /// The Agent Skills shape's top-line description.
    description: Option<String>,
    confidence: Option<String>,
    who: Vec<String>,
    jobs: Vec<(String, String)>,
}

fn parse_persona(text: &str) -> PersonaDoc {
    let mut doc = PersonaDoc {
        name: String::new(),
        status: None,
        approved: None,
        description: None,
        confidence: None,
        who: Vec::new(),
        jobs: Vec::new(),
    };
    let mut lines = text.lines().peekable();
    // Frontmatter: the `key: value` block between the leading --- pair.
    if lines.peek().is_some_and(|l| l.trim() == "---") {
        lines.next();
        // The Agent Skills shape: name and description at the top, the
        // record's own keys indented under `metadata:`. The old flat keys
        // read too, and a metadata key wins over its flat twin.
        let mut in_meta = false;
        for l in lines.by_ref() {
            let t = l.trim();
            let indented = l.starts_with(' ') || l.starts_with('\t');
            if t == "---" {
                break;
            }
            if t.is_empty() {
                continue;
            }
            if indented {
                if !in_meta {
                    continue;
                }
                if let Some(v) = t.strip_prefix("status:") {
                    doc.status = Some(v.trim().to_string());
                } else if let Some(v) = t.strip_prefix("approved:") {
                    doc.approved = Some(v.trim().to_string());
                } else if let Some(v) = t.strip_prefix("confidence:") {
                    doc.confidence = Some(v.trim().to_string());
                }
                continue;
            }
            in_meta = t == "metadata:";
            if let Some(v) = t
                .strip_prefix("persona:")
                .or_else(|| t.strip_prefix("name:"))
            {
                doc.name = v.trim().to_string();
            } else if let Some(v) = t.strip_prefix("description:") {
                doc.description = Some(v.trim().to_string());
            } else if let Some(v) = t.strip_prefix("status:") {
                doc.status = Some(v.trim().to_string());
            } else if let Some(v) = t.strip_prefix("approved:") {
                doc.approved = Some(v.trim().to_string());
            } else if let Some(v) = t.strip_prefix("confidence:") {
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

/// The path as the user reads it: the home shortens to `~`, and a path
/// past `width` middle-truncates so both ends stay visible.
pub(crate) fn display_path(path: &std::path::Path, width: usize) -> String {
    let mut s = path.display().to_string();
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            if let Some(rest) = s.strip_prefix(&home) {
                s = format!("~{rest}");
            }
        }
    }
    let n = s.chars().count();
    if n > width.max(8) {
        let keep = (width.max(8).saturating_sub(1)) / 2;
        let head: String = s.chars().take(keep).collect();
        let tail: String = s
            .chars()
            .rev()
            .take(keep)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        s = format!("{head}\u{2026}{tail}");
    }
    s
}

/// The text cut at `width` chars with a trailing ellipsis. The cut lands
/// on a word boundary, carries no space before the ellipsis, and never
/// ends in a bare period, so a cut sentence reads as cut.
fn clip_ellipsis(text: &str, width: usize) -> String {
    let n = text.chars().count();
    if n <= width.max(1) {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.max(1).saturating_sub(1)).collect();
    if let Some(cut) = out.rfind(' ') {
        out.truncate(cut);
    }
    while out.ends_with('.') || out.ends_with(',') {
        out.pop();
    }
    out.push('\u{2026}');
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
            text: display_path(path, width.saturating_sub(6)),
            role: TextStyle::Hint,
        },
    ]));
    if let Some(d) = &doc.description {
        lines.push(TLine::of(&[
            TSeg {
                text: "description: ".into(),
                role: TextStyle::Meta,
            },
            TSeg {
                text: clip_ellipsis(d, width.saturating_sub(13)),
                role: TextStyle::Item,
            },
        ]));
    }
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
    if let Some(a) = &doc.approved {
        lines.push(TLine::of(&[
            TSeg {
                text: "approved: ".into(),
                role: TextStyle::Meta,
            },
            TSeg {
                text: a.clone(),
                role: TextStyle::Item,
            },
        ]));
    }
    if !doc.who.is_empty() {
        lines.push(TLine::plain(String::new()));
        lines.push(TLine::line(TextStyle::Section, "who they are"));
        // The paragraph reflows to the pane width: the source's own line
        // breaks join into one paragraph, so the wrap never goes ragged.
        let who = doc.who.join(" ");
        for chunk in wrap_para(&who, width.saturating_sub(2).max(8)) {
            lines.push(TLine::meta(format!("  {chunk}")));
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
    let notes = read_notes(&path);
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
                pane: None,
                widen: false,
                edit_pane: None,
                claim_pane: None,
                records: Vec::new(),
                title,
                lines: Vec::new(),
                cursor: None,
                personas: Vec::new(),
                project: project.to_string(),
                path,
                draft,
                slug: persona.to_string(),
                doc: Some(doc),
                notes,
                scroll: 0,
                sel: None,
                notice: false,
                input: None,
                edit_since: None,
                edit_spawn: None,
                home: Home::Float,
                drill: false,
            }
        }
        Err(_) => PersonaOverlay {
            pane: None,
            widen: false,
            edit_pane: None,
            claim_pane: None,
            records: Vec::new(),
            title: persona.to_string(),
            lines: notice_lines(&path),
            cursor: None,
            personas: Vec::new(),
            project: project.to_string(),
            path,
            draft: false,
            slug: String::new(),
            doc: None,
            notes,
            scroll: 0,
            sel: None,
            notice: true,
            input: None,
            edit_since: None,
            edit_spawn: None,
            home: Home::Float,
            drill: false,
        },
    }
}

/// The newest note lines beside the persona file, oldest first, the last
/// three kept so the section stays a page citizen as the file grows. A
/// missing notes.md reads empty.
fn read_notes(path: &std::path::Path) -> Vec<String> {
    let Some(notes) = path.parent() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(notes.join("notes.md")) else {
        return Vec::new();
    };
    let mut lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    if lines.len() > 3 {
        lines.drain(..lines.len() - 3);
    }
    lines
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
            // An open board keeps its rows (the persona opened from a
            // decisions row); a closed one takes the live read.
            if view.decisions_board.is_none() {
                super::decisions_view::open(view);
            }
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
                .filter(|e| is_persona_entry(&e.path()))
                .count()
        })
        .unwrap_or(0);
    n.to_string()
}

/// One persona record in the D3440 layout (a directory holding
/// PERSONA.md) or the flat fallback (a `.md` file).
fn is_persona_entry(path: &std::path::Path) -> bool {
    if path.is_file() {
        return path.extension().is_some_and(|x| x == "md");
    }
    path.join("PERSONA.md").is_file()
}

/// A draft persona: the typed frontmatter reads `status: draft`; the
/// old prose shapes (`draft`, `awaiting approval` anywhere in the line)
/// still count, so unmigrated records stay honest.
fn is_draft(status: &Option<String>) -> bool {
    status
        .as_deref()
        .map(|s| {
            let s = s.to_ascii_lowercase();
            s.contains("draft") || s.contains("awaiting approval")
        })
        .unwrap_or(false)
}

/// Stamp the frontmatter approved, in place, every other byte kept: a
/// typed `status: approved` line plus a separate `approved: <date>` line
/// (the typed shape; the one-field `approved <date>` prose stays
/// readable through `is_draft` and the page). `None` approved; `Some`
/// names the failure for the toast. The clock is the render clock, so
/// the demo's frozen epoch stamps a fixed date.
fn approve_in_place(path: &std::path::Path) -> Option<String> {
    stamp_metadata(
        path,
        &[("status", "approved"), ("approved", &today_stamp())],
    )
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
        pane: None,
        widen: false,
        edit_pane: None,
        claim_pane: None,
        records: Vec::new(),
        title: title.to_string(),
        lines: picker_lines(&personas, 0),
        cursor: (!personas.is_empty()).then_some(0),
        personas,
        project: project.to_string(),
        path: std::path::PathBuf::new(),
        draft: false,
        slug: String::new(),
        doc: None,
        notes: Vec::new(),
        scroll: 0,
        sel: None,
        notice: false,
        input: None,
        edit_since: None,
        edit_spawn: None,
        home: Home::Float,
        drill: false,
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
        close_persona_pane(view);
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
    // Left: the drill pops the rightmost column (the persona page) and
    // its parent takes the keyboard back. The composer and picker own
    // their own keys, so this sits after the picker branch and only on a
    // page without an open composer. A lone Esc already pops through
    // keys_page; the chunk for Left arrives whole.
    if view
        .persona
        .as_ref()
        .is_some_and(|o| o.input.is_none() && o.cursor.is_none())
        && bytes == [0x1b, 0x5b, 0x44]
    {
        // Left pops the rightmost pane: an appended record pane
        // first, then the drilled record, then the persona column.
        if pop_record_pane(view) {
            return StdinFlow::Continue;
        }
        if view.persona.as_ref().is_some_and(|o| o.drill) {
            if let Some(o) = view.persona.as_mut() {
                o.drill = false;
            }
            if let Some(b) = view.backlog_board.as_mut() {
                b.detail = None;
            }
            return StdinFlow::Continue;
        }
        close_persona_pane(view);
        view.persona = None;
        return StdinFlow::Continue;
    }
    let slug = view
        .persona
        .as_ref()
        .map(|o| o.slug.clone())
        .unwrap_or_default();
    let serving = serving_for(view, &slug);
    // The composer owns Enter while it is open: it saves the note, it
    // never dispatches the selected serving row.
    let composing = view.persona.as_ref().is_some_and(|o| o.input.is_some());
    if let Some(i) = view
        .persona
        .as_ref()
        .and_then(|o| o.sel)
        .filter(|_| !composing && (bytes.contains(&b'\r') || bytes.contains(&b'\n')))
    {
        if let Some(s) = serving.get(i).cloned() {
            // The node record drills: the persona column stays and the
            // record opens in the board's detail column beside it. The
            // question and decision surfaces are their own families; they
            // keep today's handoff.
            // No board under the surface (the persona opened from a
            // decisions row or a question): the record has no detail
            // column to drill into, so the page hands off the way it did
            // before the drill existed.
            let mut append: Option<String> = None;
            if !matches!(s.kind, ServeKind::Node) || view.backlog_board.is_none() {
                close_persona_pane(view);
                view.persona = None;
                open_serving(view, &s);
            } else {
                // A drill past the first appends: the record takes its
                // own pane right of the row and the earlier columns stay.
                let drilled = view.persona.as_ref().is_some_and(|o| o.drill);
                if drilled {
                    append = Some(s.id.clone());
                } else {
                    if let Some(o) = view.persona.as_mut() {
                        o.drill = true;
                    }
                    open_serving(view, &s);
                }
            }
            if let Some(node) = append {
                spawn_record_pane(view, &node);
            }
            return StdinFlow::Continue;
        }
    }
    keys_page(view, bytes, &serving)
}

/// The page's own keys: the note composer owns them while it is open;
/// otherwise j/k scroll and select, `e` arms the editor-pane spawn, `n`
/// opens the composer, `a` approves a draft, Esc, `q` and `x` close.
fn keys_page(view: &mut View, bytes: &[u8], serving: &[Serving]) -> StdinFlow {
    if view.persona.as_ref().is_some_and(|o| o.input.is_some()) {
        // The note composer owns the keys while open: the field edits,
        // Enter saves, Esc stays on the page.
        return note_input_keys(view, bytes);
    }
    let Some(o) = view.persona.as_mut() else {
        return StdinFlow::Continue;
    };
    // The spawn lands after the overlay borrow ends: the edit arm records
    // the arm time and hands the file to the loop's PaneRun drain.
    let mut spawn: Option<std::path::PathBuf> = None;
    for &k in bytes {
        match k {
            0x1b => {
                // Esc pops the rightmost pane: an appended record pane
                // first, then the drilled record, then the persona
                // column.
                if pop_record_pane(view) {
                    return StdinFlow::Continue;
                }
                if view.persona.as_ref().is_some_and(|o| o.drill) {
                    if let Some(o) = view.persona.as_mut() {
                        o.drill = false;
                    }
                    if let Some(b) = view.backlog_board.as_mut() {
                        b.detail = None;
                    }
                    return StdinFlow::Continue;
                }
                close_persona_pane(view);
                view.persona = None;
                return StdinFlow::Continue;
            }
            b'q' | b'x' => {
                close_persona_pane(view);
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
            b'e' => {
                // The user's editor opens in a mux pane beside this one;
                // the frame tick polls the file and reloads on save.
                if o.input.is_none() && o.path.is_file() {
                    // The focused pane is the surface's home: after the
                    // split it keeps the page, and the editor composites
                    // beside it instead of under it.
                    o.home = Home::Pane(view.layout.focus);
                    o.edit_since = std::fs::metadata(&o.path).and_then(|m| m.modified()).ok();
                    spawn = Some(o.path.clone());
                }
            }
            b'n' => {
                if o.input.is_none() {
                    o.input = Some(super::input_field::InputField::new(
                        "note for the persona",
                        500,
                    ));
                }
            }
            _ => {}
        }
    }
    if o.draft && bytes == b"a" {
        let path = o.path.clone();
        return finish_flow(view, Flow::Approve(path));
    }
    if let Some(path) = spawn {
        o.edit_spawn = Some(path);
    }
    StdinFlow::Continue
}

/// The note composer's keys while the field is open: the field edits,
/// Enter saves the note to notes.md beside the persona file and stamps
/// the status draft (the note asks for a revision), Esc stays on the page.
fn note_input_keys(view: &mut View, bytes: &[u8]) -> StdinFlow {
    enum NoteAct {
        None,
        Save {
            path: std::path::PathBuf,
            slug: String,
            project: String,
            text: String,
        },
    }
    let mut act = NoteAct::None;
    {
        let Some(o) = view.persona.as_mut() else {
            return StdinFlow::Continue;
        };
        let Some(field) = o.input.as_mut() else {
            return StdinFlow::Continue;
        };
        let mut close = false;
        let mut save: Option<String> = None;
        for event in field.feed(bytes) {
            match event {
                super::input_field::FieldEvent::Cancel => close = true,
                super::input_field::FieldEvent::Submit(text) => {
                    close = true;
                    if !text.is_empty() {
                        save = Some(text);
                    }
                }
                super::input_field::FieldEvent::Tab => {}
                super::input_field::FieldEvent::Up | super::input_field::FieldEvent::Down => {}
            }
        }
        if close {
            o.input = None;
        }
        if let Some(text) = save {
            act = NoteAct::Save {
                path: o.path.clone(),
                slug: o.slug.clone(),
                project: o.project.clone(),
                text,
            };
        }
    }
    if let NoteAct::Save {
        path,
        slug,
        project,
        text,
    } = act
    {
        match save_note(&path, &slug, &text) {
            Some(fail) => view.set_notice(format!("persona note failed: {fail}")),
            None => {
                let fresh = view
                    .persona
                    .as_ref()
                    .map(|o| dock_carry(o, open_page(&project, &slug)));
                let Some(fresh) = fresh else {
                    return StdinFlow::Continue;
                };
                // The draft claim follows the file's own status, not the
                // save: an approved persona stays approved after a note.
                let msg = if fresh.is_draft() {
                    "note saved; the persona is draft for revision"
                } else {
                    "note saved"
                };
                view.set_notice(msg.into());
                view.persona = Some(fresh);
            }
        }
    }
    StdinFlow::Continue
}

/// Append one note line to `notes.md` beside the persona file, then
/// stamp the frontmatter status draft in place, every other byte kept.
/// `None` saved; `Some` names the failure for the toast.
fn save_note(path: &std::path::Path, slug: &str, text: &str) -> Option<String> {
    let notes = path.parent()?.join("notes.md");
    let date = today_stamp();
    let line = format!("- {date} {slug}: {text}\n");
    use std::io::Write as _;
    let wrote = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&notes)
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .is_ok();
    if !wrote {
        return Some("notes.md would not take the note".into());
    }
    stamp_metadata(path, &[("status", "draft")])
}

/// The render-clock date, the stamp `a` uses (the demo's frozen epoch
/// stamps a fixed date).
fn today_stamp() -> String {
    let secs = crate::ui::digest_overlay::now_secs();
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// Write `key: value` rows into the frontmatter's `metadata:` block, in
/// place: a matching indented row is replaced, missing rows insert right
/// after the `metadata:` line, and a file with no metadata block grows
/// one before the closing fence. Every other byte is kept. `None`
/// stamped; `Some` names the failure.
fn stamp_metadata(path: &std::path::Path, rows: &[(&str, &str)]) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    if !lines.next().is_some_and(|l| l.trim() == "---") {
        return Some("the file has no frontmatter".into());
    }
    let mut out = String::from("---\n");
    let mut left: Vec<(String, String)> = rows
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let mut in_meta = false;
    let mut closed = false;
    for l in lines {
        let t = l.trim();
        let indented = l.starts_with(' ') || l.starts_with('\t');
        if !closed && !indented && t == "---" {
            if !in_meta && !left.is_empty() {
                out.push_str("metadata:\n");
                for (k, v) in &left {
                    out.push_str(&format!("  {k}: {v}\n"));
                }
            }
            left.clear();
            closed = true;
        } else if !closed && !indented && t == "metadata:" {
            in_meta = true;
            out.push_str(l);
            out.push('\n');
            for (k, v) in &left {
                out.push_str(&format!("  {k}: {v}\n"));
            }
            left.clear();
            continue;
        } else if !closed && in_meta && indented {
            let key = t.split(':').next().unwrap_or("").trim().to_string();
            if let Some(pair) = left.iter().find(|(k, _)| *k == key) {
                out.push_str(&format!("  {}: {}\n", pair.0, pair.1));
                let key = pair.0.clone();
                left.retain(|(k, _)| *k != key);
                continue;
            }
        }
        out.push_str(l);
        out.push('\n');
    }
    if !closed {
        return Some("the file has no frontmatter".into());
    }
    std::fs::write(path, out).ok()?;
    None
}

/// The armed edit's frame-tick poll: when the persona file's mtime moved
/// past the arm time, re-read the page in place (scroll and serving
/// selection kept) and disarm. A save the editor has not flushed yet
/// keeps the arm waiting.
pub(crate) fn edit_poll(view: &mut View) {
    let armed = view.persona.as_ref().and_then(|o| o.edit_since);
    let Some(since) = armed else {
        return;
    };
    let path = view
        .persona
        .as_ref()
        .map(|o| o.path.clone())
        .unwrap_or_default();
    let Ok(now) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
        return;
    };
    if now == since {
        return;
    }
    let (project, slug, scroll, sel) = view
        .persona
        .as_ref()
        .map(|o| (o.project.clone(), o.slug.clone(), o.scroll, o.sel))
        .unwrap_or_default();
    let fresh = view
        .persona
        .as_ref()
        .map(|o| dock_carry(o, open_page(&project, &slug)));
    let Some(mut fresh) = fresh else {
        return;
    };
    fresh.scroll = scroll;
    fresh.sel = sel;
    view.persona = Some(fresh);
}

/// The armed edit's file, taken: the loop's drain and the demo frames
/// share the one consumption.
pub(crate) fn take_edit_spawn(view: &mut View) -> Option<std::path::PathBuf> {
    let taken = view.persona.as_ref().and_then(|o| o.edit_spawn.clone());
    if taken.is_some() {
        if let Some(o) = view.persona.as_mut() {
            o.edit_spawn = None;
        }
    }
    taken
}

/// The staged page's file, slug and project, for the demo frames.
pub(crate) fn persona_page_path(view: &View) -> Option<(std::path::PathBuf, String, String)> {
    view.persona.as_ref().and_then(|o| {
        (!o.slug.is_empty()).then(|| (o.path.clone(), o.slug.clone(), o.project.clone()))
    })
}

/// The user's `$EDITOR` (or `VISUAL`) as argv words; `vi` when neither
/// names a program.
fn editor_argv() -> Vec<String> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let mut argv: Vec<String> = editor.split_whitespace().map(str::to_string).collect();
    if argv.is_empty() {
        argv.push("vi".into());
    }
    argv
}

/// Drain one armed edit: open the persona file in the user's editor, in
/// a mux pane split beside this one (the mux keeps the user), and focus
/// it. The frame tick polls the file; the page reloads on save. A failed
/// spawn disarms the poll and says so.
pub(crate) async fn spawn_edit(
    view: &mut View,
    path: std::path::PathBuf,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) {
    const CONTROL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    let mut argv = editor_argv();
    argv.push(path.display().to_string());
    // The cwd names the squad the pane joins, so the persona dir would
    // drop the editor into a squad the user is not looking at. The
    // session's own cwd keeps the pane beside the page; argv carries the
    // file's absolute path.
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".into());
    // The editor appends as the next column right of the row's
    // rightmost pane, the way the persona column does: a drill never
    // folds the page beside a docked editor. Exact placement: a refused
    // split surfaces as a notice below, never a silent spawn in a fresh
    // tab.
    let min_y = view.layout.panes.iter().map(|(_, r)| r.y).min();
    let anchor = view
        .layout
        .panes
        .iter()
        .filter(|(_, r)| Some(r.y) == min_y)
        .max_by_key(|(_, r)| r.x + r.cols)
        .map(|(pid, _)| *pid);
    let Some(anchor_id) = anchor else {
        disarm_edit(view);
        view.set_notice("the editor pane never opened: no pane to append beside".into());
        return;
    };
    let placement = crate::proto::PanePlacement {
        split: Some(crate::proto::Dir::Right),
        at: Some(anchor_id),
        fallback: crate::proto::PlacementFallback::Refuse,
        ..Default::default()
    };
    let command = crate::proto::ControlCommand::PaneRun {
        cwd,
        argv,
        cols: None,
        rows: None,
        claim: false,
        placement,
        worker: None,
        fno_id: None,
    };
    let session = view.session.clone();
    let sock = match crate::proto::socket_path(&session) {
        Ok(s) => s,
        Err(e) => {
            disarm_edit(view);
            view.set_notice(format!("the editor pane never opened: {e}"));
            return;
        }
    };
    let reply = tokio::task::spawn_blocking(move || {
        crate::mux_cli::control_roundtrip_with_timeouts(
            &sock,
            &session,
            command,
            CONTROL_TIMEOUT,
            CONTROL_TIMEOUT,
        )
    })
    .await
    .map_err(|e| e.to_string());
    match reply {
        Ok(Ok(crate::proto::ServerMsg::PaneSpawned { pane_id, .. })) => {
            view.note_command_sent(&crate::proto::Command::FocusPane(pane_id));
            if let Err(e) = write_msg(
                sock_w,
                &ClientMsg::Command(crate::proto::Command::FocusPane(pane_id)),
            )
            .await
            {
                disarm_edit(view);
                view.set_notice(format!("the editor pane focus failed: {e}"));
            }
            // The width claim fires on the layout that names the
            // editor pane: the appended column claims its widest share
            // then.
            if let Some(o) = view.persona.as_mut() {
                o.edit_pane = Some(pane_id);
                o.claim_pane = Some(pane_id);
                o.widen = true;
            }
        }
        Ok(Ok(_)) => {
            disarm_edit(view);
            view.set_notice("the editor pane never opened".into());
        }
        Ok(Err(e)) => {
            disarm_edit(view);
            view.set_notice(format!("the editor pane never opened: {e}"));
        }
        Err(e) => {
            disarm_edit(view);
            view.set_notice(format!("the editor pane never opened: {e}"));
        }
    }
}

/// Take the edit arm back: the poll stops waking the loop.
fn disarm_edit(view: &mut View) {
    if let Some(o) = view.persona.as_mut() {
        o.edit_since = None;
    }
}

/// Land one picker flow on the view: stay, close, drill in, or the
/// draft's approve stamp.
fn finish_flow(view: &mut View, flow: Flow) -> StdinFlow {
    match flow {
        Flow::Stay => StdinFlow::Continue,
        Flow::Close => {
            close_persona_pane(view);
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
                    // The nested layout's file stem is PERSONA, never the
                    // slug; the overlay's own slug is the honest name.
                    let mut slug = view
                        .persona
                        .as_ref()
                        .map(|o| o.slug.clone())
                        .unwrap_or_default();
                    if slug.is_empty() {
                        slug = path
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default();
                    }
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
                    // The drill keeps the dock: the page opens in the
                    // picker's own home column.
                    Some(p) => Flow::Drill(dock_carry(o, open_page(&o.project, p))),
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
/// Dock maps for the chip doors: each records the surface the chip sat
/// on, so the page paints beside it instead of over it.
/// A fresh page from an internal navigation (the picker drill) keeps
/// the overlay's home, so the dock never floats mid-walk.
pub(crate) fn dock_carry(o: &PersonaOverlay, mut fresh: PersonaOverlay) -> PersonaOverlay {
    fresh.home = o.home;
    fresh
}

pub(crate) fn dock_sideline(mut o: PersonaOverlay) -> PersonaOverlay {
    o.home = Home::SidelineBoard;
    o
}

/// The persona column's pane: a placeholder split spawned right of the
/// session panes, so the column tiles real space instead of painting
/// over panes that cannot reflow under an overlay. The argv is the
/// orphan marker: a pane whose pty still runs it after its overlay
/// closed is a reap candidate for the server side. A failed spawn
/// leaves the dock on the panel fallback, never a half-open door.
pub(crate) fn spawn_persona_pane(view: &mut View) {
    let docked = view
        .persona
        .as_ref()
        .is_some_and(|o| o.home == Home::SidelineBoard && o.pane.is_none());
    if !docked {
        return;
    }
    // The split anchors on the row's rightmost pane, so the column opens
    // as the new rightmost pane and its seam share decides its width. A
    // focus-relative anchor would land the column mid-row wherever the
    // keyboard happened to sit. The rect read here is pre-split: the
    // layout still carries the anchor's full width, which is exactly the
    // width the seam math needs.
    // The split anchors on the top band's rightmost pane, so the
    // column opens top-right beside the widest row instead of a bottom
    // corner its band cannot widen out of.
    let min_y = view.layout.panes.iter().map(|(_, r)| r.y).min();
    let anchor = view
        .layout
        .panes
        .iter()
        .filter(|(_, r)| Some(r.y) == min_y)
        .max_by_key(|(_, r)| r.x + r.cols)
        .map(|(pid, _)| *pid);
    let Some(anchor_id) = anchor else {
        return;
    };
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".into());
    let placement = crate::proto::PanePlacement {
        split: Some(crate::proto::Dir::Right),
        at: Some(anchor_id),
        // Exact placement: a refused split surfaces as a notice below,
        // never a silent spawn in a fresh tab.
        fallback: crate::proto::PlacementFallback::Refuse,
        ..Default::default()
    };
    let command = crate::proto::ControlCommand::PaneRun {
        cwd,
        // The orphan marker: a sleep placeholder the server side can
        // recognise by argv. 2^31-1 seconds is the largest interval the
        // BSD sleep accepts, so the placeholder actually runs instead of
        // dying at spawn and reading as a pane the operator closed.
        argv: vec!["sleep".into(), "2147483647".into()],
        cols: None,
        rows: None,
        claim: false,
        placement,
        worker: None,
        fno_id: None,
    };
    let session = view.session.clone();
    let sock = match crate::proto::socket_path(&session) {
        Ok(s) => s,
        Err(e) => {
            view.set_notice(format!("the persona pane never opened: {e}"));
            return;
        }
    };
    let reply = crate::mux_cli::control_roundtrip(&sock, &session, command);
    let pane_id = match reply {
        Ok(crate::proto::ServerMsg::PaneSpawned { pane_id, .. }) => pane_id,
        Ok(other) => {
            view.set_notice(format!("the persona pane never opened: {other:?}"));
            return;
        }
        Err(e) => {
            view.set_notice(format!("the persona pane never opened: {e}"));
            return;
        }
    };
    if let Some(o) = view.persona.as_mut() {
        o.pane = Some(pane_id);
        o.claim_pane = Some(pane_id);
        // The width claim fires on the layout that names the new pane:
        // the seams read post-split rects, which this client only sees
        // after the server's next Layout push.
        o.widen = true;
    }
}

/// The seams that shape the row once the persona column exists: the
/// column claims the widest share of its band, walking leftward with
/// seam moves whose floors keep every neighbour readable. The editor
/// fold claims by shrinking the editor pane to the dock's persona
/// share. Cleared as soon as it answers.
pub(crate) async fn send_widening_seams<W>(sock_w: &mut W, view: &mut View) -> Option<String>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let seams = persona_widening_seams(view);
    if seams.is_empty() {
        return None;
    }
    for cmd in seams {
        if let Err(e) = write_msg(sock_w, &ClientMsg::Command(cmd)).await {
            return Some(format!("persona seam send failed: {e}"));
        }
    }
    None
}

pub(crate) fn persona_widening_seams(view: &mut View) -> Vec<crate::proto::Command> {
    let armed = view.persona.as_ref().is_some_and(|o| o.widen);
    if !armed {
        return Vec::new();
    }
    let Some(o) = view.persona.as_ref() else {
        return Vec::new();
    };
    const FLOOR: usize = 22;
    let mut seams = Vec::new();
    let claim = o.claim_pane.or(o.pane);
    if let Some(pane) = claim {
        let rects: Vec<(u64, (usize, usize, usize, usize))> = view
            .layout
            .panes
            .iter()
            .map(|(pid, r)| {
                (
                    *pid,
                    (r.x as usize, r.y as usize, r.cols as usize, r.rows as usize),
                )
            })
            .collect();
        let Some(&(px, py, pw, ph)) = rects.iter().find(|(pid, _)| *pid == pane).map(|(_, r)| r)
        else {
            return Vec::new();
        };
        // The row's own widest pane sets the bar: the feature column
        // claims at least that share, one neighbour at a time.
        let target = rects
            .iter()
            .filter(|(pid, _)| *pid != pane)
            .filter(|(_, (_, y, _, h))| *y < py + ph && py < y + h)
            .map(|(_, (_, _, w, _))| *w)
            .max()
            .map(|w| w + 1);
        let mut persona = (px, pw);
        for _ in 0..3 {
            let Some(target) = target else { break };
            if persona.1 >= target {
                break;
            }
            // The immediate left neighbour in the band, flanking one seam.
            let nbr = rects.iter().find(|(pid, r)| {
                *pid != pane && {
                    let (x, y, w, h) = *r;
                    y < py + ph && py < y + h && x + w + 1 == persona.0
                }
            });
            let Some((nid, (nx, _, nw, _))) = nbr else {
                break;
            };
            let need = target - persona.1;
            let donate = need.min(nw.saturating_sub(FLOOR));
            if donate == 0 {
                break;
            }
            seams.push(crate::proto::Command::ResizeSeam {
                a: *nid,
                b: pane,
                pos: (nx + nw - donate) as u16,
            });
            persona.0 -= donate;
            persona.1 += donate;
        }
    }
    // One shot: the claim answers once, from the layout that names it.
    if let Some(o) = view.persona.as_mut() {
        o.widen = false;
    }
    seams
}

/// A record drilled past the first appends its own pane right of the
/// row's rightmost pane, the same split the persona column takes. The
/// pane is a sleep placeholder; the client paints the record lines into
/// its rect, so the column tiles real space and the seam claim can
/// reshape the row.
fn spawn_record_pane(view: &mut View, node_id: &str) {
    let docked = view.persona.as_ref().is_some_and(|o| o.pane.is_some());
    if !docked {
        return;
    }
    let min_y = view.layout.panes.iter().map(|(_, r)| r.y).min();
    let anchor = view
        .layout
        .panes
        .iter()
        .filter(|(_, r)| Some(r.y) == min_y)
        .max_by_key(|(_, r)| r.x + r.cols)
        .map(|(pid, _)| *pid);
    let Some(anchor_id) = anchor else {
        return;
    };
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".into());
    let placement = crate::proto::PanePlacement {
        split: Some(crate::proto::Dir::Right),
        at: Some(anchor_id),
        fallback: crate::proto::PlacementFallback::Refuse,
        ..Default::default()
    };
    let command = crate::proto::ControlCommand::PaneRun {
        cwd,
        argv: vec!["sleep".into(), "2147483647".into()],
        cols: None,
        rows: None,
        claim: false,
        placement,
        worker: None,
        fno_id: None,
    };
    let session = view.session.clone();
    let Ok(sock) = crate::proto::socket_path(&session) else {
        return;
    };
    let reply = crate::mux_cli::control_roundtrip(&sock, &session, command);
    let Ok(crate::proto::ServerMsg::PaneSpawned { pane_id, .. }) = reply else {
        return;
    };
    if let Some(o) = view.persona.as_mut() {
        o.records.push((pane_id, node_id.to_string()));
        o.claim_pane = Some(pane_id);
        o.widen = true;
    }
}

/// Pop the newest appended record pane, if any: kill its placeholder and
/// drop the record. True when a pane popped, so the key is consumed.
fn pop_record_pane(view: &mut View) -> bool {
    let popped = view.persona.as_mut().and_then(|o| o.records.pop());
    let Some((pane, _)) = popped else {
        return false;
    };
    if let Some(o) = view.persona.as_mut() {
        if o.claim_pane == Some(pane) {
            o.claim_pane = None;
        }
    }
    let session = view.session.clone();
    if let Ok(sock) = crate::proto::socket_path(&session) {
        let reply = crate::mux_cli::control_roundtrip(
            &sock,
            &session,
            crate::proto::ControlCommand::PaneKill {
                pane,
                hand_off_to: None,
            },
        );
        if let Err(e) = reply {
            view.set_notice(format!("the record pane lingers: {e}"));
        }
    }
    true
}

/// The appended record panes: each paints the node's detail lines into
/// its own rect, the same panel the first drilled record uses.
fn paint_record_panes(view: &View, cells: &mut [Cell], rows: usize, cols: usize) {
    let Some(o) = view.persona.as_ref() else {
        return;
    };
    if o.records.is_empty() {
        return;
    }
    let Some(b) = view.backlog_board.as_ref() else {
        return;
    };
    for (pane, node_id) in &o.records {
        let Some((_, r)) = view.layout.panes.iter().find(|(pid, _)| *pid == *pane) else {
            continue;
        };
        let top = (super::TAB_BAR_ROWS as usize + r.y as usize).min(rows);
        let left = (view.left_chrome_w() as usize + r.x as usize).min(cols);
        let h = (r.rows as usize).min(rows.saturating_sub(top));
        let w = (r.cols as usize).min(cols.saturating_sub(left));
        if h == 0 || w == 0 {
            continue;
        }
        for rr in top..top + h {
            let base = rr * cols + left;
            if let Some(span) = cells.get_mut(base..base + w) {
                span.fill(Cell::default());
            }
        }
        backlog_style::paint_title_row(
            cells,
            rows,
            cols,
            top,
            left,
            w,
            &format!("details \u{b7} {node_id}"),
            false,
            &view.theme,
        );
        if h <= 1 {
            continue;
        }
        let (lines, _) = super::node_detail::pane_lines(b, node_id, None, w);
        backlog_style::paint_panel_at(
            cells,
            rows,
            cols,
            left,
            top + 1,
            w,
            h - 1,
            &lines,
            None,
            &view.theme,
        );
    }
}

/// The door closes: the persona column's pane dies with it, or a
/// sleep placeholder lingers in the session until the server-side
/// argv reap finds it. The kill runs on the control socket by id, so
/// it never steals focus.
pub(crate) fn close_persona_pane(view: &mut View) {
    let Some(o) = view.persona.as_ref() else {
        return;
    };
    let mut panes = Vec::new();
    if let Some(p) = o.pane {
        panes.push(p);
    }
    if let Some(p) = o.edit_pane {
        panes.push(p);
    }
    panes.extend(o.records.iter().map(|(p, _)| *p));
    if panes.is_empty() {
        return;
    }
    let session = view.session.clone();
    if let Ok(sock) = crate::proto::socket_path(&session) {
        for pane in panes {
            let reply = crate::mux_cli::control_roundtrip(
                &sock,
                &session,
                crate::proto::ControlCommand::PaneKill {
                    pane,
                    hand_off_to: None,
                },
            );
            // A refused kill leaves the sleep placeholder running in the
            // session, so the failure names itself instead of vanishing.
            if let Err(e) = reply {
                view.set_notice(format!("the persona pane lingers: {e}"));
            }
        }
    }
}

pub(crate) fn dock_pane(view: &View, mut o: PersonaOverlay) -> PersonaOverlay {
    o.home = Home::Pane(view.layout.focus);
    o
}

/// The bell door: the personas tab hosts the dock.
pub(crate) fn open_docked_bell(project: &str, persona: &str) -> PersonaOverlay {
    let mut o = open_page_for(project, persona);
    o.home = Home::Bell;
    o
}

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
    // The drill split gives the surface a home pane. With a sibling pane
    // beside it (the editor), the client-local hosts fold into a one-line
    // breadcrumb strip and the page takes the rest of the home rect, so
    // at most three columns show. Without a sibling the page docks as a
    // column right of its host, never a modal over it. With no home pane
    // the overlay floats over the whole surface, as before.
    // The drill docks and folds on the backlog family, whose painter
    // paints into a rect (draw_board_rect). Other hosts (bell, decisions,
    // questions) keep the floating page until their painters learn to
    // paint into a column.
    // Hosts with their own column painter dock without a board; the
    // backlog family needs its board state to scope the rect.
    let board_backed = view.backlog_board.is_some()
        || matches!(
            view.persona.as_ref().map(|o| o.home),
            Some(Home::Bell | Home::Decision | Home::Question)
        );
    if !board_backed {
        paint_unscoped(view, cells, rows, cols, content_origin, content_dims);
        return true;
    }
    let Some(home) = persona_pane_rect(view) else {
        paint_unscoped(view, cells, rows, cols, content_origin, content_dims);
        return true;
    };
    // The chip door's page owns its pane (the split landed): the board
    // keeps its own panel region, its strip hint naming the open door,
    // and the pane paints the page full-rect. Docking columns inside the
    // pane would carve the page for a host that is not there.
    if view
        .persona
        .as_ref()
        .is_some_and(|o| o.home == Home::SidelineBoard && o.pane.is_some())
    {
        if view.backlog_board.is_some() {
            // The board paints below the tab bar, never into it.
            let (h, w) = persona_panel_rect(view);
            let bar = super::TAB_BAR_ROWS as usize;
            view.draw_board_rect(cells, rows, cols, (bar, 0, h.saturating_sub(bar), w), false);
        }
        paint_column(view, cells, rows, cols, home);
        paint_record_panes(view, cells, rows, cols);
        return true;
    }
    if sibling_pane(view).is_some() {
        paint_folded(view, cells, rows, cols, home);
        return true;
    }
    if view.persona.as_ref().is_some_and(|o| o.drill) {
        paint_drilled(view, cells, rows, cols, home);
        return true;
    }
    paint_docked(view, cells, rows, cols, home);
    true
}

/// The un-scoped paint: the host full surface, the page as the floating
/// overlay. The path a surface with no home pane takes (no `e` arm yet,
/// no split).
fn paint_unscoped(
    view: &View,
    cells: &mut [Cell],
    rows: usize,
    cols: usize,
    content_origin: (usize, usize),
    content_dims: (usize, usize),
) {
    if view.backlog_board.is_some() {
        view.draw_board(cells, rows, cols);
    } else if view.decisions_board.is_some() {
        super::decisions_view::paint_full(view, cells, rows, cols);
    } else if view.question_detail.is_some()
        && view.persona.as_ref().map(|o| o.home) != Some(Home::Question)
    {
        super::questions::draw_detail(view, cells, (rows, cols), content_origin, content_dims);
    }
    paint_over(view, cells, rows, cols, content_origin, content_dims);
}

/// The docked paint: the host keeps its columns and the persona page
/// docks right of it with a one-column seam, the way the board's own
/// detail pane docks. Widths read the held seam size, so paint, drag
/// and the keyboard nudge cannot disagree.
/// The host half of the dock: the surface the chip sat on keeps a
/// column inside the home rect, so no pane text is hidden by the page.
fn paint_host_column(
    view: &View,
    cells: &mut [Cell],
    rows: usize,
    cols: usize,
    host: (usize, usize, usize, usize),
) {
    match view.persona.as_ref().map(|o| o.home) {
        Some(Home::Question) => super::questions::paint_column(view, cells, rows, cols, host),
        Some(Home::Decision) => super::decisions_view::paint_column(view, cells, rows, cols, host),
        Some(Home::Bell) => super::bell::paint_column(view, cells, rows, cols, host),
        // The backlog family: the board is the host column.
        _ => {
            if view.backlog_board.is_some() {
                view.draw_board_rect(cells, rows, cols, host, false);
            }
        }
    }
}

fn paint_docked(
    view: &View,
    cells: &mut [Cell],
    rows: usize,
    cols: usize,
    home: (usize, usize, usize, usize),
) {
    let (top, left, h, w) = home;
    // A live toast owns the row below the tab bar: the dock yields the
    // row and clears it, so the panes never paint a header under the
    // notice.
    let (top, h) = if top == super::TAB_BAR_ROWS as usize && view.notice_live() {
        let base = top * cols + left;
        if let Some(span) = cells.get_mut(base..base + w) {
            span.fill(Cell::default());
        }
        (top + 1, h.saturating_sub(1))
    } else {
        (top, h)
    };
    let Some((host_w, col)) = dock_columns(view, w) else {
        paint_unscoped(view, cells, rows, cols, (top, left), (h, w));
        return;
    };
    // The host surfaces paint full-width beneath this dock (the question
    // page, the decisions table), so the home rect clears first and the
    // two columns repaint it; nothing of the wide paint shows through.
    for r in top..(top + h).min(rows) {
        let base = r * cols + left;
        if let Some(span) = cells.get_mut(base..base + w) {
            span.fill(Cell::default());
        }
    }
    paint_host_column(view, cells, rows, cols, (top, left, h, host_w));
    paint_column(view, cells, rows, cols, (top, left + host_w + 1, h, col));
    let seam_col = left + host_w;
    paint_seam(view, cells, rows, cols, top, seam_col, h);
}

/// The drilled paint: board | persona | record. The record opens right
/// of the persona column, the next side pane to the right; the board
/// paints kanban-only while its detail lives out in the record column.
/// Past the width cap the board folds into the breadcrumb strip first
/// and persona | record split the rest, the way the editor frame folds.
fn paint_drilled(
    view: &View,
    cells: &mut [Cell],
    rows: usize,
    cols: usize,
    home: (usize, usize, usize, usize),
) {
    let (top, left, h, w) = home;
    // A live toast owns the row below the tab bar: the drill yields the
    // row and clears it, so the panes never paint a header under the
    // notice.
    let (top, h) = if top == super::TAB_BAR_ROWS as usize && view.notice_live() {
        let base = top * cols + left;
        if let Some(span) = cells.get_mut(base..base + w) {
            span.fill(Cell::default());
        }
        (top + 1, h.saturating_sub(1))
    } else {
        (top, h)
    };
    // Rightmost widest: the record takes the biggest default share and
    // the two held seams reshape all three columns.
    let (per_w, board_w, rec_w) = drill_widths(view, w);
    let Some(board_w) = board_w else {
        paint_fold_strip(view, cells, rows, cols, top, left, w);
        let ih = h.saturating_sub(1);
        paint_column(view, cells, rows, cols, (top + 1, left, ih, per_w));
        paint_seam(view, cells, rows, cols, top + 1, left + per_w, ih);
        paint_record(
            view,
            cells,
            rows,
            cols,
            (top + 1, left + per_w + 1, ih, rec_w),
        );
        return;
    };
    view.draw_board_rect(cells, rows, cols, (top, left, h, board_w), true);
    paint_column(view, cells, rows, cols, (top, left + board_w + 1, h, per_w));
    paint_seam(view, cells, rows, cols, top, left + board_w, h);
    paint_seam(view, cells, rows, cols, top, left + board_w + per_w + 1, h);
    paint_record(
        view,
        cells,
        rows,
        cols,
        (top, left + board_w + per_w + 2, h, rec_w),
    );
}

/// The drilled record column: the serving row's node, the same detail
/// lines the board's own detail pane paints, in the column right of the
/// persona. The board holds no keyboard here, so the title row paints
/// unfocused.
fn paint_record(
    view: &View,
    cells: &mut [Cell],
    rows: usize,
    cols: usize,
    rect: (usize, usize, usize, usize),
) {
    // The rect clears before every guard: a drill record column that
    // opens with no node read leaves stale pane text under the paint
    // that follows it.
    let (top, left, h, w) = rect;
    if h == 0 || w == 0 {
        return;
    }
    let Some(b) = view.backlog_board.as_ref() else {
        return;
    };
    let Some(d) = b.detail.as_ref() else {
        return;
    };
    for r in top..(top + h).min(rows) {
        let base = r * cols + left;
        if let Some(span) = cells.get_mut(base..base + w) {
            span.fill(Cell::default());
        }
    }
    backlog_style::paint_title_row(
        cells,
        rows,
        cols,
        top,
        left,
        w,
        &format!("details \u{b7} {}", d.node_id),
        false,
        &view.theme,
    );
    if h <= 1 {
        return;
    }
    let (lines, _follow) = node_detail::pane_lines(b, &d.node_id, Some(d.sel), w);
    let dlines = lines.get(d.scroll.min(lines.len())..).unwrap_or(&[]);
    backlog_style::paint_panel_at(
        cells,
        rows,
        cols,
        left,
        top + 1,
        w,
        h - 1,
        dlines,
        None,
        &view.theme,
    );
}

/// The folded paint: one breadcrumb strip row names the client-local
/// hosts that folded, and the persona column takes the rest of the home
/// rect. The editor pane composites beside in its own rect.
fn paint_folded(
    view: &View,
    cells: &mut [Cell],
    rows: usize,
    cols: usize,
    home: (usize, usize, usize, usize),
) {
    let (top, left, h, w) = home;
    // The home rect clears first: pane cells live under this paint, and
    // a folded column that skips a region leaves the pane text showing.
    for r in top..(top + h).min(rows) {
        let base = r * cols + left;
        if let Some(span) = cells.get_mut(base..base + w) {
            span.fill(Cell::default());
        }
    }
    // Three surfaces fit beside the editor pane: the board keeps a real
    // column inside the home rect and the page sits right of it. A narrow
    // home folds the board into the strip instead.
    if view.backlog_board.is_some() {
        if dock_columns(view, w).is_some() {
            // The board lives in the sideline panel at the far left, the
            // strip the pane area starts after, kanban-only like the
            // drill view (the narrow panel cannot stack a detail). The
            // seam column divides it from the page, and nothing paints
            // into the tab bar.
            let bar = super::TAB_BAR_ROWS as usize;
            view.draw_board_rect(
                cells,
                rows,
                cols,
                (bar, 0, h.saturating_sub(bar), left.saturating_sub(1)),
                true,
            );
            paint_seam(view, cells, rows, cols, top, left - 1, h);
            paint_column(view, cells, rows, cols, (top, left, h, w));
            return;
        }
    }
    paint_fold_strip(view, cells, rows, cols, top, left, w);
    paint_column(
        view,
        cells,
        rows,
        cols,
        (top + 1, left, h.saturating_sub(1), w),
    );
}

/// The pane in this tab that is not the surface's home, if any: the
/// editor pane a drill split spawned.
pub(crate) fn sibling_pane(view: &View) -> Option<u64> {
    let home_id = match view.persona.as_ref()?.home {
        Home::Pane(id) => id,
        _ => return None,
    };
    view.layout
        .panes
        .iter()
        .find(|(pid, _)| *pid != home_id)
        .map(|(pid, _)| *pid)
}

/// The persona surface's home as a full-terminal rect (top, left, h, w),
/// when the surface has one. A recorded home pane wins: the pane the
/// drill or the `e` split came from, composited beside its siblings. A
/// door-scoped home resolves its host surface's own rect: the content
/// area for a full-screen host, the sideline panel for the sideline
/// board. Pane rects arrive content-area relative, so the content origin
/// is added back, and the result clamps to the terminal.
pub(crate) fn persona_pane_rect(view: &View) -> Option<(usize, usize, usize, usize)> {
    let (rows, cols) = view.term;
    let pane_rect = |home: u64| -> Option<(usize, usize, usize, usize)> {
        let (_, rect) = view.layout.panes.iter().find(|(pid, _)| *pid == home)?;
        let top = (TAB_BAR_ROWS as usize + rect.y as usize).min(rows as usize);
        let left = (view.left_chrome_w() as usize + rect.x as usize).min(cols as usize);
        let h = (rect.rows as usize).min(rows as usize - top);
        let w = (rect.cols as usize).min(cols as usize - left);
        Some((top, left, h, w))
    };
    let content_rect = || {
        let chrome = view.bottom_row_is_chrome() as usize;
        let top = TAB_BAR_ROWS as usize;
        let h = (rows as usize).saturating_sub(top + chrome).max(1);
        Some((top, 0, h, cols as usize))
    };
    match view.persona.as_ref().map(|o| o.home) {
        Some(Home::Pane(home)) => pane_rect(home),
        // The sideline board panel: the strip row and the board render
        // inside the left panel, so the dock lives inside the same panel.
        // The dock paints over panes only inside this rect; a wider home
        // would overlap the session panes without reflowing them, and
        // their text leaks around the persona column.
        Some(Home::SidelineBoard) => {
            // The persona column's own pane, when the split landed: the
            // rect follows the pane, so seams and resizes keep the
            // column honest. No pane (spawn refused, stale id) falls
            // back to the panel dock.
            if let Some(pane) = view.persona.as_ref().and_then(|o| o.pane) {
                for (pid, r) in view.layout.panes.iter() {
                    if *pid != pane {
                        continue;
                    }
                    let top = (TAB_BAR_ROWS as usize + r.y as usize).min(rows as usize);
                    let left = (view.left_chrome_w() as usize + r.x as usize).min(cols as usize);
                    let h = (r.rows as usize).min(rows as usize - top);
                    let w = (r.cols as usize).min(cols as usize - left);
                    return Some((top, left, h, w));
                }
            }
            let (h, w) = persona_panel_rect(view);
            Some((0, 0, h, w))
        }
        Some(Home::Question | Home::Decision | Home::Bell) => content_rect(),
        // Legacy paths: a full-screen board is its own home; else the
        // focused pane, else unscoped.
        _ => {
            if view.board_full {
                if view.backlog_board.is_none() {
                    return None;
                }
                return content_rect();
            }
            pane_rect(view.layout.focus)
        }
    }
}

/// The sideline board's panel dims `(h, w)`: the board and the chip
/// door's fallback dock live inside the left panel, never past it.
fn persona_panel_rect(view: &View) -> (usize, usize) {
    let (rows, cols) = view.term;
    let w = (view.panel_w() as usize).min(cols as usize);
    let h = (rows as usize)
        .saturating_sub(view.bottom_row_is_chrome() as usize)
        .max(1);
    (h, w)
}

/// The docked layout: `(host_w, persona_col_w)` for a home `w` columns
/// wide, from the held seam size. `None` when the terminal is too narrow
/// to dock (the caller keeps the floating overlay there).
pub(crate) fn dock_columns(view: &View, w: usize) -> Option<(usize, usize)> {
    // The yazi rule: a narrow home narrows the leftmost column and the
    // new pane keeps its floor; the dock never falls back to a modal
    // while two columns physically fit.
    const MIN_HOST: usize = 18;
    const MIN_COL: usize = 30;
    if w < MIN_HOST + MIN_COL + 1 {
        return None;
    }
    let bounds = seam_bounds(w);
    // Rightmost widest: the persona column, the detail just opened,
    // takes the biggest share unless the operator dragged the seam.
    let host_w = view.edge_sizes.resolve(
        super::edge_drag::Edge::PersonaSeam,
        w * 45 / 100,
        bounds.0,
        bounds.1,
    );
    let host_w = host_w.clamp(MIN_HOST, w.saturating_sub(MIN_COL + 1));
    let col_w = w.saturating_sub(host_w + 1);
    Some((host_w, col_w))
}

/// The persona column's width bounds in a home `w` columns wide, the
/// range the seam may put the host in.
pub(crate) fn seam_bounds(w: usize) -> (usize, usize) {
    const MIN_W: usize = 24;
    (MIN_W, w.saturating_sub(MIN_W + 1).max(MIN_W))
}

/// Whether the open persona overlay docks over the question page
/// (`Home::Question`): the docked paint repaints the host column itself,
/// so the host's full-screen chrome must not paint. Its title row would
/// survive above the dock, spanning the persona column as a second
/// `question · N … esc` frame.
pub(crate) fn question_docks(view: &View, cols: usize) -> bool {
    if view.persona.as_ref().map(|o| o.home) != Some(Home::Question) {
        return false;
    }
    if sibling_pane(view).is_some() {
        return false;
    }
    dock_columns(view, cols).is_some()
}

/// The drilled layout's column widths from the held seam sizes:
/// `(persona, board, record)`. The board holds `DrillBoardSeam`, the
/// persona column holds `DrillPersonaSeam`, and the record, rightmost
/// and widest by default, takes the remainder. `None` board: the record
/// crowded the board out and the paint strips it, keeping persona |
/// record.
pub(crate) fn drill_widths(view: &View, w: usize) -> (usize, Option<usize>, usize) {
    const MIN_COL: usize = 30;
    const MIN_BOARD: usize = 24;
    // Rightmost widest: the record takes the biggest default share.
    // The record dominates: the board holds its floor and the persona
    // column its readable minimum unless the operator dragged a seam.
    let per_w = view.edge_sizes.resolve(
        super::edge_drag::Edge::DrillPersonaSeam,
        MIN_COL,
        MIN_COL,
        w.saturating_sub(MIN_COL + 1),
    );
    let board_w = view.edge_sizes.resolve(
        super::edge_drag::Edge::DrillBoardSeam,
        MIN_BOARD,
        MIN_BOARD,
        w.saturating_sub(per_w + MIN_COL + 2),
    );
    let rec_w = w.saturating_sub(board_w + per_w + 2);
    if rec_w < MIN_COL {
        (per_w, None, w.saturating_sub(per_w + 1))
    } else {
        (per_w, Some(board_w), rec_w)
    }
}

/// The persona page as a drill column: a title row naming the persona,
/// then the wrapped page, notes, serving rows and the composer,
/// scroll-clamped to the rect. Frameless, like the board's own regions:
/// drill columns match the board's panes, not the floating chrome.
fn paint_column(
    view: &View,
    cells: &mut [Cell],
    rows: usize,
    cols: usize,
    rect: (usize, usize, usize, usize),
) {
    let Some(o) = view.persona.as_ref() else {
        return;
    };
    let (top, left, h, w) = rect;
    if h == 0 || w == 0 {
        return;
    }
    let page = o.cursor.is_none() && !o.notice;
    let serving = if page {
        serving_for(view, &o.slug)
    } else {
        Vec::new()
    };
    backlog_style::paint_title_row(cells, rows, cols, top, left, w, &o.title, true, &view.theme);
    super::backlog_board::backlog_panes::paint_esc_chip(
        cells,
        rows,
        cols,
        top,
        left,
        w,
        &view.theme,
    );
    if h <= 1 {
        return;
    }
    let width = w.saturating_sub(2).max(8);
    let lines: Vec<TLine> = if page {
        let mut l = o
            .doc
            .as_ref()
            .map(|d| page_lines(d, &o.path, width))
            .unwrap_or_else(|| o.lines.clone());
        if !o.notes.is_empty() {
            l.push(TLine::plain(String::new()));
            l.push(TLine::line(TextStyle::Section, "notes"));
            for n in &o.notes {
                for chunk in wrap_para(n, width.saturating_sub(2).max(8)) {
                    l.push(TLine::meta(format!("  {chunk}")));
                }
            }
        }
        l.extend(serving_lines(&serving, o.sel));
        if let Some(f) = &o.input {
            l.push(TLine::plain(String::new()));
            l.push(TLine::of(&[
                TSeg {
                    text: "note: ".into(),
                    role: TextStyle::Meta,
                },
                TSeg {
                    text: f.display(),
                    role: TextStyle::Item,
                },
            ]));
        }
        l
    } else {
        o.lines.clone()
    };
    // The scroll row, else the top, kept on screen: the column's follow
    // row, clamped so the last page ends at the bottom.
    let body_h = h - 1;
    let follow = if page {
        Some(o.scroll.min(lines.len().saturating_sub(1)))
    } else {
        Some(0)
    };
    let start = follow
        .map(|f| f.saturating_sub(body_h.saturating_sub(1)).min(f))
        .unwrap_or(0);
    super::backlog_style::paint_panel_at(
        cells,
        rows,
        cols,
        left,
        top + 1,
        w,
        body_h,
        lines.get(start..).unwrap_or(&[]),
        None,
        &view.theme,
    );
}

/// The breadcrumb strip the folded hosts wear: one title row naming the
/// folded surface, so a drill past the three-column cap never loses the
/// trail. The strip paints dim (unfocused): the folded surface holds no
/// keys.
fn paint_fold_strip(
    view: &View,
    cells: &mut [Cell],
    rows: usize,
    cols: usize,
    top: usize,
    left: usize,
    w: usize,
) {
    let text = fold_names(view).unwrap_or_else(|| "board".into());
    // No esc chip here: the persona column beside it holds the keys and
    // wears the chip, so the strip stays a plain name.
    backlog_style::paint_title_row(cells, rows, cols, top, left, w, &text, false, &view.theme);
}

/// The names the fold strip carries: the board, plus the drill-down's
/// node title when a detail is open.
fn fold_names(view: &View) -> Option<String> {
    let b = view.backlog_board.as_ref()?;
    let detail_title = b.detail.as_ref().and_then(|d| node_title(view, &d.node_id));
    Some(match detail_title {
        Some(t) => format!("board \u{b7} {t}"),
        None => "board".into(),
    })
}

/// The card title for a node id, read off the board's live inputs.
fn node_title(view: &View, id: &str) -> Option<String> {
    let inputs = view.backlog_board.as_ref()?.inputs.as_ref()?;
    crate::backlog_model::node(inputs, id).map(|nv| nv.card.title)
}

/// The one-column rule between the host and the persona column, the same
/// hairline the board's own panes draw.
fn paint_seam(
    view: &View,
    cells: &mut [Cell],
    rows: usize,
    cols: usize,
    top: usize,
    col: usize,
    h: usize,
) {
    if col >= cols {
        return;
    }
    for r in top..(top + h).min(rows) {
        if let Some(cell) = cells.get_mut(r * cols + col) {
            *cell = Cell {
                c: '\u{2502}',
                fg: view.theme.border,
                bg: crate::proto::Color::Default,
                flags: 0,
            };
        }
    }
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
    // keys, so nothing is silent. The footer keeps whole ` \u{b7} ` pairs:
    // a narrow column drops a pair rather than cutting one mid-token.
    let footer = overlay_footer(o, serving.is_empty());
    let chrome = chrome::Chrome::new(&o.title, Anchor::Center);
    let cap = content_dims
        .1
        .saturating_sub(chrome::Chrome::FRAME_COLS)
        .max(12);
    let footer = super::backlog_board::clip_segments(&footer, cap);
    let chrome = if footer.is_empty() {
        chrome
    } else {
        chrome.footer(&footer)
    };
    let (lines, follow, paint_cursor) = if page {
        let width = cap.saturating_sub(4).min(106);
        let mut l = o
            .doc
            .as_ref()
            .map(|d| page_lines(d, &o.path, width))
            .unwrap_or_else(|| o.lines.clone());
        // The notes section rides between the record and the serving rows:
        // the newest lines of notes.md, so a saved note is never invisible.
        if !o.notes.is_empty() {
            l.push(TLine::plain(String::new()));
            l.push(TLine::line(TextStyle::Section, "notes"));
            for n in &o.notes {
                for chunk in wrap_para(n, width.saturating_sub(2).max(8)) {
                    l.push(TLine::meta(format!("  {chunk}")));
                }
            }
        }
        let serving_start = l.len();
        l.extend(serving_lines(&serving, o.sel));
        let input_open = o.input.is_some();
        if let Some(f) = &o.input {
            l.push(TLine::plain(String::new()));
            l.push(TLine::of(&[
                TSeg {
                    text: "note: ".into(),
                    role: TextStyle::Meta,
                },
                TSeg {
                    text: f.display(),
                    role: TextStyle::Item,
                },
            ]));
        }
        let total = l.len();
        let follow = match o.sel {
            // +2 skips the blank and section lines serving_lines leads
            // with, so the follow row is the selected record itself.
            Some(i) if i < serving.len() => Some(serving_start + i + 2),
            _ => Some(o.scroll.min(total.saturating_sub(1))),
        };
        let follow = if input_open {
            Some(total.saturating_sub(1))
        } else {
            follow
        };
        (l, follow, o.sel.is_some() && !input_open)
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
        2, // a wrapped-prose page keeps the two-column right gap
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
    fn dock_columns_splits_host_and_column_from_the_held_seam() {
        let v = View::new((40, 140), "s".into(), LayoutView::default());
        let (host, col) = dock_columns(&v, 140).expect("140 columns dock");
        assert_eq!(host, 63);
        assert_eq!(col, 76);
        assert!(col > host, "the rightmost column is widest by default");
        assert!(dock_columns(&v, 48).is_none(), "the dock refuses under 49");
        let (host, col) = dock_columns(&v, 49).expect("49 columns dock at the floors");
        assert_eq!(
            (host, col),
            (18, 30),
            "both floors hold at the refusal edge"
        );
    }

    #[test]
    fn drill_widths_hold_rightmost_widest_and_both_seams() {
        let mut v = View::new((40, 140), "s".into(), LayoutView::default());
        let (per, board, rec) = drill_widths(&v, 140);
        let Some(board) = board else {
            panic!("140 columns fit three real columns");
        };
        assert_eq!((per, board, rec), (42, 40, 56));
        assert!(rec > per && per > board, "rightmost widest");
        v.edge_sizes
            .set(super::edge_drag::Edge::DrillPersonaSeam, 60);
        let (per, board, rec) = drill_widths(&v, 140);
        let Some(_) = board else {
            panic!("held persona seam still fits three columns");
        };
        assert_eq!(per, 60);
        // The board default collapses under the held persona seam and
        // floors at 24; the record keeps the remainder, 54.
        assert_eq!(rec, 54);
        v.edge_sizes.set(super::edge_drag::Edge::DrillBoardSeam, 30);
        let (per, board, rec) = drill_widths(&v, 140);
        let Some(board) = board else {
            panic!("held board seam still fits");
        };
        assert_eq!((per, board, rec), (60, 30, 48));
        // the record crowding below its floor strips the board
        v.edge_sizes
            .set(super::edge_drag::Edge::DrillPersonaSeam, 110);
        let (per, board, rec) = drill_widths(&v, 140);
        assert_eq!(
            (per, board, rec),
            (109, None, 30),
            "the persona seam clamps at its ceiling so the strip record keeps 30"
        );
    }

    #[test]
    fn left_pops_the_persona_column() {
        let mut v = View::new((40, 140), "s".into(), LayoutView::default());
        v.persona = Some(PersonaOverlay {
            pane: None,
            widen: false,
            edit_pane: None,
            claim_pane: None,
            records: Vec::new(),
            title: "solo".into(),
            lines: Vec::new(),
            cursor: None,
            personas: Vec::new(),
            project: "fno".into(),
            path: "/tmp/no-such-persona.md".into(),
            draft: false,
            slug: "solo".into(),
            doc: None,
            notes: Vec::new(),
            scroll: 0,
            sel: None,
            notice: false,
            input: None,
            edit_since: None,
            edit_spawn: None,
            home: Home::Float,
            drill: false,
        });
        assert!(v.persona.is_some());
        keys(&mut v, &[0x1b, 0x5b, 0x44]);
        assert!(v.persona.is_none());
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
            text.contains("  status: approved\n") && text.contains("  approved: 20"),
            "the metadata block carries the typed status and date: {text}"
        );
        assert!(!is_draft(&Some(
            text.lines()
                .find(|l| l.starts_with("  status:"))
                .map(|l| l["  status:".len()..].trim().to_string())
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
            texts.iter().any(|t| t == "  engineering team."),
            "the who wraps at the narrow width: {texts:?}"
        );
    }

    #[test]
    fn a_full_measure_page_line_keeps_the_right_gap() {
        // A line that packs to its wrap measure exactly must still show the
        // two-column gap before the border (the flush edge the persona page
        // hit on the demo board).
        let line = TLine::line(TextStyle::Item, "x".repeat(30));
        let layout = crate::client::overlay_paint::layout_lines_overlay_at(
            (0, 0),
            (40, 60),
            &chrome::Chrome::new("gap", Anchor::Center),
            &[line],
            None,
            false,
            crate::client::overlay_paint::OverlayAnchor::Center,
            2,
        );
        let row = layout
            .framed
            .lines
            .iter()
            .find(|l| l.text.contains("xxxxx"))
            .map(|l| l.text.as_str())
            .unwrap();
        assert!(row.ends_with("  │"), "the body keeps the right gap: {row}");
    }

    #[test]
    fn the_record_resolves_nested_first_with_the_flat_fallback() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("persona-d3440-{}", std::process::id()));
        let root = dir.join("payments");
        let nested = root.join("solo").join("PERSONA.md");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, fixture_doc("solo")).unwrap();
        pin_customers_dir(&dir);
        let page = open_page("payments", "solo");
        assert_eq!(page.path, nested, "the nested PERSONA.md wins");
        // The flat twin loses once the nested record exists.
        std::fs::write(root.join("solo.md"), fixture_doc("solo")).unwrap();
        let page = open_page("payments", "solo");
        assert_eq!(page.path, nested, "the nested PERSONA.md still wins");
        unpin_customers_dir();
        std::fs::remove_dir_all(&dir).ok();
        // Unmigrated: the flat file resolves and the bell counts it.
        let dir = std::env::temp_dir().join(format!("persona-d3440-flat-{}", std::process::id()));
        let root = dir.join("payments");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("solo.md"), fixture_doc("solo")).unwrap();
        let vibe = root.join("vibe").join("PERSONA.md");
        std::fs::create_dir_all(vibe.parent().unwrap()).unwrap();
        std::fs::write(&vibe, fixture_doc("vibe")).unwrap();
        pin_customers_dir(&dir);
        let page = open_page("payments", "solo");
        assert_eq!(page.path, root.join("solo.md"), "the flat file resolves");
        assert_eq!(
            super::persona_overlay::count_personas("payments"),
            "2",
            "the flat file and the nested record both count"
        );
        unpin_customers_dir();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_note_composer_saves_and_demotes() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("persona-note-{}", std::process::id()));
        let root = dir.join("payments");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("solo.md"), fixture_doc("solo")).unwrap();
        pin_customers_dir(&dir);
        let mut v = crate::client::tests::two_pane_view();
        v.persona = Some(open_page("payments", "solo"));
        // A canned serving question gives the page a selected row: the
        // exact shape whose Enter once stole the composer's save.
        v.questions_fold = Some(crate::needs_overlay::QuestionsFold {
            items: vec![crate::needs_overlay::QuestionItem {
                id: "q-demo".into(),
                title: "Approve the persona page?".into(),
                audience: Some("customer:solo".into()),
                state: "open".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        crate::client::persona_overlay::keys(&mut v, b"j");
        // n opens the composer; the typed text rides the same key path.
        crate::client::persona_overlay::keys(&mut v, b"n");
        assert!(
            v.persona.as_ref().is_some_and(|o| o.input.is_some()),
            "n opens the note composer"
        );
        crate::client::persona_overlay::keys(&mut v, b"lead with the demo");
        crate::client::persona_overlay::keys(&mut v, b"\r");
        assert!(
            v.question_detail.is_none(),
            "the composer's Enter saves, it never dispatches the serving row"
        );
        // The note line lands beside the file and the status demotes.
        let notes = std::fs::read_to_string(root.join("notes.md")).unwrap();
        assert!(notes.contains("- "), "a note line lands: {notes:?}");
        assert!(notes.contains("lead with the demo"), "the typed text lands");
        let text = std::fs::read_to_string(root.join("solo.md")).unwrap();
        assert!(
            text.contains("status: draft"),
            "the record demotes: {text:?}"
        );
        let page = v.persona.as_ref().expect("the page stays up");
        assert!(page.is_draft(), "the re-read page is draft");
        assert!(v.notice.is_some(), "the toast names the save");
        assert!(
            page.notes.iter().any(|n| n.contains("lead with the demo")),
            "the notes section shows the saved line: {:?}",
            page.notes
        );
        // An empty note closes the composer and writes nothing.
        crate::client::persona_overlay::keys(&mut v, b"n");
        crate::client::persona_overlay::keys(&mut v, b"\r");
        let notes = std::fs::read_to_string(root.join("notes.md")).unwrap();
        assert_eq!(notes.lines().count(), 1, "an empty note writes no line");
        unpin_customers_dir();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_edit_arm_reloads_the_page_on_a_moved_mtime() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("persona-edit-{}", std::process::id()));
        let root = dir.join("payments");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("solo.md");
        std::fs::write(&path, fixture_doc("solo")).unwrap();
        pin_customers_dir(&dir);
        let mut v = crate::client::tests::two_pane_view();
        v.persona = Some(open_page("payments", "solo"));
        // e arms the poll and hands the file to the loop's spawn drain.
        crate::client::persona_overlay::keys(&mut v, b"e");
        assert!(
            v.persona.as_ref().is_some_and(|o| o.edit_spawn.is_some()),
            "e hands the file off"
        );
        assert!(
            v.persona.as_ref().is_some_and(|o| o.edit_since.is_some()),
            "e arms the poll"
        );
        assert!(
            super::persona_overlay::take_edit_spawn(&mut v).is_some(),
            "the loop's drain takes the file"
        );
        // The user saves; the poll reloads the page in place.
        std::fs::write(
            &path,
            fixture_doc("solo").replace("Ship the first feature tonight.", "Ship it in the mux."),
        )
        .unwrap();
        crate::client::persona_overlay::edit_poll(&mut v);
        let page = v.persona.as_ref().expect("the page stays up");
        assert!(page.edit_since.is_none(), "the poll disarmed");
        let body = page
            .doc
            .as_ref()
            .map(|d| d.jobs.first().map(|j| j.1.as_str()).unwrap_or(""))
            .unwrap_or("");
        assert!(
            body.contains("Ship it in the mux."),
            "the edit landed: {body:?}"
        );
        unpin_customers_dir();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_skills_frontmatter_reads_and_metadata_wins() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // The Agent Skills shape: name and description at the top, the
        // record's keys under metadata.
        let text = "---
name: solo
description: the founder the demo serves
metadata:
  status: approved 2026-11-01
  confidence: high
---

## Who they are
One person.
";
        let doc = parse_persona(text);
        assert_eq!(doc.name, "solo");
        assert_eq!(
            doc.description.as_deref(),
            Some("the founder the demo serves")
        );
        assert_eq!(doc.status.as_deref(), Some("approved 2026-11-01"));
        assert_eq!(doc.confidence.as_deref(), Some("high"));
        // The flat shape still reads.
        let flat = parse_persona(&fixture_doc("solo"));
        assert_eq!(flat.name, "solo");
        assert_eq!(flat.status.as_deref(), Some("hypothesis, untested"));
        // A metadata key wins over its flat twin.
        let both = parse_persona(
            "---
status: draft
name: solo
metadata:
  status: approved
---

## Who they are
One person.
",
        );
        assert_eq!(both.status.as_deref(), Some("approved"), "metadata wins");
        // Old prose carrying the draft word still reads draft (the
        // accepted pre-typed shape).
        let prose = parse_persona(
            "---\npersona: solo\nstatus: hypothesis, draft awaiting user approval\nconfidence: inferred\n---\n",
        );
        assert!(is_draft(&prose.status), "flat prose reads draft");
        assert!(!is_draft(&both.status), "the typed approval is not draft");
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
        // A node record drills: the persona column stays and the node
        // opens in the board's detail beside it.
        assert!(v.persona.is_some());
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
    if o.input.is_some() {
        return "enter save \u{b7} esc cancel".into();
    }
    match (o.cursor, o.notice, o.draft, serving_empty) {
        (Some(_), _, _, _) => "enter opens".into(),
        (_, true, _, _) => String::new(),
        (None, false, _, false) => {
            "j/k move \u{b7} enter open \u{b7} e edit \u{b7} n note \u{b7} esc close".into()
        }
        (None, false, true, true) => {
            "a approve \u{b7} j/k scroll \u{b7} e edit \u{b7} n note \u{b7} esc close".into()
        }
        (None, false, false, true) => {
            "j/k scroll \u{b7} e edit \u{b7} n note \u{b7} esc close".into()
        }
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
