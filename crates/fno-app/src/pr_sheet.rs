//! The read-only PR sheet: an agent row's PR facts in the key sheet's
//! modal look (dimmed overlay, centered mono panel, text roles, hairline
//! sections, one accent, keyboard hints). Alt-click on an agent row with a
//! PR opens it; esc, `q` or a click outside closes it; `o` opens the PR on
//! GitHub behind the same `is_openable` gate the server's OpenLink events
//! pass. It reads only: no editor, no comment surface.

use fno_proto::{PrFailingCheck, PrStatusFact};
use gpui::{div, prelude::*, px, rgb, rgba, Context, Div, FontWeight};

use super::ui::{self, Role};
use super::window::Root;

/// The demo gate: set by the shot entry, so the shot frames paint one
/// agent row's PR facts without a world that has PR rows. The live app
/// never sets it.
static DEMO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Turn the demo stamp on (the shot entry calls this before the window
/// opens, so the first layout already paints it).
pub(super) fn enable_demo() {
    DEMO.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub(super) fn demo_enabled() -> bool {
    DEMO.load(std::sync::atomic::Ordering::Relaxed)
}

/// Stamp the layout's rows with the demo fact: the first row that already
/// carries a PR takes it; when no row carries a PR, one live-looking agent
/// row gains an invented PR with the fact, so the shot shows the row
/// tokens and the sheet without a world that has PR rows. The live app
/// never calls this.
pub(super) fn demo_stamp(agents: &mut [fno_proto::AgentRow]) {
    if !demo_enabled() {
        return;
    }
    let fact = demo_fact();
    // Every agent row takes the tokens, so the board frame shows the look
    // wherever the world's roster is long: the display order is the
    // server's, and one picked row can sit below the fold. Rows without a
    // PR gain the invented #24, and rows without a node gain the seeded
    // demo node, so the identity column carries the `#24 · node` text the
    // tokens sit before; a row that already carries either keeps its own.
    // The sheet reads the picked row's own number and URL.
    for row in agents.iter_mut() {
        if row.pr.is_none() {
            row.pr = Some(24);
        }
        if row.node.is_none() {
            row.node = Some("x-aaaa".into());
        }
        row.pr_status = Some(fact.clone());
    }
}

/// The demo fact the shots paint: red with two failing checks (the
/// done-means shape), reviewed, OPEN and MERGEABLE. The tests reuse it.
pub(super) fn demo_fact() -> PrStatusFact {
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
        title: Some("the agent row shows a PR's checks and review".into()),
        settled: false,
        pr_state: Some("OPEN".into()),
        draft: false,
        mergeable: Some("MERGEABLE".into()),
        measured_at: 0,
    }
}

/// The window state while the root.pr_sheet holds Some.
pub(super) struct PrSheet {
    pub(super) pr: u64,
    pub(super) url: Option<String>,
    pub(super) fact: PrStatusFact,
    pub(super) agent: String,
    pub(super) slug: Option<String>,
}

/// One rendered row of the sheet, the model the tests read.
pub(super) struct Row {
    pub label: String,
    pub text: String,
    pub ok: bool,
    pub error: bool,
    pub bold: bool,
    /// Secondary: the door's raw error tail, dimmed so the step reads first.
    pub dim: bool,
}

fn row(label: &str, text: impl Into<String>, bold: bool) -> Row {
    Row {
        label: label.to_string(),
        text: text.into(),
        ok: false,
        error: false,
        bold,
        dim: false,
    }
}

/// The sheet's content from one fact: the state line, the checks line, the
/// failing checks named (the done-means), and the review line. Free so the
/// content tests without a window. The title and the footer live in
/// `render`.
pub(super) fn model(fact: &PrStatusFact) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(title) = fact.title.as_deref() {
        if !title.is_empty() {
            rows.push(row("", title.to_string(), true));
        }
    }
    let mut state = String::new();
    if let Some(word) = fact.pr_state.as_deref() {
        if fact.draft {
            state.push_str("DRAFT · ");
        }
        state.push_str(word);
        if let Some(m) = fact.mergeable.as_deref() {
            state.push_str(&format!(" · {m}"));
        }
        rows.push(row("", state, false));
    }
    rows.push(row(
        "",
        format!(
            "checks {}:{}{}{}",
            fact.pass + fact.fail + fact.pending,
            count(" pass", fact.pass),
            count(" fail", fact.fail),
            count(" pending", fact.pending)
        ),
        false,
    ));
    for f in &fact.failing {
        rows.push(Row {
            label: String::new(),
            text: format!("✗ {}", f.name),
            ok: false,
            error: true,
            bold: false,
            dim: false,
        });
        if !f.step.is_empty() {
            rows.push(Row {
                label: String::new(),
                text: f.step.clone(),
                ok: false,
                error: true,
                bold: false,
                dim: false,
            });
        }
        // The door's raw error tail is secondary: dimmed, so the step
        // carries the row and the tail answers it.
        if !f.first_error.is_empty() {
            rows.push(Row {
                label: String::new(),
                text: f.first_error.clone(),
                ok: false,
                error: false,
                bold: false,
                dim: true,
            });
        }
    }
    if let Some(word) = fact.review_state.as_deref() {
        let line = match fact.review_rounds {
            Some(n) => format!("review: {word} ({n} rounds)"),
            None => format!("review: {word}"),
        };
        rows.push(row("", line, false));
    }
    rows
}

/// The repo slug in a PR URL (`https://github.com/o/r/pull/566` reads
/// `o/r`); None when the URL names no pull path. The header's right side.
pub(super) fn slug_from_url(url: Option<&str>) -> Option<String> {
    let rest = url?
        .split("github.com/")
        .nth(1)?
        .split("/pull/")
        .next()?
        .trim_end_matches('/')
        .to_string();
    (!rest.is_empty()).then_some(rest)
}

fn count(word: &str, n: u32) -> String {
    if n > 0 {
        format!(" {n}{word}")
    } else {
        String::new()
    }
}

/// The overlay: dim + centered panel, in sheet.rs's look. The panel's
/// mousedown stops propagation so a click inside never closes.
pub(super) fn render(sheet: &PrSheet, root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let rows = model(&sheet.fact);
    let mut body = div().flex().flex_col();
    for r in rows {
        let color = if r.dim {
            rgb(p.muted)
        } else if r.error {
            rgb(p.warn)
        } else if r.ok {
            rgb(p.ok)
        } else {
            rgb(p.fg)
        };
        body = body.child(
            div()
                .flex()
                .flex_row()
                .items_baseline()
                .when(!r.label.is_empty(), |d| {
                    d.child(
                        div()
                            .w(px(56.0))
                            .flex_none()
                            .text_color(rgb(p.muted))
                            .child(r.label),
                    )
                })
                .child(
                    ui::role(div(), Role::Fact, &p)
                        .when(r.bold, |d| d.font_weight(FontWeight::SEMIBOLD))
                        .text_color(color)
                        .child(r.text),
                ),
        );
    }
    let who = [sheet.slug.as_deref(), Some(sheet.agent.as_str())]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let agent_line = div()
        .flex()
        .flex_row()
        .items_baseline()
        .child(
            div()
                .flex_1()
                .font_weight(FontWeight::SEMIBOLD)
                .child(format!("PR #{}", sheet.pr)),
        )
        .children((!who.is_empty()).then(|| ui::quiet(who, &p)));
    let panel = div()
        .id("pr-sheet")
        .flex()
        .flex_col()
        .max_h(px(720.0))
        .w(px(560.0))
        .px_4()
        .py_3()
        .rounded(px(6.0))
        .border_1()
        .overflow_y_scroll()
        .border_color(rgb(p.border))
        .bg(rgb(p.panel))
        .font_family(ui::MONO)
        .text_size(px(ui::BODY))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(agent_line)
        .child(
            ui::role(div(), Role::Hint, &p)
                .pb(px(8.0))
                .child("an agent's pull request, read-only"),
        )
        .child(div().id("pr-sheet-body").overflow_y_scroll().child(body))
        .child(
            div()
                .flex()
                .flex_row()
                .items_baseline()
                .mt(px(10.0))
                .child(ui::role(div(), Role::Hint, &p).flex_1().child("esc close"))
                .children(sheet.url.is_some().then(|| ui::kbd("o open", &p))),
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
            cx.listener(|root, _, _, cx| root.close_pr_sheet(cx)),
        )
        .child(panel)
}
