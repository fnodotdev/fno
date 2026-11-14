//! The sideline footer's button row: new session, new workspace, add host, menu.
//! Each button opens what its key opens, and one fn lays the row out so the
//! paint and the hit-test cannot diverge.

use std::ops::Range;

use super::View;

/// The three buttons, by keymap action and bare label, left to right. ASCII
/// only: the former `☰` is an ambiguous-width glyph some terminals render
/// wide, which displaced that row's whole line.
const SESSION: (&str, &str) = ("toggle-composer", "+ session");
/// The session button's widest label, Claude's agents-view prompt wording.
/// It shows only where the whole row fits; narrower rows step down.
const TASK_PROMPT: &str = "+ describe a task for a new session";
const WORKSPACE: (&str, &str) = ("new-workspace", "+ workspace");
const HOST: (&str, &str) = ("add-remote-host", "+ host");
const MENU: (&str, &str) = ("sideline-menu", "menu");

/// How much of each button's name the row spells out, widest first.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Form {
    /// `+ session (i)`
    Full,
    /// `i new`: the key and a one-word name.
    Short,
    /// `i`: the key alone.
    Keys,
}

/// The one-word name a short button carries beside its key.
fn short_name(action: &str) -> &'static str {
    match action {
        "toggle-composer" => "new",
        "new-workspace" => "space",
        "add-remote-host" => "host",
        _ => "menu",
    }
}

/// The footer row as painted, with each button's column range. A `None`
/// range means that button has no seat: the row then opens new workspace.
#[derive(Debug, Default)]
pub(super) struct FooterButtons {
    pub(super) text: String,
    pub(super) session: Option<Range<usize>>,
    pub(super) host: Option<Range<usize>>,
    pub(super) menu: Option<Range<usize>>,
}

impl View {
    /// The footer at this panel width, one row of buttons joined by ` · `:
    /// `+ session (i) · + workspace (N) · + host (+) · menu (P)`, with the
    /// session button reading the task prompt when the panel is wide. The keys
    /// resolve through the live chord table (`key_for`), so a rebind relabels
    /// the row. A narrow panel shows the keys only (`i · N · + · P`), each
    /// still a button. When even that does not fit, `+ host` drops and the
    /// three-button forms run again. Narrower still, or with a recruit-mark
    /// tally on the row, the legacy one-button row paints.
    pub(super) fn footer_buttons(&self, panel_w: usize) -> FooterButtons {
        // last column is the divider
        let tw = panel_w.saturating_sub(1);
        if self.marks.is_empty() {
            // Columns as the row painter counts them, so a rebound key
            // with a wide glyph cannot move a range off its label.
            let cols = |s: &str| s.chars().map(super::glyph_cols).sum::<usize>();
            let ladder = [
                (true, Form::Full),
                (false, Form::Full),
                (false, Form::Short),
                (false, Form::Keys),
            ];
            for (with_host, (prompt, form)) in [true, false]
                .into_iter()
                .flat_map(|h| ladder.map(|step| (h, step)))
            {
                let label = |(action, bare): (&str, &str)| {
                    let bare = if prompt && action == SESSION.0 {
                        TASK_PROMPT
                    } else {
                        bare
                    };
                    crate::keys::key_for(action).map(|k| match form {
                        Form::Full => format!("{bare} ({k})"),
                        Form::Short => format!("{k} {}", short_name(action)),
                        Form::Keys => k.to_string(),
                    })
                };
                let (Some(session), Some(workspace), Some(menu)) =
                    (label(SESSION), label(WORKSPACE), label(MENU))
                else {
                    continue;
                };
                let host = if with_host { label(HOST) } else { None };
                if with_host && host.is_none() {
                    continue;
                }
                // The host button sits after `+ session`, `+ workspace` and their separators.
                let sep = cols(" \u{b7} ");
                // One cell of air keeps the row off the screen edge, and
                // each tap range spreads one cell into the air around it
                // (the separators are three wide, so none overlap).
                let pad = 1;
                let host_at = pad + cols(&session) + sep + cols(&workspace) + sep;
                let mut parts = vec![session.clone(), workspace];
                parts.extend(host.clone());
                parts.push(menu.clone());
                let text = format!(" {}", parts.join(" \u{b7} "));
                if cols(&text) <= tw {
                    let menu_at = cols(&text) - cols(&menu);
                    return FooterButtons {
                        session: Some(0..pad + cols(&session) + 1),
                        host: host.map(|h| host_at - 1..host_at + cols(&h) + 1),
                        menu: Some(menu_at - 1..cols(&text)),
                        text,
                    };
                }
            }
            // Too narrow for three: the row is one new-workspace button.
            return FooterButtons {
                text: WORKSPACE.1.to_string(),
                ..Default::default()
            };
        }
        FooterButtons {
            text: format!("{}   {} marked \u{b7}R", WORKSPACE.1, self.marks.len()),
            ..Default::default()
        }
    }
}
