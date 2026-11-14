//! The `fno mux web install-agent` / `uninstall-agent` commands: a launchd
//! agent that keeps the read-only web bridge (`fno mux serve --web
//! --attention-api`, crates/fno/src/web.rs) up across reboots, so the /fno
//! page and the phone answer buttons come back after a machine restart
//! instead of staying down until someone notices (measured 2026-10-19 and
//! again 2026-10-26).
//!
//! The bridge's upstream task already reconnects with backoff (web.rs), so
//! the agent may start before the mux server exists: once a first `fno`
//! command spawns the server, the bridge attaches on its own.
//!
//! The launchd plumbing mirrors the fno-agents pr-watch installer
//! (bootout -> bootstrap with retry -> launchd fires RunAtLoad); this crate
//! cannot depend on that crate, so the few helpers are local twins, the same
//! way `setup_services` twins `launchd_suffix`. The plist FILENAME stays
//! fixed (`sh.fno.web-bridge.plist`); only the Label inside carries the
//! per-HOME suffix, the groom convention doctor's collectors key on.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use crate::setup_services::{web_bridge_label, WEB_BRIDGE_PLIST_FILENAME};

/// Every launchctl step gets a hard bound: a wedged launchd must surface as
/// a failed receipt, never hang the installer.
const LAUNCHCTL_TIMEOUT_S: u64 = 10;

const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!--
  Keeps the read-only web bridge and the attention answer endpoint up across
  reboots. KeepAlive restarts a crashed bridge; stopping it for real is
  `fno mux web uninstall-agent` (a plain `mux serve --web --stop` is undone
  by this KeepAlive within seconds).
-->
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>

  <key>ProgramArguments</key>
  <array>
    <string>{fno_binary}</string>
    <string>mux</string>
    <string>serve</string>
    <string>--web</string>
    <string>--attention-api</string>
    <string>--port</string>
    <string>{port}</string>
    <string>--attention-port</string>
    <string>{attention_port}</string>
  </array>

  <!-- launchd launches with a minimal PATH; capture install-time PATH so
       fno and its helpers resolve without a login shell. -->
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
    <key>HOME</key>
    <string>{home}</string>
  </dict>

  <key>RunAtLoad</key>
  <true/>

  <key>KeepAlive</key>
  <true/>

  <key>ProcessType</key>
  <string>Interactive</string>

  <key>StandardOutPath</key>
  <string>{log_out}</string>

  <key>StandardErrorPath</key>
  <string>{log_err}</string>
</dict>
</plist>
"#;

/// Parsed `mux web install-agent` arguments.
#[derive(Debug, PartialEq, Eq)]
pub struct InstallArgs {
    pub bin: Option<String>,
    pub port: u16,
    pub attention_port: u16,
    pub json: bool,
}

impl Default for InstallArgs {
    fn default() -> Self {
        InstallArgs {
            bin: None,
            port: 8722,
            attention_port: crate::attention_api::DEFAULT_ATTENTION_PORT,
            json: false,
        }
    }
}

/// `mux web install-agent [flags]`: render, write, bootstrap.
pub fn install(args: &InstallArgs) -> i32 {
    let agents_dir = launch_agents_dir();
    let receipt = install_agent(
        &agents_dir,
        args.bin.as_deref(),
        args.port,
        args.attention_port,
    );
    emit_receipt(&receipt, args.json)
}

/// `mux web uninstall-agent`: bootout the label, remove the plist.
pub fn uninstall(json: bool) -> i32 {
    let receipt = uninstall_agent(&launch_agents_dir());
    emit_receipt(&receipt, json)
}

/// The typed installer receipt: what install/uninstall report and what the
/// tests read. The fields the JSON receipt carries, minus the empty-shape
/// guesswork (R1: deserialize once, read typed fields).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct WebAgentReceipt {
    pub status: String,
    pub detail: String,
    pub plist: Option<String>,
    pub label: Option<String>,
}

impl WebAgentReceipt {
    /// Success exit: the three terminal receipts. A timed bootout's
    /// `failed` and every refusal exit 1.
    fn exit_code(&self) -> i32 {
        match self.status.as_str() {
            "installed" | "uninstalled" | "unchanged" => 0,
            _ => 1,
        }
    }
}

fn emit_receipt(receipt: &WebAgentReceipt, json: bool) -> i32 {
    if json {
        println!("{}", serde_json::to_value(receipt).unwrap_or(Value::Null));
    } else {
        println!("fno mux web: {}: {}", receipt.status, receipt.detail);
        if let Some(plist) = &receipt.plist {
            println!("fno mux web: plist {plist}");
        }
    }
    receipt.exit_code()
}

/// This HOME, `/` when HOME is unset - the same fallback every fno launchd
/// path resolves under.
fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn launch_agents_dir() -> PathBuf {
    home().join("Library").join("LaunchAgents")
}

/// The launchd PATH capture: the binary's dir, then the usual install roots.
/// Without cargo bin on PATH a launchd tick fails at binary lookup.
fn default_agent_path(fno_binary: &str) -> String {
    let home = home();
    let mut entries: Vec<String> = Vec::new();
    if let Some(parent) = Path::new(fno_binary).parent() {
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

/// Render the web-bridge LaunchAgent plist. No filesystem writes.
fn render_plist(fno_binary: &str, web_port: u16, attention_port: u16, label: &str) -> String {
    let home = home();
    let logs = home.join(".fno").join("logs");
    let esc = xml_escape;
    PLIST
        .replace("{label}", &esc(label))
        .replace("{fno_binary}", &esc(fno_binary))
        .replace("{port}", &web_port.to_string())
        .replace("{attention_port}", &attention_port.to_string())
        .replace("{path}", &esc(&default_agent_path(fno_binary)))
        .replace("{home}", &esc(&home.display().to_string()))
        .replace(
            "{log_out}",
            &esc(&logs.join("web-bridge.out.log").display().to_string()),
        )
        .replace(
            "{log_err}",
            &esc(&logs.join("web-bridge.err.log").display().to_string()),
        )
}

/// Resolve the binary the plist pins: explicit `--bin`, else `fno` from
/// PATH, else the bare name. A dev-worktree binary dies with its worktree,
/// which is exactly the corpse this fix answers (a dead worktree path in the
/// bridge state file after a reboot).
fn resolve_binary(explicit: Option<&str>) -> String {
    explicit
        .map(str::to_string)
        .or_else(|| which("fno"))
        .unwrap_or_else(|| "fno".into())
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

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The installer body, split off [`install`] so tests can pass a temp
/// LaunchAgents dir. Returns the JSON receipt the command prints.
fn install_agent(
    agents_dir: &Path,
    explicit_bin: Option<&str>,
    web_port: u16,
    attention_port: u16,
) -> WebAgentReceipt {
    if !cfg!(target_os = "macos") {
        return WebAgentReceipt {
            status: "unsupported".to_string(),
            detail: "launchd is macOS-only; keep the bridge up with your init system (e.g. `nohup fno mux serve --web --attention-api &` or a user systemd unit)".to_string(),
            plist: None,
            label: None,
        };
    }
    let fno_binary = resolve_binary(explicit_bin);
    let label = web_bridge_label();
    let rendered = render_plist(&fno_binary, web_port, attention_port, &label);
    let plist_path = agents_dir.join(WEB_BRIDGE_PLIST_FILENAME);
    if let Some(parent) = plist_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return WebAgentReceipt {
                status: "failed".to_string(),
                detail: format!("create {}: {e}", parent.display()),
                plist: None,
                label: None,
            };
        }
    }
    match write_if_changed(&plist_path, &rendered) {
        Ok(false) => {
            // Matching bytes mean nothing on their own: a bootout (manual or
            // post-crash-loop) leaves the plist on disk with nothing
            // registered, and returning unchanged here would make the
            // doctor's own remedy ("run install-agent again") a no-op.
            // Re-register whenever launchd does not hold the label.
            if agent_loaded(&label) {
                return WebAgentReceipt {
                    status: "unchanged".to_string(),
                    detail: "plist matches; not re-registered".to_string(),
                    plist: Some(plist_path.display().to_string()),
                    label: Some(label),
                };
            }
        }
        Ok(true) => {}
        Err(e) => {
            return WebAgentReceipt {
                status: "failed".to_string(),
                detail: format!("write {}: {e}", plist_path.display()),
                plist: None,
                label: None,
            };
        }
    }
    let (msg, rc) = bounce(&plist_path, &label);
    WebAgentReceipt {
        status: if rc == 0 { "installed" } else { "failed" }.to_string(),
        detail: msg,
        plist: Some(plist_path.display().to_string()),
        label: Some(label),
    }
}

/// Whether launchd holds the web-bridge label. The loaded-read seam
/// (`FNO_TEST_WEB_AGENT_LOADED`) pins the answer for tests; production
/// reads `launchctl list` (an unreadable read answers false, the setup
/// services convention, so the installer offers the registration instead
/// of claiming a service runs when it cannot see).
fn agent_loaded(label: &str) -> bool {
    if let Ok(pinned) = std::env::var("FNO_TEST_WEB_AGENT_LOADED") {
        return pinned == "true";
    }
    crate::setup_services::is_loaded(label)
}

fn uninstall_agent(agents_dir: &Path) -> WebAgentReceipt {
    if !cfg!(target_os = "macos") {
        return WebAgentReceipt {
            status: "unsupported".to_string(),
            detail: "launchd is macOS-only".to_string(),
            plist: None,
            label: None,
        };
    }
    let label = web_bridge_label();
    let plist_path = agents_dir.join(WEB_BRIDGE_PLIST_FILENAME);
    let uid = unsafe { libc::getuid() };
    let target = format!("gui/{uid}/{label}");
    let (_, timed) = run_launchctl(&["bootout", &target]);
    let _ = std::fs::remove_file(&plist_path);
    // A timed-out bootout can leave the job registered (KeepAlive keeps
    // respawning the bridge); that is a failure, not a cleanup receipt.
    WebAgentReceipt {
        status: if timed { "failed" } else { "uninstalled" }.to_string(),
        detail: if timed {
            format!("`launchctl bootout {target}` timed out after {LAUNCHCTL_TIMEOUT_S}s; the plist is removed, so a reboot will not reload it, but the agent may keep running until then")
        } else {
            "agent unloaded and plist removed".to_string()
        },
        plist: Some(plist_path.display().to_string()),
        label: Some(label),
    }
}

/// bootout -> bootstrap: the pr-watch activation dance. bootout clears a
/// wedge an old registration can leave; its nonzero rc on a not-loaded job
/// is expected. bootstrap is retried because bootout is asynchronous and an
/// immediate re-register can lose to the still-settling label (rc=5).
/// RunAtLoad fires the bridge, so no kickstart is needed.
fn bounce(plist_path: &Path, label: &str) -> (String, i32) {
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
        format!("agent loaded ({target}); the bridge starts now and at every boot"),
        0,
    )
}

/// One launchctl call with a wait bound. `(rc, timed_out)`. The test seam
/// (`FNO_TEST_WEB_AGENT_LAUNCHCTL`, a JSON `[[rc, timed], ...]` consumed in
/// invocation order) stands in for the real tool, the way the pr-watch
/// twin's `FNO_TEST_PR_WATCH_LAUNCHCTL` does.
fn run_launchctl(args: &[&str]) -> (i32, bool) {
    if let Ok(script) = std::env::var("FNO_TEST_WEB_AGENT_LAUNCHCTL") {
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
    let Ok(mut child) = Command::new("launchctl")
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
                let _ = child.kill();
                let _ = child.wait();
                return (-1, true);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return (-1, false),
        }
    }
}

/// Write only when the bytes differ: a re-install that changes nothing must
/// not re-register the agent (and restart the bridge) for no reason.
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

/// Parse the `install-agent` tail: `--bin <path>`, `--port N`,
/// `--attention-port N`, `--json`. Unknown flags refuse with the command
/// spelled, the module's usage shape.
///
/// The refusals are a typed error enum: the per-crate string-keyed-read
/// ratchet holds that error shape shrink-only, so the seam never returns a
/// bare string.
#[derive(Debug, thiserror::Error)]
pub enum InstallArgsError {
    #[error("non-UTF-8 argument")]
    NonUtf8,
    #[error("{0} needs a value")]
    MissingValue(&'static str),
    #[error("{flag}: {raw:?} is not a port number")]
    BadPort { flag: &'static str, raw: String },
    #[error("unexpected argument {0:?}")]
    Unexpected(String),
}

pub fn parse_install_args(args: &[OsString]) -> Result<InstallArgs, InstallArgsError> {
    let mut out = InstallArgs::default();
    let mut i = 0usize;
    while let Some(arg) = args.get(i) {
        let Some(s) = arg.to_str() else {
            return Err(InstallArgsError::NonUtf8);
        };
        match s {
            "--json" | "-J" => out.json = true,
            "--bin" => {
                i += 1;
                out.bin = Some(
                    args.get(i)
                        .and_then(|v| v.to_str())
                        .map(str::to_string)
                        .ok_or(InstallArgsError::MissingValue("--bin"))?,
                );
            }
            "--port" | "--attention-port" => {
                i += 1;
                let raw =
                    args.get(i)
                        .and_then(|v| v.to_str())
                        .ok_or(InstallArgsError::MissingValue(if s == "--port" {
                            "--port"
                        } else {
                            "--attention-port"
                        }))?;
                let port: u16 = raw.parse().map_err(|_| {
                    if s == "--port" {
                        InstallArgsError::BadPort {
                            flag: "--port",
                            raw: raw.to_string(),
                        }
                    } else {
                        InstallArgsError::BadPort {
                            flag: "--attention-port",
                            raw: raw.to_string(),
                        }
                    }
                })?;
                if s == "--port" {
                    out.port = port;
                } else {
                    out.attention_port = port;
                }
            }
            f => return Err(InstallArgsError::Unexpected(f.to_string())),
        }
        i += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_plist_pins_serve_args_keepalive_and_ports() {
        let text = render_plist(
            "/usr/local/bin/fno",
            8722,
            8724,
            "sh.fno.web-bridge.0123abcd",
        );
        assert!(text.contains("<string>/usr/local/bin/fno</string>"));
        assert!(text.contains("<string>--attention-api</string>"));
        assert!(text.contains("<string>--port</string>\n    <string>8722</string>"));
        assert!(text.contains("<string>--attention-port</string>\n    <string>8724</string>"));
        assert!(text.contains("<string>sh.fno.web-bridge.0123abcd</string>"));
        assert!(text.contains("<key>KeepAlive</key>\n  <true/>"));
        assert!(text.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(text.contains("web-bridge.out.log"));
        assert!(text.contains("web-bridge.err.log"));
    }

    #[test]
    fn install_args_refuse_unknown_flags_and_bad_ports() {
        let ok = parse_install_args(&[
            OsString::from("--port"),
            OsString::from("9000"),
            OsString::from("--json"),
        ])
        .unwrap();
        assert_eq!(
            ok,
            InstallArgs {
                bin: None,
                port: 9000,
                attention_port: crate::attention_api::DEFAULT_ATTENTION_PORT,
                json: true,
            }
        );
        assert!(parse_install_args(&[OsString::from("--wat")]).is_err());
        assert!(parse_install_args(&[OsString::from("--port"), OsString::from("nope")]).is_err());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn install_agent_writes_the_plist_into_the_given_dir() {
        let dir = std::env::temp_dir().join(format!("fno-web-agent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = std::env::current_exe().unwrap();
        // Both seams pinned: the launchctl seam answers bootout then
        // bootstrap green (the test never touches real launchd), and the
        // loaded seam pins launchd as NOT holding the label, so the second
        // install's re-register check follows the bounce path.
        std::env::set_var("FNO_TEST_WEB_AGENT_LAUNCHCTL", "[[0,false],[0,false]]");
        std::env::set_var("FNO_TEST_WEB_AGENT_LOADED", "false");
        let receipt = install_agent(&dir, Some(bin.to_str().unwrap()), 8722, 8724);
        assert_eq!(receipt.status, "installed");
        let plist = dir.join(WEB_BRIDGE_PLIST_FILENAME);
        assert!(plist.exists());
        let text = std::fs::read_to_string(&plist).unwrap();
        assert!(text.contains("<key>KeepAlive</key>"));
        // Matching bytes with launchd already holding the label change
        // nothing and re-register nothing.
        std::env::set_var("FNO_TEST_WEB_AGENT_LOADED", "true");
        let again = install_agent(&dir, Some(bin.to_str().unwrap()), 8722, 8724);
        assert_eq!(again.status, "unchanged");
        std::env::remove_var("FNO_TEST_WEB_AGENT_LAUNCHCTL");
        std::env::remove_var("FNO_TEST_WEB_AGENT_LOADED");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
