//! The HQ composer's mail door: one bounded shell of the `fno` porcelain,
//! the same verdict shape the web share shows. It lives in the fno crate so
//! the argv, timeout and error policy stay in one place; the app reaches it
//! through `fno::app_api`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
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

/// Run one send through the porcelain, bounded. The CLI's one-line stdout
/// verdict is the receipt verbatim; a nonzero exit surfaces the first stderr
/// line, never an empty error.
pub async fn send(target: &str, from_name: &str, text: &str) -> Result<String, String> {
    let mut parts = argv(target, from_name, text);
    let bin = parts.remove(0);
    let mut command = Command::new(bin);
    command.args(&parts).stdin(Stdio::null()).kill_on_drop(true);
    match tokio::time::timeout(MAIL_TIMEOUT, command.output()).await {
        Err(_) => Err("mail: timed out".into()),
        Ok(Err(e)) => Err(format!("mail: {e}")),
        Ok(Ok(o)) if o.status.success() => Ok(crate::mux::server::first_line_or(
            &String::from_utf8_lossy(&o.stdout),
            &format!("mailed {target}"),
        )),
        Ok(Ok(o)) => Err(crate::mux::server::first_line_or(
            &String::from_utf8_lossy(&o.stderr),
            &format!("mail {target}: failed"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_door_argv_is_the_porcelain_send() {
        let a = argv("philip", FROM, "--help");
        let tail: Vec<String> = a[1..]
            .iter()
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
        let v = crate::mux::server::first_line_or("graph locked\nmore\n", "fallback");
        assert_eq!(v, "graph locked");
        let v = crate::mux::server::first_line_or("", "mailed philip");
        assert_eq!(v, "mailed philip");
    }
}
