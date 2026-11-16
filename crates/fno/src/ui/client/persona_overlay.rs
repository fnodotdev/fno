//! The persona overlay: Enter on an audience chip opens who the record
//! serves. The page renders the persona file (`~/.fno/profile/customers/
//! <project>/<persona>.md`): who they are, their jobs, and the frontmatter
//! status and confidence. A multi-persona audience opens a picker first;
//! the bell's Personas tab lists every persona under the project's profile
//! directory. A missing file renders a notice naming the exact path.
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
    /// a notice, where any key closes.
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

fn cut(text: &str, n: usize) -> String {
    text.trim().chars().take(n).collect()
}

/// The persona file's renderable fields: the frontmatter facts, the who
/// paragraph, and one line per job slug. Only what the overlay shows; the
/// file stays the one source and the page stays bounded.
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
    // Body sections. `## Who they are` keeps up to four lines; `## Jobs`
    // keeps one `### slug` row with its first paragraph line. The `###`
    // check runs before any `#` skip, or the job slugs never reach the
    // Jobs arm.
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
                doc.jobs.push((cut(slug, 44), String::new()));
            }
            "Jobs" if !t.starts_with('#') => {
                if let Some(last) = doc.jobs.last_mut() {
                    if last.1.is_empty() {
                        last.1 = cut(t, 72);
                    }
                }
            }
            "Who they are" if !t.starts_with('#') && doc.who.len() < 4 => doc.who.push(cut(t, 96)),
            _ => {}
        }
    }
    doc
}

fn page_lines(doc: &PersonaDoc, path: &std::path::Path) -> Vec<TLine> {
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
                text: cut(s, 96),
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
                text: cut(c, 96),
                role: TextStyle::Item,
            },
        ]));
    }
    if !doc.who.is_empty() {
        lines.push(TLine::plain(String::new()));
        lines.push(TLine::line(TextStyle::Section, "who they are"));
        for w in &doc.who {
            lines.push(TLine::meta(format!("  {w}")));
        }
    }
    if !doc.jobs.is_empty() {
        lines.push(TLine::plain(String::new()));
        lines.push(TLine::line(TextStyle::Section, "jobs"));
        for (slug, desc) in &doc.jobs {
            lines.push(TLine::of(&[
                TSeg {
                    text: format!("  {slug}"),
                    role: TextStyle::Badge,
                },
                TSeg {
                    text: if desc.is_empty() {
                        String::new()
                    } else {
                        format!("  {desc}")
                    },
                    role: TextStyle::Meta,
                },
            ]));
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
    let (title, lines, draft) = match std::fs::read_to_string(&path) {
        Ok(text) => {
            let doc = parse_persona(&text);
            let title = if doc.name.is_empty() {
                persona.to_string()
            } else {
                doc.name.clone()
            };
            let draft = is_draft(&doc.status);
            (title, page_lines(&doc, &path), draft)
        }
        Err(_) => (persona.to_string(), notice_lines(&path), false),
    };
    PersonaOverlay {
        title,
        lines,
        cursor: None,
        personas: Vec::new(),
        project: project.to_string(),
        path,
        draft,
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
/// Enter; a page or notice closes on any key (the digest rule). Esc and
/// `q` close everywhere. Infallible, so it returns the flow directly (the
/// R6 ratchet, rust-style R6, holds string errors shrink-only).
pub(crate) fn keys(view: &mut View, bytes: &[u8]) -> StdinFlow {
    if bytes.is_empty() {
        // A flush is not a key: leave the overlay up.
        return StdinFlow::Continue;
    }
    let Some(o) = view.persona.as_mut() else {
        return StdinFlow::Continue;
    };
    match apply_keys(o, bytes) {
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
            match approve_in_place(&path) {
                Some(fail) => view.set_notice(format!("persona approve failed: {fail}")),
                None => {
                    let slug = o
                        .path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let fresh = open_page(&o.project, &slug);
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
/// testable without a `View`.
fn apply_keys(o: &mut PersonaOverlay, bytes: &[u8]) -> Flow {
    let Some(start) = o.cursor else {
        // A draft page: `a` alone approves in place.
        // Anything else closes (the digest rule).
        if o.draft && bytes == b"a" {
            return Flow::Approve(o.path.clone());
        }
        return Flow::Close;
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
    // The chrome's esc chip is the one closer; the footer never repeats it.
    let footer = match o.cursor {
        Some(_) => "enter opens",
        None if o.draft => "a approve",
        None => "",
    };
    let chrome = chrome::Chrome::new(&o.title, Anchor::Center);
    let chrome = if footer.is_empty() {
        chrome
    } else {
        chrome.footer(footer)
    };
    super::overlay_paint::draw_lines_overlay(
        cells,
        rows,
        cols,
        content_origin,
        content_dims,
        &chrome,
        &o.lines,
        &view.theme,
        None,
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
            vec!["One person building a product, with no".to_string()]
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
        let text = page_lines(&doc, path)
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
            Flow::Drill(page) => assert_eq!(page.title, "solo"),
            other => panic!("expected a drill, got {other:?}"),
        }
        let mut page = open_page("fno", "solo");
        assert!(matches!(apply_keys(&mut page, b"x"), Flow::Close));
        unpin_customers_dir();
        std::fs::remove_dir_all(&dir).ok();
    }
}
