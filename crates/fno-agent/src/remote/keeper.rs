//! `remote serve --install` / `--uninstall`: a launchd agent that keeps the
//! door up across reboots, in the shape of the web-bridge keeper
//! (crates/fno/src/serve/web_agent.rs): bootout, bootstrap with retry,
//! launchd fires RunAtLoad. The plist file name is fixed; the Label carries
//! this HOME's suffix like every other fno agent.
//!
//! `tailscale serve --bg` keeps its own config across reboots (Tailscale's
//! serve docs, "Effects of rebooting and restarting"), so the keeper prints
//! that one line for the user and never re-asserts it.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::paths::AgentsHome;

pub const PLIST_FILENAME: &str = "sh.fno.remote-door.plist";

#[derive(Debug, thiserror::Error)]
pub enum KeeperError {
    #[error("current exe: {0}")]
    Exe(std::io::Error),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("launchctl bootstrap {domain} {} failed (rc={rc})", plist.display())]
    Bootstrap {
        domain: String,
        plist: PathBuf,
        rc: i32,
    },
}

fn io_at(path: &Path) -> impl FnOnce(std::io::Error) -> KeeperError + '_ {
    move |source| KeeperError::Io {
        path: path.to_path_buf(),
        source,
    }
}
const LAUNCHCTL_TIMEOUT_S: u64 = 10;

pub fn label() -> String {
    format!(
        "sh.fno.remote-door.{}",
        crate::launchd_labels::launchd_suffix()
    )
}

fn launch_agents_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library")
        .join("LaunchAgents")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The plist that runs `<exe> remote serve --port <port>`.
pub fn render_plist(exe: &Path, port: u16, home: &AgentsHome) -> String {
    let log = home.remote_dir().join("door.log");
    let path = std::env::var("PATH").unwrap_or_default();
    let user_home = std::env::var("HOME").unwrap_or_default();
    let agents_home = home.root().display().to_string();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Keeps the fno remote door up across reboots. Stop it for real with
     `fno agent workspace remote serve --uninstall`. -->
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>remote</string>
    <string>serve</string>
    <string>--port</string>
    <string>{port}</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
    <key>HOME</key>
    <string>{user_home}</string>
    <key>FNO_AGENT_HOME</key>
    <string>{agents_home}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        label = xml_escape(&label()),
        exe = xml_escape(&exe.display().to_string()),
        path = xml_escape(&path),
        user_home = xml_escape(&user_home),
        agents_home = xml_escape(&agents_home),
        log = xml_escape(&log.display().to_string()),
    )
}

/// launchctl under a hard bound; the exit code, or -1 on a hang or a
/// missing binary.
fn launchctl(args: &[&str]) -> i32 {
    let mut cmd = Command::new("launchctl");
    cmd.args(args);
    match crate::bounded_cmd::output_with_timeout_result(cmd, LAUNCHCTL_TIMEOUT_S) {
        Ok(out) => out.status.code().unwrap_or(-1),
        Err(_) => -1,
    }
}

fn domain() -> String {
    // SAFETY: getuid cannot fail.
    format!("gui/{}", unsafe { libc::getuid() })
}

/// The one line the user runs once to put the door on the tailnet.
pub fn tailscale_serve_line(port: u16) -> String {
    format!("tailscale serve --bg --tcp={port} tcp://127.0.0.1:{port}")
}

/// Write the plist and load it. Returns the receipt line, or the failure.
pub fn install(home: &AgentsHome, port: u16) -> Result<String, KeeperError> {
    let exe = crate::client_exe::current().map_err(KeeperError::Exe)?;
    let dir = launch_agents_dir();
    std::fs::create_dir_all(&dir).map_err(io_at(&dir))?;
    let remote = home.remote_dir();
    std::fs::create_dir_all(&remote).map_err(io_at(&remote))?;
    let plist = dir.join(PLIST_FILENAME);
    std::fs::write(&plist, render_plist(&exe, port, home)).map_err(io_at(&plist))?;
    let target = format!("{}/{}", domain(), label());
    launchctl(&["bootout", &target]);
    // bootout settles asynchronously; an immediate bootstrap can lose to
    // the label still unloading (rc 5), so retry with a short backoff.
    let plist_arg = plist.display().to_string();
    let mut rc = -1;
    for attempt in 0..4u64 {
        rc = launchctl(&["bootstrap", &domain(), &plist_arg]);
        if rc == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250 * (attempt + 1)));
    }
    if rc != 0 {
        return Err(KeeperError::Bootstrap {
            domain: domain(),
            plist,
            rc,
        });
    }
    Ok(format!("installed {target} ({plist_arg})"))
}

/// Unload the agent and remove its plist.
pub fn uninstall() -> Result<String, KeeperError> {
    let target = format!("{}/{}", domain(), label());
    launchctl(&["bootout", &target]);
    let plist = launch_agents_dir().join(PLIST_FILENAME);
    match std::fs::remove_file(&plist) {
        Ok(()) => Ok(format!("uninstalled {target}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(format!("{target} was not installed"))
        }
        Err(e) => Err(io_at(&plist)(e)),
    }
}
