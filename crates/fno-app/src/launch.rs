//! The launch composer: a centered panel that starts a session or a lead
//! through the server's canonical launch door, in the key sheet's look.
//! The picks are stored by value, so a rebuilt candidate list never moves
//! them; a pick that leaves its list shows `gone` and the submit refuses in
//! place. Esc closes and keeps the draft: the panel's whole state survives
//! close and reopen on the root.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    canvas, div, prelude::*, px, rgb, rgba, Bounds, Context, CursorStyle, Div, FontWeight,
    MouseButton, Pixels, Point, SharedString, Stateful, Window,
};

use super::chat::composer::Draft;
use super::chat::field::{Field, FieldState};
use super::keys::Key;
use super::link::Layout;
use super::palette::Palette;
use super::ui::{self, Role};
use super::window::Root;
use fno::app_api::client::agent_launcher::{
    epic_rows, harness_rows, EpicChoice, HarnessRow, LEAD_SEED,
};
use fno::proto::chat_event::SessionKey;
use fno::proto::{AgentLaunchRequest, AgentLaunchUpdate, AgentRow, LaunchState};

/// What the submit starts. The wording follows the HQ guide's "Which one to
/// start": a worker runs one task; a lead hands a stream of tasks to
/// workers. Shell runs a local shell command in a new terminal pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Agent,
    Lead,
    Shell,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Agent => "agent",
            Kind::Lead => "lead",
            Kind::Shell => "shell",
        }
    }

    /// The one dim line under the pick: why this kind, from the HQ guide.
    fn why(self) -> &'static str {
        match self {
            Kind::Agent => "An agent runs one task. Pick it when one session can finish the work.",
            Kind::Lead => "A lead hands a stream of tasks to workers. Pick the epics it covers.",
            Kind::Shell => "A shell command runs in a new terminal pane on this machine.",
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Kind::Agent => "the task, as one message",
            Kind::Lead => "filter the epics, ⏎ picks the top match",
            Kind::Shell => "the shell command",
        }
    }

    /// The kind a cmd-digit picks, in the header's order: agent, lead, shell.
    pub(crate) fn from_index(n: u8) -> Option<Kind> {
        match n {
            1 => Some(Kind::Agent),
            2 => Some(Kind::Lead),
            3 => Some(Kind::Shell),
            _ => None,
        }
    }
}

/// Where an attempt stands. `Refused` and `Unknown` show their text under
/// the field verbatim; the draft survives both.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Editing,
    Starting,
    Refused(String),
    Unknown(String),
}

/// The row a tab or an arrow moves to, in panel order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Row {
    Kind,
    Harness,
    Project,
    Model,
    Effort,
    Worktree,
    Task,
}

impl Row {
    /// The next row in panel order, skipping the rows the current kind
    /// does not show: shell has no harness or launch axes, the lead kind
    /// carries no model, effort or worktree picks, and the task row wraps.
    fn next(self, back: bool, kind: Kind) -> Row {
        let chain: &[Row] = match kind {
            Kind::Agent => &[
                Row::Kind,
                Row::Harness,
                Row::Project,
                Row::Model,
                Row::Effort,
                Row::Worktree,
                Row::Task,
            ],
            Kind::Lead => &[Row::Kind, Row::Harness, Row::Project, Row::Task],
            Kind::Shell => &[Row::Kind, Row::Project, Row::Task],
        };
        let at = chain.iter().position(|r| *r == self).unwrap_or(0);
        let at = if back {
            (at + chain.len() - 1) % chain.len()
        } else {
            (at + 1) % chain.len()
        };
        chain[at]
    }
}

/// What one key asks the window to do after the panel's own state moved.
pub(crate) enum KeyOut {
    /// The panel consumed it.
    Taken,
    /// Send the attempt.
    Submit,
    /// Close the panel, draft intact.
    Close,
    /// Not the panel's key: text falls to the input handler, the rest dies.
    Ignored,
}

/// The composer on the root: the retained state, the open flag, the id
/// mint, and the row the last `Launched` named.
pub struct Panel {
    pub state: Launch,
    pub open: bool,
    pub next_id: u64,
    pub focus: Option<String>,
    /// The launched agent still owed the shown conversation: held until a
    /// roster row by this name carries the session id its stream opens
    /// with, then cleared.
    pub show: Option<String>,
    /// The task text the shown launch carried: the row's readable label
    /// while the list marks it (the registry name stays the mail key).
    pub show_task: Option<String>,
}

impl Default for Panel {
    fn default() -> Self {
        Panel {
            state: Launch::fresh(),
            open: false,
            next_id: 1,
            focus: None,
            show: None,
            show_task: None,
        }
    }
}

/// The chat key a launched agent's stream opens with, once its roster row
/// carries a session id. None while the row is absent or still paneless
/// and id-less: the steer waits instead of guessing.
pub(crate) fn stream_key(agents: &[AgentRow], name: &str) -> Option<SessionKey> {
    let row = agents.iter().find(|a| a.name == name)?;
    let harness = row.harness.clone()?;
    let session_id = row.harness_session_id.clone()?;
    Some(SessionKey {
        harness,
        session_id,
        subagent: None,
    })
}

/// The composer's state, retained across close and reopen.
pub struct Launch {
    pub kind: Kind,
    /// The picked harness name, by value, never an index.
    harness: String,
    /// The picked project path, by value.
    project: String,
    /// True when `project` left the candidate list: shown `gone`, and the
    /// submit refuses until a fresh pick.
    gone: bool,
    /// The lead kind's picked epic ids, in pick order. The submit seeds the
    /// lead with them space-joined; an empty pick starts it with no scope.
    pub epics: Vec<String>,
    /// The lead kind's epic pool, read once at open or on a kind flip. An
    /// empty pool starts the lead with no scope.
    epic_pool: Vec<EpicChoice>,
    /// The picked model slug; None sends nothing and the door defaults.
    model: Option<String>,
    /// The picked effort word; None sends nothing and the door defaults.
    effort: Option<String>,
    /// True when the launch lands in a worktree instead of the main checkout.
    worktree: bool,
    /// The picked project's slash skills, read at open and on a harness
    /// step. Claude only: the other harnesses have no listing surface, so
    /// their prompts autocomplete nothing.
    skills: Vec<String>,
    harnesses: Vec<HarnessRow>,
    projects: Vec<String>,
    pub draft: Draft,
    pub field: Rc<FieldState>,
    /// The task box's bounds at the last paint, for the IME candidate window.
    pub composer: Rc<Cell<Bounds<Pixels>>>,
    pub(crate) row: Row,
    /// The id this panel's attempt was submitted under; ids start at 1.
    pub request_id: u64,
    pub phase: Phase,
}

impl Launch {
    /// A fresh panel: no picks yet, the catalog read once.
    pub fn fresh() -> Self {
        let mut launch = Launch {
            kind: Kind::Agent,
            harness: String::new(),
            project: String::new(),
            gone: false,
            epics: Vec::new(),
            epic_pool: Vec::new(),
            model: None,
            effort: None,
            worktree: false,
            skills: Vec::new(),
            harnesses: Vec::new(),
            projects: Vec::new(),
            draft: Draft::default(),
            field: Rc::new(FieldState::default()),
            composer: Rc::new(Cell::new(Bounds::default())),
            row: Row::Task,
            request_id: 0,
            phase: Phase::Editing,
        };
        launch.refresh_harnesses();
        launch
    }
}

impl Launch {
    /// Re-read the harness rows (a fresh open's catalog view) and keep the
    /// picked name by value; a vanished pick falls back to the first
    /// selectable row.
    pub(crate) fn refresh_harnesses(&mut self) {
        let rows = harness_rows().unwrap_or_default();
        let keep = self.harness.clone();
        self.harness = rows
            .iter()
            .find(|r| r.name == keep)
            .map(|r| r.name.clone())
            .unwrap_or_else(|| {
                rows.iter()
                    .find(|r| r.selectable)
                    .map(|r| r.name.clone())
                    .unwrap_or_default()
            });
        self.harnesses = rows;
    }

    /// Rebuild the project candidates from the layout, in the TUI's order:
    /// the active workspace's folder, the app's own cwd, the other workspaces'.
    pub(crate) fn refresh_projects(&mut self, layout: &Layout) {
        let mut projects: Vec<String> = Vec::new();
        if let Some(s) = layout.squads.iter().find(|s| s.id == layout.active_squad) {
            if !s.canonical_cwd.is_empty() {
                projects.push(s.canonical_cwd.clone());
            }
        }
        let own = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        if !own.is_empty() && !projects.contains(&own) {
            projects.push(own);
        }
        for s in &layout.squads {
            if !s.canonical_cwd.is_empty() && !projects.contains(&s.canonical_cwd) {
                projects.push(s.canonical_cwd.clone());
            }
        }
        self.projects = projects;
        if self.project.is_empty() {
            self.project = self.projects.first().cloned().unwrap_or_default();
            self.gone = false;
        } else {
            self.gone = !self.projects.iter().any(|p| *p == self.project);
        }
    }

    /// Open (or re-open) the panel: a fresh attempt's phase, the task row
    /// focused, and `kind` set when the opener named one.
    pub(crate) fn open(&mut self, kind: Option<Kind>) {
        if let Some(k) = kind {
            self.kind = k;
        }
        self.row = Row::Task;
        self.phase = Phase::Editing;
        self.request_id = 0;
        self.refresh_skills();
        self.refresh_epics();
    }

    /// Read the epic pool for the lead kind's picker; every other kind
    /// carries none.
    pub(crate) fn refresh_epics(&mut self) {
        self.epic_pool = if self.kind == Kind::Lead {
            epic_rows()
        } else {
            Vec::new()
        };
    }

    /// One kind pick lands, the kind row's flip or a cmd-digit: the
    /// derived caches follow the new kind.
    pub(crate) fn set_kind(&mut self, kind: Kind) {
        self.kind = kind;
        self.refresh_skills();
        self.refresh_epics();
    }

    /// Read the picked project's skill names for the completion list. A
    /// claude pick lists both roots; anything else carries none.
    pub(crate) fn refresh_skills(&mut self) {
        self.skills = if self.kind == Kind::Agent && self.harness == "claude" {
            skill_names(&self.project)
        } else {
            Vec::new()
        };
    }

    /// Why submit refuses right now, shown in place; `None` sends. A lead
    /// id is judged here only when the app can judge it: nothing given, or
    /// text that is neither a known project nor an id-shaped token (those
    /// go to the door, which knows the graph). The door stays the authority
    /// on everything else (harness install state, cwd existence, seed
    /// acceptance).
    pub(crate) fn block(&self) -> Option<String> {
        if self.kind == Kind::Shell && shell_command(&self.draft).is_none() {
            return Some("type a shell command first".into());
        }
        if self.kind == Kind::Lead && !self.draft.text().trim().is_empty() {
            return Some("clear the filter or pick a match".into());
        }
        if self.gone {
            return Some("that workspace closed; pick another project".into());
        }
        if self.project.is_empty() {
            return Some("pick a project".into());
        }
        None
    }

    /// The request one submit sends: cwd is the picked project, the message
    /// is the draft (a lead carries the seed plus the picked epic ids, or
    /// the bare seed for the no-scope start), and every optional axis stays
    /// off unless its picker picked one. The draft is read, never taken: a
    /// refusal must leave it intact. A shell submit never reaches here; the
    /// window runs it as a pane command.
    pub fn request(&mut self, request_id: u64) -> AgentLaunchRequest {
        let mut message = self.draft.text().to_string();
        if self.kind == Kind::Lead {
            let ids = self.epics.join(" ");
            message = if ids.is_empty() {
                LEAD_SEED.to_string()
            } else {
                format!("{LEAD_SEED}{ids}")
            };
        }
        self.request_id = request_id;
        let revision = self.draft.revision();
        AgentLaunchRequest {
            request_id,
            revision,
            cwd: self.project.clone(),
            harness: self.harness.clone(),
            host: None,
            substrate: String::new(),
            model: self.model.clone(),
            provider: None,
            route: None,
            model_names_harness: false,
            effort: self.effort.clone(),
            permission_mode: None,
            placement: None,
            portal: None,
            split: None,
            node: None,
            message,
            extra_flags: Vec::new(),
            worktree: self.worktree,
            branch: None,
            force: false,
        }
    }

    fn step_harness(&mut self, forward: bool) {
        let n = self.harnesses.len();
        if n == 0 {
            return;
        }
        let at = self
            .harnesses
            .iter()
            .position(|r| r.name == self.harness)
            .unwrap_or(0);
        // The rows after the pick in the step direction, then the wrap.
        let mut order: Vec<&HarnessRow> = self.harnesses.iter().skip(at + 1).collect();
        order.extend(self.harnesses.iter().take(at));
        if !forward {
            order.reverse();
        }
        if let Some(row) = order.into_iter().find(|r| r.selectable) {
            self.harness = row.name.clone();
            self.refresh_skills();
        }
    }

    fn step_project(&mut self, forward: bool) {
        if self.projects.is_empty() {
            return;
        }
        let order: Vec<String> = match self.projects.iter().position(|p| *p == self.project) {
            None => self.projects.clone(),
            Some(at) => {
                let mut rest: Vec<String> = self.projects.iter().skip(at + 1).cloned().collect();
                rest.extend(self.projects.iter().take(at).cloned());
                if !forward {
                    rest.reverse();
                }
                rest
            }
        };
        if let Some(p) = order.into_iter().next() {
            self.project = p;
            self.gone = false;
        }
    }

    /// Cycle the model pick: None, then the picked harness's table slugs
    /// in order, then back to None. An empty list never leaves None.
    fn step_model(&mut self, forward: bool) {
        let row = self.harnesses.iter().find(|r| r.name == self.harness);
        let list: Vec<&String> = row.map(|r| r.models.iter().collect()).unwrap_or_default();
        if list.is_empty() {
            self.model = None;
            return;
        }
        let at = self
            .model
            .as_ref()
            .and_then(|m| list.iter().position(|c| *c == m));
        self.model = match at {
            None => {
                if forward {
                    Some(list[0].clone())
                } else {
                    None
                }
            }
            Some(at) => {
                let next = if forward {
                    (at + 1) % (list.len() + 1)
                } else {
                    at + list.len()
                };
                if next == list.len() {
                    None
                } else {
                    Some(list[next % (list.len() + 1)].clone())
                }
            }
        };
    }

    /// Cycle the effort pick over the harness's capability list; None wraps.
    fn step_effort(&mut self, forward: bool) {
        let row = self.harnesses.iter().find(|r| r.name == self.harness);
        let list: Vec<&String> = row.map(|r| r.efforts.iter().collect()).unwrap_or_default();
        if list.is_empty() {
            self.effort = None;
            return;
        }
        let at = self
            .effort
            .as_ref()
            .and_then(|e| list.iter().position(|c| *c == e));
        self.effort = match at {
            None => {
                if forward {
                    Some(list[0].clone())
                } else {
                    None
                }
            }
            Some(at) => {
                let next = if forward {
                    (at + 1) % (list.len() + 1)
                } else {
                    at + list.len()
                };
                if next == list.len() {
                    None
                } else {
                    Some(list[next % (list.len() + 1)].clone())
                }
            }
        };
    }

    fn step_worktree(&mut self) {
        self.worktree = !self.worktree;
    }

    /// The `/query` the draft leads with, if it does.
    fn skill_query(&self) -> Option<&str> {
        self.draft
            .text()
            .strip_prefix('/')
            .filter(|q| !q.contains(char::is_whitespace))
    }

    /// The skills matching the draft's leading `/query`, name order. Only
    /// the agent kind on claude lists any: the door verifies one name at a
    /// time and nothing lists them, so every other pick autocompletes
    /// nothing.
    pub(crate) fn skill_completions(&self) -> Vec<String> {
        if self.kind != Kind::Agent || self.harness != "claude" {
            return Vec::new();
        }
        let Some(q) = self.skill_query() else {
            return Vec::new();
        };
        let needle = q.to_lowercase();
        self.skills
            .iter()
            .filter(|s| s.to_lowercase().contains(&needle))
            .take(4)
            .cloned()
            .collect()
    }

    /// Tab fills the top match as `/name ` and keeps the row on the task.
    /// False when nothing shows, so tab keeps moving rows.
    pub(crate) fn complete_skill(&mut self) -> bool {
        let Some(first) = self.skill_completions().into_iter().next() else {
            return false;
        };
        self.draft.select_all();
        self.draft.replace_selection(&format!("/{first} "));
        true
    }

    /// The pool rows the draft's filter matches, pool order. Fuzzy: a
    /// case-insensitive substring over id, slug and title. An empty filter
    /// shows the pool's head, so the picker is never blank.
    pub(crate) fn epic_matches(&self) -> Vec<EpicChoice> {
        let q = self.draft.text().trim().to_lowercase();
        self.epic_pool
            .iter()
            .filter(|e| {
                q.is_empty()
                    || e.id.to_lowercase().contains(&q)
                    || e.slug.to_lowercase().contains(&q)
                    || e.title.to_lowercase().contains(&q)
            })
            .take(6)
            .cloned()
            .collect()
    }

    /// Enter on the lead kind with a filter: toggle the top match in the
    /// pick list and clear the filter. False when no match shows.
    pub(crate) fn pick_top_epic(&mut self) -> bool {
        let Some(top) = self.epic_matches().into_iter().next() else {
            return false;
        };
        if let Some(at) = self.epics.iter().position(|id| *id == top.id) {
            self.epics.remove(at);
        } else {
            self.epics.push(top.id);
        }
        self.draft.select_all();
        self.draft.replace_selection("");
        true
    }

    /// One key while the panel is open. Cmd keys never reach it: the panel
    /// is modal, and esc leaves first. An IME draft takes the keys, as the
    /// chat composer's does.
    pub(crate) fn key(&mut self, k: &Key) -> KeyOut {
        if k.cmd || self.draft.marked().is_some() {
            return KeyOut::Ignored;
        }
        match k.key {
            "escape" => KeyOut::Close,
            "enter" if !k.shift => {
                // The lead kind's enter picks the top epic match; the
                // submit comes on an empty filter.
                if self.kind == Kind::Lead && !self.draft.text().trim().is_empty() {
                    self.pick_top_epic();
                    KeyOut::Taken
                } else {
                    KeyOut::Submit
                }
            }
            "enter" => {
                self.draft.replace_selection("\n");
                KeyOut::Taken
            }
            "tab" => {
                if !self.complete_skill() {
                    self.row = self.row.next(k.shift, self.kind);
                }
                KeyOut::Taken
            }
            "up" => {
                self.row = self.row.next(true, self.kind);
                KeyOut::Taken
            }
            "down" => {
                self.row = self.row.next(false, self.kind);
                KeyOut::Taken
            }
            "backspace" => {
                self.draft.backspace();
                KeyOut::Taken
            }
            "delete" => {
                self.draft.delete();
                KeyOut::Taken
            }
            "left" | "right" => self.horizontal(k),
            _ => KeyOut::Ignored,
        }
    }

    /// Left and right do what the row they sit on means: the pick flips on
    /// the kind row, steps its list on the harness and project rows, and
    /// moves the caret on the task row.
    fn horizontal(&mut self, k: &Key) -> KeyOut {
        let forward = k.key == "right";
        match self.row {
            Row::Kind => {
                let next = match self.kind {
                    Kind::Agent => Kind::Lead,
                    Kind::Lead => Kind::Shell,
                    Kind::Shell => Kind::Agent,
                };
                self.set_kind(next);
                KeyOut::Taken
            }
            Row::Harness => {
                self.step_harness(forward);
                KeyOut::Taken
            }
            Row::Project => {
                self.step_project(forward);
                KeyOut::Taken
            }
            Row::Model => {
                self.step_model(forward);
                KeyOut::Taken
            }
            Row::Effort => {
                self.step_effort(forward);
                KeyOut::Taken
            }
            Row::Worktree => {
                self.step_worktree();
                KeyOut::Taken
            }
            Row::Task => {
                if k.alt {
                    if forward {
                        self.draft.word_right(k.shift);
                    } else {
                        self.draft.word_left(k.shift);
                    }
                } else if forward {
                    self.draft.right(k.shift);
                } else {
                    self.draft.left(k.shift);
                }
                KeyOut::Taken
            }
        }
    }
}

/// The row a kind switch lands on: the task box, every kind. The pickers
/// keep their picks; only the focus moves.
pub(crate) fn row_for_kind(_kind: Kind) -> Row {
    Row::Task
}

impl Launch {
    /// One launch update lands. A stale attempt's id never touches the
    /// panel; the window owns the `Launched` side effects (close, notice,
    /// row focus).
    pub(crate) fn apply(&mut self, update: &AgentLaunchUpdate) -> bool {
        if update.request_id != self.request_id {
            return false;
        }
        match &update.state {
            LaunchState::Starting => self.phase = Phase::Starting,
            LaunchState::Refused { reason } => self.phase = Phase::Refused(reason.clone()),
            LaunchState::Unknown { reason } => self.phase = Phase::Unknown(reason.clone()),
            LaunchState::Launched { .. } => {}
        }
        true
    }
}

/// The composer's text layout, re-shaped when the draft moved since the
/// last paint. The hit-test and IME paths read through here.
pub(super) fn with_layout<R>(
    root: &Root,
    window: &Window,
    f: impl FnOnce(&super::chat::field::Layout) -> R,
) -> Option<R> {
    let slot = root.launch.state.field.layout.borrow();
    let layout = slot.as_ref()?;
    if layout.is_current(&root.launch.state.draft) {
        return Some(f(layout));
    }
    Some(f(&layout.reshaped(&root.launch.state.draft, window)))
}

/// The byte offset nearest a window point inside the task box.
pub(super) fn caret_at(root: &Root, at: Point<Pixels>, window: &Window) -> Option<usize> {
    let len = root.launch.state.draft.text().len();
    with_layout(root, window, |l| l.index_for_point(at - l.origin, len))
}

fn segment(kind: Kind, picked: bool, p: &Palette, cx: &mut Context<Root>) -> Stateful<Div> {
    let (color, weight) = if picked {
        (p.accent, FontWeight::SEMIBOLD)
    } else {
        (p.muted, FontWeight::MEDIUM)
    };
    ui::clickable(
        div()
            .id(SharedString::from(format!("launch-kind-{}", kind.label())))
            .px_2()
            .py_1()
            .rounded(px(4.0))
            .when(picked, |d| d.bg(rgb(p.raised))),
        p,
    )
    .on_click(cx.listener(move |root, _, _, cx| {
        root.launch.state.kind = kind;
        cx.notify();
    }))
    .text_color(rgb(color))
    .font_weight(weight)
    .child(kind.label())
}

/// The panel over a dimmed window: the picks, the task box, the attempt's
/// phase. A click outside closes; the draft survives.
pub(super) fn render(root: &Root, window: &Window, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let l = &root.launch.state;
    let overlay = div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x0000_0066))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|root, _, _, cx| root.close_launch(cx)),
        );
    let panel = div()
        .id("launch-panel")
        .flex()
        .flex_col()
        .w(px(560.0))
        .px_4()
        .py_3()
        .gap_2()
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(p.border))
        .bg(rgb(p.panel))
        .font_family(ui::MONO)
        .text_size(px(ui::BODY))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .flex_row()
                .items_baseline()
                .child(
                    div()
                        .flex_1()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("New session"),
                )
                .child(ui::kbd("esc", &p)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap(px(4.0))
                .child(segment(Kind::Agent, l.kind == Kind::Agent, &p, cx))
                .child(segment(Kind::Lead, l.kind == Kind::Lead, &p, cx))
                .child(segment(Kind::Shell, l.kind == Kind::Shell, &p, cx)),
        )
        .child(ui::text(l.kind.why(), Role::Meta, &p))
        .child(harness_row(l, &p, cx))
        .child(project_row(l, &p, cx))
        .when(l.kind == Kind::Agent, |d| {
            d.child(picker_row(
                "launch-model",
                "model",
                model_label(l),
                &p,
                cx,
                |root, _| {
                    root.launch.state.step_model(true);
                },
            ))
            .child(picker_row(
                "launch-effort",
                "effort",
                effort_label(l),
                &p,
                cx,
                |root, _| {
                    root.launch.state.step_effort(true);
                },
            ))
            .child(picker_row(
                "launch-worktree",
                "worktree",
                if l.worktree {
                    "worktree".to_string()
                } else {
                    "main checkout".to_string()
                },
                &p,
                cx,
                |root, _| root.launch.state.step_worktree(),
            ))
        })
        .child(task_box(root, window, cx))
        .children(skill_hints(l, &p))
        .children(epic_list(l, &p))
        .children(phase_line(l, &p))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .pt(px(4.0))
                .child(ui::kbd("⏎", &p))
                .child(ui::text("start", Role::Hint, &p))
                .child(ui::text("·", Role::Hint, &p))
                .child(ui::kbd("esc", &p))
                .child(ui::text("close", Role::Hint, &p)),
        );
    overlay.child(panel)
}

fn harness_row(l: &Launch, p: &Palette, cx: &mut Context<Root>) -> Stateful<Div> {
    let row = l.harnesses.iter().find(|r| r.name == l.harness);
    let selectable = row.is_some_and(|r| r.selectable);
    ui::clickable(
        div()
            .id("launch-harness")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .py_1(),
        p,
    )
    .on_click(cx.listener(|root, _, _, cx| {
        root.launch.state.step_harness(true);
        cx.notify();
    }))
    .child(
        div()
            .w(px(56.0))
            .flex_none()
            .child(ui::text("harness", Role::Label, p)),
    )
    .child(
        ui::role(div(), Role::Name, p)
            .text_color(rgb(if selectable { p.fg } else { p.faint }))
            .child(l.harness.clone()),
    )
    .children(
        row.filter(|r| !r.selectable)
            .map(|r| ui::text(r.reason.clone(), Role::Hint, p)),
    )
}

/// The shell command a draft carries: a leading `!` (stripped) with
/// non-blank text, from any kind; `None` sends the draft as the launch.
pub(crate) fn shell_command(draft: &Draft) -> Option<String> {
    let text = draft.text().trim();
    let rest = text.strip_prefix('!')?.trim();
    (!rest.is_empty()).then(|| rest.to_string())
}

/// The skill dir names under the two roots Claude Code reads: the user's
/// `~/.claude/skills` and the picked project's `.claude/skills`. A dir
/// without a SKILL.md is not a skill. Missing roots read as none; the
/// read is best-effort and never a submit blocker.
fn skill_names(project: &str) -> Vec<String> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".claude/skills"));
    }
    if !project.is_empty() {
        roots.push(PathBuf::from(project).join(".claude/skills"));
    }
    let mut out = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for e in entries.flatten() {
            if e.path().join("SKILL.md").is_file() {
                if let Some(name) = e.file_name().to_str() {
                    out.push(name.to_string());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The project row's display path: home becomes `~`, and a long path
/// middle-truncates so the row keeps its shape. The request itself always
/// carries the full path.
fn short_project(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    short_project_with(path, &home)
}

fn short_project_with(path: &str, home: &str) -> String {
    let shown = match path.strip_prefix(home) {
        // A sibling directory (`/home2` under home `/home`) never tilde-folds.
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => path.to_string(),
    };
    let n = shown.chars().count();
    if n <= 42 {
        return shown;
    }
    let head: String = shown.chars().take(18).collect();
    let tail: String = shown.chars().skip(n - 20).collect();
    format!("{head}\u{2026}{tail}")
}

fn project_row(l: &Launch, p: &Palette, cx: &mut Context<Root>) -> Stateful<Div> {
    ui::clickable(
        div()
            .id("launch-project")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .py_1(),
        p,
    )
    .on_click(cx.listener(|root, _, _, cx| {
        root.launch.state.step_project(true);
        cx.notify();
    }))
    .child(
        div()
            .w(px(56.0))
            .flex_none()
            .child(ui::text("project", Role::Label, p)),
    )
    .child(ui::clamp(short_project(&l.project), Role::Name, p))
    .when(l.gone, |d| d.child(ui::tag("gone", p.warn, p)))
}

/// The label the model row shows: the pick, or the door default.
fn model_label(l: &Launch) -> String {
    l.model.clone().unwrap_or_else(|| "default".to_string())
}

/// The label the effort row shows: the pick, or the door default.
fn effort_label(l: &Launch) -> String {
    l.effort.clone().unwrap_or_else(|| "default".to_string())
}

/// One picker row: the axis name, the shown value, a click cycles forward.
fn picker_row(
    id: &'static str,
    label: &'static str,
    value: String,
    p: &Palette,
    cx: &mut Context<Root>,
    step: impl Fn(&mut Root, &mut Context<Root>) + 'static,
) -> Stateful<Div> {
    ui::clickable(
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .py_1(),
        p,
    )
    .on_click(cx.listener(move |root, _, _, cx| step(root, cx)))
    .child(
        div()
            .w(px(56.0))
            .flex_none()
            .child(ui::text(label, Role::Label, p)),
    )
    .child(ui::clamp(value, Role::Name, p))
}

/// The agent kind's skill matches under the task box; tab fills the top
/// one. None show unless the draft leads with a `/` and the picked
/// harness lists skills.
fn skill_hints(l: &Launch, p: &Palette) -> Option<Div> {
    let rows = l.skill_completions();
    if rows.is_empty() {
        return None;
    }
    Some(
        div().flex().flex_col().px_2().gap(px(2.0)).children(
            rows.into_iter()
                .map(|s| ui::text(format!("/{s}"), Role::Hint, p)),
        ),
    )
}

/// The lead kind's epic picker under the filter box: up to six matches,
/// the picked ones filled. No filter shows the pool's head, so the
/// picker is never blank.
fn epic_list(l: &Launch, p: &Palette) -> Option<Div> {
    if l.kind != Kind::Lead {
        return None;
    }
    let matches = l.epic_matches();
    if matches.is_empty() {
        return None;
    }
    Some(
        div()
            .flex()
            .flex_col()
            .px_2()
            .gap(px(2.0))
            .children(matches.into_iter().map(|e| {
                let picked = l.epics.contains(&e.id);
                div()
                    .flex()
                    .flex_row()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_color(rgb(if picked { p.accent } else { p.faint }))
                            .child(if picked { "●" } else { "○" }.to_string()),
                    )
                    .child(ui::text(
                        format!("{} {}", e.id, e.title),
                        if picked { Role::Name } else { Role::Fact },
                        p,
                    ))
            })),
    )
}

fn task_box(root: &Root, window: &Window, cx: &mut Context<Root>) -> Stateful<Div> {
    let p = root.palette;
    let l = &root.launch.state;
    let at = l.composer.clone();
    let text = Field {
        draft: l.draft.clone(),
        placeholder: l.kind.placeholder(),
        fg: rgb(p.fg).into(),
        muted: rgb(p.muted).into(),
        selection: (p.accent << 8) | 0x55,
        focused: root.has_focus(window),
        state: l.field.clone(),
    };
    div()
        .id("launch-task")
        .relative()
        .w_full()
        .max_h(px(120.0))
        .overflow_y_scroll()
        .track_scroll(&l.field.scroll)
        .py_1()
        .px_2()
        .rounded(px(6.0))
        .bg(rgb(p.raised))
        .cursor(CursorStyle::IBeam)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|root, ev, window, cx| root.launch_down(ev, window, cx)),
        )
        .on_mouse_move(cx.listener(|root, ev, window, cx| root.launch_drag(ev, window, cx)))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|root, ev, window, cx| root.launch_up(ev, window, cx)),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|root, ev, window, cx| root.launch_up(ev, window, cx)),
        )
        .child(
            canvas(move |bounds, _, _| at.set(bounds), |_, _, _, _| {})
                .absolute()
                .size_full(),
        )
        .child(text)
}

fn phase_line(l: &Launch, p: &Palette) -> Option<Div> {
    match &l.phase {
        Phase::Editing => None,
        Phase::Starting => Some(ui::text("starting…", Role::Hint, p).py_1()),
        Phase::Refused(text) | Phase::Unknown(text) => Some(
            ui::role(div(), Role::Hint, p)
                .text_color(rgb(p.error))
                .py_1()
                .child(text.clone()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fno::proto::SquadMeta;

    fn lead_with_task(task: &str) -> Launch {
        let mut l = Launch::fresh();
        l.kind = Kind::Lead;
        l.harness = "claude".into();
        l.project = "/w".into();
        l.draft.replace_selection(task);
        l
    }

    #[test]
    fn a_lead_submit_seeds_the_picked_epics_and_a_bare_seed_without_any() {
        let mut l = lead_with_task("");
        l.epics = vec!["fno-a1b2".into(), "fno-c3d4".into()];
        let req = l.request(7);
        assert_eq!(req.message, "/fno:lead fno-a1b2 fno-c3d4");
        assert_eq!(req.harness, "claude");
        assert_eq!(req.cwd, "/w");
        assert_eq!(req.substrate, "");
        // The no-scope start: an empty pick seeds bare, whatever the draft.
        let mut l = lead_with_task("ignored");
        assert_eq!(l.request(8).message, "/fno:lead ");
    }

    #[test]
    fn an_agent_submit_sends_bare_text() {
        let mut l = Launch::fresh();
        l.kind = Kind::Agent;
        l.harness = "claude".into();
        l.project = "/w".into();
        l.draft.replace_selection("fix the crash");
        let req = l.request(3);
        assert_eq!(req.message, "fix the crash");
    }

    #[test]
    fn the_row_chain_skips_the_axes_a_kind_does_not_show() {
        let mut l = Launch::fresh();
        // Agent: the full chain wraps task -> kind.
        l.row = Row::Task;
        assert_eq!(l.row.next(false, Kind::Agent), Row::Kind);
        // Shell: kind -> project -> task, no harness or axes.
        l.row = Row::Kind;
        assert_eq!(l.row.next(false, Kind::Shell), Row::Project);
        assert_eq!(l.row.next(false, Kind::Shell), Row::Task);
        assert_eq!(l.row.next(false, Kind::Shell), Row::Kind);
        // Lead: kind -> harness -> project -> task.
        l.row = Row::Project;
        assert_eq!(l.row.next(false, Kind::Lead), Row::Task);
        assert_eq!(l.row.next(false, Kind::Lead), Row::Kind);
        // Backwards from kind lands on the kind's last row.
        l.row = Row::Kind;
        assert_eq!(l.row.next(true, Kind::Shell), Row::Task);
    }

    #[test]
    fn the_model_and_effort_picks_cycle_and_wrap_to_default() {
        // The catalog is cleared: fresh() reads the machine's real table,
        // and the runner's first model must not decide the outcome.
        let mut l = Launch::fresh();
        l.harnesses.clear();
        l.harness = "claude".into();
        l.harnesses.push(HarnessRow {
            name: "claude".into(),
            selectable: true,
            reason: String::new(),
            models: vec!["opus".into(), "sonnet".into()],
            efforts: vec!["low".into(), "high".into()],
        });
        assert_eq!(l.model, None);
        l.step_model(true);
        assert_eq!(l.model.as_deref(), Some("opus"));
        l.step_model(true);
        assert_eq!(l.model.as_deref(), Some("sonnet"));
        l.step_model(true);
        assert_eq!(l.model, None);
        l.step_model(false);
        assert_eq!(l.model.as_deref(), Some("sonnet"));
        // An unknown harness carries no lists; the pick stays None.
        let mut l = Launch::fresh();
        l.harnesses.clear();
        l.harness.clear();
        l.step_model(true);
        assert_eq!(l.model, None);
        l.step_effort(true);
        assert_eq!(l.effort, None);
    }

    #[test]
    fn the_request_carries_the_picked_axes_and_the_worktree_flip() {
        let mut l = Launch::fresh();
        l.kind = Kind::Agent;
        l.harness = "claude".into();
        l.project = "/w".into();
        l.model = Some("opus".into());
        l.effort = Some("high".into());
        l.worktree = true;
        let req = l.request(9);
        assert_eq!(req.model.as_deref(), Some("opus"));
        assert_eq!(req.effort.as_deref(), Some("high"));
        assert!(req.worktree);
        l.step_worktree();
        assert!(!l.request(10).worktree);
    }

    #[test]
    fn the_agent_task_box_autocompletes_the_harness_s_skills() {
        let mut l = Launch::fresh();
        l.kind = Kind::Agent;
        l.harness = "claude".into();
        l.skills = vec!["target".into(), "think".into()];
        l.draft.replace_selection("/ta");
        assert_eq!(l.skill_completions(), vec!["target".to_string()]);
        assert!(l.complete_skill());
        assert_eq!(l.draft.text(), "/target ");
    }

    #[test]
    fn enter_picks_the_top_epic_and_a_repeat_toggles_it_off() {
        let mut l = Launch::fresh();
        l.kind = Kind::Lead;
        l.epic_pool = vec![
            EpicChoice {
                id: "fno-a1b2".into(),
                slug: "mux-index".into(),
                title: "Mux index".into(),
            },
            EpicChoice {
                id: "fno-c3d4".into(),
                slug: "orchestration".into(),
                title: "Orchestration".into(),
            },
        ];
        l.draft.replace_selection("a1");
        l.key(&Key {
            key: "enter",
            ..Key::default()
        });
        assert_eq!(l.epics, vec!["fno-a1b2".to_string()]);
        assert!(l.draft.text().is_empty());
        // The same match again: the pick leaves the list.
        l.draft.replace_selection("a1");
        l.key(&Key {
            key: "enter",
            ..Key::default()
        });
        assert!(l.epics.is_empty());
    }

    #[test]
    fn a_lead_submit_refuses_a_stray_filter_but_seeds_the_picks() {
        let mut l = lead_with_task("mux");
        assert_eq!(
            l.block().as_deref(),
            Some("clear the filter or pick a match")
        );
        l.draft.select_all();
        l.draft.replace_selection("");
        l.epics = vec!["fno-a1b2".into(), "fno-c3d4".into()];
        assert_eq!(l.block(), None);
        assert_eq!(
            l.request(7).message,
            "/fno:lead fno-a1b2 fno-c3d4".to_string()
        );
    }

    #[test]
    fn the_completions_leave_the_other_harnesses_and_kinds_alone() {
        let mut l = Launch::fresh();
        l.harness = "codex".into();
        l.skills = vec!["target".into()];
        l.draft.replace_selection("/ta");
        assert!(l.skill_completions().is_empty());
        assert!(!l.complete_skill());
        // A lead on claude stays silent: no listing surface, no matches.
        l.kind = Kind::Lead;
        l.harness = "claude".into();
        assert!(l.skill_completions().is_empty());
    }

    #[test]
    fn cmd_digits_one_two_three_map_to_the_three_kinds() {
        assert_eq!(Kind::from_index(1), Some(Kind::Agent));
        assert_eq!(Kind::from_index(2), Some(Kind::Lead));
        assert_eq!(Kind::from_index(3), Some(Kind::Shell));
        assert_eq!(Kind::from_index(4), None);
    }

    #[test]
    fn a_shell_command_is_a_leading_bang_with_text_from_any_kind() {
        // Each case starts from a fresh draft: replace_selection appends
        // at the caret, it never resets the text.
        let mut d = Draft::default();
        d.replace_selection("!cargo test");
        assert_eq!(shell_command(&d).as_deref(), Some("cargo test"));

        let mut d = Draft::default();
        d.replace_selection("  !ls -la  ");
        assert_eq!(shell_command(&d).as_deref(), Some("ls -la"));

        let mut d = Draft::default();
        d.replace_selection("fix the bug");
        assert_eq!(shell_command(&d), None);

        let mut d = Draft::default();
        d.replace_selection("!");
        assert_eq!(shell_command(&d), None);

        // The kind does not matter: the bang wins from every kind.
        let mut l = Launch::fresh();
        l.kind = Kind::Lead;
        l.draft.replace_selection("!ls");
        assert_eq!(shell_command(&l.draft).as_deref(), Some("ls"));
    }

    #[test]
    fn a_refusal_shows_and_keeps_the_draft() {
        let mut l = lead_with_task("ship it");
        let _ = l.request(5);
        // A stale attempt's id never touches the panel.
        let stale = l.apply(&AgentLaunchUpdate {
            request_id: 4,
            state: LaunchState::Refused {
                reason: "no".into(),
            },
        });
        assert!(!stale);
        assert_eq!(l.draft.text(), "ship it");
        let refused = l.apply(&AgentLaunchUpdate {
            request_id: 5,
            state: LaunchState::Refused {
                reason: "no harness".into(),
            },
        });
        assert!(refused);
        assert_eq!(l.phase, Phase::Refused("no harness".into()));
        assert_eq!(l.draft.text(), "ship it");
    }

    #[test]
    fn a_workspace_that_left_the_list_refuses_the_submit_in_place() {
        let mut l = Launch::fresh();
        let squad = SquadMeta {
            id: 1,
            name: "w".into(),
            canonical_cwd: "/w".into(),
            tabs: Vec::new(),
            active_tab: 0,
            panes: 0,
        };
        let layout = Layout {
            squads: vec![squad],
            active_squad: 1,
            ..Layout::default()
        };
        l.refresh_projects(&layout);
        assert_eq!(l.project, "/w");
        assert!(!l.gone);
        l.refresh_projects(&Layout::default());
        assert!(l.gone);
        assert_eq!(
            l.block(),
            Some("that workspace closed; pick another project".into())
        );
    }

    #[test]
    fn an_empty_lead_draft_starts_no_scope_freely() {
        // The epic picker replaced the free-typed id: an empty filter
        // submits the bare seed, and stray text refuses in place.
        let mut empty = lead_with_task("");
        assert_eq!(empty.block(), None);
        assert_eq!(empty.request(4).message, "/fno:lead ".to_string());

        let stray = lead_with_task("checkout-2");
        assert_eq!(
            stray.block(),
            Some("clear the filter or pick a match".into())
        );
    }

    #[test]
    fn the_project_row_shortens_home_and_long_paths() {
        assert_eq!(short_project_with("/Users/demo/w", "/Users/demo"), "~/w");
        // A sibling of home never folds.
        assert_eq!(
            short_project_with("/Users/demo2/w", "/Users/demo"),
            "/Users/demo2/w"
        );
        let long = "/tmp/fno-demo.ab/code/checkout";
        assert_eq!(short_project_with(long, "/nope"), long);
        let huge = format!("/{}", "x".repeat(60));
        let got = short_project_with(&huge, "/nope");
        assert_eq!(got.chars().count(), 39);
        assert!(got.contains('\u{2026}'));
    }

    fn roster_row(name: &str, harness: &str, sid: &str) -> AgentRow {
        serde_json::from_value(serde_json::json!({
            "squad": null, "name": name, "pane_id": null, "badge": null,
            "reason": null, "exited": false, "harness": harness,
            "harness_session_id": sid,
        }))
        .unwrap()
    }

    #[test]
    fn the_launched_row_opens_its_stream_only_with_a_session_id() {
        let named = roster_row(
            "t-land-the-fix",
            "claude",
            "7d1e2f3a-4b5c-4d6e-8f70-81a2b3c4d5e6",
        );
        let key = stream_key(&[named], "t-land-the-fix").expect("steers");
        assert_eq!(key.harness, "claude");
        assert_eq!(key.session_id, "7d1e2f3a-4b5c-4d6e-8f70-81a2b3c4d5e6");

        // A row still without its session id (the wrapper has not pinned
        // one yet) holds the steer instead of opening on a guess.
        let no_sid: AgentRow = serde_json::from_value(serde_json::json!({
            "squad": null, "name": "t-land-the-fix", "pane_id": null,
            "badge": null, "reason": null, "exited": false,
        }))
        .unwrap();
        assert!(stream_key(&[no_sid], "t-land-the-fix").is_none());

        // A row the launch did not name never steers.
        let other = roster_row("scout", "claude", "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee");
        assert!(stream_key(&[other], "t-land-the-fix").is_none());
    }
}
