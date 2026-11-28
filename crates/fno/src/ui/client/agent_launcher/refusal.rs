//! A refused launch in the card: the exit code and the last error lines,
//! every line under ctrl+o, and the saved log in a tab under a second one.
//! The log opens in a read-only pager, never a shell.

use std::io::Write as _;

use super::{close, write_msg, ClientMsg, Phase, View};
use crate::mux_cli::control_roundtrip_with_timeouts;
use crate::proto::{
    socket_path, Command, ControlCommand, PanePlacement, PaneTarget, ServerMsg, TabSel,
};

/// The error lines the folded card shows.
const TAIL: usize = 2;

const CONTROL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The door's report: the `exit <code>` head and the lines under it. Any
/// other refusal is one sentence with no log to open.
pub(super) fn report(reason: &str) -> Option<(&str, Vec<&str>)> {
    let mut lines = reason.lines();
    let head = lines.next().filter(|head| head.starts_with("exit "))?;
    Some((head, lines.collect()))
}

/// The footer of a reported refusal, one error line to a row.
pub(super) fn footer(head: &str, lines: &[&str], all: bool) -> String {
    let mut out = vec![head.to_string()];
    if all {
        // The sideline clips a long report, so the keys come before it.
        out.push("(^o log in a tab, esc folds)".to_string());
        out.extend(lines.iter().map(|line| line.to_string()));
        return out.join("\n");
    }
    let folded = lines.len().saturating_sub(TAIL);
    out.extend(lines.iter().skip(folded).map(|line| line.to_string()));
    out.push(format!("(^o all {} lines)", lines.len()));
    out.join("\n")
}

/// Why the log did not open.
#[derive(Debug, thiserror::Error)]
pub(super) enum LogError {
    #[error("the log could not be saved: {0}")]
    Save(#[from] std::io::Error),
    #[error("{0}")]
    Socket(String),
    #[error("the log tab never started: {0}")]
    Pane(String),
    #[error("send failed: {0}")]
    Send(String),
}

/// The pager's argv. LESSSECURE takes away the shell, editor and log
/// commands unless LESSSECURE_ALLOW gives them back, so that one is unset.
/// -L takes away the input preprocessor, and -+F keeps a short log open.
fn pager_argv(path: &std::path::Path) -> Vec<String> {
    [
        "env",
        "-u",
        "LESSSECURE_ALLOW",
        "LESSSECURE=1",
        "less",
        "-L",
        "-+F",
        "--",
    ]
    .into_iter()
    .map(str::to_string)
    .chain([path.to_string_lossy().into_owned()])
    .collect()
}

fn run_in_tab(session: &str, squad: u64, report: &str) -> Result<u64, LogError> {
    let mut file = tempfile::Builder::new()
        .prefix("fno-launch-")
        .suffix(".log")
        .tempfile()?;
    writeln!(file, "{report}")?;
    let (_, path) = file.keep().map_err(|e| LogError::Save(e.error))?;
    let command = ControlCommand::PaneRun {
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        argv: pager_argv(&path),
        cols: None,
        rows: None,
        claim: false,
        placement: PanePlacement {
            target: PaneTarget::SquadId(squad),
            tab: Some(TabSel::New),
            ..Default::default()
        },
        worker: None,
        fno_id: None,
    };
    let sock = socket_path(session).map_err(LogError::Socket)?;
    match control_roundtrip_with_timeouts(&sock, session, command, CONTROL_TIMEOUT, CONTROL_TIMEOUT)
    {
        Ok(ServerMsg::PaneSpawned { pane_id, .. }) => Ok(pane_id),
        Ok(other) => Err(LogError::Pane(format!("{other:?}"))),
        Err(e) => Err(LogError::Pane(e.to_string())),
    }
}

/// The log of the `!` run that ended, while the card shows no refusal.
pub(super) fn run_log(view: &View) -> Option<(u64, String)> {
    let l = view.launcher.as_ref()?;
    if !l.shell || !matches!(l.phase, Phase::Editing) {
        return None;
    }
    Some((l.squad, l.run.as_ref()?.log()?))
}

/// The second ctrl+o of a refusal, or the first after a `!` run: save the
/// report and open it in a new tab of the card's workspace. The draft is
/// held, as after esc.
pub(super) async fn open_log(
    view: &mut View,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), LogError> {
    let refused = view.launcher.as_ref().and_then(|l| match &l.phase {
        Phase::Refused { reason, .. } => Some((l.squad, reason.clone())),
        _ => None,
    });
    let Some((squad, text)) = refused.or_else(|| run_log(view)) else {
        return Ok(());
    };
    let session = view.session.clone();
    let opened = tokio::task::spawn_blocking(move || run_in_tab(&session, squad, &text))
        .await
        .map_err(|e| LogError::Pane(e.to_string()))?;
    match opened {
        Ok(pane) => {
            close(view);
            view.note_command_sent(&Command::FocusPane(pane));
            write_msg(sock_w, &ClientMsg::Command(Command::FocusPane(pane)))
                .await
                .map_err(|e| LogError::Send(e.to_string()))?;
        }
        Err(e) => view.set_notice(e.to_string()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reported_refusal_folds_to_its_code_and_last_lines() {
        let (head, lines) = report("exit 78\none\ntwo\nthree").expect("a report");
        assert_eq!(
            footer(head, &lines, false),
            "exit 78\ntwo\nthree\n(^o all 3 lines)"
        );
        assert_eq!(
            footer(head, &lines, true),
            "exit 78\n(^o log in a tab, esc folds)\none\ntwo\nthree"
        );
        assert_eq!(report("no folder"), None);
        assert_eq!(
            pager_argv(std::path::Path::new("/l")).join(" "),
            "env -u LESSSECURE_ALLOW LESSSECURE=1 less -L -+F -- /l"
        );
    }
}
