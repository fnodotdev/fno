//! The native left pane: agents, sessions, questions and machines. Agents
//! order through the TUI sideline's own comparator, and a click sends the
//! command the sideline click sends. A section whose read failed shows the
//! error and keeps its last good rows.

use gpui::{div, prelude::*, px, rgb, Context, Div, FontWeight, Pixels, Stateful};

use super::link::Layout;
use super::palette::Palette;
use super::ui;
use super::window::{Action, MainView, Root};
use fno::app_api::client::agent_lattice_state;
use fno::app_api::client::card_line::{binding_suffix, card_facts, CardFacts};
use fno::app_api::client::header_band::section_rollup;
use fno::app_api::lattice::{lattice_glyph, status_long, LatticeState};
use fno::app_api::web_hq::Board;
use fno::proto::{AgentRow, Command};

pub const WIDTH: f32 = 320.0;
/// The agents shown before a "+N more" row, so the sections below stay above
/// the fold. Ten one-line rows keep every section header in the default
/// 1360x840 window.
pub const AGENTS_SHOWN: usize = 10;

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
    },
    Item {
        label: String,
        detail: String,
        /// The node and PR an agent works on.
        identity: Option<String>,
        /// The mux card's served facts; `None` for anything but an agent.
        card: Option<CardFacts>,
        /// An agent's lattice state; `None` for sessions, machines, questions.
        state: Option<LatticeState>,
        /// A question: it needs the user.
        question: bool,
        mark: Mark,
        action: Option<Action>,
    },
    /// The agents past [`AGENTS_SHOWN`], folded, or the row that folds
    /// them back.
    More { hidden: usize },
    /// A failed read. The section's last good rows follow it.
    Error(String),
    /// A section with nothing to show, and why.
    Empty(&'static str),
}

fn header(title: &'static str) -> Row {
    Row::Header {
        title,
        rollup: Vec::new(),
    }
}

fn agent_identity(a: &AgentRow) -> Option<String> {
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

pub fn rows(layout: &Layout, board: &Board, now_secs: u64, agents_open: bool) -> Vec<Row> {
    let agents = fno::app_api::client::agent_sort::sideline_order(&layout.agents, now_secs);
    let mut out = vec![Row::Header {
        title: "Agents",
        rollup: section_rollup(agents.iter().map(|a| agent_lattice_state(a))),
    }];
    if agents.is_empty() {
        out.push(Row::Empty("no agents in this session"));
    }
    let total = agents.len();
    let shown = if agents_open {
        total
    } else {
        total.min(AGENTS_SHOWN)
    };
    for a in agents.into_iter().take(shown) {
        use fno::app_api::client::agent_group::{click_command, RowClick};
        let action = match click_command(a) {
            RowClick::Send(cmd) => Action::Send(cmd),
            RowClick::Notice(notice) => Action::Notice(notice),
        };
        let state = agent_lattice_state(a);
        let harness = a.harness.as_deref().unwrap_or("pane");
        // A one-shot's end word outranks the lane status: ended, not stuck.
        let status = a.result.as_deref().unwrap_or(status_long(state));
        out.push(Row::Item {
            label: a.name.clone(),
            detail: format!("{harness} · {status}"),
            identity: agent_identity(a),
            card: Some(card_facts(a, now_secs)),
            state: Some(state),
            question: false,
            mark: if a.pane_id == Some(layout.focus) {
                Mark::Cursor
            } else {
                Mark::None
            },
            action: Some(action),
        });
    }
    if total > AGENTS_SHOWN {
        out.push(Row::More {
            hidden: total - shown,
        });
    }

    out.push(header("Sessions"));
    for s in &layout.squads {
        let tabs = s.tabs.len();
        out.push(Row::Item {
            label: s.name.clone(),
            detail: format!(
                "{tabs} tab{} · {} pane{}",
                if tabs == 1 { "" } else { "s" },
                s.panes,
                if s.panes == 1 { "" } else { "s" }
            ),
            identity: None,
            card: None,
            state: None,
            question: false,
            mark: if s.id == layout.active_squad {
                Mark::Current("current")
            } else {
                Mark::None
            },
            action: Some(Action::Send(Command::SelectSquad(s.id))),
        });
    }

    // Before the first read, a section waits; after it, a failure is an error.
    let failed = |e: Option<&str>| -> Option<Row> {
        match (board.read_yet(), e) {
            (false, _) => Some(Row::Empty("reading…")),
            (true, Some(e)) => Some(Row::Error(e.to_string())),
            (true, None) => None,
        }
    };
    out.push(header("Questions"));
    out.extend(failed(board.questions_error()));
    let questions = board.questions_newest_first();
    if questions.is_empty() && board.questions_error().is_none() {
        out.push(Row::Empty("nothing waits on you"));
    }
    for q in questions {
        let q = &q.item;
        out.push(Row::Item {
            label: q.title.clone(),
            detail: q.node.clone().unwrap_or_else(|| q.kind.clone()),
            identity: None,
            card: None,
            state: None,
            question: true,
            mark: Mark::None,
            action: Some(Action::Show(MainView::Orchestration)),
        });
    }

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
    out
}

/// An agent row's fact slots in their fixed order: model, cost, context,
/// tokens, session age, then the node and PR. A fact that arrives late
/// fills its own slot and never pushes another out.
fn slots(card: &CardFacts, identity: Option<&str>) -> [Option<String>; SLOTS] {
    [
        card.model.clone(),
        card.cost.clone(),
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
/// The name column's bounds, in body advances.
const NAME_MIN: usize = 6;
const NAME_MAX: usize = 12;

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
    let shown = ui::fit(&used.iter().map(|(_, w)| *w).collect::<Vec<_>>(), budget, 1);
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

/// An agent row's cells: the name column, the fact columns every agent row
/// shares, and how long ago it last did anything.
fn agent_cells(
    item: Stateful<Div>,
    card: &CardFacts,
    identity: Option<&str>,
    (label, name_w, weight): (String, usize, FontWeight),
    cols: &[(usize, usize)],
    p: &Palette,
    adv: Pixels,
) -> Stateful<Div> {
    let small = adv * (ui::SMALL / ui::BODY);
    let facts = slots(card, identity);
    let mut line = div().flex_1().min_w_0().flex().flex_row().gap(small);
    for (slot, w) in cols {
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
            .w(adv * name_w as f32)
            .flex_none()
            .overflow_hidden()
            .child(
                ui::role(div(), ui::Role::Name, p)
                    .font_weight(weight)
                    .child(ui::cut(&label, name_w)),
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
}

/// One sidebar row as an element.
fn row_element(i: usize, row: Row, grid: &Grid, cx: &mut Context<Root>) -> Stateful<Div> {
    let (p, adv) = (grid.p, grid.adv);
    match row {
        Row::Header { title, rollup } => div().id(i).px_3().child(
            ui::section(title, &p).justify_between().child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(8.0))
                    .children(rollup.into_iter().map(|(state, n)| {
                        ui::role(div(), ui::Role::Fact, &p)
                            .flex()
                            .flex_row()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .text_color(rgb(state_color(&p, state)))
                                    .child(lattice_glyph(state).0.to_string()),
                            )
                            .child(n.to_string())
                    })),
            ),
        ),
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
        Row::More { hidden } => ui::clickable(ui::row(i, false, &p).w(px(WIDTH)), &p)
            .on_click(cx.listener(|root, _, _, cx| root.act(Action::ToggleAgents, cx)))
            .child(div().w(adv * 2.0).flex_none())
            .child(ui::text(
                if hidden > 0 {
                    format!("+{hidden} more")
                } else {
                    "show fewer".to_string()
                },
                ui::Role::Hint,
                &p,
            )),
        Row::Item {
            label,
            detail,
            identity,
            card,
            state,
            question,
            mark,
            action,
        } => {
            let cursor = mark == Mark::Cursor;
            let blocked = state == Some(LatticeState::Blocked);
            // Shape carries the state: a triangle for blocked, a circle for
            // work. A question leads with a diamond, never the `?` an
            // unmeasured agent wears, so none leans on color.
            let (glyph, glyph_color) = match (state, question) {
                (Some(s), _) => (lattice_glyph(s).0.to_string(), state_color(&p, s)),
                (None, true) => ("◇".to_string(), p.warn),
                (None, false) => (String::new(), p.faint),
            };
            let mut item = ui::row(i, cursor, &p).w(px(WIDTH)).child(
                div()
                    .w(adv * 2.0)
                    .flex_none()
                    .text_color(rgb(glyph_color))
                    .child(glyph),
            );
            let weight = if blocked || cursor {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::MEDIUM
            };
            item = match card {
                Some(card) => {
                    let name = (label, grid.name_w, weight);
                    agent_cells(item, &card, identity.as_deref(), name, &grid.cols, &p, adv)
                }
                None => plain_cells(item, &label, &detail, mark, &p, adv),
            };
            if let Some(action) = action {
                // On click: a press that leaves the row before release
                // does nothing.
                item = ui::clickable(item, &p)
                    .on_click(cx.listener(move |root, _, _, cx| root.act(action.clone(), cx)));
            }
            item
        }
    }
}

/// `adv` is one SF Mono advance at the body size.
pub(super) fn render(
    root: &Root,
    now_secs: u64,
    adv: Pixels,
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
    let rows = rows(&root.layout, &root.board, now_secs, root.agents_open);
    let name_w = rows
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
        .unwrap_or(0)
        .clamp(NAME_MIN, NAME_MAX);
    // The row's inside, less the glyph, the name, the age and the gaps
    // between those four, in small advances.
    let gap = px(8.0);
    let left = px(WIDTH) - px(24.0) - adv * (2 + name_w) as f32 - small * AGE_W as f32 - gap * 3.0;
    let budget = (left / small).floor().max(0.0) as usize;
    let cols = columns(&rows, budget);
    let grid = Grid {
        p,
        adv,
        name_w,
        cols,
    };
    for (i, row) in rows.into_iter().enumerate() {
        pane = pane.child(row_element(i, row, &grid, cx));
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
    use super::*;
    use fno::proto::{AgentBadge, SquadMeta, TabMeta};

    fn agent(name: &str, pane: u64, badge: Option<AgentBadge>) -> AgentRow {
        serde_json::from_value(serde_json::json!({
            "squad": null, "name": name, "pane_id": pane, "badge": badge,
            "reason": null, "exited": false,
        }))
        .unwrap()
    }

    fn tab(id: u64, name: &str) -> TabMeta {
        serde_json::from_value(serde_json::json!({"id": id, "name": name})).unwrap()
    }

    #[test]
    fn an_unmeasured_exit_is_not_exited_and_a_bound_agent_shows_node_and_pr() {
        let a: AgentRow = serde_json::from_value(serde_json::json!({
            "squad": null, "name": "gone", "pane_id": 1, "badge": null,
            "reason": null, "exited": true, "unmeasured": true,
            "node": "x-aaaa", "pr": 84,
        }))
        .unwrap();
        assert_eq!(agent_lattice_state(&a), LatticeState::Unmeasured);
        assert_eq!(agent_identity(&a).as_deref(), Some("#84 · x-aaaa"));
        let layout = Layout {
            agents: vec![a],
            ..Layout::default()
        };
        let rows = rows(&layout, &Board::default(), 100, false);
        assert!(
            matches!(&rows[1], Row::Item { state: Some(LatticeState::Unmeasured), detail, .. } if detail.ends_with("unknown"))
        );
    }

    #[test]
    fn an_agent_row_carries_the_mux_cards_served_facts_and_nothing_unserved() {
        let served: AgentRow = serde_json::from_value(serde_json::json!({
            "squad": null, "name": "rich", "pane_id": 1, "badge": null,
            "reason": null, "exited": false, "harness": "claude",
            "model": "claude-opus-5-5", "session_cost_cents": 4497,
            "started_at": 100, "context_used_pct": 41, "session_tokens": 12345,
            "last_activity_age_s": 90,
        }))
        .unwrap();
        let bare = agent("bare", 2, None);
        let layout = Layout {
            agents: vec![served, bare],
            ..Layout::default()
        };
        let rows = rows(&layout, &Board::default(), 100 + 7200, false);
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
        assert_eq!(card("bare").unwrap(), CardFacts::default());
        // The columns are the widest value per slot, and the bare agent's
        // empty slots leave no column of their own. A tight budget drops
        // whole slots from the right: model, cost and context stay.
        let model = rich.model.as_deref().unwrap().chars().count();
        let wide = columns(&rows, 200);
        assert_eq!(wide, [(0, model), (1, 7), (2, 3), (3, 9), (4, 5)]);
        let tight = columns(&rows, model + 1 + 7 + 1 + 3 + 1 + 8);
        assert_eq!(tight, [(0, model), (1, 7), (2, 3)]);
    }

    #[test]
    fn rows_follow_the_sideline_order_and_keep_last_good_on_a_failed_read() {
        let agents = vec![
            agent("idle", 1, None),
            agent("stuck", 2, Some(AgentBadge::Blocked)),
            agent("busy", 3, Some(AgentBadge::Working)),
        ];
        let squad = SquadMeta {
            id: 7,
            name: "fno".into(),
            canonical_cwd: "/r".into(),
            tabs: vec![tab(10, "main"), tab(11, "review")],
            active_tab: 1,
            panes: 3,
        };
        let mut layout = Layout {
            squads: vec![squad],
            active_squad: 7,
            focus: 3,
            agents,
            ..Layout::default()
        };
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
        board.record(
            Err("no fleet".into()),
            Err("needs timed out".into()),
            &none,
            2,
            &[],
        );

        let rows = rows(&layout, &board, 100, false);
        let items = |rows: &[Row]| -> Vec<(String, Option<Action>)> {
            rows.iter()
                .filter_map(|r| match r {
                    Row::Item { label, action, .. } => Some((label.clone(), action.clone())),
                    _ => None,
                })
                .collect()
        };
        // The sideline's default Status sort: blocked, then working, then idle.
        let got = items(&rows);
        let names: Vec<&str> = got.iter().take(3).map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["stuck", "busy", "idle"]);
        let stuck = got.iter().find(|(n, _)| n == "stuck").unwrap();
        assert_eq!(stuck.1, Some(Action::Send(Command::FocusPane(2))));
        // One cursor: the focused agent. The active session says "current".
        let marks: Vec<Mark> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Item { mark, .. } => Some(*mark),
                _ => None,
            })
            .collect();
        assert_eq!(marks.iter().filter(|m| **m == Mark::Cursor).count(), 1);
        assert!(marks.contains(&Mark::Current("current")));
        // The Agents header rolls up blocked 1, working 1, idle 1.
        assert!(
            matches!(&rows[0], Row::Header { rollup, .. } if rollup == &vec![
                (LatticeState::Blocked, 1),
                (LatticeState::Working, 1),
                (LatticeState::Idle, 1),
            ])
        );
        // The failed question read names its error and keeps "ship it?".
        let at = rows
            .iter()
            .position(|r| *r == Row::Error("needs timed out".into()))
            .expect("the error line shows");
        assert!(matches!(&rows[at + 1], Row::Item { label, .. } if label == "ship it?"));
        assert!(rows.contains(&Row::Error("no fleet".into())));

        // The strip shows both tabs and selects by id; a rename shows on the
        // next layout.
        let strip = super::super::tabs::strip(&layout);
        assert_eq!(strip.len(), 2);
        assert!(strip[1].active);
        assert_eq!(strip[0].action, Action::Send(Command::SelectTab(10)));
        layout.squads[0].tabs[0] = TabMeta {
            named: true,
            ..tab(10, "build")
        };
        assert_eq!(super::super::tabs::strip(&layout)[0].label, "build");
    }
}
