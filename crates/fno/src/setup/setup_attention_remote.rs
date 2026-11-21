//! The `attention-remote` setup step: the phone reaches the board. It writes one `[[attention.sinks]]` ntfy row carrying `view_base_url`, one `[[web.invites]]` owner row, and the `tailscale serve` route for the web bridge, then prints the subscribe link and the HQ link. The push is only the alert; the page is the answer surface, so the bridge must run under launchd (the `web-bridge-agent` step) and the invites row this step writes is what lets the phone answer. The no-prompt path writes nothing and names the re-entry command.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::setup_messaging::{machine, Ask, Outcome};

/// The re-entry command every skip names.
pub const REENTER: &str = "fno config setup run --only attention-remote";

const ENV_FILE: &str = "~/.fno/.env";

/// The default ntfy server.
const DEFAULT_SERVER: &str = "https://ntfy.sh";

/// The default web-bridge port the serve route maps.
const BRIDGE_PORT: u16 = 8722;

/// The ways the remote step fails: a tailnet read, or a config write.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("{0}")]
    Tailscale(String),
    #[error("{0}")]
    Config(String),
}

impl RemoteError {
    fn message(&self) -> &str {
        match self {
            Self::Tailscale(m) | Self::Config(m) => m,
        }
    }
}

/// The tailscale seam: the status read that names this machine's tailnet DNS name and login, and the serve command the route runs. Tests inject both.
pub trait Tailscale {
    fn status(&mut self) -> Result<(String, String), RemoteError>;
    fn serve(&mut self, port: u16) -> Result<String, RemoteError>;
}

/// The real seam: two `tailscale` child processes.
pub struct CmdTailscale;

impl Tailscale for CmdTailscale {
    fn status(&mut self) -> Result<(String, String), RemoteError> {
        let out = std::process::Command::new("tailscale")
            .arg("status")
            .arg("--json")
            .output()
            .map_err(|e| RemoteError::Tailscale(format!("tailscale: {e}")))?;
        if !out.status.success() {
            return Err(RemoteError::Tailscale(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        let v: Value =
            serde_json::from_slice(&out.stdout).map_err(|e| format!("tailscale: {e}"))?;
        let dns = v["Self"]["DNSName"]
            .as_str()
            .unwrap_or_default()
            .trim_end_matches('.')
            .to_string();
        let login = v["Self"]["LoginName"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if dns.is_empty() || login.is_empty() {
            return Err(RemoteError::Tailscale(String::from(
                "tailscale status --json names no Self.DNSName/LoginName; is this machine on a tailnet?",
            )));
        }
        Ok((dns, login))
    }

    fn serve(&mut self, port: u16) -> Result<String, RemoteError> {
        let out = std::process::Command::new("tailscale")
            .arg("serve")
            .arg("--bg")
            .arg(format!("http://127.0.0.1:{port}"))
            .output()
            .map_err(|e| RemoteError::Tailscale(format!("tailscale: {e}")))?;
        if !out.status.success() {
            return Err(RemoteError::Tailscale(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

/// Whether this machine already carries an ntfy row with a `view_base_url`.
pub fn view_reachable(rows: &[Value]) -> bool {
    rows.iter()
        .any(|r| r["type"] == "ntfy" && r["view_base_url"].is_string())
}

/// The default topic: `fno-questions-` plus the machine id's safe characters, so two machines never share a public topic by accident.
pub fn default_topic(machine: &str) -> String {
    let safe: String = machine
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    format!("fno-questions-{safe}")
}

/// 32 random bytes as hex, the same mint shape the web bridge uses.
pub fn mint_token() -> Option<String> {
    use std::io::Read;
    let mut buf = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .ok()?;
    Some(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// The prompting step. `rows` are the current `attention.sinks` rows, `invites` the current `web.invites` rows; `set_sinks` and `set_invites` are the validated `fno config set` doors; `tailscale` carries the status and serve seams.
pub fn step(
    rows: Vec<Value>,
    invites: Vec<Value>,
    ask: &mut dyn Ask,
    set_sinks: &dyn Fn(&str) -> Result<PathBuf, RemoteError>,
    set_invites: &dyn Fn(&str) -> Result<PathBuf, RemoteError>,
    tailscale: &mut dyn Tailscale,
    home: &Path,
) -> Outcome {
    let mut out = Outcome::default();
    let me = machine();
    let (dns, login) = match tailscale.status() {
        Ok(pair) => pair,
        Err(e) => {
            out.needs_human
                .push(format!("attention-remote: {}", e.message()));
            return out;
        }
    };
    let view_base = format!("https://{dns}");
    // One ntfy row on this machine: amend it, never a duplicate push.
    let existing = rows
        .iter()
        .position(|r| r["type"] == "ntfy" && r["url"].is_string() && r["topic"].is_string());
    let (next_rows, url, topic, _env) = match existing {
        Some(i) => {
            let r = rows[i].clone();
            let url = r["url"].as_str().unwrap_or_default().to_string();
            let topic = r["topic"].as_str().unwrap_or_default().to_string();
            let env = r["token_env"].as_str().unwrap_or_default().to_string();
            let mut next = rows.clone();
            next[i]["view_base_url"] = json!(view_base);
            (next, url, topic, env)
        }
        None => {
            let topic = ask
                .line(&format!("ntfy topic (Enter = {}):", default_topic(&me)))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| default_topic(&me));
            let env = "FNO_SINK_PHONE_TOKEN".to_string();
            let token = mint_token().unwrap_or_default();
            if token.is_empty() {
                out.needs_human
                    .push("attention-remote: cannot mint a sink token".to_string());
                return out;
            }
            // The set validates the token, so the env write lands first, the
            // same order messaging uses.
            let env_file = home.join(".fno/.env");
            if let Err(e) = crate::setup_messaging::write_env(&env_file, &env, Some(&token)) {
                out.needs_human
                    .push(format!("attention-remote: cannot write {ENV_FILE}: {e}"));
                return out;
            }
            let mut next = rows.clone();
            next.push(json!({
                "name": "phone",
                "type": "ntfy",
                "url": DEFAULT_SERVER,
                "topic": topic,
                "token_env": env,
                "view_base_url": view_base,
            }));
            (next, DEFAULT_SERVER.to_string(), topic, env)
        }
    };
    // Fill the invites owner row: the tailnet login answers the HQ door.
    let mut next_invites = invites.clone();
    if !next_invites.iter().any(|r| r["email"] == login) {
        next_invites.push(json!({
            "email": login,
            "scope": "*",
            "role": "owner",
        }));
    }
    // The serve route: the bridge port mapped at the tailnet root.
    match tailscale.serve(BRIDGE_PORT) {
        Ok(line) => {
            out.done
                .push(format!("attention-remote: serve route set: {line}"));
        }
        Err(e) => {
            out.needs_human.push(format!(
                "attention-remote: the serve route needs a human: tailscale serve --bg http://127.0.0.1:{BRIDGE_PORT} ({})",
                e.message()
            ));
        }
    }
    if let Err(e) = set_sinks(&Value::Array(next_rows).to_string()) {
        out.needs_human.push(format!(
            "attention-remote: cannot write the sink row: {}",
            e.message()
        ));
    } else {
        out.done.push(format!(
            "attention-remote: subscribe to {url}/{topic} in the ntfy app",
            url = url.trim_end_matches('/'),
        ));
        out.done.push(format!(
            "attention-remote: HQ link {view_base}/hq (open a question at {view_base}/hq?question=<id>)"
        ));
    }
    if let Err(e) = set_invites(&Value::Array(next_invites).to_string()) {
        out.needs_human.push(format!(
            "attention-remote: cannot write the invites row: {}",
            e.message()
        ));
    }
    out
}

/// The no-prompt summary line.
pub fn summary_line(rows: &[Value]) -> String {
    if view_reachable(rows) {
        "attention-remote: configured".to_string()
    } else {
        format!("attention-remote: skipped - set it up later with `{REENTER}`")
    }
}
