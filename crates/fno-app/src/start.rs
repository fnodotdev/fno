//! The empty pane's start view: the launch composer inline, the recent
//! workspaces, and the hint row. It replaces the "focused pane hosts no
//! agent" line, to the approved start-view mockups. The card binds the
//! launch panel's own state, so Enter submits through the same door the
//! modal uses and the two never diverge.

use gpui::{
    canvas, div, prelude::*, px, rgb, Context, CursorStyle, MouseButton, SharedString, Stateful,
    Window,
};

use crate::chat::field::Field;
use crate::launch::{self, Kind};
use crate::link::Layout;
use crate::palette::Palette;
use crate::ui::{self, Role};
use crate::window::{Action, MainView, Root};
use fno::app_api::client::agent_lattice_state;
use fno::app_api::client::card_line::card_facts;
use fno::app_api::lattice::{status_long, LatticeState};
use fno::proto::AgentRow;

/// The card column's width, centered in the pane.
const CARD: f32 = 600.0;

/// The start view's precondition: the chat pane with no session in it.
pub(crate) fn visible(root: &Root) -> bool {
    root.main == MainView::Chat && root.chat.key.is_none()
}

/// A path's final component, the name a squad and its rows meet on: the
/// wire carries only the basename.
fn base_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or("").to_string()
}

/// The branch half of a row's subline (`<branch> · <cwd-tail>`, a harness
/// title optionally first). The tail is always last when the cwd exists, so
/// the branch is the segment before it. A segment with a space is a title,
/// never a branch: git refs hold no spaces.
pub(super) fn branch_of<'a>(a: &'a AgentRow, base: &str) -> Option<&'a str> {
    // Walked from the right: the tail (the cwd basename) is always last
    // when the cwd exists, and the branch is the segment before it. A
    // segment with a space is a title, never a branch: git refs hold no
    // spaces.
    let mut segs = a.subline.as_deref()?.rsplit(" · ");
    if segs.next()? != base {
        return None;
    }
    let head = segs.next()?;
    (!head.is_empty() && !head.contains(' ')).then_some(head)
}

/// One recent workspace row, derived: the squad's project with the facts
/// its members carry.
pub(crate) struct WorkspaceRow {
    pub name: String,
    pub cwd: String,
    pub branch: Option<String>,
    pub agents: usize,
    pub age: Option<String>,
    pub state: Option<LatticeState>,
}

/// The squads as recent-workspace rows: members grouped by cwd basename,
/// freshest member first, the four freshest kept. A squad with no member
/// carries no recency signal and drops out.
pub(crate) fn workspaces(layout: &Layout, now: u64) -> Vec<WorkspaceRow> {
    let mut scored: Vec<(u64, WorkspaceRow)> = layout
        .squads
        .iter()
        .filter(|s| !s.canonical_cwd.is_empty())
        .filter_map(|squad| {
            let base = base_name(&squad.canonical_cwd);
            let members: Vec<&AgentRow> = layout
                .agents
                .iter()
                .filter(|a| a.cwd_base.as_deref() == Some(base.as_str()))
                .collect();
            let freshest = members
                .iter()
                .min_by_key(|a| a.last_activity_age_s.unwrap_or(u64::MAX))?;
            let score = freshest.last_activity_age_s.unwrap_or(u64::MAX);
            let facts = card_facts(*freshest, now);
            let row = WorkspaceRow {
                name: base.clone(),
                cwd: squad.canonical_cwd.clone(),
                branch: members
                    .iter()
                    .min_by_key(|a| a.last_activity_age_s.unwrap_or(u64::MAX))
                    .and_then(|a| branch_of(*a, &base))
                    .map(str::to_string),
                agents: members.len(),
                age: facts.activity.or(facts.created),
                state: Some(agent_lattice_state(*freshest)),
            };
            Some((score, row))
        })
        .collect();
    scored.sort_by_key(|(score, _)| *score);
    scored.truncate(4);
    scored.into_iter().map(|(_, row)| row).collect()
}

/// The line above the card: the project, its path, and the branch and
/// machine when they are known.
struct HeaderFacts {
    project: String,
    path: String,
    branch: Option<String>,
    machine: Option<String>,
}

fn header_facts(root: &Root) -> HeaderFacts {
    let l = &root.launch.state;
    let project = base_name(l.project());
    let branch = root
        .layout
        .agents
        .iter()
        .filter(|a| a.cwd_base.as_deref() == Some(project.as_str()))
        .min_by_key(|a| a.last_activity_age_s.unwrap_or(u64::MAX))
        .and_then(|a| branch_of(a, &project))
        .map(str::to_string);
    let machine = root
        .board
        .fleet()
        .and_then(|f| f.machines.iter().find(|m| m.local))
        .map(|m| m.display_name.clone().unwrap_or_else(|| m.host.clone()));
    HeaderFacts {
        path: launch::short_project(l.project()),
        project,
        branch,
        machine,
    }
}

/// The whole start view. The card column sits a touch above center, as the
/// mockups do.
pub(crate) fn render(root: &Root, window: &Window, cx: &mut Context<Root>) -> gpui::Div {
    let p = root.palette;
    let facts = header_facts(root);
    let rows = workspaces(&root.layout, fno::app_api::digest_overlay::now_secs());
    div()
        .relative()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .flex_1()
        .min_h_0()
        .w_full()
        .pb(px(120.0))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(28.0))
                .w(px(CARD))
                .font_family(ui::MONO)
                .text_size(px(ui::BODY))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_center()
                        .child(crate::first_run::mark_playing(p, root.shot)),
                )
                .children(header_row(&facts, &p))
                .child(card(root, window, cx))
                .children(recent(&rows, &p, cx))
                .child(if root.first_run {
                    crate::first_run::key_card(&p)
                } else {
                    hints(root, &p)
                }),
        )
}

fn header_row(f: &HeaderFacts, p: &Palette) -> Option<gpui::Div> {
    if f.project.is_empty() {
        return None;
    }
    let mut tail: Vec<&str> = Vec::new();
    if let Some(b) = &f.branch {
        tail.push(b);
    }
    if let Some(m) = &f.machine {
        tail.push(m);
    }
    Some(
        div()
            .flex()
            .flex_row()
            .items_baseline()
            .gap(px(10.0))
            .child(ui::text(f.project.clone(), Role::Heading, p))
            .child(ui::text(f.path.clone(), Role::Meta, p))
            .when(!tail.is_empty(), |d| {
                d.child(ui::text(tail.join(" · "), Role::Meta, p))
            }),
    )
}

/// The composer card: the launch draft's field, the pick chips, the
/// monochrome send. A click on the field lands through the launch panel's
/// own handlers, so the caret, the drag select and the IME window all
/// behave as they do in the modal.
fn card(root: &Root, window: &Window, cx: &mut Context<Root>) -> Stateful<gpui::Div> {
    let p = root.palette;
    let l = &root.launch.state;
    let at = l.composer.clone();
    let base = base_name(l.project());
    let placeholder: SharedString = match l.kind {
        Kind::Worker if !base.is_empty() => {
            format!("Describe the work. Enter starts a worker in {base}.").into()
        }
        // The card carries no epic list: an empty draft starts the lead
        // with no scope, and the modal picks epics.
        Kind::Lead => "Enter starts a lead with no scope; cmd-n opens the epic picker".into(),
        _ => l.kind.placeholder().into(),
    };
    let text = Field {
        draft: l.draft.clone(),
        placeholder,
        fg: rgb(p.fg).into(),
        muted: rgb(p.muted).into(),
        selection: (p.accent << 8) | 0x55,
        focused: root.has_focus(window),
        state: l.field.clone(),
    };
    let field = div()
        .id("start-field")
        .relative()
        .w_full()
        .max_h(px(160.0))
        .overflow_y_scroll()
        .track_scroll(&l.field.scroll)
        .py_1()
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
        .child(text);
    div()
        .id("start-card")
        .relative()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .px_3()
        .py_2()
        .rounded(px(8.0))
        .border_1()
        .border_color(rgb(p.border))
        .bg(rgb(p.raised))
        .children(launch::skill_hints(root))
        .child(field)
        .child(chips(root, &p, cx))
        .children(launch::phase_line(l, &p))
}

/// The card's bottom row: the kind and pick chips at the left, the return
/// hint (cmd held only) and the send at the right.
fn chips(root: &Root, p: &Palette, cx: &mut Context<Root>) -> gpui::Div {
    let l = &root.launch.state;
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .child(
            chip("start-chip-kind", l.kind.label().to_string(), p).on_click(cx.listener(
                |root, _, _, cx| {
                    let next = match root.launch.state.kind {
                        Kind::Worker => Kind::Lead,
                        Kind::Lead => Kind::Shell,
                        Kind::Shell => Kind::Worker,
                    };
                    root.launch.state.set_kind(next);
                    cx.notify();
                },
            )),
        )
        .child(
            chip(
                "start-chip-axis",
                format!("{} · {}", l.harness(), launch::model_label(l)),
                p,
            )
            .on_click(cx.listener(|root, _, _, cx| {
                root.launch.state.step_model(true);
                cx.notify();
            })),
        )
        .child(
            chip(
                "start-chip-effort",
                format!("effort {}", launch::effort_label(l)),
                p,
            )
            .on_click(cx.listener(|root, _, _, cx| {
                root.launch.state.step_effort(true);
                cx.notify();
            })),
        )
        .child(div().flex_1())
        .when(root.picking, |d| d.child(ui::kbd("↵", p)))
        .child(send(p, cx))
}

fn chip(id: &'static str, label: String, p: &Palette) -> Stateful<gpui::Div> {
    ui::clickable(
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .px_2()
            .py_1()
            .rounded(px(4.0)),
        p,
    )
    .text_size(px(ui::SMALL))
    .child(ui::text(label, Role::Meta, p))
    .child(ui::kbd("⌄", p))
}

/// The send button, monochrome: the fill is the theme's own foreground, so
/// it never reads blue and never needs per-theme colors. The hover dims the
/// fill; the ui::clickable hover would repaint the fill out from under the
/// glyph.
fn send(p: &Palette, cx: &mut Context<Root>) -> Stateful<gpui::Div> {
    div()
        .id("start-send")
        .cursor_pointer()
        .hover(|s| s.opacity(0.8))
        .size(px(24.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .bg(rgb(p.fg))
        .on_click(cx.listener(|root, _, _, cx| root.act(Action::SubmitLaunch, cx)))
        .child(
            div()
                .text_color(rgb(p.bg))
                .text_size(px(ui::BODY))
                .child("↑".to_string()),
        )
}

/// The recent workspaces: the section with its rows, or nothing when no
/// squad has members. A click moves the composer's pick to that workspace.
/// The rows take no cmd digits.
fn recent(rows: &[WorkspaceRow], p: &Palette, cx: &mut Context<Root>) -> Option<gpui::Div> {
    if rows.is_empty() {
        return None;
    }
    let list = rows.iter().enumerate().map(|(i, w)| {
        let dot = w
            .state
            .map(|s| rgb(p.status(status_long(s))))
            .unwrap_or_else(|| rgb(p.muted));
        let agents = if w.agents == 1 {
            "1 agent".to_string()
        } else {
            format!("{} agents", w.agents)
        };
        ui::clickable(ui::row(i, false, p).w_full(), p)
            .on_click({
                let cwd = w.cwd.clone();
                cx.listener(move |root, _, _, cx| {
                    root.launch.state.pick_project(cwd.clone());
                    cx.notify();
                })
            })
            .child(div().flex_none().size(px(6.0)).rounded_full().bg(dot))
            .child(ui::clamp(w.name.clone(), Role::Name, p).flex_1().min_w_0())
            .children(w.branch.clone().map(|b| ui::text(b, Role::Meta, p)))
            .child(ui::text(agents, Role::Meta, p))
            .children(w.age.clone().map(|a| {
                div()
                    .w(px(40.0))
                    .flex()
                    .justify_end()
                    .child(ui::text(a, Role::Meta, p))
            }))
    });
    Some(
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(ui::section("Recent workspaces", p))
            .children(list),
    )
}

/// The hint row: real chords only, every binding named here exists in the
/// app's key table. The row's slot is reserved in both states: the content
/// fades out while cmd is up and never unmounts, so a cmd press adds no
/// height and the centered block never shifts.
fn hints(root: &Root, p: &Palette) -> gpui::Div {
    let pair = |keys: &'static str, label: &'static str| {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .child(ui::kbd(keys, p))
            .child(ui::quiet(label, p))
    };
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap_x(px(18.0))
        .gap_y(px(6.0))
        .when(!root.picking, |d| d.opacity(0.0))
        .child(pair("⌘N", "new agent, lead or shell"))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .child(ui::kbd("⌘1", p))
                .child(ui::quiet("to", p))
                .child(ui::kbd("⌘9", p))
                .child(ui::quiet("jump to a row", p)),
        )
        .child(pair("⌘K", "palette"))
        .child(pair("⌘D", "split"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fno::proto::SquadMeta;

    fn agent(name: &str, cwd_base: Option<&str>, age: Option<u64>) -> AgentRow {
        serde_json::from_value(serde_json::json!({
            "squad": null, "name": name, "pane_id": null, "badge": null,
            "reason": null, "exited": false,
            "cwd_base": cwd_base, "last_activity_age_s": age,
        }))
        .unwrap()
    }

    fn squad(id: u64, cwd: &str) -> SquadMeta {
        SquadMeta {
            id,
            name: base_name(cwd),
            canonical_cwd: cwd.to_string(),
            project: None,
            tabs: Vec::new(),
            active_tab: 0,
            panes: 0,
        }
    }

    #[test]
    fn workspaces_group_members_and_order_freshest_first() {
        let mut layout = Layout::default();
        layout.squads = vec![squad(1, "/code/harbor"), squad(2, "/code/quarry")];
        layout.agents = vec![
            agent("slow", Some("harbor"), Some(600)),
            agent("quick", Some("quarry"), Some(30)),
            agent("other", Some("elsewhere"), Some(1)),
        ];
        let rows = workspaces(&layout, 100);
        assert_eq!(rows.len(), 2);
        let first = rows.first().expect("two squads with members yield rows");
        assert_eq!(first.name, "quarry");
        assert_eq!(first.agents, 1);
        // The dot reads the freshest member's state: recent activity with no
        // live badge is Idle, never Unmeasured.
        assert_eq!(first.state, Some(LatticeState::Idle));
        let second = rows.get(1).expect("a second row");
        assert_eq!(second.name, "harbor");
    }

    #[test]
    fn four_squads_are_the_cap() {
        let mut layout = Layout::default();
        layout.squads = (1..=5).map(|i| squad(i, &format!("/code/p{i}"))).collect();
        layout.agents = (1..=5)
            .map(|i| {
                let mut a = agent(&format!("a{i}"), Some(&format!("p{i}")), Some(i * 10));
                a.subline = Some(format!("main · p{i}"));
                a
            })
            .collect();
        let rows = workspaces(&layout, 100);
        assert_eq!(rows.len(), 4);
        let first = rows.first().expect("five squads cap at four rows");
        assert_eq!(first.branch.as_deref(), Some("main"));
    }

    #[test]
    fn a_branch_needs_its_separator_and_no_title_space() {
        let mut with = agent("a", Some("harbor"), Some(5));
        with.subline = Some("feat/ledger · harbor".into());
        assert_eq!(branch_of(&with, "harbor"), Some("feat/ledger"));

        let mut titled = agent("b", Some("harbor"), Some(5));
        titled.subline = Some("Refactor the auth cache · feat/x · harbor".into());
        assert_eq!(branch_of(&titled, "harbor"), Some("feat/x"));

        let mut title_only = agent("c", Some("harbor"), Some(5));
        title_only.subline = Some("Refactor the auth cache · harbor".into());
        assert_eq!(branch_of(&title_only, "harbor"), None);

        let bare = agent("d", Some("harbor"), Some(5));
        assert_eq!(branch_of(&bare, "harbor"), None);
    }

    #[test]
    fn the_freshest_member_names_the_branch() {
        let mut layout = Layout::default();
        layout.squads = vec![squad(1, "/code/harbor")];
        let mut stale = agent("stale", Some("harbor"), Some(600));
        stale.subline = Some("feat/old · harbor".into());
        let mut fresh = agent("fresh", Some("harbor"), Some(5));
        fresh.subline = Some("feat/new · harbor".into());
        layout.agents = vec![stale, fresh];
        let rows = workspaces(&layout, 100);
        let first = rows.first().expect("one squad with members yields a row");
        assert_eq!(first.branch.as_deref(), Some("feat/new"));
    }
}
