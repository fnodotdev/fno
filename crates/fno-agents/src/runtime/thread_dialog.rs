//! What a parked claude thread's dialog says. A native `claude --bg` worker
//! has no mux pane, so the dialog is read off the attach frame stream: the
//! daemon redraws the screen to a fresh attacher. A numbered picker takes a
//! key. A macOS grant screen (Accessibility, Screen Recording) takes none; a
//! person has to act at the machine, and the answer path must say so instead
//! of typing a key the screen ignores.

use crate::claude_attach::{attach_for_frames, AttachRequest};
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

/// How long one read waits for the redraw to settle.
const READ_WINDOW: Duration = Duration::from_millis(1500);
/// A redraw is a few KB; this bounds a chatty worker.
const MAX_BYTES: usize = 256 * 1024;
const GRANTS: [(&str, &str); 2] = [
    ("accessibility", "Accessibility"),
    ("screen recording", "Screen Recording"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Dialog {
    /// Numbered options: a response key answers it.
    Picker,
    /// A system grant screen. No key answers it; the named grants are missing.
    Grant(Vec<&'static str>),
    Unknown,
}

pub(crate) fn classify(screen: &str) -> Dialog {
    let lower = screen.to_ascii_lowercase();
    let numbered = |n: u8| {
        screen.lines().any(|l| {
            let t = l.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
            t.strip_prefix(&n.to_string())
                .is_some_and(|rest| rest.starts_with(". ") || rest.starts_with(") "))
        })
    };
    let asks = ["system settings", "not granted"]
        .iter()
        .any(|w| lower.contains(w));
    let missing: Vec<&'static str> = GRANTS
        .iter()
        .filter(|(needle, _)| lower.contains(needle))
        .map(|(_, name)| *name)
        .collect();
    if asks && !missing.is_empty() {
        Dialog::Grant(missing)
    } else if numbered(1) && numbered(2) {
        Dialog::Picker
    } else {
        Dialog::Unknown
    }
}

/// The remedy a grant dialog needs, in one sentence.
pub(crate) fn grant_remedy(missing: &[&str]) -> String {
    format!(
        "a macOS grant screen is showing ({} not granted). No key answers it: a person must grant it at the machine",
        missing.join(" and ")
    )
}

/// Read the dialog of the thread whose short id the roster names.
pub(crate) fn read_thread(daemon_dir: &Path, short: &str) -> Result<String, String> {
    let roster = crate::claude_roster::ClaudeRoster::load(&daemon_dir.join("roster.json"))
        .map_err(|e| format!("roster unreadable: {e:?}"))?;
    let worker = roster.find(short).ok_or("thread not in the roster")?;
    let sock = worker
        .resolve_control_sock()
        .ok_or("thread has no control socket")?;
    read_screen(
        &sock,
        worker.short_id(),
        crate::claude_roster::read_control_key_in(daemon_dir),
    )
}

/// Attach, feed the redraw through the terminal grid, return the visible text.
pub(crate) fn read_screen(
    socket: &Path,
    short: &str,
    auth: Option<String>,
) -> Result<String, String> {
    let stream = UnixStream::connect(socket).map_err(|e| format!("connect: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(400)))
        .map_err(|e| e.to_string())?;
    let writer = stream.try_clone().map_err(|e| e.to_string())?;
    let (_, mut frames) = attach_for_frames(
        writer,
        stream,
        &AttachRequest::for_frame_stream(short, auth),
    )
    .map_err(|e| e.to_string())?;
    let mut grid = crate::screen::TerminalGrid::with_default_size();
    let (start, mut total) = (Instant::now(), 0usize);
    let mut buf = [0u8; 8192];
    while start.elapsed() < READ_WINDOW && total < MAX_BYTES {
        match frames.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                grid.feed(buf.get(..n).unwrap_or_default());
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if total > 0 {
                    break;
                }
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(grid.snapshot().text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_numbered_picker_takes_a_key() {
        let s = "Allow this tool?\n ❯ 1. Yes\n   2. Yes, always\n   3. No";
        assert_eq!(classify(s), Dialog::Picker);
    }

    #[test]
    fn a_grant_screen_names_each_missing_grant_and_takes_no_key() {
        let s = "computer-use needs permission.\nGrant Accessibility and Screen Recording in System Settings.";
        let Dialog::Grant(missing) = classify(s) else {
            panic!("expected a grant dialog");
        };
        assert_eq!(missing, vec!["Accessibility", "Screen Recording"]);
        assert!(grant_remedy(&missing).contains("Accessibility and Screen Recording"));
    }

    #[test]
    fn plain_text_is_unknown() {
        assert_eq!(classify("working on it..."), Dialog::Unknown);
    }
}
