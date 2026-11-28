//! The `!` box: one command at a time on its own pty, drawn in the card.
//! While it runs every key but ctrl+o is the command's. At its end the box
//! shows the exit code and the prompt takes the next line.

use std::sync::atomic::{AtomicU64, Ordering};

use super::{bang, card::CardLine, placement::Placement, write_msg, ClientMsg, Launcher, View};
use crate::proto::shell_run::{ShellRunRequest, ShellRunState, ShellRunUpdate};
use crate::proto::Frame;

/// The rows of output the card shows, and the rows of a run's pty.
const ROWS: u16 = 10;

const CTRL_O: u8 = 0x0f;

/// Run ids are the client's, so two workspaces' cards never share one.
static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, thiserror::Error)]
pub(super) enum BoxError {
    #[error("the ! run was not sent: {0}")]
    Send(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Starting,
    Running { pane: u64, screen: String },
    Done { exit: Option<i32>, output: String },
    Refused { reason: String },
}

/// The card's run: the live one, or the last one that ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellBox {
    run: u64,
    cwd: String,
    shell: String,
    line: String,
    state: State,
}

impl ShellBox {
    pub(super) fn live(&self) -> bool {
        matches!(self.state, State::Starting | State::Running { .. })
    }

    /// The report ctrl+o opens once the run ended.
    pub(super) fn log(&self) -> Option<String> {
        match &self.state {
            State::Done { exit, output } => {
                Some(format!("! {}\n{output}\n{}", self.line, exit_word(*exit)))
            }
            _ => None,
        }
    }

    /// The box above the prompt: the command, its last rows, then its state.
    pub(super) fn lines(&self, w: usize) -> Vec<CardLine> {
        let (body, last, alert) = match &self.state {
            State::Starting => ("", "starting...".to_string(), false),
            State::Running { screen, .. } => {
                (screen.as_str(), "running \u{b7} ^o tab".to_string(), false)
            }
            State::Done { exit, output } => (output.as_str(), exit_word(*exit), *exit != Some(0)),
            State::Refused { reason } => ("", format!("refused: {reason}"), true),
        };
        let rows: Vec<&str> = body.lines().collect();
        let from = rows.len().saturating_sub(usize::from(ROWS));
        let mut out = vec![CardLine::plain(
            crate::chrome::clip(&format!("! {}", self.line), w),
            false,
        )];
        for row in rows.into_iter().skip(from) {
            out.push(CardLine::plain(
                crate::chrome::clip(&format!("  {row}"), w),
                true,
            ));
        }
        out.push(CardLine {
            alert,
            ..CardLine::plain(crate::chrome::clip(&format!("  {last}"), w), !alert)
        });
        if matches!(self.state, State::Done { .. }) {
            let keys = "\u{21b5} run \u{b7} ^o full log in a tab \u{b7} esc close";
            out.push(CardLine::plain(crate::chrome::clip(keys, w), true));
        }
        out
    }
}

/// A box staged for a shot, with invented output.
pub(super) fn staged(cwd: String, line: String, failed: bool) -> ShellBox {
    let output = "checking format\nchecking docs".to_string();
    let state = if failed {
        State::Done {
            exit: Some(1),
            output: format!("{output}\nexample.md: line 4 is too long"),
        }
    } else {
        State::Running {
            pane: 0,
            screen: output,
        }
    };
    ShellBox {
        run: 0,
        cwd,
        shell: "/bin/sh".to_string(),
        line,
        state,
    }
}

fn exit_word(exit: Option<i32>) -> String {
    exit.map_or("ended with no exit code".to_string(), |code| {
        format!("exit {code}")
    })
}

/// A line with no placement pick runs in the card. A bare `!`, a pick, a
/// remote host and an older server keep the pane.
pub(super) fn takes(view: &View) -> bool {
    view.launcher
        .as_ref()
        .is_some_and(|l| l.shell && l.draft.placement == Placement::LastPortal && fits(l))
        && crate::mux_rows::wire_at_least(&view.session, crate::proto::LAUNCH_PLACEMENT_PROTO)
}

/// Whether the draft is a line the card can host.
fn fits(l: &Launcher) -> bool {
    l.draft.host.is_none() && !l.draft.message.trim().is_empty() && !l.draft.cwd().is_empty()
}

/// Where a `!` with no pick runs, as the chip names it before Enter.
pub(super) fn default_word(l: &Launcher) -> &'static str {
    if l.places.typed && fits(l) {
        "in the card"
    } else {
        "new tab"
    }
}

/// Enter in shell mode: start the one run.
pub(super) async fn start(
    view: &mut View,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), BoxError> {
    let cols = view.card_width().saturating_sub(6).max(20);
    let Some(l) = view.launcher.as_mut() else {
        return Ok(());
    };
    // An attempt that is not settled blocks a second send, as on the pane path.
    if l.armed.is_some() || l.run.as_ref().is_some_and(ShellBox::live) {
        return Ok(());
    }
    let run = ShellBox {
        run: NEXT_RUN.fetch_add(1, Ordering::Relaxed),
        cwd: l.draft.cwd(),
        shell: bang::user_shell(),
        line: l.draft.message.trim().to_string(),
        state: State::Starting,
    };
    let request = ShellRunRequest::Start {
        run: run.run,
        squad: l.squad,
        cwd: run.cwd.clone(),
        shell: run.shell.clone(),
        line: run.line.clone(),
        rows: ROWS,
        cols: u16::try_from(cols).unwrap_or(u16::MAX),
    };
    l.phase = super::Phase::Editing;
    l.run = Some(run);
    send(sock_w, request).await
}

/// Keys while a run is live. `false` leaves the bytes to the card.
pub(super) async fn keys(
    view: &View,
    bytes: &[u8],
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<bool, BoxError> {
    let Some(live) = view
        .launcher
        .as_ref()
        .and_then(|l| l.run.as_ref())
        .filter(|run| run.live())
    else {
        return Ok(false);
    };
    let run = live.run;
    let typed: Vec<u8> = bytes.iter().copied().take_while(|b| *b != CTRL_O).collect();
    // A run that has no pane yet has nothing to move.
    let to_tab = typed.len() < bytes.len() && matches!(live.state, State::Running { .. });
    if !typed.is_empty() {
        send(sock_w, ShellRunRequest::Input { run, bytes: typed }).await?;
    }
    if to_tab {
        send(sock_w, ShellRunRequest::ToTab { run }).await?;
    }
    Ok(true)
}

async fn send(
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
    request: ShellRunRequest,
) -> Result<(), BoxError> {
    write_msg(sock_w, &ClientMsg::ShellRun(request))
        .await
        .map_err(|e| BoxError::Send(e.to_string()))
}

/// The open card and the held ones: a run outlives a closed card.
fn cards(view: &mut View) -> impl Iterator<Item = &mut Launcher> {
    view.launcher
        .iter_mut()
        .chain(view.launcher_closed.values_mut())
}

/// Fold one update into the card that started the run.
pub(crate) fn apply(view: &mut View, update: ShellRunUpdate) {
    let session = view.session.clone();
    // The moved run has the keys in its tab, so its open card folds.
    let folds = update.state == ShellRunState::Moved
        && view
            .launcher
            .as_ref()
            .and_then(|l| l.run.as_ref())
            .is_some_and(|r| r.run == update.run);
    let Some(l) = cards(view).find(|l| l.run.as_ref().is_some_and(|r| r.run == update.run)) else {
        return;
    };
    let Some(run) = l.run.as_mut() else {
        return;
    };
    let mut clear = false;
    match update.state {
        ShellRunState::Running { pane } => {
            bang::journal_ran(&session, &run.cwd, &run.shell, &run.line, pane);
            let screen = String::new();
            run.state = State::Running { pane, screen };
        }
        ShellRunState::Moved => {
            l.run = None;
            clear = true;
        }
        ShellRunState::Done { exit, output } => {
            // A failure keeps the line for the retry.
            clear = exit == Some(0);
            run.state = State::Done { exit, output };
        }
        ShellRunState::Refused { reason } => {
            bang::journal_refused(&session, &run.cwd, &run.line, &reason);
            run.state = State::Refused { reason };
        }
    }
    if clear {
        l.draft.message.clear();
        l.draft.cursor_chars = 0;
        l.draft.bump();
    }
    if folds {
        super::close(view);
    }
}

/// Keep a frame that is a run's screen. `true` when a card took it.
pub(crate) fn take_frame(view: &mut View, pane_id: u64, frame: &Frame) -> bool {
    for l in cards(view) {
        if let Some(State::Running { pane, screen }) = l.run.as_mut().map(|run| &mut run.state) {
            if *pane == pane_id {
                *screen = crate::vt::frame_text(frame);
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composer_shell_box_table() {
        let running = State::Running {
            pane: 4,
            screen: "checking format".into(),
        };
        let done = |code: i32| State::Done {
            exit: Some(code),
            output: "bad indent".into(),
        };
        // (state, it holds the keys, the row that says where it stands, red, a log)
        let cases = [
            (State::Starting, true, "starting...", false, false),
            (running, true, "running \u{b7} ^o tab", false, false),
            (done(0), false, "exit 0", false, true),
            (done(2), false, "exit 2", true, true),
        ];
        for (state, live, says, alert, log) in cases {
            let run = ShellBox {
                run: 1,
                cwd: "/tmp".into(),
                shell: "/bin/sh".into(),
                line: "make check".into(),
                state,
            };
            assert_eq!(run.live(), live, "{says}");
            assert_eq!(run.log().is_some(), log, "{says}");
            let lines = run.lines(60);
            assert_eq!(lines.first().map(|l| l.text.as_str()), Some("! make check"));
            let row = lines.iter().find(|l| l.text.trim() == says);
            assert_eq!(row.map(|l| l.alert), Some(alert), "{says}");
        }
    }
}
