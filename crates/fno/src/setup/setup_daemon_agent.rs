//! The fno-agent daemon's boot LaunchAgent: render, write, bootstrap, in
//! the shape of the web-bridge keeper (crates/fno/src/serve/web_agent.rs).
//! The node behind it: after a reboot the daemon used to stay down until a
//! person typed, because only client commands lazy-start it. This agent
//! starts it at login.
//!
//! The KeepAlive is deliberately NOT the web-bridge's blanket `<true/>`.
//! The daemon idle-exits cleanly after 30 quiet minutes by design and the
//! next client command lazy-starts it; a blanket KeepAlive would relaunch
//! it the moment it idles out and turn that design into a relaunch loop.
//! `SuccessfulExit = false` keeps the crash half: a nonzero or signal exit
//! earns a relaunch, a clean exit leaves the field to the lazy-start path.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::setup_services::{daemon_label, DAEMON_PLIST_FILENAME};

/// One launchctl call's wait bound, before the poll loop kills it.
const LAUNCHCTL_TIMEOUT_S: u64 = 10;

/// The typed installer receipt: what the step reports and what the tests
/// read (the web-bridge receipt's shape).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct DaemonAgentReceipt {
    pub status: String,
    pub detail: String,
    pub plist: Option<String>,
    pub label: Option<String>,
}

/// The typed receipt the step folds into report rows.
pub(crate) fn install() -> DaemonAgentReceipt {
    let daemon_bin = resolve_daemon_binary();
    let agents_dir = launch_agents_dir();
    let agents_home = fno_agents_home();
    if cfg!(target_os = "macos") && !daemon_bin.is_empty() {
        // The logs dir must exist before launchd's first write; launchd does
        // not create missing StandardOutPath parents.
        let logs = crate::state_layout::home_of(&home().join(".fno"), "logs");
        if let Err(e) = std::fs::create_dir_all(&logs) {
            return DaemonAgentReceipt {
                status: "failed".to_string(),
                detail: format!("create {}: {e}", logs.display()),
                plist: None,
                label: None,
            };
        }
    }
    install_into(&agents_dir, &daemon_bin, &agents_home)
}

/// The installer body, split off [`install`] so tests can pass a temp
/// LaunchAgents dir, a fake daemon path and the launchctl seam.
pub(crate) fn install_into(
    agents_dir: &Path,
    daemon_bin: &str,
    agents_home: &str,
) -> DaemonAgentReceipt {
    if !cfg!(target_os = "macos") {
        return DaemonAgentReceipt {
            status: "unsupported".to_string(),
            detail: "launchd is macOS-only; keep the daemon up with your init system".to_string(),
            plist: None,
            label: None,
        };
    }
    if daemon_bin.is_empty() {
        return DaemonAgentReceipt {
            status: "failed".to_string(),
            detail: "fno-agent-daemon binary not found next to fno-agent; reinstall (`curl -fsSL fno.sh | sh`)".to_string(),
            plist: None,
            label: None,
        };
    }
    let label = daemon_label();
    let rendered = render_plist(daemon_bin, agents_home, &label);
    let plist_path = agents_dir.join(DAEMON_PLIST_FILENAME);
    if let Some(parent) = plist_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return DaemonAgentReceipt {
                status: "failed".to_string(),
                detail: format!("create {}: {e}", parent.display()),
                plist: None,
                label: None,
            };
        }
    }
    match write_if_changed(&plist_path, &rendered) {
        Ok(false) => {
            // Matching bytes alone prove nothing: a bootout (manual or
            // post-crash) leaves the plist on disk with nothing registered,
            // so re-register whenever launchd does not hold the label (the
            // web-bridge's reason, verbatim).
            if agent_loaded(&label) {
                return DaemonAgentReceipt {
                    status: "unchanged".to_string(),
                    detail: "plist matches; not re-registered".to_string(),
                    plist: Some(plist_path.display().to_string()),
                    label: Some(label),
                };
            }
        }
        Ok(true) => {}
        Err(e) => {
            return DaemonAgentReceipt {
                status: "failed".to_string(),
                detail: format!("write {}: {e}", plist_path.display()),
                plist: None,
                label: None,
            };
        }
    }
    let (msg, rc) = bounce(&plist_path, &label);
    DaemonAgentReceipt {
        status: if rc == 0 { "installed" } else { "failed" }.to_string(),
        detail: msg,
        plist: Some(plist_path.display().to_string()),
        label: Some(label),
    }
}

/// The daemon binary the plist pins. The plist must outlive this process:
/// a dev-worktree binary dies with its worktree (the corpse class the
/// web-bridge resolver names), so the deployed copy on PATH is the first
/// pick and the `fno-agent-daemon` sibling of the resolved fno-agent is
/// the fallback. Empty when neither resolves.
fn resolve_daemon_binary() -> String {
    if let Some(found) = which("fno-agent-daemon") {
        return found;
    }
    crate::digest_overlay::fno_agent_bin_auto()
        .and_then(|bin| bin.parent().map(|d| d.join("fno-agent-daemon")))
        .filter(|bin| bin.is_file())
        .map(|bin| bin.display().to_string())
        .unwrap_or_default()
}

fn which(program: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

/// The agents home the plist names: `$FNO_AGENT_HOME`, else
/// `$HOME/.fno/agents`, the same fallback `AgentsHome::from_env` applies.
fn fno_agents_home() -> String {
    if let Some(v) = std::env::var_os("FNO_AGENT_HOME") {
        return v.to_string_lossy().into_owned();
    }
    home().join(".fno").join("agents").display().to_string()
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn launch_agents_dir() -> PathBuf {
    home().join("Library").join("LaunchAgents")
}

/// Whether launchd holds the daemon label. The seam
/// (`FNO_TEST_DAEMON_AGENT_LOADED`) pins the answer for tests; production
/// reads `launchctl list`, and an unreadable read answers not-loaded (the
/// setup services convention).
fn agent_loaded(label: &str) -> bool {
    if let Ok(pinned) = std::env::var("FNO_TEST_DAEMON_AGENT_LOADED") {
        return pinned == "true";
    }
    crate::setup_services::is_loaded(label)
}

/// The launchd PATH capture: the daemon's dir, then the usual install
/// roots, so a boot-time start resolves its helpers without a login shell.
fn launchd_path(daemon_bin: &str) -> String {
    let home = home();
    let mut entries: Vec<String> = Vec::new();
    if let Some(parent) = Path::new(daemon_bin).parent() {
        entries.push(parent.display().to_string());
    }
    let cargo_bin = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cargo"))
        .join("bin");
    for p in [
        cargo_bin.display().to_string(),
        home.join(".local/bin").display().to_string(),
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".to_string(),
        "/usr/bin".to_string(),
        "/bin".to_string(),
    ] {
        if !entries.contains(&p) {
            entries.push(p);
        }
    }
    entries.join(":")
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Render the daemon LaunchAgent plist. No filesystem writes.
fn render_plist(daemon_bin: &str, agents_home: &str, label: &str) -> String {
    let home = home();
    let logs = crate::state_layout::home_of(&home.join(".fno"), "logs");
    let esc = xml_escape;
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!--
  fno-agent daemon boot agent: starts the supervisor daemon at login, so
  fleet wakes, watchdog and daemon beats run without a person typing first.
  Stop it for real with:
    launchctl bootout gui/$(id -u)/{label}
    rm ~/Library/LaunchAgents/{file}
  KeepAlive restarts a crashed daemon only: a clean exit is the designed
  idle-exit, and the next fno-agent command lazy-starts a fresh daemon.
-->
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>

  <key>ProgramArguments</key>
  <array>
    <string>{daemon_bin}</string>
    <string>--home</string>
    <string>{agents_home}</string>
  </array>

  <!-- launchd launches with a minimal PATH; capture install-time PATH so
       the daemon's children resolve without a login shell. -->
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
    <key>HOME</key>
    <string>{home}</string>
    <key>FNO_AGENT_HOME</key>
    <string>{agents_home}</string>
    <key>FNO_MUX_DIR</key>
    <string>{mux_dir}</string>
  </dict>

  <key>RunAtLoad</key>
  <true/>

  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>

  <!-- Set cwd to HOME so any relative path lands somewhere writable; the
       daemon re-anchors itself to a repo root or the agents home. -->
  <key>WorkingDirectory</key>
  <string>{home}</string>

  <key>StandardOutPath</key>
  <string>{log_out}</string>

  <key>StandardErrorPath</key>
  <string>{log_err}</string>
</dict>
</plist>
"#,
        label = esc(label),
        file = DAEMON_PLIST_FILENAME,
        daemon_bin = esc(daemon_bin),
        agents_home = esc(agents_home),
        mux_dir = esc(&crate::proto::mux_dir().display().to_string()),
        path = esc(&launchd_path(daemon_bin)),
        home = esc(&home.display().to_string()),
        log_out = esc(&logs.join("daemon-agent.out.log").display().to_string()),
        log_err = esc(&logs.join("daemon-agent.err.log").display().to_string()),
    )
}

/// bootout -> bootstrap with retry: the web-bridge activation dance.
/// bootout clears a wedge an old registration leaves; its nonzero rc on a
/// not-loaded job is expected. Bootstrap retries because bootout is
/// asynchronous and an immediate re-register can lose to the
/// still-settling label. RunAtLoad fires the daemon, so no kickstart.
fn bounce(plist_path: &Path, label: &str) -> (String, i32) {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let domain = format!("gui/{uid}");
    let target = format!("{domain}/{label}");
    let (_, timed) = run_launchctl(&["bootout", &target]);
    if timed {
        return (
            format!("`launchctl bootout {target}` timed out after {LAUNCHCTL_TIMEOUT_S}s"),
            1,
        );
    }
    const BOOTSTRAP_RETRIES: u32 = 4;
    let mut rc = -1;
    for attempt in 0..BOOTSTRAP_RETRIES {
        let (code, timed) =
            run_launchctl(&["bootstrap", &domain, &plist_path.display().to_string()]);
        if timed {
            return (
                format!("`launchctl bootstrap {domain}` timed out after {LAUNCHCTL_TIMEOUT_S}s"),
                1,
            );
        }
        rc = code;
        if rc == 0 {
            break;
        }
        if attempt + 1 < BOOTSTRAP_RETRIES {
            std::thread::sleep(Duration::from_millis(250 * (attempt as u64 + 1)));
        }
    }
    if rc != 0 {
        return (
            format!(
                "`launchctl bootstrap {domain} {}` failed (rc={rc})",
                plist_path.display()
            ),
            1,
        );
    }
    (
        format!("agent loaded ({target}); the daemon starts now and at every login"),
        0,
    )
}

/// One launchctl call with a wait bound. `(rc, timed_out)`. The test seam
/// (`FNO_TEST_DAEMON_AGENT_LAUNCHCTL`, a JSON `[[rc, timed], ...]`
/// consumed in invocation order) stands in for the real tool.
fn run_launchctl(args: &[&str]) -> (i32, bool) {
    if let Ok(script) = std::env::var("FNO_TEST_DAEMON_AGENT_LAUNCHCTL") {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CALL: AtomicUsize = AtomicUsize::new(0);
        let idx = CALL.fetch_add(1, Ordering::Relaxed);
        let parsed: Option<Vec<Vec<Value>>> = serde_json::from_str(&script).ok();
        let step = parsed.as_ref().and_then(|steps| {
            steps
                .get(idx)
                .and_then(|step| Some((step.first()?.as_i64()? as i32, step.get(1)?.as_bool()?)))
        });
        return step.unwrap_or((-1, false));
    }
    let Ok(mut child) = std::process::Command::new("launchctl")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    else {
        return (-1, false);
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(LAUNCHCTL_TIMEOUT_S);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return (status.code().unwrap_or(-1), false),
            Ok(None) if std::time::Instant::now() >= deadline => {
                child.kill().ok();
                child.wait().ok();
                return (-1, true);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return (-1, false),
        }
    }
}

/// Write only when the bytes differ: a re-install that changes nothing
/// must not re-register the agent (and restart the daemon) for no reason.
fn write_if_changed(plist_path: &Path, rendered: &str) -> std::io::Result<bool> {
    if std::fs::read_to_string(plist_path)
        .map(|old| old == rendered)
        .unwrap_or(false)
    {
        return Ok(false);
    }
    std::fs::write(plist_path, rendered)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    use std::sync::Mutex;

    /// The seams are process-global env reads, so the receipt tests take
    /// this lock to run one at a time. macOS-gated with its only user: on
    /// Linux the receipt tests are compiled out and a bare lock is a
    /// dead-code error under deny warnings.
    #[cfg(target_os = "macos")]
    static SEAM_LOCK: Mutex<()> = Mutex::new(());

    #[cfg(target_os = "macos")]
    fn receipt_status(dir: &Path, loaded: bool, script: &str) -> String {
        let _guard = SEAM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("FNO_TEST_DAEMON_AGENT_LOADED", loaded.to_string());
        std::env::set_var("FNO_TEST_DAEMON_AGENT_LAUNCHCTL", script);
        let receipt = install_into(
            dir,
            "/usr/local/bin/fno-agent-daemon",
            "/Users/u/.fno/agents",
        );
        std::env::remove_var("FNO_TEST_DAEMON_AGENT_LOADED");
        std::env::remove_var("FNO_TEST_DAEMON_AGENT_LAUNCHCTL");
        receipt.status
    }

    #[test]
    fn the_plist_starts_at_login_and_restarts_crashes_only() {
        let rendered = render_plist(
            "/usr/local/bin/fno-agent-daemon",
            "/Users/u/.fno/agents",
            &daemon_label(),
        );
        assert!(rendered.contains("<key>RunAtLoad</key>\n  <true/>"));
        // The load-bearing key: a blanket KeepAlive would relaunch the
        // daemon out of its designed idle-exit.
        assert!(rendered.contains("<key>SuccessfulExit</key>\n    <false/>"));
        assert!(!rendered.contains("<key>KeepAlive</key>\n  <true/>"));
        // The home travels twice: argv for ps attribution, env for the
        // binary's own resolution.
        assert!(rendered.contains("<string>--home</string>"));
        assert!(rendered.contains("<key>FNO_AGENT_HOME</key>"));
        assert!(rendered.contains("<key>FNO_MUX_DIR</key>"));
        assert!(rendered.contains("/Users/u/.fno/agents"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn install_skips_reregistration_when_loaded_and_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        // The unchanged verdict needs the plist bytes already on disk, or
        // the write lands as changed and the flow reaches bounce instead.
        let rendered = render_plist(
            "/usr/local/bin/fno-agent-daemon",
            "/Users/u/.fno/agents",
            &daemon_label(),
        );
        std::fs::write(dir.path().join(DAEMON_PLIST_FILENAME), rendered).unwrap();
        let first = receipt_status(dir.path(), true, "[]");
        assert_eq!(first, "unchanged");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn install_registers_a_missing_agent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            receipt_status(dir.path(), false, "[[0,false],[0,false]]"),
            "installed"
        );
        assert!(dir.path().join(DAEMON_PLIST_FILENAME).exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_failed_bootstrap_reads_failed_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            receipt_status(
                dir.path(),
                false,
                "[[0,false],[5,false],[5,false],[5,false],[5,false]]"
            ),
            "failed"
        );
    }
}
