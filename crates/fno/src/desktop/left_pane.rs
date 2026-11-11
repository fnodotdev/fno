//! The native left pane: agents, sessions, questions and machines. Agents
//! order through the TUI sideline's own comparator, and a click sends the
//! command the sideline click sends. A section whose read failed shows the
//! error and keeps its last good rows.

use gpui::{div, prelude::*, px, rgb, Context, Div, Stateful};

use super::link::Layout;
use super::window::{Action, MainView, Root};
use crate::proto::{AgentBadge, AgentRow, Command};
use crate::web_hq::Board;

pub const WIDTH: f32 = 260.0;
/// The agents shown before a "+N more" row, so the sections below stay above
/// the fold.
pub const AGENTS_SHOWN: usize = 8;

/// How a row's label reads: a blocked agent in the error color, a question
/// in the needs-you color the board's feed uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Blocked,
    Question,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Header(&'static str),
    Item {
        label: String,
        detail: String,
        active: bool,
        tone: Tone,
        action: Option<Action>,
    },
    /// The agents past [`AGENTS_SHOWN`], folded, or the row that folds
    /// them back.
    More {
        hidden: usize,
    },
    /// A failed read. The section's last good rows follow it.
    Error(String),
    /// A section with nothing to show, and why.
    Empty(&'static str),
}

fn agent_detail(a: &AgentRow) -> String {
    let state = if a.exited {
        Some("exited")
    } else {
        a.badge.map(|b| match b {
            AgentBadge::Working => "working",
            AgentBadge::Blocked => "blocked",
            AgentBadge::Done => "done",
        })
    };
    let harness = a.harness.as_deref().unwrap_or("pane");
    match state {
        Some(s) => format!("{harness} · {s}"),
        None => harness.to_string(),
    }
}

pub fn rows(layout: &Layout, board: &Board, now_secs: u64, agents_open: bool) -> Vec<Row> {
    let mut out = vec![Row::Header("Agents")];
    let agents = crate::client::agent_sort::sideline_order(&layout.agents, now_secs);
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
        use crate::client::agent_group::{click_command, RowClick};
        let action = match click_command(a) {
            RowClick::Send(cmd) => Action::Send(cmd),
            RowClick::Notice(notice) => Action::Notice(notice),
        };
        out.push(Row::Item {
            label: a.name.clone(),
            detail: agent_detail(a),
            active: a.pane_id == Some(layout.focus),
            tone: if a.badge == Some(AgentBadge::Blocked) {
                Tone::Blocked
            } else {
                Tone::Plain
            },
            action: Some(action),
        });
    }
    if total > AGENTS_SHOWN {
        out.push(Row::More {
            hidden: total - shown,
        });
    }

    out.push(Row::Header("Sessions"));
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
            active: s.id == layout.active_squad,
            tone: Tone::Plain,
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
    out.push(Row::Header("Questions"));
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
            active: false,
            tone: Tone::Question,
            action: Some(Action::Show(MainView::Orchestration)),
        });
    }

    out.push(Row::Header("Machines"));
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
            active: m.local,
            tone: Tone::Plain,
            action: None,
        });
    }
    out
}

pub(super) fn render(root: &Root, now_secs: u64, cx: &mut Context<Root>) -> Stateful<Div> {
    let p = root.palette;
    // The top band clears the traffic lights the transparent title bar
    // draws over this pane.
    let mut pane = div()
        .id("left-pane")
        .flex()
        .flex_col()
        .flex_none()
        .w(px(WIDTH))
        .h_full()
        .overflow_y_scroll()
        .pt(px(28.0))
        .pb_2()
        .bg(rgb(p.panel))
        .border_r_1()
        .border_color(rgb(p.border));
    let rows = rows(&root.layout, &root.board, now_secs, root.agents_open);
    for (i, row) in rows.into_iter().enumerate() {
        let hover = p.border;
        pane = pane.child(match row {
            Row::Header(title) => div()
                .id(i)
                .px_3()
                .pt_3()
                .pb_1()
                .text_xs()
                .text_color(rgb(p.muted))
                .child(title.to_uppercase()),
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
                .active(move |s| s.bg(rgb(p.focus)))
                .on_click(cx.listener(|root, _, _, cx| root.act(Action::ToggleAgents, cx)))
                .child(if hidden > 0 {
                    format!("+{hidden} more")
                } else {
                    "show fewer".to_string()
                }),
            Row::Item {
                label,
                detail,
                active,
                tone,
                action,
            } => {
                let label_color = match tone {
                    Tone::Plain => p.fg,
                    Tone::Blocked => p.error,
                    Tone::Question => p.warn,
                };
                // The active row carries a bar in the focus color as well as
                // the fill, so it reads at a glance.
                let mut item = div()
                    .id(i)
                    .mx_1()
                    .pl(px(6.0))
                    .pr_2()
                    .py_1()
                    .rounded_md()
                    .flex()
                    .flex_col()
                    .border_l_2()
                    .border_color(rgb(if active { p.ok } else { p.panel }))
                    .when(active, |d| d.bg(rgb(p.border)))
                    .child(div().truncate().text_color(rgb(label_color)).child(label))
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(rgb(p.muted))
                            .child(detail),
                    );
                if let Some(action) = action {
                    // On click: a press that leaves the row before release
                    // does nothing.
                    item = item
                        .cursor_pointer()
                        .hover(move |s| s.bg(rgb(hover)))
                        .active(move |s| s.bg(rgb(p.focus)))
                        .on_click(cx.listener(move |root, _, _, cx| root.act(action.clone(), cx)));
                }
                item
            }
        });
    }
    pane
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{SquadMeta, TabMeta};

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
        let question = crate::needs_overlay::QuestionItem {
            id: "q-1".into(),
            title: "ship it?".into(),
            state: "open".into(),
            ..Default::default()
        };
        let fold = crate::needs_overlay::QuestionsFold {
            items: vec![question],
            ..Default::default()
        };
        let none = crate::backlog_model::Inputs::default();
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
