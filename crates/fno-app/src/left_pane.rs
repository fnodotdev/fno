//! The native left pane: agents, sessions, questions and machines. Agents
//! order through the TUI sideline's own comparator, and a click sends the
//! command the sideline click sends. A section whose read failed shows the
//! error and keeps its last good rows.

use gpui::{div, prelude::*, px, rgb, Context, Div, FontWeight, Stateful};

use super::link::Layout;
use super::palette::Palette;
use super::window::{Action, MainView, Root};
use fno::app_api::client::agent_lattice_state;
use fno::app_api::client::card_line::{binding_suffix, card_facts, CardFacts};
use fno::app_api::client::header_band::section_rollup;
use fno::app_api::lattice::{lattice_glyph, status_long, LatticeState};
use fno::app_api::web_hq::Board;
use fno::proto::{AgentRow, Command};

pub const WIDTH: f32 = 260.0;
/// The agents shown before a "+N more" row, so the sections below stay above
/// the fold. Six keeps every section header in the default 1360x800 window.
pub const AGENTS_SHOWN: usize = 6;

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

/// A row line in its own full-width box, so a long one ends in an ellipsis
/// and never clips mid-glyph.
/// The width is fixed, not `w_full`: gpui wraps and truncates text against
/// the width each layout pass offers, and a min-content pass inside the
/// scrolling pane offered a few pixels, so labels showed three characters.
fn clamp(text: String, color: u32, small: bool) -> Div {
    // The inner line follows the box it lands in, so the line clamp
    // ellipsizes at the width the row actually gives it: a fixed inner width
    // is wider than the flex item once the identity box takes its share, and
    // the outer overflow_hidden then clips mid-glyph before the ellipsis can
    // render. The ellipsis rides the line clamp, the same pair the identity
    // tail uses.
    let line = div()
        .w_full()
        .line_clamp(1)
        .text_ellipsis()
        .text_color(rgb(color))
        .child(text);
    div()
        .min_w_0()
        .overflow_hidden()
        .child(if small { line.text_xs() } else { line })
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

/// The glyph's color. Shape carries the state; color only reinforces it.
fn state_color(p: &Palette, s: LatticeState) -> u32 {
    match s {
        LatticeState::Blocked => p.warn,
        LatticeState::DoneUnseen => p.ok,
        LatticeState::Working => p.fg,
        _ => p.muted,
    }
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

pub(super) fn render(root: &Root, now_secs: u64, cx: &mut Context<Root>) -> Stateful<Div> {
    let p = root.palette;
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
    for (i, row) in rows.into_iter().enumerate() {
        let hover = p.hover;
        pane = pane.child(match row {
            Row::Header { title, rollup } => div()
                .id(i)
                .px_3()
                .pt_3()
                .pb_1()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .text_xs()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(p.fg))
                        .child(title),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .children(rollup.into_iter().map(|(state, n)| {
                            div()
                                .text_color(rgb(state_color(&p, state)))
                                .child(format!("{} {n}", lattice_glyph(state).0))
                        })),
                ),
            Row::Error(e) => div()
                .id(i)
                .px_3()
                .py_1()
                .text_xs()
                .text_color(rgb(p.error))
                .child(e),
            Row::Empty(why) => div()
                .id(i)
                .px_3()
                .py_1()
                .text_xs()
                .text_color(rgb(p.muted))
                .child(why),
            Row::More { hidden } => div()
                .id(i)
                .mx_1()
                .px_2()
                .py_1()
                .rounded_md()
                .text_xs()
                .text_color(rgb(p.muted))
                .cursor_pointer()
                .hover(move |s| s.bg(rgb(hover)))
                .active(move |s| s.bg(rgb(p.pressed)))
                .on_click(cx.listener(|root, _, _, cx| root.act(Action::ToggleAgents, cx)))
                .child(if hidden > 0 {
                    format!("+{hidden} more")
                } else {
                    "show fewer".to_string()
                }),
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
                let card = card.unwrap_or_default();
                let cursor = mark == Mark::Cursor;
                let blocked = state == Some(LatticeState::Blocked);
                let label_color = if question { p.warn } else { p.fg };
                // Shape carries the state: a triangle for blocked, a circle for
                // work. A question leads with a diamond, never the `?` an
                // unmeasured agent wears, so none leans on color.
                let (glyph, glyph_color) = match (state, question) {
                    (Some(s), _) => (lattice_glyph(s).0.to_string(), state_color(&p, s)),
                    (None, true) => ("◇".to_string(), p.warn),
                    (None, false) => (String::new(), p.muted),
                };
                let mut title = div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(clamp(label, label_color, false).flex_1().font_weight(
                        if blocked || cursor {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::MEDIUM
                        },
                    ));
                if let Mark::Current(word) = mark {
                    title = title.child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(rgb(p.muted))
                            .child(word),
                    );
                }
                // How long ago the agent last did anything, at the title's
                // right edge where the eye lands on a fresh row.
                if let Some(age) = card.activity.clone() {
                    title = title.child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(rgb(p.muted))
                            .child(age),
                    );
                }
                // The meta line budgets by role: the state word never
                // truncates (it is the row's key fact), and the node and PR
                // tail takes the leftover width - it shrinks before anything
                // else, the node id ellipsizes first, and the whole tail drops
                // when even that cannot fit.
                let mut meta = div().w_full().flex().flex_row().gap_2().child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .text_xs()
                        .text_color(rgb(p.muted))
                        .child(detail),
                );
                if let Some(identity) = identity {
                    meta = meta.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .line_clamp(1)
                            .text_ellipsis()
                            .text_xs()
                            .text_color(rgb(p.muted))
                            .child(identity),
                    );
                }
                // The card's served facts on a third quiet line. An unserved
                // fact leaves no gap, so an agent with none shows no line.
                let facts = [
                    card.model,
                    card.cost,
                    card.created.map(|c| format!("up {c}")),
                    card.context.map(|c| format!("ctx {c}")),
                    card.tokens.map(|t| format!("{t} tok")),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
                let facts = (!facts.is_empty()).then(|| clamp(facts, p.muted, true));
                // One cursor: the fill belongs to the focused agent alone. It
                // marks the row by itself, with no bar or outline on any side.
                let mut item = div()
                    .id(i)
                    .mx_1()
                    .mb(px(2.0))
                    // A definite width, so a label's ellipsis box measures the
                    // row and not its own text.
                    .w(px(WIDTH - 9.0))
                    .px_2()
                    .py(px(6.0))
                    .rounded_lg()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .min_w_0()
                    .when(cursor, |d| d.bg(rgb(p.row_fill)))
                    .child(
                        div()
                            .flex_none()
                            .w(px(14.0))
                            .text_color(rgb(glyph_color))
                            .child(glyph),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(title)
                            .child(meta)
                            .children(facts),
                    );
                if let Some(action) = action {
                    // On click: a press that leaves the row before release
                    // does nothing.
                    item = item
                        .cursor_pointer()
                        .hover(move |s| s.bg(rgb(hover)))
                        .active(move |s| s.bg(rgb(p.pressed)))
                        .on_click(cx.listener(move |root, _, _, cx| root.act(action.clone(), cx)));
                }
                item
            }
        });
    }
    div()
        .id("left-pane")
        .flex()
        .flex_col()
        .flex_none()
        .w(px(WIDTH))
        .h_full()
        .pt(px(28.0))
        .bg(rgb(p.panel))
        .border_r_1()
        .border_color(rgb(p.border))
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
        assert_eq!(rich.tokens.as_deref(), Some("12.3k"));
        assert!(rich.activity.is_some());
        assert_eq!(card("bare").unwrap(), CardFacts::default());
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
