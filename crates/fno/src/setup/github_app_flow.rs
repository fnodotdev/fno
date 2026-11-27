//! The fleet's own GitHub App through GitHub's manifest flow. A local
//! launcher page POSTs the manifest to GitHub's prefilled creation form;
//! the redirect code exchanges at `POST /app-manifests/{code}/conversions`;
//! the returned key lands at 0600 and the public App id rides the proposal
//! the wizard already applies. The redirect code and the pem never enter
//! wizard state.

use serde::Deserialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where the gate's App reader looks when config carries no
/// `github.app_key_path` (its `default_key_path`).
pub(crate) const KEY_REL_PATH: &str = ".fno/secrets/github-app.pem";

/// The redirect landing GitHub bounces to with `?code=...` in the address
/// bar. A static page on purpose: setup hosts nothing and owns no URL.
pub(crate) const REDIRECT_URL: &str = "https://example.com/";

/// One exchanged App: the public id, the slug behind the install URL, and
/// the private key (logged nowhere, stored only at the key path).
#[derive(Debug, Clone)]
pub(crate) struct Conversion {
    pub app_id: String,
    pub slug: String,
    pub pem: String,
}

/// The flow's failures, typed. [`FlowFault::GitHub`] carries the status and
/// a trimmed body so the screen can show what GitHub actually answered.
#[derive(Debug)]
pub(crate) enum FlowFault {
    Home,
    Io(String),
    Curl(String),
    GitHub { status: u16, detail: String },
    Shape(&'static str),
}

impl std::fmt::Display for FlowFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Home => write!(f, "HOME is unset, so the key has nowhere to live"),
            Self::Io(e) => write!(f, "file write failed: {e}"),
            Self::Curl(e) => write!(f, "curl failed: {e}"),
            Self::GitHub { status, detail } => {
                write!(f, "GitHub answered http {status}: {detail}")
            }
            Self::Shape(what) => write!(f, "the conversion response is missing {what}"),
        }
    }
}

/// The manifest the creation page fills from: the fleet read+write set,
/// metadata read, no webhook.
pub(crate) fn manifest_json(name: &str) -> String {
    serde_json::json!({
        "name": name,
        "url": "https://github.com/fnodotdev/fno",
        "hook_attributes": { "active": false },
        "redirect_url": REDIRECT_URL,
        "public": false,
        "default_permissions": {
            "actions": "write",
            "checks": "write",
            "contents": "write",
            "issues": "write",
            "pull_requests": "write",
            "statuses": "write",
            "metadata": "read"
        }
    })
    .to_string()
}

/// A short round-trip nonce: enough for the person to eyeball the redirect
/// against the launcher page, not a secret.
pub(crate) fn state_nonce() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("fno-{:x}-{:x}", nanos, std::process::id())
}

/// The App name: the host distinguishes machines so two Apps do not
/// collide, and the creation page lets the person rename on a clash.
pub(crate) fn app_name() -> String {
    let host = crate::setup::setup_detect::bounded_output(&["hostname"])
        .ok()
        .and_then(|h| h.lines().next().map(|l| l.trim().to_string()))
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "fleet".to_string());
    format!("fno-{}", host)
}

/// The default key path under HOME.
pub(crate) fn default_key_path() -> Result<PathBuf, FlowFault> {
    let home = std::env::var_os("HOME").ok_or(FlowFault::Home)?;
    Ok(PathBuf::from(home).join(KEY_REL_PATH))
}

/// The launcher page: a local HTML file with one button whose form POSTs
/// the manifest to GitHub's creation URL. An empty org field means the
/// personal form; a name targets the org form. Nothing here is secret, so
/// the temp file carries no cleanup contract.
pub(crate) fn write_launcher(path: &Path, state: &str, name: &str) -> Result<(), FlowFault> {
    // The manifest rides a JS single-quoted string inside a <script> block,
    // whose content is raw text: no HTML entity ever decodes there. Escape
    // for the string literal instead, and send `<` as an escape so the text
    // can never spell a closing script tag.
    let manifest = manifest_json(name)
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('<', "\\u003c")
        .replace('\n', "\\n");
    let page = format!(
        "<!doctype html><html><body>\
<p>fno is creating the fleet's GitHub App (state {state}).</p>\
<p>Organization (leave empty for your personal account): \
<input id=\"org\"></p>\
<button onclick=\"go()\">Create the GitHub App on GitHub</button>\
<script>\
function go() {{\
  var org = document.getElementById('org').value.trim();\
  var base = org\
    ? 'https://github.com/organizations/' + encodeURIComponent(org) + '/settings/apps/new'\
    : 'https://github.com/settings/apps/new';\
  var f = document.createElement('form');\
  f.method = 'post';\
  f.action = base + '?state={state}';\
  var i = document.createElement('input');\
  i.type = 'hidden';\
  i.name = 'manifest';\
  i.value = '{manifest}';\
  f.appendChild(i);\
  document.body.appendChild(f);\
  f.submit();\
}}\
</script></body></html>"
    );
    std::fs::write(path, page).map_err(|e| FlowFault::Io(e.to_string()))
}

/// Best-effort browser open; false means the screen prints the path
/// instead (a fleet host may have no browser, and any machine can open the
/// file).
pub(crate) fn open_in_browser(path: &Path) -> bool {
    for opener in ["open", "xdg-open"] {
        if Command::new(opener).arg(path).spawn().is_ok() {
            return true;
        }
    }
    false
}

/// Accept either the bare code or the whole redirected URL: the person
/// pastes what the address bar shows.
fn extract_code(paste: &str) -> &str {
    let trimmed = paste.trim();
    let Some(pos) = trimmed.find("code=") else {
        return trimmed;
    };
    let rest = &trimmed[pos + "code=".len()..];
    let end = rest.find(['&', '#']).unwrap_or(rest.len());
    &rest[..end]
}

/// Exchange the redirect code. The documented request carries no
/// Authorization header: the one-hour code is the credential. The curl
/// config rides stdin so nothing lands in `ps` output or the environment.
pub(crate) fn exchange(code: &str) -> Result<Conversion, FlowFault> {
    let code = extract_code(code);
    let url = format!("https://api.github.com/app-manifests/{code}/conversions");
    let config = "silent\nshow-error\nmax-time = 30\nheader = \"Accept: application/vnd.github+json\"\nheader = \"X-GitHub-Api-Version: 2022-11-28\"\nwrite-out = \"\\n%{http_code}\"\n";
    let mut child = Command::new("curl")
        .arg("-K")
        .arg("-")
        .args(["-X", "POST"])
        .arg(&url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| FlowFault::Curl(e.to_string()))?;
    child
        .stdin
        .take()
        .ok_or_else(|| FlowFault::Curl("stdin unavailable".into()))?
        .write_all(config.as_bytes())
        .map_err(|e| FlowFault::Curl(e.to_string()))?;
    let out = child
        .wait_with_output()
        .map_err(|e| FlowFault::Curl(e.to_string()))?;
    let text = String::from_utf8_lossy(&out.stdout);
    if text.is_empty() {
        return Err(FlowFault::Curl(
            String::from_utf8_lossy(out.stderr.trim_ascii()).into_owned(),
        ));
    }
    let (body, status) = match text.rsplit_once('\n') {
        Some((b, c)) => (b, c.trim().parse::<u16>().unwrap_or(0)),
        None => (text.as_ref(), 0),
    };
    from_status(status, body.trim())
}

/// The status arm: 404 names the expired code, other non-2xx carries the
/// trimmed body, 2xx parses.
pub(crate) fn from_status(status: u16, body: &str) -> Result<Conversion, FlowFault> {
    if status == 404 {
        return Err(FlowFault::GitHub {
            status,
            detail: "the code is wrong or already used (it lives one hour)".into(),
        });
    }
    if !(200..300).contains(&status) {
        let detail: String = body.chars().take(200).collect();
        return Err(FlowFault::GitHub { status, detail });
    }
    from_body(body)
}

#[derive(Deserialize)]
struct ConversionBody {
    id: i64,
    slug: String,
    pem: String,
}

fn from_body(body: &str) -> Result<Conversion, FlowFault> {
    let parsed: ConversionBody = serde_json::from_str(body)
        .map_err(|_| FlowFault::Shape("a JSON object with id, slug and pem"))?;
    Ok(Conversion {
        app_id: parsed.id.to_string(),
        slug: parsed.slug,
        pem: parsed.pem,
    })
}

/// Write the key at 0600 via temp+rename (the setup_messaging pattern);
/// the secrets directory is created 0700 when missing.
pub(crate) fn write_key(path: &Path, pem: &str) -> Result<(), FlowFault> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| FlowFault::Io(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| FlowFault::Io(e.to_string()))?;
        }
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&tmp).map_err(|e| FlowFault::Io(e.to_string()))?;
    file.write_all(pem.as_bytes())
        .map_err(|e| FlowFault::Io(e.to_string()))?;
    file.sync_all().map_err(|e| FlowFault::Io(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| FlowFault::Io(e.to_string()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| FlowFault::Io(e.to_string()))
}

/// The configured App id, when the global config already carries one. The
/// screen's precondition: an existing App is left alone, never duplicated.
pub(crate) fn configured_app_id() -> Option<String> {
    let text = std::fs::read_to_string(crate::config_defaults::global_config_path()).ok()?;
    app_id_from_config(&text)
}

#[derive(Deserialize)]
struct GithubKeys {
    app_id: Option<String>,
}

#[derive(Deserialize)]
struct ConfigTop {
    github: Option<GithubKeys>,
}

fn app_id_from_config(text: &str) -> Option<String> {
    let top: ConfigTop = toml::from_str(text).ok()?;
    top.github?.app_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_carries_the_fleet_permissions_and_no_webhook() {
        let body = manifest_json("fno-test");
        assert!(body.contains("\"name\":\"fno-test\""));
        assert!(body.contains("\"actions\":\"write\""));
        assert!(body.contains("\"checks\":\"write\""));
        assert!(body.contains("\"contents\":\"write\""));
        assert!(body.contains("\"issues\":\"write\""));
        assert!(body.contains("\"pull_requests\":\"write\""));
        assert!(body.contains("\"statuses\":\"write\""));
        assert!(body.contains("\"metadata\":\"read\""));
        assert!(body.contains("\"hook_attributes\":{\"active\":false}"));
        assert!(body.contains(REDIRECT_URL));
    }

    #[test]
    fn launcher_carries_the_manifest_and_the_org_field() {
        let dir = std::env::temp_dir().join(format!("fno-flow-launcher-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fno-fleet-app.html");
        write_launcher(&path, "fno-state-1", "fno-test").unwrap();
        let page = std::fs::read_to_string(&path).unwrap();
        assert!(page.contains("i.name = 'manifest'"));
        // The manifest must reach the browser as parseable JSON: raw quotes,
        // no HTML entity residue a <script> block would never decode.
        assert!(page.contains("i.value = '{\"name\":\"fno-test\""));
        assert!(!page.contains("&quot;"));
        assert!(!page.contains("&amp;"));
        assert!(page.contains("settings/apps/new"));
        assert!(page.contains("organizations/' + encodeURIComponent(org)"));
        assert!(page.contains("state fno-state-1"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn conversion_parses_id_slug_pem() {
        let body = r#"{"id":1234,"slug":"fno-imac","pem":"-----BEGIN RSA PRIVATE KEY-----"}"#;
        let c = from_body(body).unwrap();
        assert_eq!(c.app_id, "1234");
        assert_eq!(c.slug, "fno-imac");
        assert!(c.pem.starts_with("-----BEGIN"));
    }

    #[test]
    fn conversion_refuses_missing_pem() {
        let body = r#"{"id":1234,"slug":"fno-imac"}"#;
        let err = from_body(body).unwrap_err().to_string();
        assert!(err.contains("pem"));
    }

    #[test]
    fn status_404_names_the_expired_code() {
        let err = from_status(404, "{}").unwrap_err().to_string();
        assert!(err.contains("404"));
        assert!(err.contains("one hour"));
    }

    #[test]
    fn status_500_carries_the_body() {
        let err = from_status(500, "boom").unwrap_err().to_string();
        assert!(err.contains("500"));
        assert!(err.contains("boom"));
    }

    #[test]
    fn extract_code_takes_the_code_from_a_pasted_url() {
        assert_eq!(extract_code("a1b2c3"), "a1b2c3");
        assert_eq!(
            extract_code("https://example.com/?code=a1b2c3&state=fno-1"),
            "a1b2c3"
        );
        assert_eq!(extract_code("  ?code=a1b2c3  "), "a1b2c3");
        assert_eq!(extract_code("https://example.com/?code=a1b2c3"), "a1b2c3");
    }

    #[test]
    fn write_key_lands_at_0600() {
        let dir = std::env::temp_dir().join(format!("fno-flow-key-{}", std::process::id()));
        let path = dir.join("github-app.pem");
        write_key(&path, "key-material").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "key-material");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn configured_check_reads_the_github_block() {
        assert_eq!(
            app_id_from_config("[github]\napp_id = \"99\"\n"),
            Some("99".to_string())
        );
        assert_eq!(
            app_id_from_config("[review]\nposture = \"self_review\"\n"),
            None
        );
    }
}
