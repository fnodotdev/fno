//! The HQ composer's mail door: one bounded shell of the `fno` porcelain,
//! the same verdict shape the web share shows. It lives in the fno crate so
//! the argv, timeout and error policy stay in one place; the app reaches it
//! through `fno::app_api`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// How long the send may run before the door names the timeout. The mux's
/// own mail door uses the same bound.
const MAIL_TIMEOUT: Duration = Duration::from_secs(20);

/// The sender stamp the HQ composer sends under: a display stamp, not a
/// session. The CLI accepts any XML-safe from-name (the web share passes
/// arbitrary viewer names through this door), and a lead reading the mail
/// sees where it came from.
pub const FROM: &str = "hq";

/// Resolve the `fno` porcelain: `$FNO_BIN` wins, else bare `fno` on PATH.
/// Never `current_exe()`: this door also runs inside the desktop app, where
/// the running executable is `fno-app`, not the porcelain.
fn fno_bin() -> PathBuf {
    std::env::var_os("FNO_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("fno"))
}

/// The child argv, factored out so the unit test reads the exact door shape
/// without spawning anything.
fn argv(target: &str, from_name: &str, text: &str) -> Vec<OsString> {
    let mut out = vec![fno_bin().into_os_string()];
    for part in [
        "agent",
        "mail",
        "send",
        "--from-name",
        from_name,
        "--",
        target,
        text,
    ] {
        out.push(OsString::from(part));
    }
    out
}

/// The door's outcome: the receipt the porcelain printed, or the reason it
/// refused. Either half is user-visible verbatim; a refused mail is a
/// result, not a Rust error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailVerdict {
    /// The porcelain's one-line stdout receipt.
    Sent(String),
    /// The first stderr line, or the timeout.
    Refused(String),
}

/// Run one send through the porcelain, bounded. The CLI's one-line stdout
/// verdict is the receipt verbatim; a nonzero exit surfaces the first stderr
/// line, never an empty error.
pub async fn send(target: &str, from_name: &str, text: &str) -> MailVerdict {
    let mut parts = argv(target, from_name, text);
    let bin = parts.remove(0);
    let mut command = Command::new(bin);
    command.args(&parts).stdin(Stdio::null()).kill_on_drop(true);
    match tokio::time::timeout(MAIL_TIMEOUT, command.output()).await {
        Err(_) => MailVerdict::Refused("mail: timed out".into()),
        Ok(Err(e)) => MailVerdict::Refused(format!("mail: {e}")),
        Ok(Ok(o)) => verdict(
            o.status.success(),
            &String::from_utf8_lossy(&o.stdout),
            &String::from_utf8_lossy(&o.stderr),
            target,
        ),
    }
}

/// The door's verdict from one child run: the CLI's one-line stdout on
/// success, the first stderr line on failure, never an empty error.
fn verdict(ok: bool, stdout: &str, stderr: &str, target: &str) -> MailVerdict {
    if ok {
        MailVerdict::Sent(crate::mux::server::first_line_or(
            stdout,
            &format!("mailed {target}"),
        ))
    } else {
        MailVerdict::Refused(crate::mux::server::first_line_or(
            stderr,
            &format!("mail {target}: failed"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_door_argv_is_the_porcelain_send() {
        let a = argv("philip", FROM, "--help");
        let tail: Vec<String> = a
            .iter()
            .skip(1)
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            tail,
            [
                "agent",
                "mail",
                "send",
                "--from-name",
                "hq",
                "--",
                "philip",
                "--help"
            ]
        );
        assert!(!a.is_empty());
    }

    #[test]
    fn a_failed_send_surfaces_the_first_stderr_line() {
        let v = verdict(true, "", "", "philip");
        assert_eq!(v, MailVerdict::Sent("mailed philip".into()));
        let v = verdict(false, "", "graph locked\nmore\n", "philip");
        assert_eq!(v, MailVerdict::Refused("graph locked".into()));
        let v = verdict(false, "", "", "philip");
        assert_eq!(v, MailVerdict::Refused("mail philip: failed".into()));
    }
}
