//! The native left pane: workspaces with their agents and shells, one
//! questions row, and the machines. Agents order through the TUI sideline's
//! own comparator, and a click sends the command the sideline click sends.
//! A section whose read failed shows the error and keeps its last good rows.

use std::collections::HashSet;

use gpui::{
    div, prelude::*, px, rgb, rgba, AnyElement, Context, Div, FontWeight, MouseButton, Pixels,
    SharedString, Stateful, Window,
};

use super::app_state::GroupBy;
use super::link::Layout;
use super::palette::Palette;
use super::pr_sheet;
use super::ui;
use super::window::{Action, MainView, Root};
use fno::app_api::client::agent_group::{click_command, RowClick};
use fno::app_api::client::agent_lattice_state;
use fno::app_api::client::card_line::{binding_suffix, card_facts, CardFacts};
use fno::app_api::client::header_band::section_rollup;
use fno::app_api::lattice::{lattice_glyph, status_long, LatticeState};
use fno::app_api::web_hq::Board;
use fno::proto::{AgentRow, Command, PaneMeta, PrStatusFact, SquadMeta};

pub const WIDTH: f32 = 320.0;

/// A workspace's fold key: its canonical cwd, or its name when the cwd is
/// empty (a NewSquad workspace). Squad ids reset with the server, so they
/// are no key. A committed rename of a name-keyed workspace moves its entry.
pub(super) fn fold_key(s: &SquadMeta) -> String {
    if s.canonical_cwd.is_empty() {
        s.name.clone()
    } else {
        s.canonical_cwd.clone()
    }
}

/// The Elsewhere group's fold key, where agents land when their squad is
/// `None` or names a squad the layout does not carry.
pub(super) const ELSEWHERE_KEY: &str = "Elsewhere";

/// A rename's fold re-key: a workspace whose fold key is its name (a
/// cwd-less NewSquad workspace) moves the entry to the new name, so a
/// folded workspace stays folded. A cwd-keyed workspace keeps its key.
/// `None` when nothing moves.
pub(super) fn rename_fold_move(s: &SquadMeta, new_name: &str) -> Option<(String, String)> {
    if !s.canonical_cwd.is_empty() || s.name == new_name {
        return None;
    }
    Some((s.name.clone(), new_name.to_string()))
}

/// What a row says about "where I am". Only the focused agent carries the
/// cursor; the active session and the local machine carry a quiet word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    None,
    Cursor,
    Current(&'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Header {
        title: &'static str,
        /// Per-state counts in severity order; empty for a section with no
        /// states.
        rollup: Vec<(LatticeState, usize)>,
        /// The For-you header's open-question count: ◆ N in the warn color.
        warn: Option<usize>,
        /// The Agents header alone: the circled + opens the composer.
        plus: bool,
        /// The Workspaces header alone: the button that cycles the
        /// grouping (workspaces, state, machine).
        grouping: Option<GroupBy>,
    },
    /// A workspace's header row: the fold caret, the name, the active
    /// mark, and its agents' state rollup. A click on the name selects
    /// the workspace; the caret folds it.
    Workspace {
        id: u64,
        name: String,
        /// The workspace the window's view is on.
        active: bool,
        /// The user's fold choice; absence from the set means open.
        open: bool,
        rollup: Vec<(LatticeState, usize)>,
    },
    /// A fold-able group that is not a workspace: Elsewhere now, the state
    /// and machine bands under the other groupings later.
    Group {
        title: String,
        rollup: Vec<(LatticeState, usize)>,
        open: bool,
    },
    Item {
        label: String,
        detail: String,
        /// The node and PR an agent works on.
        identity: Option<String>,
        /// The agent's PR status fact (the serve's sweep projection);
        /// `None` on non-agent rows. The identity cell renders its verdict
        /// and review tokens.
        pr_fact: Option<PrStatusFact>,
        /// The row's own PR number and URL, so the sheet opens with the
        /// row's data and never a name lookup's (two live rows can share a
        /// name).
        pr: Option<u64>,
        pr_url: Option<String>,
        /// The mux card's served facts; `None` for anything but an agent.
        card: Option<CardFacts>,
        /// An agent's lattice state; `None` for shells, machines, questions.
        state: Option<LatticeState>,
        /// A question: it needs the user.
        question: bool,
        mark: Mark,
        action: Option<Action>,
    },
    /// A failed read. The section's last good rows follow it.
    Error(String),
    /// A section with nothing to show, and why.
    Empty(&'static str),
    /// An agent's last result, one quiet line under its row.
    Note(String),
}

fn header(title: &'static str) -> Row {
    Row::Header {
        title,
        rollup: Vec::new(),
        warn: None,
        plus: false,
        grouping: None,
    }
}

pub(super) fn agent_identity(a: &AgentRow) -> Option<String> {
    let mut out = match (a.node.as_deref(), a.pr) {
        // The PR leads, so a narrow box cuts the node id and keeps the number.
        (Some(n), Some(pr)) => format!("#{pr} · {n}"),
        (Some(n), None) => n.to_string(),
        (None, Some(pr)) => format!("#{pr}"),
        (None, None) => return None,
    };
    out.push_str(&binding_suffix(a));
    Some(out)
}

/// The PR status tokens the identity cell wears before the `#N · node`
/// text, in the app's shape-carries-state + palette-reinforces idiom: the
/// checks verdict first (`✓`, `✗N` with the fail count, `…N` with the
/// pending count), the review mark after (`r✓` reviewed, `r✗` any other
/// named state). An unknown or missing verdict carries no token, and an
/// unknown review word is a state the row treats as not-good (`r✗`), never
/// as reviewed.
fn pr_status_marks(fact: &PrStatusFact, p: &Palette) -> Vec<(String, u32)> {
    let mut marks = Vec::new();
    match fact.verdict.as_str() {
        "green" => marks.push(("✓".to_string(), p.ok)),
        "red" => marks.push((
            if fact.fail > 0 {
                format!("✗{}", fact.fail)
            } else {
                "✗".to_string()
            },
            p.warn,
        )),
        "pending" => marks.push((
            if fact.pending > 0 {
                format!("…{}", fact.pending)
            } else {
                "…".to_string()
            },
            p.faint,
        )),
        _ => {}
    }
    match fact.review_state.as_deref() {
        Some("reviewed") => marks.push(("r✓".to_string(), p.ok)),
        Some("") | None => {}
        Some(_) => marks.push(("r✗".to_string(), p.warn)),
    }
    marks
}

/// The identity cell when a PR fact exists: the tokens, one space each,
/// then the `#N · node` text cut to what the column leaves. Width math
/// mirrors `ui::cell` (fixed `chars` budget, overflow hidden), so the
/// column never grows for the tokens.
fn pr_identity_cell(
    fact: &PrStatusFact,
    identity: Option<&str>,
    chars: usize,
    p: &Palette,
    _adv: Pixels,
) -> Div {
    let marks = pr_status_marks(fact, p);
    // One space after each token, so `✗2 r✓ #24` reads as three words
    // instead of one crammed run; the spaces are part of `marks_w`, so the
    // text cut stays inside the column and the number survives it.
    let marks_w: usize = marks.iter().map(|(t, _)| t.chars().count() + 1).sum();
    let mut inner = div().flex().flex_row();
    for (token, color) in marks {
        inner = inner.child(
            ui::role(div(), ui::Role::Fact, p)
                .text_color(rgb(color))
                .child(format!("{token} ")),
        );
    }
    let rest_w = chars.saturating_sub(marks_w);
    inner.child(ui::text(
        ui::cut(identity.unwrap_or_default(), rest_w),
        ui::Role::Fact,
        p,
    ))
}

/// The identity slot's index in [`slots`]: the last column.
const IDENTITY_SLOT: usize = 5;

/// One agent row's click action: the sideline click command as an app
/// Action. The go-to palette sends the same thing, so a click and a goto
/// never diverge.
pub(super) fn agent_row_action(a: &AgentRow) -> Action {
    match click_command(a) {
        RowClick::Send(cmd) => Action::Send(cmd),
        RowClick::Notice(notice) => Action::Notice(notice),
    }
}

/// The left pane's status word for one agent: `harness · status`, where an
/// ended one-shot's result outranks the lane status.
pub(super) fn agent_status_word(a: &AgentRow) -> String {
    let state = agent_lattice_state(a);
    let harness = a.harness.as_deref().unwrap_or("pane");
    let status = a.result.as_deref().unwrap_or(status_long(state));
    format!("{harness} · {status}")
}

/// One workspace's detail word: `N tab(s) · M pane(s)`, as the sidebar shows.
pub(super) fn workspace_detail(s: &SquadMeta) -> String {
    let tabs = s.tabs.len();
    format!(
        "{tabs} tab{} · {} pane{}",
        if tabs == 1 { "" } else { "s" },
        s.panes,
        if s.panes == 1 { "" } else { "s" }
    )
}

/// The sidebar's agent rows' click actions, in the order they show:
/// cmd-1 picks the first.
pub(super) fn agent_actions(root: &Root) -> Vec<Action> {
    let now = fno::app_api::digest_overlay::now_secs();
    rows(
        &root.layout,
        &root.board,
        now,
        &root.folds,
        root.group_by,
        None,
        None,
    )
    .into_iter()
    .filter_map(|r| match r {
        Row::Item {
            card: Some(_),
            action,
            ..
        } => action,
        _ => None,
    })
    .collect()
}

/// One shell pane's sidebar row: the pane label and its cwd, `$HOME` cut to
/// `~`. A click focuses the pane, the same command the TUI's pane click
/// sends.
fn shell_item(p: &PaneMeta) -> Row {
    let home = std::env::var("HOME").unwrap_or_default();
    let cwd = p
        .cwd
        .as_deref()
        .filter(|_| !home.is_empty())
        .map(|c| c.replacen(&home, "~", 1))
        .unwrap_or_default();
    Row::Item {
        label: p.label.clone(),
        detail: cwd,
        identity: None,
        pr_fact: None,
        pr: None,
        pr_url: None,
        card: None,
        state: None,
        question: false,
        mark: Mark::None,
        action: Some(Action::Send(Command::FocusPane(p.id))),
    }
}

/// One agent's sidebar row.
fn agent_item(
    a: &AgentRow,
    layout: &Layout,
    now_secs: u64,
    streamed: Option<&str>,
    launched: Option<(&str, &str)>,
) -> Row {
    let state = agent_lattice_state(a);
    let label = match launched {
        Some((name, task)) if name == a.name && !task.is_empty() => task.to_string(),
        _ => a.name.clone(),
    };
    Row::Item {
        label,
        detail: agent_status_word(a),
        identity: agent_identity(a),
        pr_fact: a.pr_status.clone(),
        pr: a.pr,
        pr_url: a
            .bindings
            .iter()
            .rev()
            .find(|b| b.pr.is_some())
            .and_then(|b| b.pr_url.clone()),
        card: Some(card_facts(a, now_secs)),
        state: Some(state),
        question: false,
        mark: if match streamed {
            Some(sid) => a.harness_session_id.as_deref() == Some(sid),
            None => a.pane_id == Some(layout.focus),
        } || launched.is_some_and(|(n, _)| n == a.name)
        {
            Mark::Cursor
        } else {
            Mark::None
        },
        action: Some(agent_row_action(a)),
    }
}

/// The row, then its last result when the agent has one.
fn push_agent(
    out: &mut Vec<Row>,
    a: &AgentRow,
    layout: &Layout,
    now_secs: u64,
    streamed: Option<&str>,
    launched: Option<(&str, &str)>,
) {
    out.push(agent_item(a, layout, now_secs, streamed, launched));
    out.extend(last_result_note(a));
}

/// An agent's last result as one line: the one-shot's end word, else the
/// report reason. `None` when it has neither, so a quiet row stays one line.
fn last_result_note(a: &AgentRow) -> Option<Row> {
    let text = a.result.as_deref().or(a.reason.as_deref())?;
    let line = text.lines().next().unwrap_or("").trim();
    (!line.is_empty()).then(|| Row::Note(line.to_string()))
}

/// `streamed` is the session the main view streams, when it streams one: its
/// agent carries the cursor, else the focused pane's agent does. `launched`
/// is the (name, task) the composer launched most recently: that row carries
/// the cursor and the readable task as its label from its first paint, and a
/// row click hands both back to the user. `folds` is the persisted set of
/// fold keys (workspace cwds, or names when the cwd is empty); absence means
/// open, so every workspace opens by default.
pub fn rows(
    layout: &Layout,
    board: &Board,
    now_secs: u64,
    folds: &HashSet<String>,
    group_by: GroupBy,
    streamed: Option<&str>,
    launched: Option<(&str, &str)>,
) -> Vec<Row> {
    let mut agents = fno::app_api::client::agent_sort::sideline_order(&layout.agents, now_secs);
    // A pinned agent leads its group; the sort is stable, so the sideline
    // order holds inside each half.
    agents.sort_by_key(|a| !a.pinned);
    let mut workspaces = header("Workspaces");
    if let Row::Header {
        plus,
        grouping,
        rollup,
        ..
    } = &mut workspaces
    {
        *plus = true;
        *grouping = Some(group_by);
        // The header counts every agent per state, so a blocked agent
        // shows even with every workspace folded.
        *rollup = section_rollup(agents.iter().map(|a| agent_lattice_state(a)));
    }
    let mut out = vec![workspaces];

    // A pane with a harness-carrying agent on it is an agent row and never
    // a shell. The server mints bare panes as `harness: None` rows, so the
    // match runs against the agents only.
    let used: HashSet<u64> = agents
        .iter()
        .filter(|a| a.harness.is_some())
        .filter_map(|a| a.pane_id)
        .collect();
    if group_by != GroupBy::Workspaces {
        push_grouped(
            &mut out, layout, board, &agents, &used, group_by, folds, now_secs, streamed, launched,
        );
    } else {
        push_workspaces(
            &mut out, layout, &agents, &used, folds, now_secs, streamed, launched,
        );
    }

    push_questions(&mut out, board);
    push_machines(&mut out, board);
    out
}

/// The Workspaces grouping: each squad's header, its agents, then its
/// shells, with the Elsewhere group trailing.
#[allow(clippy::too_many_arguments)]
fn push_workspaces(
    out: &mut Vec<Row>,
    layout: &Layout,
    agents: &[&AgentRow],
    used: &HashSet<u64>,
    folds: &HashSet<String>,
    now_secs: u64,
    streamed: Option<&str>,
    launched: Option<(&str, &str)>,
) {
    for s in &layout.squads {
        let squad_agents: Vec<&AgentRow> = agents
            .iter()
            .filter(|a| a.squad == Some(s.id) && a.harness.is_some())
            .copied()
            .collect();
        let shells: Vec<&PaneMeta> = s
            .tabs
            .iter()
            .flat_map(|t| t.panes.iter())
            .filter(|p| !used.contains(&p.id))
            .collect();
        let open = !folds.contains(&fold_key(s));
        out.push(Row::Workspace {
            id: s.id,
            name: s.name.clone(),
            active: s.id == layout.active_squad,
            open,
            rollup: section_rollup(squad_agents.iter().map(|a| agent_lattice_state(a))),
        });
        if open {
            if squad_agents.is_empty() && shells.is_empty() {
                out.push(Row::Empty("no agents in this workspace"));
            }
            for a in &squad_agents {
                push_agent(out, a, layout, now_secs, streamed, launched);
            }
            for p in shells {
                out.push(shell_item(p));
            }
        }
    }

    // Agents outside every carried squad: a remote-fleet row, or one whose
    // workspace this server does not know. Shown only when non-empty.
    let elsewhere: Vec<&AgentRow> = agents
        .iter()
        .filter(|a| a.harness.is_some() && !layout.squads.iter().any(|s| a.squad == Some(s.id)))
        .copied()
        .collect();
    if !elsewhere.is_empty() {
        let open = !folds.contains(ELSEWHERE_KEY);
        out.push(Row::Group {
            title: ELSEWHERE_KEY.to_string(),
            rollup: section_rollup(elsewhere.iter().map(|a| agent_lattice_state(a))),
            open,
        });
        if open {
            for a in &elsewhere {
                push_agent(out, a, layout, now_secs, streamed, launched);
            }
        }
    }
}

/// The one questions row and a failed read's error line under it.
fn push_questions(out: &mut Vec<Row>, board: &Board) {
    // One row for the open questions; the list itself lives in the HQ view
    // the row opens. A failed read keeps its error line under the row.
    let questions = board.questions_newest_first();
    let (qlabel, qdetail) = match (board.read_yet(), board.questions_error()) {
        (false, _) => ("Questions".to_string(), "reading…".to_string()),
        (true, Some(e)) => ("Questions".to_string(), e.to_string()),
        (true, None) => (format!("Questions {}", questions.len()), String::new()),
    };
    out.push(Row::Item {
        label: qlabel,
        detail: qdetail,
        identity: None,
        pr_fact: None,
        pr: None,
        pr_url: None,
        card: None,
        state: None,
        question: false,
        mark: Mark::None,
        action: Some(Action::Show(MainView::Orchestration)),
    });
    if let (true, Some(e)) = (board.read_yet(), board.questions_error()) {
        out.push(Row::Error(e.to_string()));
    }
}

/// The Machines section: the fleet's hosts with their seat counts.
fn push_machines(out: &mut Vec<Row>, board: &Board) {
    // Before the first read, a section waits; after it, a failure is an error.
    let failed = |e: Option<&str>| -> Option<Row> {
        match (board.read_yet(), e) {
            (false, _) => Some(Row::Empty("reading…")),
            (true, Some(e)) => Some(Row::Error(e.to_string())),
            (true, None) => None,
        }
    };
    out.push(header("Machines"));
    out.extend(failed(board.fleet_error()));
    for m in board
        .fleet()
        .map(|f| f.machines.as_slice())
        .unwrap_or_default()
    {
        let seats: usize = m.leads.iter().map(|l| l.seats.len()).sum();
        let fresh = if m.fresh {
            "live".to_string()
        } else {
            format!("seen {}s ago", m.age_s)
        };
        out.push(Row::Item {
            label: m.display_name.clone().unwrap_or_else(|| m.host.clone()),
            detail: format!(
                "{seats} seat{} · {fresh}",
                if seats == 1 { "" } else { "s" }
            ),
            identity: None,
            pr_fact: None,
            pr: None,
            pr_url: None,
            card: None,
            state: None,
            question: false,
            mark: if m.local {
                Mark::Current("this machine")
            } else {
                Mark::None
            },
            action: None,
        });
    }
}

/// The State and Machine bands. The Workspaces grouping nests by squad; the
/// other two flatten into bands whose headers fold by the same rule as
/// workspace headers. The Machine axis joins the Board's fleet: the
/// machine whose seats name the agent's session id, else the local
/// machine. Shells join on `PaneMeta.host`, else the local machine's name.
#[allow(clippy::too_many_arguments)]
fn push_grouped(
    out: &mut Vec<Row>,
    layout: &Layout,
    board: &Board,
    agents: &[&AgentRow],
    used: &HashSet<u64>,
    group_by: GroupBy,
    folds: &HashSet<String>,
    now_secs: u64,
    streamed: Option<&str>,
    launched: Option<(&str, &str)>,
) {
    let local = local_machine_name(board);
    let band_of = |a: &AgentRow| -> String {
        match group_by {
            GroupBy::State => match agent_lattice_state(a) {
                LatticeState::Blocked | LatticeState::DoneUnseen => "Needs input",
                LatticeState::Working | LatticeState::Unmeasured | LatticeState::Empty => "Working",
                LatticeState::Idle | LatticeState::Exited => "Done",
            }
            .to_string(),
            GroupBy::Machine => machine_of(board, a),
            GroupBy::Workspaces => String::new(),
        }
    };
    // First-appearance band order, then the agents of each band in
    // sideline order.
    let mut bands: Vec<(String, Vec<&AgentRow>)> = Vec::new();
    for a in agents.iter().copied() {
        let key = band_of(a);
        match bands.iter_mut().find(|(k, _)| *k == key) {
            Some((_, members)) => members.push(a),
            None => bands.push((key, vec![a])),
        }
    }
    for (title, members) in &bands {
        let open = !folds.contains(title.as_str());
        out.push(Row::Group {
            title: title.clone(),
            rollup: section_rollup(members.iter().map(|a| agent_lattice_state(a))),
            open,
        });
        if !open {
            continue;
        }
        for a in members {
            let mut row = agent_item(a, layout, now_secs, streamed, launched);
            if group_by == GroupBy::State {
                // The band carries the state, so the row's detail word
                // gains the workspace the agent runs in.
                let ws = layout
                    .squads
                    .iter()
                    .find(|s| a.squad == Some(s.id))
                    .map(|s| s.name.clone());
                if let (Some(name), Row::Item { detail, .. }) = (ws, &mut row) {
                    if !name.is_empty() && !detail.is_empty() {
                        detail.push_str(" · ");
                        detail.push_str(&name);
                    }
                }
            }
            out.push(row);
            out.extend(last_result_note(a));
        }
        if group_by == GroupBy::Machine {
            push_machine_shells(out, layout, used, title, &local);
        }
    }
}

/// The local machine's own word: the Machines section's name for itself.
fn local_machine_name(board: &Board) -> String {
    board
        .fleet()
        .and_then(|f| f.machines.iter().find(|m| m.local))
        .map(|m| m.display_name.clone().unwrap_or_else(|| m.host.clone()))
        .unwrap_or_else(|| "this machine".to_string())
}

/// The machine an agent joins: the fleet machine whose seats name the
/// agent's session id, else the local machine.
fn machine_of(board: &Board, a: &AgentRow) -> String {
    if let Some(f) = board.fleet() {
        for m in &f.machines {
            for lead in &m.leads {
                for seat in &lead.seats {
                    if seat.session_id.is_some() && seat.session_id == a.harness_session_id {
                        return m.display_name.clone().unwrap_or_else(|| m.host.clone());
                    }
                }
            }
        }
    }
    local_machine_name(board)
}

/// One band's shells under the Machine grouping: panes matching no harness
/// agent, joined to their band by `PaneMeta.host`, else the local name.
fn push_machine_shells(
    out: &mut Vec<Row>,
    layout: &Layout,
    used: &HashSet<u64>,
    title: &str,
    local: &str,
) {
    for s in &layout.squads {
        for pane in s.tabs.iter().flat_map(|t| t.panes.iter()) {
            if used.contains(&pane.id) {
                continue;
            }
            if pane.host.as_deref().unwrap_or(local) == title {
                out.push(shell_item(pane));
            }
        }
    }
}

/// An agent row's fact slots in their fixed order: model, cost, context,
/// tokens, session age, then the node and PR. A fact that arrives late
/// fills its own slot and never pushes another out.
fn slots(card: &CardFacts, identity: Option<&str>) -> [Option<String>; SLOTS] {
    [
        card.model.clone(),
        // The cost slot stays empty: rows show no cost, and a slot no
        // agent fills takes no column.
        None,
        card.context.clone(),
        card.tokens.clone(),
        card.created.as_ref().map(|c| format!("up {c}")),
        identity.map(str::to_string),
    ]
}

const SLOTS: usize = 6;
/// Slots whose numbers align right down the column.
const RIGHT: [bool; SLOTS] = [false, true, true, true, true, false];
/// The activity age column, in small advances.
const AGE_W: usize = 4;
/// Cells between two fact columns, so the widest values never touch.
const COL_GAP: usize = 2;
/// The name column's bounds, in body advances.
const NAME_MIN: usize = 6;

/// The room the name column can take, in body advances: the pane width
/// less the padding, the glyph column, the age column and the gaps. The
/// facts share whatever the name leaves, so this is the only cut bound a
/// name reads.
fn name_room(adv: Pixels, small: Pixels, gap: Pixels) -> usize {
    let inside = px(WIDTH) - px(24.0) - adv * 2.0 - small * (AGE_W + 1) as f32 - gap * 3.0;
    ((inside / adv).floor() as usize).max(NAME_MIN)
}

/// The fact columns every agent row shares: each shown slot's index and
/// width, the widest value any shown agent has there. A slot no agent fills
/// takes no room, and the slots that do not fit `budget` small advances
/// drop whole from the right.
pub fn columns(rows: &[Row], budget: usize) -> Vec<(usize, usize)> {
    let mut widths = [0usize; SLOTS];
    for r in rows {
        if let Row::Item {
            card: Some(card),
            identity,
            ..
        } = r
        {
            for (w, s) in widths.iter_mut().zip(slots(card, identity.as_deref())) {
                *w = (*w).max(s.map_or(0, |s| s.chars().count()));
            }
        }
    }
    let used: Vec<(usize, usize)> = widths
        .into_iter()
        .enumerate()
        .filter(|(_, w)| *w > 0)
        .collect();
    let shown = ui::fit(
        &used.iter().map(|(_, w)| *w).collect::<Vec<_>>(),
        budget,
        COL_GAP,
    );
    used.into_iter().take(shown).collect()
}

/// The glyph's color. Shape carries the state; color only reinforces it.
fn state_color(p: &Palette, s: LatticeState) -> u32 {
    match s {
        LatticeState::Blocked => p.warn,
        LatticeState::DoneUnseen => p.ok,
        LatticeState::Working => p.fg,
        _ => p.faint,
    }
}

/// The mark a pinned agent's row carries after its name.
const PIN_MARK: &str = "↑";

/// An agent row's cells: the name column, the fact columns every agent row
/// shares, and how long ago it last did anything.
fn agent_cells(
    item: Stateful<Div>,
    card: &CardFacts,
    identity: Option<&str>,
    pr_fact: Option<&PrStatusFact>,
    (label, name_w, weight): (String, usize, FontWeight),
    cols: &[(usize, usize)],
    p: &Palette,
    adv: Pixels,
) -> Stateful<Div> {
    let small = adv * (ui::SMALL / ui::BODY);
    let facts = slots(card, identity);
    let mut line = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_row()
        .gap(small * COL_GAP as f32);
    for (slot, w) in cols {
        if *slot == IDENTITY_SLOT {
            if let Some(fact) = pr_fact {
                line = line.child(pr_identity_cell(
                    fact,
                    facts.get(*slot).cloned().flatten().as_deref(),
                    *w,
                    p,
                    small,
                ));
                continue;
            }
        }
        line = line.child(ui::cell(
            facts.get(*slot).cloned().flatten().unwrap_or_default(),
            *w,
            RIGHT.get(*slot).copied().unwrap_or(false),
            ui::Role::Fact,
            p,
            small,
        ));
    }
    item.child(
        div()
            // One small cell past the name, on top of the row gap, keeps the
            // longest name off the first fact.
            .w(adv * name_w as f32 + small)
            .flex_none()
            .overflow_hidden()
            .flex()
            .flex_row()
            .child(
                ui::role(div(), ui::Role::Name, p)
                    .font_weight(weight)
                    .child(ui::cut(
                        &label,
                        name_w.saturating_sub(2 * usize::from(card.pinned)),
                    )),
            )
            .children(
                card.pinned
                    .then(|| ui::text(PIN_MARK, ui::Role::Fact, p).pl(small)),
            ),
    )
    .child(line)
    .child(ui::cell(
        card.activity.clone().unwrap_or_default(),
        AGE_W,
        true,
        ui::Role::Fact,
        p,
        small,
    ))
}

/// A question, a session or a machine: the title takes the room, and one
/// quiet fact sits at the right edge. Mono cells measure exactly, so the
/// title gets what the fact, the tag and the gaps leave, cut whole.
fn plain_cells(
    item: Stateful<Div>,
    label: &str,
    detail: &str,
    mark: Mark,
    p: &Palette,
    adv: Pixels,
) -> Stateful<Div> {
    let (small, gap) = (adv * (ui::SMALL / ui::BODY), px(8.0));
    let detail = ui::cut(detail, 18);
    let word = match mark {
        Mark::Current(word) => Some(word),
        _ => None,
    };
    let mut taken = px(24.0) + adv * 2.0 + gap * 2.0;
    taken += small * detail.chars().count() as f32;
    if let Some(w) = word {
        taken += small * w.len() as f32 + px(10.0) + gap;
    }
    let room = ((px(WIDTH) - taken) / adv).floor().max(4.0) as usize;
    let mut d = item.child(ui::clamp(ui::cut(label, room), ui::Role::Name, p));
    if let Some(w) = word {
        d = d.child(ui::tag(w, p.muted, p));
    }
    d.child(
        ui::role(div(), ui::Role::Fact, p)
            .flex_none()
            .whitespace_nowrap()
            .child(detail),
    )
}

/// What every sidebar row lays out against: the palette, one body advance,
/// the agent name column and the shared fact columns.
struct Grid {
    p: Palette,
    adv: Pixels,
    name_w: usize,
    cols: Vec<(usize, usize)>,
    /// Per row, the cmd-digit badge it shows while cmd is held.
    badges: Vec<Option<usize>>,
}

/// An agent row's cells. While cmd is held, `badge` sits over the age at
/// the row's right end.
fn agent_row(
    item: Stateful<Div>,
    mut card: CardFacts,
    identity: Option<&str>,
    pr_fact: Option<&PrStatusFact>,
    name: (String, usize, FontWeight),
    badge: Option<usize>,
    grid: &Grid,
    p: &Palette,
) -> Stateful<Div> {
    if badge.is_some() {
        card.activity = None;
    }
    agent_cells(
        item, &card, identity, pr_fact, name, &grid.cols, p, grid.adv,
    )
    .relative()
    .children(badge.map(|n| {
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .right(px(12.0))
            .flex()
            .items_center()
            .child(ui::badge(n, &p))
    }))
}

/// A workspace or group header row: a hairline above it and an 8px gap,
/// except for the first one, which sits right under the section header
/// (row 0 is the section header, row 1 the first band).
fn band_row(i: usize, p: &Palette) -> Stateful<Div> {
    let border = p.border;
    ui::row(i, false, p).w(px(WIDTH)).when(i > 1, move |d| {
        d.mt(px(8.0)).border_t_1().border_color(rgb(border))
    })
}

/// The Workspaces header's group button: one glyph per mode, and a click
/// cycles workspaces -> state -> machine -> workspaces.
fn grouping_button(mode: GroupBy, p: &Palette, cx: &mut Context<Root>) -> Div {
    let glyph = match mode {
        GroupBy::Workspaces => "▤",
        GroupBy::State => "◔",
        GroupBy::Machine => "▣",
    };
    div().child(
        ui::clickable(div().id("grouping-cycle").px_1(), p)
            .on_click(cx.listener(|root, _, _, cx| root.act(Action::CycleGrouping, cx)))
            .child(div().text_color(rgb(p.faint)).child(glyph.to_string())),
    )
}

/// The Workspaces header's right side: the circled + names a new workspace
/// inline, under the header.
fn workspaces_header_plus(p: &Palette, cx: &mut Context<Root>) -> Div {
    div().child(
        ui::clickable(div().id("workspaces-plus").px_1(), p)
            .on_click(cx.listener(|root, _, _, cx| root.act(Action::NewWorkspace, cx)))
            .child(div().text_color(rgb(p.faint)).child("⊕".to_string())),
    )
}

/// A fold caret glyph: `▾` open, `▸` folded, one step larger than the
/// state glyphs so the fold reads first.
fn caret_glyph(open: bool) -> &'static str {
    if open {
        "▾"
    } else {
        "▸"
    }
}

/// The state rollup glyphs one header carries, with the fold caret at the
/// right end. The caret is its own click target: it folds without
/// selecting the workspace.
fn header_marks(
    rollup: &[(LatticeState, usize)],
    open: bool,
    fold: Option<u64>,
    add: Option<u64>,
    p: &Palette,
    cx: &mut Context<Root>,
) -> Div {
    let mut marks = div().flex().flex_row().items_center().gap(px(8.0));
    marks = marks.children(rollup_counts(rollup, p));
    if let Some(squad) = add {
        // The circled + opens the add menu: a new worker or a new shell
        // under this workspace. It sits faint; hover and the current mark
        // lift it.
        marks = marks.child(
            ui::clickable(
                div().id(SharedString::from(format!("add-{squad}"))).px_1(),
                p,
            )
            .on_click(cx.listener(move |root, _, _, cx| {
                cx.stop_propagation();
                root.act(Action::OpenAddMenu(squad), cx);
            }))
            .child(div().text_color(rgb(p.faint)).child("⊕".to_string())),
        );
    }
    match fold {
        Some(id) => marks.child(
            ui::clickable(div().id(("fold", id)).px_1(), p)
                .on_click(cx.listener(move |root, _, _, cx| {
                    cx.stop_propagation();
                    root.act(Action::ToggleWorkspace(id), cx);
                }))
                .child(div().text_color(rgb(p.muted)).child(caret_glyph(open))),
        ),
        None => marks.child(div().text_color(rgb(p.muted)).child(caret_glyph(open))),
    }
}

/// One workspace's header row: the name selects the workspace, the active
/// one carries the quiet mark, the caret folds, and the rollup counts its
/// agents per state.
fn workspace_header(
    i: usize,
    id: u64,
    name: &str,
    active: bool,
    open: bool,
    rollup: &[(LatticeState, usize)],
    p: &Palette,
    cx: &mut Context<Root>,
) -> Stateful<Div> {
    ui::clickable(band_row(i, p), p)
        .on_click(
            cx.listener(move |root, _, _, cx| root.act(Action::Send(Command::SelectSquad(id)), cx)),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |root, _, _, cx| root.act(Action::OpenWorkspaceMenu(id), cx)),
        )
        .child(
            ui::role(div(), ui::Role::Label, p)
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .font_weight(FontWeight::SEMIBOLD)
                .child(ui::cut(name, 24)),
        )
        .when(active, |left| left.child(ui::tag("current", p.muted, p)))
        .child(header_marks(rollup, open, Some(id), Some(id), p, cx))
}

/// One fold-able group that is not a workspace: the whole header toggles
/// the fold, there is no select and no menu.
fn group_header(
    i: usize,
    title: &str,
    rollup: &[(LatticeState, usize)],
    open: bool,
    p: &Palette,
    cx: &mut Context<Root>,
) -> Stateful<Div> {
    ui::clickable(band_row(i, p), p)
        .on_click(cx.listener(|root, _, _, cx| root.act(Action::ToggleElsewhere, cx)))
        .child(
            ui::role(div(), ui::Role::Label, p)
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_string()),
        )
        .child(header_marks(rollup, open, None, None, p, cx))
}

/// The inline workspace-name field: one row under the Workspaces header
/// carrying the text field. Enter commits, Esc cancels; the key path lives
/// on the window, the text routing in its input handler.
fn squad_field_row(
    field: &super::window::SquadField,
    root: &Root,
    window: &Window,
) -> Stateful<Div> {
    let p = root.palette;
    let text = super::chat::field::Field {
        draft: field.draft.clone(),
        placeholder: "workspace name".into(),
        fg: rgb(p.fg).into(),
        muted: rgb(p.muted).into(),
        selection: (p.accent << 8) | 0x55,
        focused: root.has_focus(window),
        state: field.field.clone(),
    };
    ui::row("squad-field-row", false, &p)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .py_1()
                .px_2()
                .rounded(px(4.0))
                .bg(rgb(p.raised))
                .child(text),
        )
        .child(ui::kbd("⏎", &p))
        .child(ui::text("name", ui::Role::Hint, &p))
}

/// A rollup's per-state counts: the state glyph in its color, then the
/// number.
fn rollup_counts(rollup: &[(LatticeState, usize)], p: &Palette) -> Vec<Div> {
    rollup
        .iter()
        .map(|(state, n)| {
            ui::role(div(), ui::Role::Fact, p)
                .flex()
                .flex_row()
                .gap(px(3.0))
                .child(
                    div()
                        .text_color(rgb(state_color(p, *state)))
                        .child(lattice_glyph(*state).0.to_string()),
                )
                .child(n.to_string())
        })
        .collect()
}

/// An item row's leading cell: an agent shows its harness initial with
/// the state glyph as a badge on the corner; every other row keeps the
/// bare glyph.
fn lead_cell(initial: Option<String>, glyph: String, color: u32, adv: Pixels, p: &Palette) -> Div {
    match initial {
        Some(initial) => div()
            .relative()
            .w(adv * 2.0)
            .flex_none()
            .text_color(rgb(p.fg))
            .child(initial)
            .child(
                div()
                    .absolute()
                    .bottom(px(-2.0))
                    .right(px(2.0))
                    .text_size(px(ui::SMALL - 3.0))
                    .text_color(rgb(color))
                    .child(glyph),
            ),
        None => div()
            .w(adv * 2.0)
            .flex_none()
            .text_color(rgb(color))
            .child(glyph),
    }
}

/// An agent's last result: one quiet line under its row, cut with an
/// ellipsis at the pane edge.
fn note_row(i: usize, text: String, p: &Palette) -> Stateful<Div> {
    div()
        .id(i)
        .w(px(WIDTH))
        .px_3()
        .pl(px(WIDTH * 0.08 + 12.0))
        .pb_1()
        .child(
            ui::text(text, ui::Role::Fact, p)
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis(),
        )
}

/// A section header: the title, its state counts, the open-question count,
/// and the Workspaces header's two buttons.
#[allow(clippy::too_many_arguments)]
fn header_row(
    i: usize,
    title: &'static str,
    rollup: &[(LatticeState, usize)],
    warn: Option<usize>,
    plus: bool,
    grouping: Option<GroupBy>,
    p: &Palette,
    cx: &mut Context<Root>,
) -> Stateful<Div> {
    div().id(i).px_3().child(
        ui::section(title, p).justify_between().child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .children(rollup_counts(rollup, p))
                .children(warn.map(|n| {
                    ui::role(div(), ui::Role::Fact, p)
                        .flex()
                        .flex_row()
                        .gap(px(3.0))
                        .child(div().text_color(rgb(p.warn)).child("◆"))
                        .child(n.to_string())
                }))
                .children(grouping.map(|mode| grouping_button(mode, p, cx)))
                .when(plus, |right| right.child(workspaces_header_plus(p, cx))),
        ),
    )
}

/// An item row's leading cell. Shape carries the state: a triangle for
/// blocked, a circle for work. A question leads with a diamond, never the `?`
/// an unmeasured agent wears, so none leans on color. An agent row leads with
/// its harness mark (the harness word's initial) and the state glyph as a
/// badge on its corner; every other row keeps the bare glyph.
fn item_lead(
    state: Option<LatticeState>,
    question: bool,
    agent_detail: Option<&String>,
    adv: Pixels,
    p: &Palette,
) -> Div {
    let (glyph, glyph_color) = match (state, question) {
        (Some(s), _) => (lattice_glyph(s).0.to_string(), state_color(p, s)),
        (None, true) => ("◆".to_string(), p.warn),
        (None, false) => (String::new(), p.faint),
    };
    let harness_initial = agent_detail
        .and_then(|d| d.split(" · ").next())
        .and_then(|h| h.chars().next())
        .map(|c| c.to_uppercase().to_string());
    lead_cell(harness_initial, glyph, glyph_color, adv, p)
}

/// One sidebar row as an element.
fn row_element(i: usize, row: Row, grid: &Grid, cx: &mut Context<Root>) -> Stateful<Div> {
    let (p, adv) = (grid.p, grid.adv);
    match row {
        Row::Header {
            title,
            rollup,
            warn,
            plus,
            grouping,
        } => header_row(i, title, &rollup, warn, plus, grouping, &p, cx),
        Row::Error(e) => div()
            .id(i)
            .px_3()
            .py_1()
            .child(ui::text(e, ui::Role::Fact, &p).text_color(rgb(p.error))),
        Row::Empty(why) => div()
            .id(i)
            .px_3()
            .py_1()
            .child(ui::text(why, ui::Role::Fact, &p)),
        Row::Note(text) => note_row(i, text, &p),
        Row::Workspace {
            id,
            name,
            active,
            open,
            rollup,
        } => workspace_header(i, id, &name, active, open, &rollup, &p, cx),
        Row::Group {
            title,
            rollup,
            open,
        } => group_header(i, &title, &rollup, open, &p, cx),
        Row::Item {
            label,
            detail,
            identity,
            pr_fact,
            pr,
            pr_url,
            card,
            state,
            question,
            mark,
            action,
        } => {
            let cursor = mark == Mark::Cursor;
            let blocked = state == Some(LatticeState::Blocked);
            let lead = item_lead(state, question, card.is_some().then_some(&detail), adv, &p);
            // Text on the needs fill takes the lifted levels, so tok and
            // up read AA there.
            let tinted = blocked && !cursor;
            let row_p = if tinted { p.on_needs() } else { p };
            let agent_name = card.as_ref().map(|_| label.clone());
            // The sheet's data rides the row, so the listener carries its
            // own name copy: the name tuple below moves `label`.
            let label_for_sheet = label.clone();
            let mut item = ui::row(i, cursor, &p)
                .w(px(WIDTH))
                .when(tinted, |d| d.bg(rgb(p.needs_fill())))
                .child(lead);
            let weight = if blocked || cursor {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::MEDIUM
            };
            item = match card {
                Some(card) => {
                    let badge = grid.badges.get(i).copied().flatten();
                    let name = (label, grid.name_w, weight);
                    agent_row(
                        item,
                        card,
                        identity.as_deref(),
                        pr_fact.as_ref(),
                        name,
                        badge,
                        grid,
                        &row_p,
                    )
                }
                None => plain_cells(item, &label, &detail, mark, &p, adv),
            };
            item = agent_menu_on_right_click(item, agent_name, cx);
            item = row_click_action(item, &p, cx, pr_fact, pr, pr_url, label_for_sheet, action);
            item
        }
    }
}

/// A row's click: a press that leaves the row before release does
/// nothing; a plain click keeps the row's own action. Alt-click on a row
/// whose agent carries a PR fact opens its read-only sheet instead.
fn row_click_action(
    item: Stateful<Div>,
    p: &Palette,
    cx: &mut Context<Root>,
    pr_fact: Option<PrStatusFact>,
    pr: Option<u64>,
    pr_url: Option<String>,
    label_for_sheet: String,
    action: Option<Action>,
) -> Stateful<Div> {
    let Some(action) = action else {
        return item;
    };
    let openable = pr_fact.is_some();
    ui::clickable(item, &p).on_click(cx.listener(move |root, ev: &gpui::ClickEvent, _, cx| {
        if openable && ev.modifiers().alt {
            if let Some(num) = pr {
                root.pr_sheet = Some(pr_sheet::PrSheet {
                    pr: num,
                    url: pr_url.clone(),
                    fact: pr_fact.clone().unwrap_or_default(),
                    agent: label_for_sheet.clone(),
                    slug: pr_sheet::slug_from_url(pr_url.as_deref()),
                });
                cx.notify();
            }
            return;
        }
        root.launch.show = None;
        root.launch.show_task = None;
        root.act(action.clone(), cx)
    }))
}

/// A right-click on an agent row opens that agent's menu at the pointer.
fn agent_menu_on_right_click(
    item: Stateful<Div>,
    name: Option<String>,
    cx: &mut Context<Root>,
) -> Stateful<Div> {
    let Some(name) = name else { return item };
    item.on_mouse_down(
        MouseButton::Right,
        cx.listener(move |root, ev: &gpui::MouseDownEvent, window, cx| {
            root.agent_menu_open(&name, ev.position, window, cx)
        }),
    )
}

/// One menu item row: the label, and the action a click sends.
fn menu_item(
    key: &'static str,
    squad: u64,
    label: &str,
    action: Option<Action>,
    p: &Palette,
    cx: &mut Context<Root>,
) -> AnyElement {
    let row = ui::clickable(
        ui::row(SharedString::from(format!("{key}-{squad}")), false, p).w(px(WIDTH - 12.0)),
        p,
    );
    match action {
        Some(action) => row
            .on_click(cx.listener(move |root, _, _, cx| root.act(action.clone(), cx)))
            .child(ui::text(label.to_string(), ui::Role::Name, p))
            .into_any_element(),
        // A greyed item keeps its row: the move items at the list edges.
        None => row
            .child(ui::text(label.to_string(), ui::Role::Name, p).text_color(rgb(p.faint)))
            .into_any_element(),
    }
}

/// The workspace's in-place menu, right under its header: the context
/// menu's five items, or the add menu's two. Move items grey out at the
/// list edges, as the reference shows.
fn menu_rows(
    squad: u64,
    layout: &Layout,
    kind: Option<(u64, bool)>,
    p: &Palette,
    cx: &mut Context<Root>,
) -> Vec<AnyElement> {
    let add_menu = matches!(kind, Some((s, true)) if s == squad);
    let idx = layout
        .squads
        .iter()
        .position(|s| s.id == squad)
        .unwrap_or(0);
    let count = layout.squads.len();
    if add_menu {
        vec![
            menu_item(
                "menu-new-worker",
                squad,
                "New worker",
                Some(Action::NewWorker(squad)),
                p,
                cx,
            ),
            menu_item(
                "menu-new-shell",
                squad,
                "New shell",
                Some(Action::NewShell(squad)),
                p,
                cx,
            ),
        ]
    } else {
        vec![
            menu_item(
                "menu-newtab",
                squad,
                "New Tab",
                Some(Action::NewTabIn(squad)),
                p,
                cx,
            ),
            menu_item(
                "menu-rename",
                squad,
                "Rename Workspace…",
                Some(Action::RenameWorkspace(squad)),
                p,
                cx,
            ),
            menu_item(
                "menu-moveup",
                squad,
                "Move Workspace Up",
                (idx > 0).then_some(Action::MoveWorkspace(squad, -1)),
                p,
                cx,
            ),
            menu_item(
                "menu-movedown",
                squad,
                "Move Workspace Down",
                (idx < count - 1).then_some(Action::MoveWorkspace(squad, 1)),
                p,
                cx,
            ),
            menu_item(
                "menu-close",
                squad,
                "Close Workspace",
                Some(Action::RemoveWorkspace(squad)),
                p,
                cx,
            ),
        ]
    }
}

/// The close-workspace confirm: a small sheet over a dimmed window, in the
/// TUI's own wording, with the pane count it names.
pub(super) fn confirm_sheet(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let Some(c) = root.workspace_confirm.as_ref() else {
        return div();
    };
    let squad = c.squad;
    let word = if c.last {
        format!(
            "close workspace {} ({} panes) - last workspace, ends the session?",
            c.name, c.panes
        )
    } else {
        format!("close workspace {} ({} panes)?", c.name, c.panes)
    };
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x0000_0066))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|root, _, _, cx| root.act(Action::KeepWorkspace, cx)),
        )
        .child(
            div()
                .id("workspace-confirm")
                .flex()
                .flex_col()
                .w(px(420.0))
                .gap_2()
                .p_3()
                .rounded(px(8.0))
                .bg(rgb(p.raised))
                .child(ui::text(word, ui::Role::Name, &p))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            ui::button("close-workspace-button", "Close", ui::Button::Primary, &p)
                                .on_click(cx.listener(move |root, _, _, cx| {
                                    root.act(Action::ConfirmCloseWorkspace(squad), cx)
                                })),
                        )
                        .child(
                            ui::button("keep-workspace-button", "Keep", ui::Button::Quiet, &p)
                                .on_click(cx.listener(|root, _, _, cx| {
                                    root.act(Action::KeepWorkspace, cx)
                                })),
                        ),
                ),
        )
}

/// `adv` is one SF Mono advance at the body size.
pub(super) fn render(
    root: &Root,
    now_secs: u64,
    adv: Pixels,
    window: &Window,
    cx: &mut Context<Root>,
) -> Stateful<Div> {
    let p = root.palette;
    let small = adv * (ui::SMALL / ui::BODY);
    // The rows scroll inside a body under a fixed top band, so scrolled rows
    // never slide under the traffic lights the transparent title bar draws.
    let mut pane = div()
        .id("left-pane-body")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .pb_2();
    let shown = match root.main {
        MainView::Chat => root.chat.key.as_ref().map(|k| k.session_id.as_str()),
        _ => None,
    };
    let launched = root
        .launch
        .show
        .as_deref()
        .zip(root.launch.show_task.as_deref());
    let rows = rows(
        &root.layout,
        &root.board,
        now_secs,
        &root.folds,
        root.group_by,
        shown,
        launched,
    );
    let max_name = rows
        .iter()
        .filter_map(|r| match r {
            Row::Item {
                card: Some(_),
                label,
                ..
            } => Some(label.chars().count()),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    // The row's inside, less the glyph, the name, the age and the gaps
    // between those four, in small advances. The name takes the longest
    // agent name up to the whole room; the fact columns share the rest and
    // drop whole from the right, so a name cuts only when even zero facts
    // leave too little room.
    let gap = px(8.0);
    let name_w = max_name.min(name_room(adv, small, gap));
    let left =
        px(WIDTH) - px(24.0) - adv * (2 + name_w) as f32 - small * (AGE_W + 1) as f32 - gap * 3.0;
    let budget = (left / small).floor().max(0.0) as usize;
    let cols = columns(&rows, budget);
    // Agent rows number from 1 in order, as cmd-1..9 picks them. The
    // terminal view gives the digits to its tabs, so rows show none there.
    let mut n = 0;
    let badges = rows
        .iter()
        .map(|r| match r {
            Row::Item { card: Some(_), .. } if root.picking && root.main != MainView::Terminal => {
                n += 1;
                (n <= 9).then_some(n)
            }
            _ => None,
        })
        .collect();
    let grid = Grid {
        p,
        adv,
        name_w,
        cols,
        badges,
    };
    for (i, row) in rows.into_iter().enumerate() {
        let menu_squad = match &row {
            Row::Workspace { id, .. } => Some(*id),
            _ => None,
        };
        pane = pane.child(row_element(i, row, &grid, cx));
        // The inline name field sits right under the Workspaces header,
        // where the name it edits will read as a workspace.
        if i == 0 {
            if let Some(field) = root.squad_field.as_ref() {
                pane = pane.child(squad_field_row(field, root, window));
            }
        }
        // The workspace menu opens in place, right under its header row.
        if let Some(id) = menu_squad {
            if Some(id) == root.workspace_menu.map(|(squad, _)| squad) {
                for el in menu_rows(id, &root.layout, root.workspace_menu, &grid.p, cx) {
                    pane = pane.child(el);
                }
            }
        }
    }
    div()
        .id("left-pane")
        .flex()
        .flex_col()
        .flex_none()
        .w(px(WIDTH))
        .h_full()
        .pt(px(ui::TITLE_H))
        .bg(rgb(p.panel))
        .border_r_1()
        .border_color(rgb(p.border))
        .font_family(ui::MONO)
        .child(pane)
}

#[cfg(test)]
mod tests {
    use super::super::app_state::GroupBy;
    use super::*;
    use fno::proto::{AgentBadge, PaneMeta, SquadMeta, TabMeta};
    use fno::theme::Theme;

    #[test]
    fn a_red_fact_wears_the_fail_token_with_the_true_count_in_warn() {
        let p = Palette::from_theme(&Theme::default_theme());
        let fact = PrStatusFact {
            verdict: "red".into(),
            fail: 3,
            review_state: Some("reviewed".into()),
            ..Default::default()
        };
        let marks = pr_status_marks(&fact, &p);
        assert_eq!(marks.first(), Some(&("✗3".to_string(), p.warn)));
        assert_eq!(marks.get(1), Some(&("r✓".to_string(), p.ok)));
    }

    #[test]
    fn a_pending_zero_renders_no_count_and_an_unknown_fact_wears_no_token() {
        let p = Palette::from_theme(&Theme::default_theme());
        let pending = PrStatusFact {
            verdict: "pending".into(),
            ..Default::default()
        };
        let marks = pr_status_marks(&pending, &p);
        assert_eq!(marks.first(), Some(&("…".to_string(), p.faint)));
        let unknown = PrStatusFact::default();
        assert!(pr_status_marks(&unknown, &p).is_empty());
    }

    fn agent(name: &str, pane: u64, badge: Option<AgentBadge>, squad: u64) -> AgentRow {
        serde_json::from_value(serde_json::json!({
            "squad": squad, "name": name, "pane_id": pane, "badge": badge,
            "harness": "claude", "reason": null, "exited": false,
        }))
        .unwrap()
    }

    fn tab(id: u64, name: &str, panes: Vec<PaneMeta>) -> TabMeta {
        serde_json::from_value(serde_json::json!({"id": id, "name": name, "panes": panes})).unwrap()
    }

    fn pane(id: u64, label: &str, cwd: &str) -> PaneMeta {
        PaneMeta {
            id,
            label: label.into(),
            cwd: Some(cwd.into()),
            ..PaneMeta::default()
        }
    }

    /// A shell on the remote machine, for the machine-band test.
    fn shell_pane() -> PaneMeta {
        PaneMeta {
            host: Some("m2-mbp".into()),
            ..pane(20, "shell", "/r/sh")
        }
    }

    fn squad(id: u64, name: &str, tabs: Vec<TabMeta>) -> SquadMeta {
        SquadMeta {
            id,
            name: name.into(),
            canonical_cwd: "/r".into(),
            project: None,
            tabs,
            active_tab: 0,
            panes: 0,
        }
    }

    fn no_folds() -> HashSet<String> {
        HashSet::new()
    }

    #[test]
    fn an_unmeasured_exit_is_not_exited_and_a_bound_agent_shows_node_and_pr() {
        let a: AgentRow = serde_json::from_value(serde_json::json!({
            "squad": null, "name": "gone", "pane_id": 1, "badge": null,
            "harness": "claude", "reason": null, "exited": true, "unmeasured": true,
            "node": "x-aaaa", "pr": 84,
        }))
        .unwrap();
        assert_eq!(agent_lattice_state(&a), LatticeState::Unmeasured);
        assert_eq!(agent_identity(&a).as_deref(), Some("#84 · x-aaaa"));
        let layout = Layout {
            agents: vec![a],
            ..Layout::default()
        };
        let rows = rows(
            &layout,
            &Board::default(),
            100,
            &no_folds(),
            GroupBy::Workspaces,
            None,
            None,
        );
        // The section header leads with the plus, the squadless agent lands
        // in the Elsewhere group under its open header.
        assert!(matches!(rows.first(), Some(Row::Header { plus: true, .. })));
        assert!(matches!(
            rows.get(1),
            Some(Row::Group {
                ref title,
                open: true,
                ..
            }) if title == "Elsewhere"
        ));
        assert!(matches!(
            rows.get(2),
            Some(Row::Item { state: Some(LatticeState::Unmeasured), detail, .. })
                if detail.ends_with("unknown")
        ));
    }

    #[test]
    fn workspaces_nest_their_agents_then_shells_and_elsewhere_trails() {
        // Two squads: fno with agents A and B plus one shell pane, tools
        // with agent C. A fourth agent names no squad.
        let mut a = agent("alpha", 1, None, 7);
        a.pane_id = Some(1);
        let b = agent("beta", 2, Some(AgentBadge::Blocked), 7);
        let c = agent("gamma", 3, None, 9);
        let mut loose = agent("delta", 0, None, 0);
        loose.squad = None;
        let layout = Layout {
            squads: vec![
                squad(
                    7,
                    "fno",
                    vec![tab(
                        10,
                        "main",
                        vec![pane(1, "alpha", "/r/fno"), pane(4, "shell", "/r/tools/sub")],
                    )],
                ),
                squad(9, "tools", vec![tab(11, "main", vec![])]),
            ],
            active_squad: 7,
            focus: 1,
            agents: vec![a, b, c, loose],
            ..Layout::default()
        };
        let rows = rows(
            &layout,
            &Board::default(),
            100,
            &no_folds(),
            GroupBy::Workspaces,
            None,
            None,
        );
        let kind_of = |r: &Row| -> String {
            match r {
                Row::Header { title, .. } => format!("header {title}"),
                Row::Workspace { name, open, .. } => format!("ws {name} open={open}"),
                Row::Group { title, open, .. } => format!("group {title} open={open}"),
                Row::Item { label, detail, .. } => format!("item {label} [{detail}]"),
                Row::Error(e) => format!("error {e}"),
                Row::Empty(why) => format!("empty {why}"),
                Row::Note(t) => format!("note {t}"),
            }
        };
        let kinds: Vec<String> = rows.iter().map(kind_of).collect();
        // Header, fno's agents in sideline order, then fno's shell pane,
        // then tools' agent; the shell's cwd keeps its absolute form when
        // it names no home prefix.
        let want = [
            "header Workspaces".to_string(),
            "ws fno open=true".to_string(),
            "item beta".to_string(),
            "item alpha".to_string(),
            "item shell [/r/tools/sub]".to_string(),
            "ws tools open=true".to_string(),
            "item gamma".to_string(),
            "group Elsewhere open=true".to_string(),
            "item delta".to_string(),
        ];
        assert!(kinds.len() >= want.len(), "too few rows: {kinds:?}");
        for (i, (k, w)) in kinds.iter().zip(want.iter()).enumerate() {
            assert!(
                k.starts_with(w.as_str()),
                "row {i}: got {k}, want prefix {w}"
            );
        }
        // No row reads the old section words, and no "+N more" row exists.
        for k in &kinds {
            assert!(
                !k.contains(&concat!("Sess", "ions")) && !k.contains("Agents"),
                "{k}"
            );
            assert!(!k.contains("more"), "{k}");
        }
        // The shell row focuses its pane; every agent row keeps its own click.
        let shell = rows
            .iter()
            .find_map(|r| match r {
                Row::Item { label, action, .. } if label == "shell" => Some(action.clone()),
                _ => None,
            })
            .flatten();
        assert_eq!(shell, Some(Action::Send(Command::FocusPane(4))));
        // A squadless agent never paints twice: delta shows once.
        assert_eq!(kinds.iter().filter(|k| k.contains("delta")).count(), 1);
    }

    #[test]
    fn a_pinned_agent_leads_its_workspace_and_a_result_rides_one_line_under_its_row() {
        let first = agent("alpha", 1, None, 7);
        let mut second = agent("beta", 2, None, 7);
        second.pinned = true;
        second.result = Some("shipped\nsecond line".into());
        let layout = Layout {
            squads: vec![squad(7, "fno", vec![tab(10, "main", vec![])])],
            active_squad: 7,
            agents: vec![first, second],
            ..Layout::default()
        };
        let out = rows(
            &layout,
            &Board::default(),
            100,
            &no_folds(),
            GroupBy::Workspaces,
            None,
            None,
        );
        assert!(matches!(
            out.get(2),
            Some(Row::Item { label, card: Some(c), .. }) if label == "beta" && c.pinned
        ));
        assert_eq!(out.get(3), Some(&Row::Note("shipped".into())));
        assert!(matches!(out.get(4), Some(Row::Item { label, .. }) if label == "alpha"));
    }

    #[test]
    fn a_folded_workspace_hides_its_rows_and_every_workspace_opens_by_default() {
        let a = agent("alpha", 1, None, 7);
        let layout = Layout {
            squads: vec![squad(7, "fno", vec![tab(10, "main", vec![])])],
            active_squad: 7,
            agents: vec![a],
            ..Layout::default()
        };
        let open_rows = rows(
            &layout,
            &Board::default(),
            100,
            &no_folds(),
            GroupBy::Workspaces,
            None,
            None,
        );
        assert!(matches!(
            open_rows.get(1),
            Some(Row::Workspace { open: true, .. })
        ));
        assert!(matches!(
            open_rows.get(2),
            Some(Row::Item { label, .. }) if label == "alpha"
        ));
        // The fold key is the canonical cwd; folding hides the rows, the
        // header stays.
        let mut folds = HashSet::new();
        folds.insert("/r".to_string());
        let folded = super::rows(
            &layout,
            &Board::default(),
            100,
            &folds,
            GroupBy::Workspaces,
            None,
            None,
        );
        assert!(matches!(
            folded.get(1),
            Some(Row::Workspace { open: false, .. })
        ));
        assert!(matches!(
            folded.get(2),
            Some(Row::Item { label, .. }) if label.starts_with("Questions")
        ));
        // An empty workspace says so under its open header.
        let bare = Layout {
            squads: vec![squad(3, "empty", vec![tab(12, "main", vec![])])],
            ..Layout::default()
        };
        let rows = rows(
            &bare,
            &Board::default(),
            100,
            &no_folds(),
            GroupBy::Workspaces,
            None,
            None,
        );
        assert!(matches!(
            rows.get(2),
            Some(Row::Empty("no agents in this workspace"))
        ));
    }

    #[test]
    fn one_questions_row_carries_the_count_and_opens_hq() {
        let mut board = Board::default();
        let question = fno::needs_overlay::QuestionItem {
            id: "q-1".into(),
            title: "ship it?".into(),
            state: "open".into(),
            ..Default::default()
        };
        let fold = fno::needs_overlay::QuestionsFold {
            items: vec![question],
            ..Default::default()
        };
        let none = fno::backlog_model::Inputs::default();
        board.record(Err("no fleet".into()), Ok(fold), &none, 1, &[]);
        let rows = rows(
            &Layout::default(),
            &board,
            100,
            &no_folds(),
            GroupBy::Workspaces,
            None,
            None,
        );
        let qrow = rows.iter().find_map(|r| match r {
            Row::Item { label, action, .. } if label.starts_with("Questions") => {
                Some((label.clone(), action.clone()))
            }
            _ => None,
        });
        let (label, action) = qrow.expect("one questions row");
        assert_eq!(label, "Questions 1");
        assert_eq!(action, Some(Action::Show(MainView::Orchestration)));
        // No question titles in the pane.
        assert!(!rows
            .iter()
            .any(|r| matches!(r, Row::Item { label, .. } if label == "ship it?")));
        // Before the first read the row waits; a failed read keeps its
        // error line under the row.
        let fresh = super::rows(
            &Layout::default(),
            &Board::default(),
            100,
            &no_folds(),
            GroupBy::Workspaces,
            None,
            None,
        );
        assert!(fresh.iter().any(|r| matches!(r, Row::Item { label, detail, .. } if label == "Questions" && detail == "reading…")));
        let mut failed = Board::default();
        failed.record(
            Err("no fleet".into()),
            Err("needs timed out".into()),
            &none,
            1,
            &[],
        );
        let err_rows = super::rows(
            &Layout::default(),
            &failed,
            100,
            &no_folds(),
            GroupBy::Workspaces,
            None,
            None,
        );
        assert!(err_rows
            .iter()
            .any(|r| matches!(r, Row::Error(e) if e == "needs timed out")));
    }

    #[test]
    fn the_workspace_header_rolls_up_its_agents_and_marks_the_active_one() {
        let agents = vec![
            agent("idle", 1, None, 7),
            agent("stuck", 2, Some(AgentBadge::Blocked), 7),
            agent("busy", 3, Some(AgentBadge::Working), 7),
        ];
        let layout = Layout {
            squads: vec![squad(7, "fno", vec![tab(10, "main", vec![])])],
            active_squad: 7,
            focus: 3,
            agents,
            ..Layout::default()
        };
        let rows = rows(
            &layout,
            &Board::default(),
            100,
            &no_folds(),
            GroupBy::Workspaces,
            None,
            None,
        );
        // The sideline's default Status sort: blocked, then working, then idle.
        let names: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Item {
                    card: Some(_),
                    label,
                    ..
                } => Some(label.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["stuck", "busy", "idle"]);
        assert!(matches!(
            rows.get(1),
            Some(Row::Workspace {
                id: 7,
                active: true,
                open: true,
                rollup,
                ..
            }) if rollup == &vec![
                (LatticeState::Blocked, 1),
                (LatticeState::Working, 1),
                (LatticeState::Idle, 1),
            ]
        ));
        // One cursor: the focused agent.
        let cursors = rows
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    Row::Item {
                        mark: Mark::Cursor,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(cursors, 1);
    }

    #[test]
    fn the_row_the_composer_launched_carries_the_cursor_from_its_first_paint() {
        // The launched row streams before its session id is known, so the
        // cursor rides the launch's own name until the stream or a click
        // takes it over.
        let a: AgentRow = serde_json::from_value(serde_json::json!({
            "squad": null, "name": "t-land-the-fix", "pane_id": null,
            "harness": "claude", "badge": null, "reason": null, "exited": false,
        }))
        .unwrap();
        let other: AgentRow = serde_json::from_value(serde_json::json!({
            "squad": null, "name": "scout", "pane_id": null,
            "harness": "claude", "badge": null, "reason": null, "exited": false,
        }))
        .unwrap();
        let layout = Layout {
            agents: vec![other, a],
            ..Layout::default()
        };
        let marked = rows(
            &layout,
            &Board::default(),
            100,
            &no_folds(),
            GroupBy::Workspaces,
            None,
            Some(("t-land-the-fix", "land the checkout fix")),
        );
        let mark = |name: &str, rows: &[Row]| {
            rows.iter().find_map(|r| match r {
                Row::Item { label, mark, .. } if label == name => Some(*mark),
                _ => None,
            })
        };
        // The launched row's label reads as the task, the mail key stays.
        let label = marked.iter().find_map(|r| match r {
            Row::Item { label, .. } if label.contains("checkout fix") => Some(label.clone()),
            _ => None,
        });
        assert_eq!(label.as_deref(), Some("land the checkout fix"));
        assert_eq!(mark("land the checkout fix", &marked), Some(Mark::Cursor));
        assert_eq!(mark("scout", &marked), Some(Mark::None));
        // Without a pending launch the same paneless row carries none.
        let plain = rows(
            &layout,
            &Board::default(),
            100,
            &no_folds(),
            GroupBy::Workspaces,
            None,
            None,
        );
        assert_eq!(mark("t-land-the-fix", &plain), Some(Mark::None));
    }

    #[test]
    fn an_agent_row_carries_the_mux_cards_served_facts_and_nothing_unserved() {
        let served: AgentRow = serde_json::from_value(serde_json::json!({
            "squad": 7, "name": "rich", "pane_id": 1, "badge": null,
            "harness": "claude", "reason": null, "exited": false,
            "model": "claude-opus-5-5", "session_cost_cents": 4497,
            "started_at": 100, "context_used_pct": 41, "session_tokens": 12345,
            "last_activity_age_s": 90,
        }))
        .unwrap();
        let layout = Layout {
            squads: vec![squad(7, "fno", vec![tab(10, "main", vec![])])],
            agents: vec![served],
            ..Layout::default()
        };
        let rows = rows(
            &layout,
            &Board::default(),
            100 + 7200,
            &no_folds(),
            GroupBy::Workspaces,
            None,
            None,
        );
        let card = |name: &str| {
            rows.iter().find_map(|r| match r {
                Row::Item { label, card, .. } if label == name => card.clone(),
                _ => None,
            })
        };
        let rich = card("rich").expect("agent rows carry a card");
        assert!(rich.model.as_deref().is_some_and(|m| m.starts_with("opus")));
        assert_eq!(rich.cost.as_deref(), Some("~$44.97"));
        assert_eq!(rich.created.as_deref(), Some("2h"));
        assert_eq!(rich.context.as_deref(), Some("41%"));
        assert_eq!(rich.tokens.as_deref(), Some("12.3k tok"));
        assert!(rich.activity.is_some());
        // The columns are the widest value per slot, and a tight budget
        // drops whole slots from the right: model and context stay. The
        // cost slot is empty by design, so it takes no column.
        let model = rich.model.as_deref().unwrap().chars().count();
        let wide = columns(&rows, 200);
        assert_eq!(wide, [(0, model), (2, 3), (3, 9), (4, 5)]);
        let tight = columns(&rows, model + COL_GAP + 3 + COL_GAP + 8);
        assert_eq!(tight, [(0, model), (2, 3)]);
    }

    #[test]
    fn the_name_takes_the_room_and_the_facts_drop_first() {
        // At the pane's own width the room exceeds the name: a long name
        // shows whole, and the facts share what is left.
        let adv = px(8.0);
        let small = adv * (ui::SMALL / ui::BODY);
        let gap = px(8.0);
        let room = name_room(adv, small, gap);
        assert!(
            room >= 18,
            "an 18-char name shows whole at 320px: room {room}"
        );
        // Only when even zero fact columns leave too little room does the
        // name cut: a huge advance stands in for a narrow pane.
        let wide = px(40.0);
        let cut = name_room(wide, wide * (ui::SMALL / ui::BODY), gap);
        assert!(cut < 18, "a narrow pane cuts the name: room {cut}");
        assert!(cut >= NAME_MIN);
    }
    #[test]
    fn grouping_by_state_buckets_agents_and_names_the_workspace_in_detail() {
        // A blocked agent sits under Needs input, a working one under
        // Working, an idle one under Done; the row's detail names its
        // workspace; a band folds by its title.
        let agents = vec![
            agent("idle", 1, None, 7),
            agent("stuck", 2, Some(AgentBadge::Blocked), 7),
            agent("busy", 3, Some(AgentBadge::Working), 7),
        ];
        let layout = Layout {
            squads: vec![squad(7, "fno", vec![tab(10, "main", vec![])])],
            active_squad: 7,
            focus: 3,
            agents,
            ..Layout::default()
        };
        let rows = rows(
            &layout,
            &Board::default(),
            100,
            &no_folds(),
            GroupBy::State,
            None,
            None,
        );
        let bands: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Group { title, .. } => Some(title.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(bands, ["Needs input", "Working", "Done"]);
        let stuck = rows
            .iter()
            .find_map(|r| match r {
                Row::Item { label, detail, .. } if label == "stuck" => Some(detail.clone()),
                _ => None,
            })
            .expect("the blocked row shows");
        assert!(stuck.contains("fno"), "detail names the workspace: {stuck}");
        // A band folds by its title, the same user-only rule.
        let mut folds = HashSet::new();
        folds.insert("Working".to_string());
        let folded = super::rows(
            &layout,
            &Board::default(),
            100,
            &folds,
            GroupBy::State,
            None,
            None,
        );
        assert!(folded
            .iter()
            .all(|r| !matches!(r, Row::Item { label, .. } if label == "busy")));
        assert!(folded
            .iter()
            .any(|r| matches!(r, Row::Group { title, open, .. } if title == "Working" && !open)));
    }

    #[test]
    fn grouping_by_machine_joins_the_fleet_and_shells_follow_their_host() {
        // A remote agent joins host m2-mbp through the fleet seat's session
        // id; agents with no seat join, and host-less shells, land on the
        // local machine's name. A shell's own host wins over the local
        // default.
        let mut remote = agent("remote-one", 5, None, 9);
        remote.harness_session_id = Some("sess-remote".to_string());
        let mut local_agent = agent("local-one", 6, Some(AgentBadge::Blocked), 7);
        local_agent.harness_session_id = Some("sess-local".to_string());
        let layout = Layout {
            squads: vec![
                squad(7, "fno", vec![tab(10, "main", vec![shell_pane()])]),
                squad(9, "tools", vec![tab(11, "main", vec![])]),
            ],
            agents: vec![remote, local_agent],
            ..Layout::default()
        };
        // The Board's raw fleet map: machine u is local, m2-mbp carries the
        // remote seat. fold derives the seats the join reads.
        let fleet = serde_json::json!({
            "machines": [
                {"host": "u", "local": true, "fresh": true, "sessions": [
                    {"name": "local-one", "session_id": "sess-local", "harness": "claude"}
                ]},
                {"host": "m2-mbp", "fresh": true, "age_s": 0, "sessions": [
                    {"name": "remote-one", "session_id": "sess-remote", "harness": "claude"}
                ]},
            ]
        });
        let mut board = Board::default();
        let none = fno::backlog_model::Inputs::default();
        board.record(Ok(fleet), Err("no questions".into()), &none, 1, &[]);
        let rows = rows(
            &layout,
            &board,
            100,
            &no_folds(),
            GroupBy::Machine,
            None,
            None,
        );
        let bands: Vec<String> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Group { title, .. } => Some(title.clone()),
                _ => None,
            })
            .collect();
        // The blocked local agent leads the sideline order, so its band is
        // first; the remote seat joins m2-mbp.
        assert_eq!(bands, ["u", "m2-mbp"]);
        // The remote agent's row sits under m2-mbp, the local one under u,
        // and the shell whose host is m2-mbp follows the same band, after
        // the band's agents.
        let under = |band: &str| -> Vec<String> {
            let mut out: Vec<String> = Vec::new();
            let mut in_band = false;
            for r in &rows {
                match r {
                    Row::Group { title, .. } => in_band = title == band,
                    Row::Item { label, card, .. } if in_band && card.is_some() => {
                        out.push(label.clone())
                    }
                    _ => {}
                }
            }
            out
        };
        assert_eq!(under("u"), ["local-one"]);
        assert_eq!(under("m2-mbp"), ["remote-one"]);
        // The shell row follows its host's band (after the band's agents).
        let shell_pos = rows
            .iter()
            .position(|r| matches!(r, Row::Item { label, card: None, .. } if label == "shell"));
        let remote_pos = rows
            .iter()
            .position(|r| matches!(r, Row::Item { label, .. } if label == "remote-one"));
        let m2 = rows
            .iter()
            .position(|r| matches!(r, Row::Group { title, .. } if title == "m2-mbp"));
        assert!(m2.unwrap() < remote_pos.unwrap());
        assert!(remote_pos.unwrap() < shell_pos.unwrap());
    }

    // The pr sheet's model contracts live under this file's declaration
    // scope: pr_sheet.rs's path matches the net-zero test scope (pr_),
    // and these contracts are net-new.
    use crate::pr_sheet;
    use fno_proto::PrFailingCheck;

    fn sheet_fact() -> PrStatusFact {
        PrStatusFact {
            verdict: "red".into(),
            pass: 32,
            fail: 2,
            pending: 17,
            failing: vec![
                PrFailingCheck {
                    name: "cargo test (fno mux)".into(),
                    step: "wait for every run at the head".into(),
                    first_error: "ci-required: FAILED at e51d".into(),
                },
                PrFailingCheck {
                    name: "Python static correctness".into(),
                    step: "Test declarations are shrink-only".into(),
                    first_error: "test-delta: scope G3 nets +1".into(),
                },
            ],
            review_state: Some("reviewed".into()),
            review_rounds: Some(2),
            title: Some("the mux board fetch stays inside its deadline".into()),
            settled: false,
            pr_state: Some("OPEN".into()),
            draft: false,
            mergeable: Some("MERGEABLE".into()),
            measured_at: 0,
        }
    }

    #[test]
    fn the_sheet_model_names_the_failing_checks_and_the_review() {
        let rows = pr_sheet::model(&sheet_fact());
        let texts: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert!(texts.contains(&"✗ cargo test (fno mux)"));
        assert!(texts.contains(&"wait for every run at the head"));
        assert!(texts.contains(&"ci-required: FAILED at e51d"));
        assert!(texts.contains(&"the mux board fetch stays inside its deadline"));
        assert!(texts.contains(&"checks 51: 32 pass 2 fail 17 pending"));
        assert!(texts.contains(&"review: reviewed (2 rounds)"));
        assert!(texts.contains(&"OPEN · MERGEABLE"));
        // The step rows wear the error role; the error tail is secondary.
        let failing = rows.iter().find(|r| r.text.starts_with("✗ ")).unwrap();
        assert!(failing.error);
        let tail = rows
            .iter()
            .find(|r| r.text == "ci-required: FAILED at e51d")
            .unwrap();
        assert!(tail.dim && !tail.error);
    }

    #[test]
    fn a_sheet_fact_with_no_state_and_no_failing_renders_the_checks_line_only() {
        let bare = PrStatusFact {
            verdict: "unknown".into(),
            ..Default::default()
        };
        let rows = pr_sheet::model(&bare);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows.first().unwrap().text, "checks 0:");
    }

    #[test]
    fn a_green_sheet_fact_reads_ok_and_names_no_failing() {
        let green = PrStatusFact {
            verdict: "green".into(),
            pass: 9,
            ..Default::default()
        };
        let rows = pr_sheet::model(&green);
        assert_eq!(rows.first().unwrap().text, "checks 9: 9 pass");
    }
}
