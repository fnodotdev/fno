//! The go-to palette's core: one catalog of every row the app can go to,
//! and the ranker over it. Pure: no gpui, so the tests run without a
//! window.
//!
//! The tiers mirror the mux navigator's ranker (`nav_rank.rs`, the TUI
//! twin). The freeze on `crates/fno` merges until fno ships v0.1.0 stable
//! keeps the two from being one shared function for now, so this side
//! names its toolbag twin in this note. Same tier order, same subsequence
//! rule, and one caller-side difference: the navigator scores a hit in
//! hidden identity tokens as its last tier, the palette scores the visible
//! detail instead. The subsequence tier calls the navigator's own
//! `fuzzy_hit`, so the deepest tier is one leg. Whoever lifts the ranker
//! into one shared function deletes this note.

use fno::app_api::client::agent_sort::sideline_order;
use fno::app_api::web_hq::Board;
use fno::needs_overlay::QuestionItem;
use fno::proto::{AgentRow, Command, SquadMeta};
use fno::search_query::fuzzy_hit;

use gpui::{div, prelude::*, px, rgb, rgba, Context, Div, Stateful};

use super::launch;
use super::left_pane::{agent_identity, agent_row_action, agent_status_word, workspace_detail};
use super::link::Layout;
use super::tabs::VIEWS;
use super::window::{Action, MainView, Root};

use super::keys;
use super::palette::Palette;
use super::ui::{self, LineField};

/// The palette shows at most this many ranked rows.
pub(super) const ROWS_SHOWN: usize = 12;

/// The query cap. Longer input keeps its tail cut, like the install URL
/// field caps its own.
pub(super) const MAX_QUERY: usize = 256;

/// What a row is. The word doubles as the line's leading word in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Agent,
    Workspace,
    Node,
    Question,
    View,
    Command,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Agent => "agent",
            Kind::Workspace => "workspace",
            Kind::Node => "node",
            Kind::Question => "question",
            Kind::View => "view",
            Kind::Command => "command",
        }
    }
}

/// One go-to row: what it is, what it says, and the action Enter or a click
/// runs.
#[derive(Debug, Clone)]
pub(super) struct Target {
    pub kind: Kind,
    pub label: String,
    pub detail: String,
    pub action: Action,
}

/// The palette's command table: `(label, key hint, action builder)`. The
/// composer's two rows open the launch panel with their kind picked; the
/// palette never builds a launch path of its own.
pub(super) const COMMANDS: &[(&str, &str, fn() -> Action)] = &[
    ("Keyboard shortcuts", "⌘/", || Action::ToggleKeySheet),
    ("New worker", "⌘N", || {
        Action::OpenLaunch(Some(launch::Kind::Worker))
    }),
    ("New lead", "⌘N", || {
        Action::OpenLaunch(Some(launch::Kind::Lead))
    }),
];

// [[CHUNK-4]]

/// One fleet seat as the palette reads it, so this module never names the
/// server's seat structs. `local` joins a seat to an agent row when the
/// seat runs in this mux.
pub(super) struct SeatRef<'a> {
    name: &'a str,
    host: &'a str,
    work_id: Option<&'a str>,
    work_title: Option<&'a str>,
    local: Option<&'a AgentRow>,
    status: &'a str,
}

// [[CHUNK-5]]

/// The palette's catalog: every agent (the sidebar folds past ten rows,
/// the palette does not), then workspaces, nodes, questions, views and
/// commands, in catalog order. `seats` are fleet seats joined to their
/// machine's host.
pub(super) fn targets(layout: &Layout, board: &Board, now: u64) -> Vec<Target> {
    let questions: Vec<&QuestionItem> = board
        .questions_newest_first()
        .iter()
        .map(|q| &q.item)
        .collect();
    let seats: Vec<SeatRef> = board
        .fleet()
        .map(|f| {
            f.machines
                .iter()
                .flat_map(|m| {
                    m.leads.iter().flat_map(|l| {
                        l.seats.iter().map(|s| SeatRef {
                            name: &s.name,
                            host: &m.host,
                            work_id: s.work.as_ref().map(|w| w.id.as_str()),
                            work_title: s.work.as_ref().and_then(|w| w.title.as_deref()),
                            local: s.local.as_ref(),
                            status: s.status,
                        })
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    catalog(&layout.agents, &layout.squads, &questions, &seats, now)
}

/// The catalog from its parts, so tests seed agents, squads, seats and
/// questions without a live board.
fn catalog(
    agents: &[AgentRow],
    squads: &[SquadMeta],
    questions: &[&QuestionItem],
    seats: &[SeatRef],
    now: u64,
) -> Vec<Target> {
    let mut out: Vec<Target> = Vec::new();
    // Agents first, every one of them, in the sidebar's own order.
    for a in sideline_order(agents, now) {
        out.push(Target {
            kind: Kind::Agent,
            label: a.name.clone(),
            detail: match agent_identity(a) {
                Some(identity) => format!("{} · {identity}", agent_status_word(a)),
                None => agent_status_word(a),
            },
            action: agent_row_action(a),
        });
    }
    // Remote seats follow the local roster as `name@host`.
    for s in seats {
        if s.local.is_some() {
            continue;
        }
        out.push(Target {
            kind: Kind::Agent,
            label: format!("{}@{}", s.name, s.host),
            detail: s.status.to_string(),
            action: Action::Show(MainView::Orchestration),
        });
    }
    // Workspaces next.
    for s in squads {
        out.push(Target {
            kind: Kind::Workspace,
            label: s.name.clone(),
            detail: workspace_detail(s),
            action: Action::Send(Command::SelectSquad(s.id)),
        });
    }
    catalog_nodes(&mut out, agents, questions, seats);
    out.extend(catalog_fixed(questions));
    out
}

/// The node rows: one per distinct id across agents, seats and questions.
/// The action goes to the agent working it when one is local, else HQ.
fn catalog_nodes(
    out: &mut Vec<Target>,
    agents: &[AgentRow],
    questions: &[&QuestionItem],
    seats: &[SeatRef],
) {
    let mut nodes: Vec<(String, Option<String>)> = Vec::new();
    let mut push_node = |id: &str, title: Option<&str>| {
        if id.is_empty() || nodes.iter().any(|(n, _)| n == id) {
            return;
        }
        nodes.push((id.to_string(), title.map(str::to_string)));
    };
    for a in agents {
        if let Some(node) = a.node.as_deref() {
            push_node(node, None);
        }
    }
    for s in seats {
        if let Some(id) = s.work_id {
            push_node(id, s.work_title);
        }
    }
    for q in questions {
        if let Some(node) = q.node.as_deref() {
            push_node(node, None);
        }
    }
    for (id, title) in &nodes {
        let action = agents
            .iter()
            .find(|a| a.node.as_deref() == Some(id.as_str()))
            .map(agent_row_action)
            .or_else(|| {
                seats.iter().find_map(|s| {
                    s.local.and_then(|a| {
                        (a.node.as_deref() == Some(id.as_str())).then_some(agent_row_action(a))
                    })
                })
            })
            .unwrap_or(Action::Show(MainView::Orchestration));
        let label = match title {
            Some(t) => format!("{id} {t}"),
            None => id.clone(),
        };
        out.push(Target {
            kind: Kind::Node,
            label,
            detail: String::new(),
            action,
        });
    }
}

/// The rows after the node section: questions, views and commands.
fn catalog_fixed(questions: &[&QuestionItem]) -> Vec<Target> {
    let mut out: Vec<Target> = Vec::new();
    // Questions: the same row the sidebar's For-you section sends.
    for q in questions {
        out.push(Target {
            kind: Kind::Question,
            label: q.title.clone(),
            detail: q.node.clone().unwrap_or_else(|| q.kind.clone()),
            action: Action::OpenAnswer(q.id.clone()),
        });
    }
    // Views: the tab bar's own table.
    for (name, view) in VIEWS.iter() {
        out.push(Target {
            kind: Kind::View,
            label: (*name).to_string(),
            detail: String::new(),
            action: Action::Show(*view),
        });
    }
    // Commands: one row per COMMANDS entry, ranked like everything else.
    for (label, key, build) in COMMANDS.iter() {
        out.push(Target {
            kind: Kind::Command,
            label: (*label).to_string(),
            detail: (*key).to_string(),
            action: build(),
        });
    }
    out
}

/// How well `label` matches `query`, lower first; `None` drops the row.
/// Both sides are lowercased here. The tiers mirror the navigator's
/// `nav_score`.
fn tier(query: &str, label: &str, detail: &str) -> Option<u8> {
    let q = query.to_lowercase();
    if q.is_empty() {
        return Some(0);
    }
    let l = label.to_lowercase();
    let words = || {
        l.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
    };
    if l == q || l.split(" › ").any(|s| s.trim() == q.as_str()) {
        Some(0)
    } else if l.starts_with(q.as_str()) || l.split(" › ").any(|s| s.trim().starts_with(q.as_str()))
    {
        Some(1)
    } else if words().any(|w| w.starts_with(q.as_str())) {
        Some(2)
    } else if l.contains(q.as_str()) {
        Some(3)
    } else if fuzzy_hit(&q, &l) {
        Some(4)
    } else if detail.to_lowercase().contains(q.as_str()) {
        Some(5)
    } else {
        None
    }
}

/// The palette's order: the catalog indices whose label matches `query`,
/// best tier first, catalog order inside a tier (stable sort). An empty
/// query keeps catalog order.
pub(super) fn rank(query: &str, targets: &[Target]) -> Vec<usize> {
    if query.is_empty() {
        return (0..targets.len()).collect();
    }
    let mut hits: Vec<(usize, u8)> = targets
        .iter()
        .enumerate()
        .filter_map(|(i, t)| tier(query, &t.label, &t.detail).map(|n| (i, n)))
        .collect();
    hits.sort_by_key(|&(_, n)| n);
    hits.into_iter().map(|(i, _)| i).collect()
}

/// The palette's state while it shows: the query and the selected row.
pub(super) struct GoTo {
    pub query: LineField,
    pub selected: usize,
}

/// Run the ranked row at display index `at`, if it exists, and close.
fn run_ranked(root: &mut Root, at: usize, cx: &mut Context<Root>) {
    let query = root
        .goto
        .as_ref()
        .map_or(String::new(), |g| g.query.text().to_string());
    let now = fno::app_api::digest_overlay::now_secs();
    let targets = targets(&root.layout, &root.board, now);
    let order = rank(&query, &targets);
    let Some(&t) = order.get(at) else {
        return;
    };
    let target = targets.get(t).expect("rank returns in-range indices");
    let action = target.action.clone();
    root.goto = None;
    root.act(action, cx);
}

/// The selected row clamped to the ranked list's length.
fn selected(root: &Root) -> usize {
    let Some(goto) = root.goto.as_ref() else {
        return 0;
    };
    let now = fno::app_api::digest_overlay::now_secs();
    let targets = targets(&root.layout, &root.board, now);
    let order = rank(goto.query.text(), &targets);
    goto.selected.min(order.len().saturating_sub(1))
}

/// Every key while the palette shows. True consumes the key; false closed
/// the palette and lets a cmd key run as today.
pub(super) fn key(root: &mut Root, k: &keys::Key, cx: &mut Context<Root>) -> bool {
    if root.goto.is_none() {
        return false;
    }
    if k.cmd {
        if k.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                let first = text.lines().next().unwrap_or_default().to_string();
                if let Some(goto) = root.goto.as_mut() {
                    goto.query.edit("", Some(&first));
                    goto.selected = 0;
                }
                cx.notify();
            }
            return true;
        }
        // Another cmd key closes the palette and runs as today.
        root.goto = None;
        cx.notify();
        return false;
    }
    match k.key {
        "escape" => {
            root.goto = None;
            cx.notify();
        }
        "enter" => run_ranked(root, selected(root), cx),
        "up" => {
            if let Some(goto) = root.goto.as_mut() {
                goto.selected = goto.selected.saturating_sub(1);
            }
            cx.notify();
        }
        "down" => {
            if let Some(goto) = root.goto.as_mut() {
                goto.selected += 1;
            }
            cx.notify();
        }
        _ => {
            let text = k.key_char.filter(|_| !k.ctrl);
            if let Some(goto) = root.goto.as_mut() {
                goto.query.edit(k.key, text);
                goto.selected = 0;
            }
            cx.notify();
        }
    }
    true
}

/// The palette over a dimmed window, in the key sheet's look: a p.panel
/// panel, a p.border hairline, 6 px radius, ui::MONO at ui::BODY. Top: the
/// query field and an esc hint. Body: up to 12 ranked rows. Footer: the
/// move/go/close hints. A click outside closes it, like the sheet.
pub(super) fn render(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let now = fno::app_api::digest_overlay::now_secs();
    let targets = targets(&root.layout, &root.board, now);
    let query = root
        .goto
        .as_ref()
        .map_or(String::new(), |g| g.query.text().to_string());
    let order = rank(&query, &targets);
    let sel = selected(root);
    let mut body = div().flex().flex_col();
    for (i, &at) in order.iter().take(ROWS_SHOWN).enumerate() {
        // rank only ever returns catalog indices.
        let t = targets.get(at).expect("rank returns in-range indices");
        body = body.child(row(t, at, i, i == sel, &p, cx));
    }
    if order.is_empty() {
        body = body.child(ui::quiet(format!("nothing matches \"{query}\""), &p));
    }
    let panel = div()
        .id("goto")
        .flex()
        .flex_col()
        .w(px(560.0))
        .max_h(px(520.0))
        .px_3()
        .py_3()
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(p.border))
        .bg(rgb(p.panel))
        .font_family(ui::MONO)
        .text_size(px(ui::BODY))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(field(&query, &p))
        .child(div().id("goto-body").overflow_y_scroll().mt_2().child(body))
        .child(
            ui::role(div(), ui::Role::Hint, &p)
                .pt(px(8.0))
                .child("↑↓ move · ⏎ go · esc close"),
        );
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x0000_0066))
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|root, _, _, cx| root.close_goto(cx)),
        )
        .child(panel)
}

/// The query field: the typed text or the faint placeholder, with the caret.
fn field(query: &str, p: &Palette) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .bg(rgb(p.raised))
        .rounded(px(4.0))
        .px_2()
        .py_1()
        .child(match query.is_empty() {
            true => ui::text("go to…", ui::Role::Hint, p).into_any_element(),
            false => ui::text(query.to_string(), ui::Role::Body, p).into_any_element(),
        })
        .child(ui::text("▏", ui::Role::Name, p))
}

/// One ranked row: kind word, label, detail. Clicked rows run their
/// action; the selected row carries the row-fill band.
fn row(
    t: &Target,
    at: usize,
    i: usize,
    sel: bool,
    p: &Palette,
    cx: &mut Context<Root>,
) -> Stateful<Div> {
    div()
        .id(gpui::ElementId::NamedInteger("goto-row".into(), i as u64))
        .h(px(ui::ROW_H))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .min_w_0()
        .when(sel, |d| d.bg(rgb(p.row_fill)))
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(p.hover)))
        .active(move |s| s.bg(rgb(p.pressed)))
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(move |root, _, _, cx| run_ranked(root, at, cx)),
        )
        .child(ui::text(t.kind.word(), ui::Role::Hint, p))
        .child(ui::text("·", ui::Role::Hint, p))
        .child(ui::clamp(t.label.clone(), ui::Role::Name, p))
        .when(!t.detail.is_empty(), |d| {
            d.child(ui::text("·", ui::Role::Hint, p)).child(ui::clamp(
                t.detail.clone(),
                ui::Role::Meta,
                p,
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str, node: Option<&str>) -> AgentRow {
        AgentRow {
            name: name.into(),
            node: node.map(str::to_string),
            ..AgentRow::default()
        }
    }

    fn squad(id: u64, name: &str) -> SquadMeta {
        SquadMeta {
            id,
            name: name.into(),
            canonical_cwd: String::new(),
            project: None,
            tabs: Vec::new(),
            active_tab: 0,
            panes: 0,
        }
    }

    fn seat<'a>(
        host: &'a str,
        node: &'a str,
        title: &'a str,
        local: Option<&'a AgentRow>,
    ) -> SeatRef<'a> {
        SeatRef {
            name: "seat-name",
            host,
            work_id: Some(node),
            work_title: Some(title),
            local,
            status: "writing",
        }
    }

    #[test]
    fn catalog_lists_every_agent_and_one_row_per_kind() {
        let agents: Vec<AgentRow> = (0..12)
            .map(|i| agent(&format!("agent-{i:02}"), None))
            .collect();
        let squads = vec![squad(1, "fno"), squad(2, "c3po")];
        let local = agent("t-worker", Some("x-aaaa"));
        let seat = seat("imac", "x-aaaa", "one palette to rule them", Some(&local));
        let seats = [seat];
        let questions = [QuestionItem {
            title: "pick the shot key".into(),
            kind: "question".into(),
            ..QuestionItem::default()
        }];
        let qrefs: Vec<&QuestionItem> = questions.iter().collect();
        let targets = catalog(&agents, &squads, &qrefs, &seats, 0);
        assert_eq!(targets.iter().filter(|t| t.kind == Kind::Agent).count(), 12);
        assert_eq!(
            targets.iter().filter(|t| t.kind == Kind::Workspace).count(),
            2
        );
        let node_rows: Vec<&Target> = targets.iter().filter(|t| t.kind == Kind::Node).collect();
        assert_eq!(node_rows.len(), 1);
        let node_row = node_rows.first().expect("the seat's node row exists");
        assert!(node_row.label.starts_with("x-aaaa"));
        assert_eq!(
            targets.iter().filter(|t| t.kind == Kind::Question).count(),
            1
        );
        assert_eq!(targets.iter().filter(|t| t.kind == Kind::View).count(), 5);
        assert!(
            targets.iter().any(|t| t.label == "agent-11"),
            "the 12th agent's row exists although the sidebar folds it"
        );
    }

    #[test]
    fn rank_tiers_reviewer_above_fno_review_and_drops_misses() {
        let targets = vec![
            Target {
                kind: Kind::Agent,
                label: "fno-review".into(),
                detail: String::new(),
                action: Action::Show(MainView::Orchestration),
            },
            Target {
                kind: Kind::Agent,
                label: "reviewer".into(),
                detail: String::new(),
                action: Action::Show(MainView::Orchestration),
            },
        ];
        // "rev" keeps both rows (contains keeps fno-review at tier 3);
        // the tier order puts reviewer first.
        assert_eq!(rank("rev", &targets), vec![1, 0], "reviewer comes first");
        assert_eq!(
            rank("frv", &targets),
            vec![0],
            "only the subsequence hit stays"
        );
        assert!(rank("zzz", &targets).is_empty());
        assert_eq!(
            rank("", &targets),
            vec![0, 1],
            "an empty query keeps catalog order"
        );
    }

    #[test]
    fn commands_ship_the_key_sheet_toggle() {
        let (label, key, build) = COMMANDS.first().copied().expect("the table is non-empty");
        assert_eq!((label, key), ("Keyboard shortcuts", "⌘/"));
        assert_eq!(build(), Action::ToggleKeySheet);
    }
}
