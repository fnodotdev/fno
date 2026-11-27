//! The claude `--settings` route-settings writer, ported from Python
//! `model_routing._write_settings_env_file` (change 6). ONE writer owns the
//! payload shape (the env map, an optional sandbox block, the foreign-endpoint
//! tool denies), the sha256[:16] content address, the 0600 atomic publish and
//! the stable `lane-<row>.json` alias a routing row's native line prints. The
//! digest name is the dedup store; when a write (or the spawn mint, after the
//! session uuid resolves) knows the session id, it also publishes the twin
//! `session-<id>.json` beside it, so `ls route-settings` joins a file to its
//! session by eye. The
//! door command reads the composed env on STDIN - it carries
//! `ANTHROPIC_AUTH_TOKEN`, which must never ride argv - and answers
//! `{"path", "alias_path"}` on stdout.

use crate::accounts::credentials::{SCRUB_AUTH_VARS, SECRET_ROUTE_VARS};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Tools a worker on a non-Anthropic endpoint must not be offered. Artifact
/// publishes through Anthropic's own backend, and z.ai refuses any tools
/// pattern holding a `\p{...}` escape (model_routing.py:FOREIGN_ENDPOINT_DENY_TOOLS).
const FOREIGN_ENDPOINT_DENY_TOOLS: &[&str] = &["Artifact"];

const ANTHROPIC_API_HOST: &str = "api.anthropic.com";

/// The lowercased host of `base_url`, or "" when there is none. The authority
/// follows the first `//` (a scheme prefix or none) and runs to the first
/// `/`, `?` or `#` (model_routing.py:_base_url_host).
fn base_url_host(base_url: &str) -> String {
    let base = base_url.trim();
    if base.is_empty() {
        return String::new();
    }
    let authority = match base.find("//") {
        Some(index) => &base[index + 2..],
        None => base,
    }
    .split(['/', '?', '#'])
    .next()
    .unwrap_or("");
    // Drop userinfo; keep the host, lowercased.
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    host.to_ascii_lowercase()
}

fn foreign_endpoint_tool_denies(env: &Map<String, Value>) -> Vec<&'static str> {
    let base_url = env
        .get("ANTHROPIC_BASE_URL")
        .and_then(Value::as_str)
        .unwrap_or("");
    let host = base_url_host(base_url);
    if host.is_empty() || host == ANTHROPIC_API_HOST {
        return Vec::new();
    }
    FOREIGN_ENDPOINT_DENY_TOOLS.to_vec()
}

/// One write ask: the composed env, the optional sandbox block, the
/// optional stable lane alias (`lane-<alias>.json`), and the optional
/// session id the file belongs to (`session-<id>.json`).
pub struct WriteAsk {
    pub env: Map<String, Value>,
    pub sandbox: Option<Value>,
    pub alias: Option<String>,
    pub session: Option<String>,
    /// The routine-work permission rules, empty unless the switch is on.
    pub rules: crate::spawn::auto_approve::Rules,
}

pub struct WriteReceipt {
    pub path: PathBuf,
    pub alias_path: Option<PathBuf>,
}

/// The payload bytes: `{"env": ...}` plus the optional sandbox block and the
/// foreign-endpoint deny, keys inserted in sorted order so the content address
/// is deterministic (Python wrote `json.dumps(payload, sort_keys=True)`).
pub(crate) fn payload_bytes(
    env: &Map<String, Value>,
    sandbox: Option<&Value>,
    rules: &crate::spawn::auto_approve::Rules,
) -> Vec<u8> {
    let mut sorted_env = Map::new();
    let mut keys: Vec<&String> = env.keys().collect();
    keys.sort();
    for k in keys {
        sorted_env.insert(k.clone(), env[k].clone());
    }
    let mut root = Map::new();
    root.insert("env".to_string(), Value::Object(sorted_env));
    let deny = foreign_endpoint_tool_denies(env);
    let mut permissions = Map::new();
    if !deny.is_empty() {
        permissions.insert("deny".to_string(), json!(deny));
    }
    if !rules.allow.is_empty() {
        permissions.insert("allow".to_string(), json!(rules.allow));
    }
    if !rules.ask.is_empty() {
        permissions.insert("ask".to_string(), json!(rules.ask));
    }
    if !permissions.is_empty() {
        root.insert("permissions".to_string(), Value::Object(permissions));
    }
    if let Some(sb) = sandbox {
        root.insert("sandbox".to_string(), sb.clone());
    }
    if let Some(theme) = claude_theme_env() {
        root.insert("theme".to_string(), Value::String(theme));
    }
    serde_json::to_vec(&Value::Object(root)).expect("payload serializes")
}

/// The Claude Code theme the mux pinned for this pane (`FNO_CLAUDE_THEME`,
/// set from `theme::claude_theme`). A `claude --bg` session is forked by the
/// claude daemon and the attached view renders with the theme the session
/// launched with, so its settings file must carry it. `claude attach` itself
/// ignores `--settings`.
pub(crate) fn claude_theme_env() -> Option<String> {
    let v = std::env::var("FNO_CLAUDE_THEME").ok()?;
    let ok = !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_'));
    ok.then_some(v)
}

fn digest_of(payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(payload);
    let full = hasher.finalize();
    full.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

const ENV_FALSY_WORDS: [&str; 5] = ["", "0", "false", "no", "off"];

/// The inherited model claims worth flooring, read from the spawn's own
/// environment: the five model vars carrying a value, but ONLY while no
/// vendor lane is on and the endpoint is Anthropic's own (or unset). A
/// foreign base serves those model ids, so flooring would break a working
/// lane; Bedrock and Vertex serve Anthropic models under other id shapes.
fn unrouted_model_keys(spawn_env: &Map<String, Value>) -> Vec<&'static str> {
    for flag in ["CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX"] {
        let value = spawn_env
            .get(flag)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !ENV_FALSY_WORDS.contains(&value.as_str()) {
            return Vec::new();
        }
    }
    let base = spawn_env
        .get("ANTHROPIC_BASE_URL")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !base.is_empty() {
        let host = base_url_host(&base);
        if host != "anthropic.com" && !host.ends_with(".anthropic.com") {
            return Vec::new();
        }
    }
    crate::model_env_scrub::MODEL_ENV_KEYS
        .iter()
        .filter(|key| {
            spawn_env
                .get(**key)
                .and_then(Value::as_str)
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
        })
        .copied()
        .collect()
}

/// The composed settings env from the RAW overlays - the port of Python's
/// `route_settings_path_for` compose, so the Python leg is a transport to
/// this door and a fourth seam cannot hand-roll a disagreeing order. The
/// scrub floor first, the account overlay minus its config dir (the config
/// dir selects the config the file is read FROM, so it rides the process
/// env), the route written last so it wins every credential var as one unit,
/// a self-authed route's drop of the account secrets it does not name, and
/// the unrouted-model floor.
pub fn compose_env(
    route_env: &Map<String, Value>,
    account_env: &Map<String, Value>,
    spawn_env: &Map<String, Value>,
) -> Map<String, Value> {
    let mut merged: Map<String, Value> = Map::new();
    for var in SCRUB_AUTH_VARS {
        merged.insert(var.to_string(), Value::String(String::new()));
    }
    for (key, value) in account_env {
        if key.as_str() == "CLAUDE_CONFIG_DIR" {
            continue;
        }
        merged.insert(key.clone(), value.clone());
    }
    for (key, value) in route_env {
        merged.insert(key.clone(), value.clone());
    }
    let self_authed = route_env
        .get("ANTHROPIC_AUTH_TOKEN")
        .or_else(|| route_env.get("ANTHROPIC_API_KEY"))
        .map(|v| !v.as_str().unwrap_or_default().trim().is_empty())
        .unwrap_or(false);
    if self_authed {
        for var in SECRET_ROUTE_VARS {
            if !route_env.contains_key(var) {
                merged.remove(var);
            }
        }
    }
    for key in unrouted_model_keys(spawn_env) {
        merged
            .entry(key.to_string())
            .or_insert_with(|| Value::String(String::new()));
    }
    merged
}

/// A lane alias is a CLI-handle-shaped name: one write per config row, and a
/// stranger name must never turn into a path outside route-settings.
fn valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias != "."
        && alias != ".."
        && !alias.starts_with('.')
        && alias
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// A session-named entry is keyed by the harness session id (a uuid: hex and
/// hyphens), so the name can never escape route-settings and can never
/// collide with the digest or `lane-` namespaces.
fn valid_session(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// The per-process temp name beside `path` that a rename publishes.
fn tmp_beside(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id()
    ))
}

fn write_atomic_0600(path: &Path, bytes: &[u8], overwrite: bool) -> Result<(), String> {
    if !overwrite && path.exists() {
        return Ok(());
    }
    let tmp = tmp_beside(path);
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Write the content-addressed settings file (and the lane alias when asked)
/// under `<state>/route-settings`. Identical payloads collapse to one file;
/// racing writers publish identical bytes, so a last-writer-wins rename stays
/// correct (model_routing.py:_write_settings_env_file). When the ask names a
/// session id, the returned path is the session-named twin
/// `session-<id>.json` instead of the digest name: the twin replaces on a
/// re-route of the same session, the digest twin stays the skip-if-exists
/// dedup store, and the resume reader takes whatever path the row records.
pub fn write(ask: &WriteAsk, cwd: &Path) -> Result<WriteReceipt, String> {
    let state = crate::agents_config::state_dir(cwd)
        .ok_or_else(|| "no resolvable state root (set FNO_STATE_DIR or HOME)".to_string())?;
    // The row's destination, always: a drain row never relies on the legacy
    // answer, so a fresh overlay never lands beside the registry paths that
    // outlive it.
    let base = crate::state_layout::place_dir(&state, "route-settings")
        .ok_or_else(|| "route-settings is not a state-root dir".to_string())?;
    std::fs::create_dir_all(&base).map_err(|e| format!("{}: {e}", base.display()))?;
    let payload = payload_bytes(&ask.env, ask.sandbox.as_ref(), &ask.rules);
    let digest = digest_of(&payload);
    let digest_path = base.join(format!("{digest}.json"));
    write_atomic_0600(&digest_path, &payload, false)?;
    let path = match ask.session.as_deref() {
        Some(id) if !id.is_empty() => {
            if !valid_session(id) {
                return Err(format!("session id {id:?} is not a session handle"));
            }
            let session_path = base.join(format!("session-{id}.json"));
            write_atomic_0600(&session_path, &payload, true)?;
            session_path
        }
        _ => digest_path,
    };
    let alias_path = match &ask.alias {
        Some(name) => {
            if !valid_alias(name) {
                return Err(format!("lane alias {name:?} is not a CLI handle"));
            }
            let alias_path = base.join(format!("lane-{name}.json"));
            write_atomic_0600(&alias_path, &payload, false)?;
            Some(alias_path)
        }
        None => None,
    };
    Ok(WriteReceipt { path, alias_path })
}

/// The spawn-mint's join. The settings file is written before the session
/// uuid exists (the claude daemon mints the id at first launch and the mint
/// resolves it by polling afterward), so the mint publishes the session-named
/// twin once the id lands: a hardlink beside the recorded file, a copy where
/// the filesystem refuses the link. A stale twin from an earlier publish of
/// the same id is replaced, never trusted. io::Result keeps this helper off
/// the crate's string-error ratchet; the caller maps the error.
pub fn publish_session_entry(recorded: &Path, session_id: &str) -> std::io::Result<PathBuf> {
    if !valid_session(session_id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("session id {session_id:?} is not a session handle"),
        ));
    }
    let dest = recorded.with_file_name(format!("session-{session_id}.json"));
    if dest == recorded {
        return Ok(dest);
    }
    let tmp = tmp_beside(&dest);
    std::fs::remove_file(&tmp).ok();
    if std::fs::hard_link(recorded, &tmp).is_ok() {
        std::fs::rename(&tmp, &dest)?;
    } else {
        let bytes = std::fs::read(recorded)?;
        write_atomic_0600(&dest, &bytes, true).map_err(std::io::Error::other)?;
    }
    Ok(dest)
}

/// The late-uuid join for a row the mint recorded before its session id
/// resolved: publish the twin when `recorded` is a route-writer file, and
/// return the path the row should now record. A caller's own `--settings`
/// file (outside route-settings) is never twinned.
pub fn publish_for_row(recorded: &str, session_id: &str) -> Option<String> {
    let recorded = Path::new(recorded);
    let owned = recorded
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|d| d == "route-settings");
    if !owned {
        return None;
    }
    match publish_session_entry(recorded, session_id) {
        Ok(path) => Some(path.to_string_lossy().into_owned()),
        Err(e) => {
            eprintln!("fno agent: warning: route-settings session twin for {session_id}: {e}");
            None
        }
    }
}

/// Compose from the RAW overlays and write, the one helper behind both
/// doors (the native exec and the print kind). `Ok(None)` when the ask
/// names neither overlay nor sandbox: a plain lane launch carries no
/// settings file, but a write policy alone still owes the scrub floor. A
/// non-empty `session` names the returned file `session-<id>.json` (the
/// resume reader replays the recorded path verbatim). `node` is the child's
/// node from the spawn payload, never the spawner's own env.
pub fn write_composed(
    route_env: &Map<String, Value>,
    account_env: &Map<String, Value>,
    sandbox: Option<Value>,
    alias: Option<&str>,
    session: Option<&str>,
    cwd: &Path,
    node: Option<&str>,
    role: Option<&str>,
) -> Result<Option<String>, String> {
    let rules = crate::spawn::auto_approve::rules_for(cwd, node, role);
    let Some(env) = env_for_write(route_env, account_env, sandbox.is_some(), !rules.is_empty())
    else {
        return Ok(None);
    };
    let receipt = write(
        &WriteAsk {
            env,
            sandbox,
            alias: alias.map(str::to_string),
            session: session.map(str::to_string),
            rules,
        },
        cwd,
    )
    .map_err(|e| format!("lane settings: {e}"))?;
    Ok(Some(receipt.path.to_string_lossy().into_owned()))
}

/// The env a composed settings file carries, or `None` when the ask owes no
/// file. Rules alone owe no scrub floor: a plain lane keeps its own auth env.
fn env_for_write(
    route_env: &Map<String, Value>,
    account_env: &Map<String, Value>,
    has_sandbox: bool,
    has_rules: bool,
) -> Option<Map<String, Value>> {
    let plain = route_env.is_empty() && account_env.is_empty() && !has_sandbox;
    if plain {
        return has_rules.then(Map::new);
    }
    let spawn_env: Map<String, Value> = std::env::vars()
        .map(|(k, v)| (k, Value::String(v)))
        .collect();
    Some(compose_env(route_env, account_env, &spawn_env))
}

/// The write kind behind the spawn-overlay action (law d-fe66560a allows no
/// new action): Python's forwarders send `{"kind": "write-settings", ...}`.
/// Two ask shapes: `{"env", ...}` is a pre-composed env (the revive
/// readers), and `{"route_env", "account_env", ...}` names the RAW overlays
/// for the door's own compose. The transport is the contract Python's
/// forwarder holds, so a refusal is a loud error, never a silent fallback to
/// a Python write leg.
pub fn write_settings_kind(payload: &Value) -> Result<Value, String> {
    let string_object = |field: &str| -> Result<Option<Map<String, Value>>, String> {
        let obj = match payload.get(field).and_then(Value::as_object) {
            Some(obj) => obj,
            None => return Ok(None),
        };
        if obj.values().all(Value::is_string) {
            Ok(Some(obj.clone()))
        } else {
            Err(format!(
                "route-settings: {field} carries a non-string value"
            ))
        }
    };
    let env = if payload.get("route_env").is_some() || payload.get("account_env").is_some() {
        let route_env = string_object("route_env")?;
        let account_env = string_object("account_env")?;
        let (Some(route_env), Some(account_env)) = (route_env, account_env) else {
            return Err(
                "route-settings: the overlay compose needs route_env and account_env objects"
                    .into(),
            );
        };
        let spawn_env: Map<String, Value> = std::env::vars()
            .map(|(k, v)| (k, Value::String(v)))
            .collect();
        compose_env(&route_env, &account_env, &spawn_env)
    } else {
        match string_object("env")? {
            Some(env) => env,
            None => {
                return Err("route-settings: payload carries no env or overlay object".into());
            }
        }
    };
    let ask = WriteAsk {
        env,
        sandbox: payload.get("sandbox").filter(|v| v.is_object()).cloned(),
        alias: payload
            .get("alias")
            .and_then(Value::as_str)
            .map(str::to_string),
        // The transport contract carries no session id today; a fresh
        // compose that knows one goes through write_composed.
        session: None,
        rules: Default::default(),
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let receipt = write(&ask, &cwd)?;
    Ok(json!({
        "path": receipt.path.to_string_lossy(),
        "alias_path": receipt
            .alias_path
            .map(|p| p.to_string_lossy().into_owned()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The writer resolves its state root from the env (FNO_STATE_DIR, then
    /// config, then $HOME/.fno); a test that writes pins it under the claims
    /// env lock, so the sandbox never touches the real state root.
    fn state_pin(
        root: &Path,
    ) -> (
        std::sync::MutexGuard<'static, ()>,
        Option<std::ffi::OsString>,
    ) {
        let guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os("FNO_STATE_DIR");
        std::env::set_var("FNO_STATE_DIR", root);
        (guard, saved)
    }

    fn state_restore(saved: Option<std::ffi::OsString>) {
        match saved {
            Some(v) => std::env::set_var("FNO_STATE_DIR", v),
            None => std::env::remove_var("FNO_STATE_DIR"),
        }
    }

    fn env_of(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect()
    }

    /// The whole-host table the Python side pinned as
    /// test_deny_matches_the_whole_host: a substring test got both ends wrong.
    #[test]
    fn the_deny_decision_parses_the_whole_host() {
        let rows: Vec<(&str, bool)> = vec![
            ("https://api.anthropic.com", false),
            ("https://api.anthropic.com/v1", false),
            ("HTTPS://API.ANTHROPIC.COM", false),
            ("api.anthropic.com", false),
            ("https://api.anthropic.com:443", false),
            // A host that merely STARTS with Anthropic's is somebody else's.
            ("https://api.anthropic.com.proxy.example", true),
            ("https://evil.example/api.anthropic.com", true),
            ("https://api.z.ai/api/anthropic", true),
            ("", false),
        ];
        for (base_url, denied) in rows {
            let got = foreign_endpoint_tool_denies(&env_of(&[("ANTHROPIC_BASE_URL", base_url)]));
            assert_eq!(got.is_empty(), !denied, "{base_url}");
        }
    }

    #[test]
    fn rules_ride_the_settings_file_and_a_plain_lane_keeps_its_own_auth_env() {
        let rules = crate::spawn::auto_approve::Rules {
            allow: vec!["Read(//w/**)".into()],
            ask: vec!["Edit(//w/**/.git)".into()],
        };
        let bytes = payload_bytes(&Map::new(), None, &rules);
        let doc: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            doc.pointer("/permissions/allow"),
            Some(&json!(["Read(//w/**)"]))
        );
        assert_eq!(
            doc.pointer("/permissions/ask"),
            Some(&json!(["Edit(//w/**/.git)"]))
        );
        let none = Map::new();
        let env = env_for_write(&none, &none, false, true).expect("rules alone owe a file");
        assert!(env.is_empty(), "no scrub floor on a plain lane");
        assert!(
            env_for_write(&none, &none, false, false).is_none(),
            "nothing to write"
        );
        let route = env_of(&[("ANTHROPIC_MODEL", "glm-5.2")]);
        assert!(env_for_write(&route, &none, false, false).is_some());
        let root = tempfile::tempdir().unwrap();
        let (guard, saved) = state_pin(root.path());
        let ask = WriteAsk {
            env,
            sandbox: None,
            alias: None,
            session: None,
            rules,
        };
        let path = write(&ask, root.path()).unwrap().path;
        let doc: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(doc.pointer("/env"), Some(&json!({})));
        assert_eq!(
            doc.pointer("/permissions/ask"),
            Some(&json!(["Edit(//w/**/.git)"]))
        );
        state_restore(saved);
        drop(guard);
    }

    #[test]
    fn the_alias_lands_at_lane_name_and_refuses_a_stranger() {
        let root = tempfile::tempdir().unwrap();
        let (guard, saved) = state_pin(root.path());
        let ask = WriteAsk {
            env: env_of(&[("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic")]),
            sandbox: None,
            alias: Some("zai-flash".to_string()),
            session: None,
            rules: Default::default(),
        };
        let receipt = write(&ask, root.path()).unwrap();
        let alias = receipt.alias_path.unwrap();
        assert!(alias.ends_with("lane-zai-flash.json"));
        assert_eq!(
            std::fs::read(&alias).unwrap(),
            std::fs::read(&receipt.path).unwrap()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&alias).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let bad = WriteAsk {
            alias: Some("../escape".to_string()),
            ..ask
        };
        assert!(write(&bad, root.path()).is_err());
        state_restore(saved);
        drop(guard);
    }

    /// The compose from raw overlays: the scrub floor, the account overlay
    /// minus its config dir, the route winning the credential slot, and the
    /// self-authed drop of the account secret the route does not name.
    #[test]
    fn the_compose_floors_overlays_and_drops_the_foreign_secret() {
        let route_env = env_of(&[
            ("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic"),
            ("ANTHROPIC_AUTH_TOKEN", "zk-route"),
            ("ANTHROPIC_MODEL", "glm-5.2"),
        ]);
        let account_env = env_of(&[
            ("ANTHROPIC_API_KEY", "acct-key"),
            ("CLAUDE_CONFIG_DIR", "/accounts/ccm"),
            ("ANTHROPIC_LOG_LEVEL", "debug"),
        ]);
        let spawn_env = env_of(&[("ANTHROPIC_MODEL", "claude-opus-5-5")]);
        let merged = compose_env(&route_env, &account_env, &spawn_env);
        let as_str = |k: &str| merged.get(k).and_then(Value::as_str).expect(k).to_string();
        // The route wins the credential slot; the account secret a
        // self-authed route does not name is dropped, and the config dir
        // never rides the file.
        assert_eq!(
            as_str("ANTHROPIC_BASE_URL"),
            "https://api.z.ai/api/anthropic"
        );
        assert_eq!(as_str("ANTHROPIC_AUTH_TOKEN"), "zk-route");
        assert!(!merged.contains_key("ANTHROPIC_API_KEY"));
        assert!(!merged.contains_key("CLAUDE_CONFIG_DIR"));
        assert_eq!(as_str("ANTHROPIC_LOG_LEVEL"), "debug");
        assert_eq!(as_str("ANTHROPIC_MODEL"), "glm-5.2");
        // The floor covers every scrub var; a key the overlays do not name
        // stays at the floor's empty string.
        assert_eq!(as_str("CLAUDE_CODE_OAUTH_TOKEN"), "");
        assert_eq!(as_str("FNO_ROUTE_PROVIDER"), "");
        assert_eq!(as_str("ANTHROPIC_DEFAULT_FABLE_MODEL"), "");
    }

    const SESSION_ID: &str = "3e7f0a47-3777-459d-9e8a-6304803bd2d1";

    /// The session join: a write that names a session returns the
    /// session-named path, lands the digest twin beside it with identical
    /// bytes, and stamps 0600 on the twin.
    #[test]
    fn the_session_entry_lands_beside_its_digest_twin() {
        let root = tempfile::tempdir().unwrap();
        let (guard, saved) = state_pin(root.path());
        let route_env = env_of(&[("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic")]);
        let path = write_composed(
            &route_env,
            &Map::new(),
            None,
            None,
            Some(SESSION_ID),
            root.path(),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        let path = PathBuf::from(&path);
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            format!("session-{SESSION_ID}.json")
        );
        let digest_twin = path
            .parent()
            .unwrap()
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|n| n.ends_with(".json") && !n.starts_with("session-"))
            .expect("digest twin present");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            std::fs::read(path.parent().unwrap().join(digest_twin)).unwrap()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        state_restore(saved);
        drop(guard);
    }

    /// A re-route of the same session replaces the twin with the new route;
    /// the digest store keeps one file per payload.
    #[test]
    fn a_session_reroute_replaces_the_entry() {
        let root = tempfile::tempdir().unwrap();
        let (guard, saved) = state_pin(root.path());
        let first = env_of(&[("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic")]);
        let second = env_of(&[("ANTHROPIC_BASE_URL", "https://api.anthropic.com")]);
        let first_path = write_composed(
            &first,
            &Map::new(),
            None,
            None,
            Some(SESSION_ID),
            root.path(),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        let path = write_composed(
            &second,
            &Map::new(),
            None,
            None,
            Some(SESSION_ID),
            root.path(),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(first_path, path);
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("api.anthropic.com"));
        assert!(!body.contains("api.z.ai"));
        state_restore(saved);
        drop(guard);
    }

    /// The mint-side publish: a stranger id refuses, and a stale twin from an
    /// earlier publish of the same id is replaced, never trusted. Writes land
    /// beside `recorded`, so no state-root pin applies.
    #[test]
    fn publish_session_entry_refuses_a_stranger_and_replaces_a_stale_twin() {
        let dir = tempfile::tempdir().unwrap();
        let recorded = dir.path().join("deadbeef01234567.json");
        std::fs::write(&recorded, b"first").unwrap();
        assert!(publish_session_entry(&recorded, "../escape").is_err());
        assert!(publish_session_entry(&recorded, "short").is_err());
        assert!(!dir
            .path()
            .join(format!("session-{SESSION_ID}.json"))
            .exists());
        let dest = publish_session_entry(&recorded, SESSION_ID).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"first");
        std::fs::write(&recorded, b"second").unwrap();
        let dest = publish_session_entry(&recorded, SESSION_ID).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"second");
    }
}
