//! The opencode serve seat for a portal reach. A Locate-tier row whose
//! harness declares the serve-attach capability can still Drive: the seat
//! reads the shared serve's state file, probes its health, gates the TUI
//! generation against the serve's, and builds the attach argv the pane
//! execs. Every blocked state names the real blocker.

use super::*;

pub(crate) enum ServeSeat {
    Attach {
        argv: Vec<String>,
        env: Vec<(String, String)>,
    },
    Skew(String),
    Honest(ServeHonest),
    Outside,
}

pub(crate) struct ServeHonest {
    pub why: String,
    pub facts: Vec<String>,
}

#[derive(serde::Deserialize, Clone)]
struct ServeState {
    base_url: String,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    token: String,
}

#[derive(Clone)]
struct ServeHealth {
    healthy: bool,
    version: Option<String>,
}

/// Resolve the agents home: `FNO_AGENTS_HOME` when declared, else
/// `$HOME/.fno/agents` - the same shape the runtime's AgentsHome resolves
/// (paths.rs), mirrored here because the dep direction forbids importing
/// it (fno-agents depends on fno, never the reverse).
fn agents_home() -> Option<std::path::PathBuf> {
    if let Some(declared) = std::env::var_os("FNO_AGENTS_HOME") {
        if !declared.is_empty() {
            return Some(std::path::PathBuf::from(declared));
        }
    }
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .map(|h| h.join(".fno").join("agents"))
}

/// The state file path at the agents home.
fn serve_state_path() -> Option<std::path::PathBuf> {
    agents_home().map(|h| h.join("opencode-serve.json"))
}

/// The TUI candidate the pane would exec: `FNO_OPENCODE_BIN` when
/// declared, else the PATH `opencode`.
fn tui_candidate() -> String {
    std::env::var("FNO_OPENCODE_BIN").unwrap_or_else(|_| "opencode".to_string())
}

/// One bounded probe of `/global/health` with `opencode:<token>` Basic
/// auth, mirroring the runtime's http_json (opencode_serve.rs) because the
/// dep direction forbids importing it. One attempt, 500ms connect and read
/// bounds: the reach runs on the server loop, and a dead port must not
/// stall it long.
fn serve_health(base_url: &str, token: &str) -> Option<ServeHealth> {
    let after_scheme = base_url.strip_prefix("http://")?;
    let authority = after_scheme.trim_end_matches('/');
    use std::io::{Read as _, Write as _};
    let addr_iter = std::net::ToSocketAddrs::to_socket_addrs(authority).ok()?;
    let mut stream = None;
    for addr in addr_iter {
        if let Ok(s) = std::net::TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) {
            stream = Some(s);
            break;
        }
    }
    let mut stream = stream?;
    stream.set_read_timeout(Some(PROBE_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(PROBE_TIMEOUT)).ok()?;
    let request = format!(
        "GET /global/health HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Basic {}\r\nConnection: close\r\n\r\n",
        basic_auth("opencode", token),
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf).ok()?;
    let status_ok = buf.starts_with("HTTP/1.1 200") || buf.starts_with("HTTP/1.0 200");
    let body = buf.split("\r\n\r\n").nth(1)?;
    let doc: serde_json::Value = serde_json::from_str(body).ok()?;
    let healthy = doc
        .get("healthy")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let version = doc
        .get("version")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    (status_ok && healthy).then_some(ServeHealth { healthy, version })
}

/// The connect/read bound for the health probe.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// RFC 4648 standard base64 through the crate: the same one-line encode
/// opencode_serve.rs runs for its serve, so the workspace carries one
/// base64 implementation instead of two hand-rolled copies.
fn basic_auth(user: &str, password: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
}

/// The TUI binary's self-reported major version, measured with one bounded
/// subprocess: `opencode --version`. The npm wrapper answers in a few
/// hundred ms; a wedged binary is bounded by a 2s poll deadline.
fn tui_version(bin: &str) -> Option<String> {
    #[cfg(test)]
    if let Some(pinned) = TUI_VERSION_OVERRIDE.with(|p| p.borrow().clone()) {
        return pinned;
    }
    let mut child = std::process::Command::new(bin)
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut out = String::new();
                std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
                return Some(out);
            }
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(_) => return None,
        }
    }
}

/// Parse a `major.minor.patch` (or `v`-prefixed) version line to its major.
fn parse_major(version: &str) -> Option<u32> {
    version
        .trim()
        .strip_prefix('v')?
        .split('.')
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
thread_local! {
    /// Test override for the serve state read: `Some(None)` simulates no
    /// state file, `Some(Some(..))` pins a state.
    static STATE_OVERRIDE: std::cell::RefCell<Option<Option<ServeState>>> =
        const { std::cell::RefCell::new(None) };
    /// Test override for the health probe: `None` = real probe, `Some(h)` =
    /// pinned verdict (healthy false simulates unhealthy).
    static HEALTH_OVERRIDE: std::cell::RefCell<Option<ServeHealth>> =
        const { std::cell::RefCell::new(None) };
    /// Test override for the TUI version probe: `Some(None)` simulates a
    /// binary that reports none, `Some(Some(v))` pins a version.
    static TUI_VERSION_OVERRIDE: std::cell::RefCell<Option<Option<String>>> =
        const { std::cell::RefCell::new(None) };
}

/// Read the shared serve's state file. Typed; unreadable is None.
fn read_serve_state() -> Option<ServeState> {
    #[cfg(test)]
    if let Some(pinned) = STATE_OVERRIDE.with(|p| p.borrow().clone()) {
        return pinned;
    }
    state_path_read()
}

/// The real state read, split for the override arm.
fn state_path_read() -> Option<ServeState> {
    let path = serve_state_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// The health verdict, override-aware for tests.
fn health_verdict(base_url: &str, token: &str) -> Option<ServeHealth> {
    #[cfg(test)]
    if let Some(pinned) = HEALTH_OVERRIDE.with(|p| p.borrow().clone()) {
        return Some(pinned);
    }
    serve_health(base_url, token)
}

/// The primary TUI candidate: the FNO_OPENCODE_BIN / PATH binary with its
/// version. The only probe a healthy reach pays.
fn tui_primary() -> (String, Option<String>) {
    let bin = tui_candidate();
    let version = tui_version(&bin);
    (bin, version)
}

/// Every opencode TUI candidate on this machine, the primary first: the
/// vendor installer path and the bun path join when they exist. Probes run
/// only on the skew arm, where the notice names each with its version so
/// the fix is readable, not guessed.
fn tui_candidates(primary: (String, Option<String>)) -> Vec<(String, Option<String>)> {
    let mut out = vec![primary];
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    for path in [
        home.as_ref().map(|h| h.join(".opencode/bin/opencode")),
        home.as_ref().map(|h| h.join(".bun/bin/opencode")),
    ] {
        if let Some(p) = path {
            if p.exists() {
                let shown = p.to_string_lossy().to_string();
                out.push((shown, tui_version(&p.to_string_lossy())));
            }
        }
    }
    out
}

/// The serve seat verdict for one row. `Outside` for any harness that
/// declares no serve-attach capability; every other verdict names what
/// measured false.

pub(crate) fn serve_seat(row: &RegistryAgent) -> ServeSeat {
    let Some(harness) = row.harness.as_deref() else {
        return ServeSeat::Outside;
    };
    let Some(form) = crate::agents_view::serve_attach_form(harness) else {
        return ServeSeat::Outside;
    };
    let Some(sid) = row.effective_identity() else {
        return honest("the row carries no session id, so a serve attach cannot name it".into());
    };
    let Some(state) = read_serve_state() else {
        return honest(
            "no shared opencode serve is recorded (no opencode-serve.json at the agents home)"
                .into(),
        );
    };
    let Some(health) = health_verdict(&state.base_url, &state.token) else {
        return honest_at(
            &state,
            format!(
                "the shared serve at {} answered nothing on /global/health",
                state.base_url
            ),
        );
    };
    if !health.healthy {
        return honest_at(
            &state,
            "the shared serve answered unhealthy on /global/health".into(),
        );
    }
    let Some(serve_version) = health.version.clone() else {
        return honest_at(
            &state,
            "the shared serve answered healthy but reported no version".into(),
        );
    };
    let Some(serve_major) = parse_major(&serve_version) else {
        return honest_at(
            &state,
            format!("the shared serve's version {serve_version:?} reads no major"),
        );
    };
    let primary = tui_primary();
    let Some(tui_version) = primary.1.as_deref() else {
        return honest_at(&state, "the opencode binary reported no version".into());
    };
    let Some(tui_major) = parse_major(tui_version) else {
        return honest_at(
            &state,
            format!("the opencode binary's version {tui_version:?} reads no major"),
        );
    };
    if tui_major != serve_major {
        let candidates = tui_candidates(primary);
        return ServeSeat::Skew(notice_text(&serve_version, &candidates));
    }
    #[cfg_attr(not(test), allow(unused_mut))]
    let mut argv = form.render(tui_major, &state.base_url, sid);
    let env = vec![(form.password_env.clone(), state.token.clone())];
    #[cfg(test)]
    {
        capture_serve_env(&env);
        capture_serve_argv(&argv);
        if let Some(bin) = BIN_OVERRIDE.with(|p| p.borrow().clone()) {
            if let Some(first) = argv.first_mut() {
                *first = bin;
            }
        }
    }
    ServeSeat::Attach { argv, env }
}

/// The serve facts a Locate screen renders even when unhealthy: pid and
/// port, read off the recorded state.
fn serve_facts(state: &ServeState) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(pid) = state.pid {
        out.push(format!("serve pid:  {pid}"));
    }
    if let Some(port) = state.port {
        out.push(format!("serve port: {port}"));
    }
    out
}

/// The skew notice: the serve version, every candidate with its version,
/// and the two fixes (point FNO_OPENCODE_BIN at a matching build, or
/// restart the serve on the installed binary).
fn notice_text(serve_version: &str, candidates: &[(String, Option<String>)]) -> String {
    let mut out = format!(
        "opencode serve speaks {serve_version} but the TUI candidates do not match its generation; no pane spawns"
    );
    for (path, version) in candidates {
        match version {
            Some(v) => out.push_str(&format!("; {path} -> {v}")),
            None => out.push_str(&format!("; {path} -> no version")),
        }
    }
    out.push_str(
        "; point FNO_OPENCODE_BIN at a matching build or restart the serve on the installed binary",
    );
    out
}

/// The Honest verdict for a state with no serve facts to render.
fn honest(why: String) -> ServeSeat {
    ServeSeat::Honest(ServeHonest {
        why,
        facts: Vec::new(),
    })
}

/// The Honest verdict carrying the serve facts block.
fn honest_at(state: &ServeState, why: String) -> ServeSeat {
    ServeSeat::Honest(ServeHonest {
        why,
        facts: serve_facts(state),
    })
}

#[cfg(test)]
thread_local! {
    /// Test-only: replaces the rendered serve argv's program token, so a
    /// reach in tests execs a safe binary instead of the real opencode.
    static BIN_OVERRIDE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    /// Test-only: captures the env pairs the seat produced, so a test
    /// asserts the password rode env without reading a live pane.
    static LAST_SERVE_ENV: std::cell::RefCell<Option<Vec<(String, String)>>> =
        const { std::cell::RefCell::new(None) };
    /// Test-only: captures the full serve argv, so a test asserts the
    /// rendered shape without reading a live pane.
    static LAST_SERVE_ARGV: std::cell::RefCell<Option<Vec<String>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn set_serve_bin(bin: &str) {
    BIN_OVERRIDE.with(|p| *p.borrow_mut() = Some(bin.to_string()));
}

#[cfg(test)]
pub(super) fn last_serve_env() -> Option<Vec<(String, String)>> {
    LAST_SERVE_ENV.with(|p| p.borrow().clone())
}

#[cfg(test)]
fn capture_serve_env(env: &[(String, String)]) {
    LAST_SERVE_ENV.with(|p| *p.borrow_mut() = Some(env.to_vec()));
}

#[cfg(test)]
pub(super) fn last_serve_argv() -> Option<Vec<String>> {
    LAST_SERVE_ARGV.with(|p| p.borrow().clone())
}

#[cfg(test)]
fn capture_serve_argv(argv: &[String]) {
    LAST_SERVE_ARGV.with(|p| *p.borrow_mut() = Some(argv.to_vec()));
}

#[cfg(test)]
pub(super) fn set_pinned_state(base_url: &str, port: Option<u16>, pid: Option<u32>, token: &str) {
    STATE_OVERRIDE.with(|p| {
        *p.borrow_mut() = Some(Some(ServeState {
            base_url: base_url.to_string(),
            port,
            pid,
            token: token.to_string(),
        }))
    });
}

#[cfg(test)]
pub(super) fn clear_pinned_state() {
    STATE_OVERRIDE.with(|p| *p.borrow_mut() = Some(None));
}

#[cfg(test)]
pub(super) fn set_pinned_health(healthy: bool, version: Option<&str>) {
    HEALTH_OVERRIDE.with(|p| {
        *p.borrow_mut() = Some(ServeHealth {
            healthy,
            version: version.map(str::to_string),
        })
    });
}

#[cfg(test)]
pub(super) fn clear_pinned_health() {
    HEALTH_OVERRIDE.with(|p| *p.borrow_mut() = None);
}

#[cfg(test)]
pub(super) fn set_pinned_tui_version(version: Option<&str>) {
    TUI_VERSION_OVERRIDE.with(|p| *p.borrow_mut() = Some(version.map(str::to_string)));
}
