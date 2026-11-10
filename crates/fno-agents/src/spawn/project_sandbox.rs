//! `fno config project sandbox`: one named sandbox holds its own fno state
//! root, mux dir, claude config dir and codex home, so a demo or a
//! fresh-install test logs in once, wipes fno state between runs, and never
//! sees or touches the live fleet. The full contract lives in
//! docs/architecture/fno-project-environments.md ("Sandbox: both layers").
//!
//! Route: a sandbox `HOME` (route 1). Every HOME-rooted reader is sandboxed
//! by construction. Two create-time links keep the real-home tools working:
//! `home/Library/Keychains` (macOS resolves the default keychain through
//! `$HOME/Library/Keychains`; without the link no login is reachable) and
//! `home/.ssh`. The env table carries the tool roots (`GIT_CONFIG_GLOBAL`,
//! `GH_CONFIG_DIR`, `CARGO_HOME`, `RUSTUP_HOME`) and unsets every inherited
//! `ANTHROPIC_*` key next to the identity keys: a parent
//! `ANTHROPIC_AUTH_TOKEN` reads `loggedIn: true` in a scratch config dir.

use std::io::IsTerminal;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where sandbox roots live (`~` expands against `$HOME`). Under the
/// baseline-named `state/` folder: the state-root row gate is shrink-only
/// and refuses a new top-level folder.
const SANDBOXES_ROOT: &str = "~/.fno/state/sandboxes";

/// One stored sandbox. `root` is canonicalized once at create so the claude
/// Keychain hash (sha256 over the config dir path string) never drifts.
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct Manifest {
    pub name: String,
    pub root: PathBuf,
    pub created_at: String,
    pub layout_version: i64,
    /// The invoking HOME at create; the tool carries point here so they
    /// survive being read from inside the sandbox shell.
    pub real_home: PathBuf,
}

fn expand_tilde(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => PathBuf::from(path),
        },
        None => PathBuf::from(path),
    }
}

fn sandboxes_root() -> PathBuf {
    expand_tilde(SANDBOXES_ROOT)
}

pub(crate) fn root_for(name: &str) -> PathBuf {
    sandboxes_root().join(name)
}

/// Sandbox names are one path segment of a stored root: lowercase
/// alphanumeric lead, then alphanumerics or dashes, at most 32 chars.
pub(crate) fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub(crate) fn read_manifest(root: &Path) -> Option<Manifest> {
    let body = std::fs::read_to_string(root.join("sandbox.toml")).ok()?;
    toml::from_str::<Manifest>(&body).ok()
}

fn write_manifest(manifest: &Manifest) -> std::io::Result<()> {
    let mut doc = toml_edit::DocumentMut::new();
    doc.insert("name", toml_edit::value(manifest.name.as_str()));
    doc.insert(
        "root",
        toml_edit::value(manifest.root.to_string_lossy().as_ref()),
    );
    doc.insert("created_at", toml_edit::value(manifest.created_at.as_str()));
    doc.insert("layout_version", toml_edit::value(manifest.layout_version));
    doc.insert(
        "real_home",
        toml_edit::value(manifest.real_home.to_string_lossy().as_ref()),
    );
    std::fs::write(manifest.root.join("sandbox.toml"), doc.to_string())
}

/// The sandbox env table: (sets, unsets). One function serves `shell`, `env`
/// and `status`, so a shell and its printed env can never disagree. The
/// unsets enumerate the caller's own `ANTHROPIC_*` keys at call time: the
/// family is open-ended (new exporter keys appear every few versions), and a
/// parent token here reads as a live login in a scratch config dir.
pub(crate) fn env_table(manifest: &Manifest) -> (Vec<(String, String)>, Vec<String>) {
    let root = &manifest.root;
    let home = root.join("home");
    let real = &manifest.real_home;
    let carry = |sub: &str| {
        let path = real.join(sub);
        path.exists()
            .then(|| (sub_carrier(sub), path.to_string_lossy().into_owned()))
    };
    let mut sets = vec![
        (
            "FNO_CONFIG".to_string(),
            root.join("fno/config.toml").to_string_lossy().into_owned(),
        ),
        (
            "FNO_MUX_DIR".to_string(),
            root.join("fno/mux").to_string_lossy().into_owned(),
        ),
        (
            "FNO_AGENTS_HOME".to_string(),
            root.join("agents").to_string_lossy().into_owned(),
        ),
        (
            "FNO_CLAIMS_ROOT".to_string(),
            root.join("claims").to_string_lossy().into_owned(),
        ),
        (
            "FNO_SPACES_DIR".to_string(),
            root.join("fno/spaces").to_string_lossy().into_owned(),
        ),
        (
            "CLAUDE_CONFIG_DIR".to_string(),
            home.join(".claude").to_string_lossy().into_owned(),
        ),
        (
            "CODEX_HOME".to_string(),
            home.join(".codex").to_string_lossy().into_owned(),
        ),
        ("FNO_SANDBOX".to_string(), manifest.name.clone()),
        ("HOME".to_string(), home.to_string_lossy().into_owned()),
    ];
    for sub in [".gitconfig", ".config/gh", ".cargo", ".rustup"] {
        if let Some((carrier, path)) = carry(sub) {
            sets.push((carrier.to_string(), path));
        }
    }
    for key in ["USER", "LOGNAME"] {
        if let Ok(value) = std::env::var(key) {
            sets.push((key.to_string(), value));
        }
    }
    sets.sort();
    sets.dedup();
    let mut unsets: Vec<String> = UNSET_KEYS.iter().map(|key| key.to_string()).collect();
    let inherited = std::env::vars()
        .map(|(key, _)| key)
        .filter(|key| key.starts_with("ANTHROPIC_"));
    unsets.extend(inherited);
    unsets.sort();
    unsets.dedup();
    (sets, unsets)
}

/// The identity keys `scripts/ops/mux-demo-snapshot.sh` unsets, plus the
/// owner-pid family, the auth-scoped keychain override, the OAuth token, and
/// the roster override. An inherited one stamps the live session into the
/// sandbox store or points the sandbox at the live login.
const UNSET_KEYS: [&str; 28] = [
    "FNO_SERVER",
    "FNO_SESSION",
    "FNO_PANE",
    "FNO_OWNER_PID",
    "FNO_OWNER_BIRTH",
    "FNO_OWNER_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_SESSION_ID",
    "CLAUDECODE",
    "CLAUDECODE_SESSION_ID",
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "CODEX_COMPANION_SESSION_ID",
    "CODEX_COMPANION_TRANSCRIPT_PATH",
    "GEMINI_SESSION_ID",
    "OPENCODE_SESSION_ID",
    "PI_SESSION_ID",
    "PI_SESSION_FILE",
    "HERMES_SESSION_ID",
    "TARGET_SESSION_ID",
    "FNO_HARNESS_NAME",
    "FNO_HARNESS_SESSION_ID",
    "REX_SERVER",
    "REX_SESSION",
    "REX_BLOCK",
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_DIR_OVERRIDE",
];

fn sub_carrier(sub: &str) -> &'static str {
    match sub {
        ".gitconfig" => "GIT_CONFIG_GLOBAL",
        ".config/gh" => "GH_CONFIG_DIR",
        ".cargo" => "CARGO_HOME",
        ".rustup" => "RUSTUP_HOME",
        _ => "",
    }
}

/// Entry for the `fno config project sandbox <action>` surface.
pub fn run(args: &[String]) -> i32 {
    match args.split_first() {
        Some((action, rest)) if action == "create" => cmd_create(rest),
        Some((action, rest)) if action == "shell" => cmd_shell(rest),
        Some((action, rest)) if action == "env" => cmd_env(rest),
        Some((action, rest)) if action == "status" => cmd_status(rest),
        Some((action, rest)) if action == "reset" => cmd_reset(rest),
        Some((action, rest)) if action == "rm" => cmd_rm(rest),
        Some((action, rest)) if action == "list" => cmd_list(rest),
        _ => usage(),
    }
}

fn usage() -> i32 {
    eprintln!("usage: fno config project sandbox <create|shell|env|status|reset|rm|list> [<name>]");
    2
}

/// RED pending: reset is still stubbed until its failing test is green.
fn cmd_create(args: &[String]) -> i32 {
    let Some((Some(name), _)) = parse_name_flags(args) else {
        return usage();
    };
    let Some(real_home_os) = std::env::var_os("HOME") else {
        eprintln!("refusing: HOME is unset, so no sandbox root can be placed");
        return 2;
    };
    let real_home =
        std::fs::canonicalize(&real_home_os).unwrap_or_else(|_| PathBuf::from(&real_home_os));
    let keychains = real_home.join("Library/Keychains");
    if !keychains.is_dir() {
        eprintln!(
            "refusing: {} is missing. The sandbox login needs the real keychain dir.",
            keychains.display()
        );
        return 2;
    }
    if !valid_name(&name) {
        eprintln!(
            "refusing: '{name}' is not a sandbox name (lowercase letters, digits and dashes, at most 32 chars)"
        );
        return 2;
    }
    let root = root_for(&name);
    if root.exists() {
        eprintln!(
            "refusing: {} already holds a sandbox. rm it first or pick another name.",
            root.display()
        );
        return 2;
    }
    let home = root.join("home");
    for dir in [
        "fno",
        "agents",
        "claims/.fno/claims",
        "home/.claude",
        "home/.codex",
        "home/Library",
    ] {
        if let Err(error) = std::fs::create_dir_all(root.join(dir)) {
            eprintln!("fno config project sandbox create: {error}");
            return 1;
        }
    }
    if let Err(error) = write_fresh_config(&root) {
        eprintln!("fno config project sandbox create: {error}");
        return 1;
    }
    if std::fs::write(
        home.join(".codex/config.toml"),
        "cli_auth_credentials_store = \"file\"\n",
    )
    .is_err()
    {
        eprintln!("fno config project sandbox create: could not write the codex pin");
        return 1;
    }
    use std::os::unix::fs::symlink;
    if symlink(&keychains, home.join("Library/Keychains")).is_err() {
        eprintln!("fno config project sandbox create: could not link the keychain dir");
        return 1;
    }
    let ssh = real_home.join(".ssh");
    if ssh.is_dir() && symlink(&ssh, home.join(".ssh")).is_err() {
        eprintln!("fno config project sandbox create: could not link the ssh dir");
        return 1;
    }
    let stored_root = std::fs::canonicalize(&root).unwrap_or(root.clone());
    let manifest = Manifest {
        name,
        root: stored_root,
        created_at: chrono::Utc::now().to_rfc3339(),
        layout_version: 1,
        real_home,
    };
    if let Err(error) = write_manifest(&manifest) {
        eprintln!("fno config project sandbox create: {error}");
        return 1;
    }
    if std::io::stdin().is_terminal() {
        run_login_if_needed(&manifest);
    }
    println!("sandbox {}: created", manifest.name);
    println!("  root {}", manifest.root.display());
    println!("  env    fno config project sandbox env {}", manifest.name);
    if let Ok(item) = crate::claude_vault::scoped_service(&claude_dir_of(&manifest)) {
        println!("  keychain item {item} (written on first login)");
    }
    0
}

/// The fresh empty state config both create and reset lay down.
fn write_fresh_config(root: &Path) -> std::io::Result<()> {
    let state_dir = root.join("fno");
    std::fs::write(
        root.join("fno/config.toml"),
        format!(
            "schema_version = 1\nstate_dir = '{}'\n",
            state_dir.display()
        ),
    )
}

/// One login per harness, only for what is missing, under the sandbox env.
/// Runs from create on a TTY: the person signs in once and types no command.
fn run_login_if_needed(manifest: &Manifest) {
    let claude_missing = crate::claude_vault::scoped_service(&claude_dir_of(manifest))
        .map(|item| !keychain_item_exists(&item))
        .unwrap_or(true);
    if claude_missing {
        let mut cmd = Command::new("claude");
        cmd.arg("auth").arg("login");
        apply_env(&mut cmd, manifest);
        let _ = cmd.status();
    }
    if !codex_login_pinned(&manifest.root.join("home/.codex")) {
        let mut cmd = Command::new("codex");
        cmd.arg("login");
        apply_env(&mut cmd, manifest);
        let _ = cmd.status();
    }
}

fn cmd_reset(args: &[String]) -> i32 {
    let Some((Some(name), _)) = parse_name_flags(args) else {
        return usage();
    };
    let Ok(manifest) = load_manifest(&name) else {
        return 2;
    };
    // Refuse while a sandbox session is live, and name it.
    let live = processes_with_root(&manifest.root);
    if !live.is_empty() {
        let pids = live
            .iter()
            .map(|pid| pid.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!("refusing: live sandbox process(es) {pids} - stop them first");
        return 2;
    }
    stop_sandbox_processes(&manifest);
    for dir in ["fno", "agents", "claims"] {
        match std::fs::remove_dir_all(manifest.root.join(dir)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                eprintln!("fno config project sandbox reset: {error}");
                return 1;
            }
        }
        if let Err(error) = std::fs::create_dir_all(manifest.root.join(dir)) {
            eprintln!("fno config project sandbox reset: {error}");
            return 1;
        }
    }
    if let Err(error) = write_fresh_config(&manifest.root) {
        eprintln!("fno config project sandbox reset: {error}");
        return 1;
    }
    if let Err(error) = std::fs::create_dir_all(manifest.root.join("claims/.fno/claims")) {
        eprintln!("fno config project sandbox reset: {error}");
        return 1;
    }
    println!("sandbox {}: reset (harness homes untouched)", manifest.name);
    0
}

/// One optional positional name plus boolean flags.
/// One optional positional name plus boolean flags. Prints its own refusal
/// and answers `None` on a malformed argv; callers answer with their exit.
fn parse_name_flags(args: &[String]) -> Option<(Option<String>, bool)> {
    let mut name: Option<String> = None;
    let mut json = false;
    for arg in args {
        if arg == "--json" {
            json = true;
        } else if arg.starts_with('-') {
            eprintln!("fno config project sandbox: unknown flag {arg}");
            return None;
        } else if name.is_none() {
            name = Some(arg.clone());
        } else {
            eprintln!("fno config project sandbox: unexpected argument {arg}");
            return None;
        }
    }
    Some((name, json))
}

fn load_manifest(name: &str) -> Result<Manifest, i32> {
    let root = root_for(name);
    match read_manifest(&root) {
        Some(manifest) => Ok(manifest),
        None => {
            eprintln!(
                "fno config project sandbox: no stored sandbox at {} (create it first)",
                root.display()
            );
            Err(2)
        }
    }
}

/// Apply the table to a spawned command. The `ANTHROPIC_*` family is
/// removed from the parent env explicitly: the unsets list was computed for
/// a possibly different caller env.
fn apply_env(cmd: &mut Command, manifest: &Manifest) {
    let (sets, unsets) = env_table(manifest);
    for (key, value) in sets {
        cmd.env(key, value);
    }
    for key in unsets {
        cmd.env_remove(key);
    }
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("ANTHROPIC_")) {
        cmd.env_remove(key);
    }
}

fn cmd_shell(args: &[String]) -> i32 {
    let Some((Some(name), _)) = parse_name_flags(args) else {
        return usage();
    };
    let Ok(manifest) = load_manifest(&name) else {
        return 2;
    };
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let mut cmd = Command::new(&shell);
    cmd.arg("-l");
    apply_env(&mut cmd, &manifest);
    let error = cmd.exec();
    eprintln!("fno config project sandbox shell: exec {shell} failed: {error}");
    2
}

/// Shell-quote one value for an `export` line.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn cmd_env(args: &[String]) -> i32 {
    let Some((Some(name), json)) = parse_name_flags(args) else {
        return usage();
    };
    let Ok(manifest) = load_manifest(&name) else {
        return 2;
    };
    let (sets, unsets) = env_table(&manifest);
    if json {
        let mut set = serde_json::Map::new();
        for (key, value) in &sets {
            set.insert(key.clone(), serde_json::Value::String(value.clone()));
        }
        let out = serde_json::json!({
            "name": manifest.name,
            "root": manifest.root.display().to_string(),
            "set": set,
            "unset": unsets,
        });
        println!("{out}");
        return 0;
    }
    for (key, value) in &sets {
        println!("export {}={}", key, shell_quote(value));
    }
    for key in &unsets {
        println!("unset {key}");
    }
    0
}

/// The claude config dir of a stored sandbox.
fn claude_dir_of(manifest: &Manifest) -> PathBuf {
    manifest.root.join("home/.claude")
}

/// The 8-hex daemon-dir suffix claude derives from the config dir path (the
/// same sha256[..8] the scoped Keychain item name uses). Reuses
/// `claude_vault::scoped_service` so fno adds no second hash.
fn daemon_hash(manifest: &Manifest) -> Option<String> {
    crate::claude_vault::scoped_service(&claude_dir_of(manifest))
        .ok()
        .and_then(|service| service.rsplit('-').next().map(str::to_string))
}

fn daemon_dir_of(manifest: &Manifest) -> Option<PathBuf> {
    let hash = daemon_hash(manifest)?;
    Some(
        std::env::temp_dir()
            .join(format!("cc-daemon-{}", unsafe { libc::geteuid() }))
            .join(hash),
    )
}

/// Cheap presence checks for `list`: the scoped Keychain item and the codex
/// auth file. The live probe stays in `status`, where one sandbox at a time
/// pays the ~5s.
fn keychain_item_exists(service: &str) -> bool {
    let mut cmd = Command::new("security");
    cmd.args(["find-generic-password", "-s", service]);
    crate::bounded_cmd::output_with_timeout_result(cmd, 5)
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn codex_login_pinned(codex_home: &Path) -> bool {
    codex_home.join("auth.json").is_file()
}

fn processes_with_root(root: &Path) -> Vec<u32> {
    let mut cmd = Command::new("pgrep");
    cmd.arg("-f").arg(root.to_string_lossy().as_ref());
    crate::bounded_cmd::output_with_timeout_result(cmd, 5)
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| line.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn codex_login_status(codex_home: &Path) -> &'static str {
    let mut cmd = Command::new("codex");
    cmd.args(["login", "status"]).env("CODEX_HOME", codex_home);
    for name in crate::accounts::store::CODEX_AUTH_ENV_VARS {
        cmd.env_remove(name);
    }
    match crate::bounded_cmd::output_with_timeout_result(cmd, 10) {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            if text.contains("Not logged in") {
                "logged_out"
            } else if out.status.success() {
                "logged_in"
            } else {
                "unknown"
            }
        }
        Err(_) => "unknown",
    }
}

fn cmd_status(args: &[String]) -> i32 {
    let Some((Some(name), json)) = parse_name_flags(args) else {
        return usage();
    };
    let Ok(manifest) = load_manifest(&name) else {
        return 2;
    };
    let claude = match crate::claude_login::probe_config_dir(&claude_dir_of(&manifest)) {
        crate::claude_login::Login::LoggedIn => "logged_in",
        crate::claude_login::Login::LoggedOut(_) => "logged_out",
        crate::claude_login::Login::Unknown(_) => "unknown",
    };
    let codex = codex_login_status(&manifest.root.join("home/.codex"));
    let daemon_dir = daemon_dir_of(&manifest);
    let daemon = daemon_dir
        .as_ref()
        .map(|dir| {
            if dir.is_dir() {
                processes_with_root(dir).len()
            } else {
                0
            }
        })
        .unwrap_or(0);
    let mux_dir = manifest.root.join("fno/mux");
    let sockets = std::fs::read_dir(&mux_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "sock"))
                .count()
        })
        .unwrap_or(0);
    let live = processes_with_root(&manifest.root).len();
    if json {
        let out = serde_json::json!({
            "name": manifest.name,
            "root": manifest.root.display().to_string(),
            "claude": claude,
            "codex": codex,
            "daemon_pids": daemon,
            "mux_sockets": sockets,
            "live_processes": live,
        });
        println!("{out}");
        return 0;
    }
    println!("name: {}", manifest.name);
    println!("root: {}", manifest.root.display());
    println!("claude: {claude}");
    println!("codex: {codex}");
    println!(
        "claude daemon dir: {} ({daemon} live)",
        daemon_dir.map_or_else(|| "unresolvable".to_string(), |d| d.display().to_string())
    );
    println!("mux sockets: {sockets}");
    println!("live processes: {live}");
    0
}

/// Stop the sandbox's own processes: the claude supervisor (scoped by the
/// config dir, per the measured `daemon stop` semantics) and the mux server
/// under the sandbox env. The live fleet's supervisor is never addressed:
/// every stop command runs with the sandbox env applied, so claude resolves
/// the sandbox daemon dir.
fn stop_sandbox_processes(manifest: &Manifest) {
    if daemon_dir_of(manifest).is_some_and(|dir| dir.is_dir()) {
        let mut cmd = Command::new("claude");
        cmd.args(["daemon", "stop", "--any"]);
        apply_env(&mut cmd, manifest);
        let _ = crate::bounded_cmd::output_with_timeout_result(cmd, 15);
    }
    let mut cmd = Command::new("fno");
    cmd.args(["mux", "kill-server"]);
    apply_env(&mut cmd, manifest);
    let _ = crate::bounded_cmd::output_with_timeout_result(cmd, 15);
}

fn cmd_rm(args: &[String]) -> i32 {
    let Some((Some(name), _)) = parse_name_flags(args) else {
        return usage();
    };
    let Ok(manifest) = load_manifest(&name) else {
        return 2;
    };
    stop_sandbox_processes(&manifest);
    if let Ok(item) = crate::claude_vault::scoped_service(&claude_dir_of(&manifest)) {
        let mut cmd = Command::new("security");
        cmd.args(["delete-generic-password", "-s", &item]);
        let _ = crate::bounded_cmd::output_with_timeout_result(cmd, 5);
    }
    if let Err(error) = std::fs::remove_dir_all(&manifest.root) {
        eprintln!(
            "fno config project sandbox rm: could not remove {}: {error}",
            manifest.root.display()
        );
        return 1;
    }
    println!("removed {}", manifest.root.display());
    0
}

fn cmd_list(args: &[String]) -> i32 {
    let Some((None, json)) = parse_name_flags(args) else {
        return usage();
    };
    let mut rows = Vec::new();
    if let Ok(entries) = std::fs::read_dir(sandboxes_root()) {
        for entry in entries.flatten() {
            let Some(manifest) = read_manifest(&entry.path()) else {
                continue;
            };
            let claude = crate::claude_vault::scoped_service(&claude_dir_of(&manifest))
                .map(|service| keychain_item_exists(&service))
                .unwrap_or(false);
            let row = serde_json::json!({
                "name": manifest.name,
                "root": manifest.root.display().to_string(),
                "claude": claude,
                "codex": codex_login_pinned(&manifest.root.join("home/.codex")),
                "live": processes_with_root(&manifest.root).len(),
            });
            rows.push(row);
        }
    }
    if json {
        println!("{}", serde_json::Value::Array(rows));
        return 0;
    }
    for row in &rows {
        println!(
            "{}  claude={} codex={} live={}  {}",
            row["name"].as_str().unwrap_or("?"),
            row["claude"]
                .as_bool()
                .map_or("?", |b| if b { "yes" } else { "no" }),
            row["codex"]
                .as_bool()
                .map_or("?", |b| if b { "yes" } else { "no" }),
            row["live"].as_u64().unwrap_or(0),
            row["root"].as_str().unwrap_or("?"),
        );
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn set_home(home: &Path) -> Option<std::ffi::OsString> {
        let old = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        old
    }

    fn restore_env(key: &str, old: Option<std::ffi::OsString>) {
        match old {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    /// A temp HOME holding every real-home input create links or carries:
    /// the keychain dir (its absence refuses create), ssh, and the tool
    /// carry sources.
    fn fake_real_home(home: &Path) {
        std::fs::create_dir_all(home.join("Library/Keychains")).unwrap();
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::write(home.join(".gitconfig"), "[user]\n").unwrap();
        std::fs::create_dir_all(home.join(".config/gh")).unwrap();
        std::fs::create_dir_all(home.join(".cargo")).unwrap();
        std::fs::create_dir_all(home.join(".rustup")).unwrap();
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    // Create is the write door: a traversal or overlong name refuses before
    // anything exists, and an existing root refuses rather than clobbering
    // the logins stored under it.
    #[test]
    fn create_refuses_bad_names_and_existing_roots() {
        let _env = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let old_home = set_home(temp.path());
        fake_real_home(temp.path());

        let rc_bad = run(&args(&["create", "../x"]));
        let roots = sandboxes_root();
        restore_env("HOME", old_home);
        assert_ne!(rc_bad, 0, "a traversal name refuses");
        assert!(
            !roots.exists(),
            "a refused create writes nothing, not even the folders root"
        );

        let old_home2 = set_home(temp.path());
        let rc_first = run(&args(&["create", "demo"]));
        let root = root_for("demo");
        let rc_dup = run(&args(&["create", "demo"]));
        restore_env("HOME", old_home2);
        assert_eq!(rc_first, 0, "the first create on a fresh name succeeds");
        assert_ne!(rc_dup, 0, "a second create on the same name refuses");
        assert!(
            root.join("sandbox.toml").is_file(),
            "the refusal leaves the stored root readable"
        );
    }

    // The env world is the whole isolation contract: the fno and harness
    // roots move under the sandbox, the identity keys and the measured
    // ANTHROPIC_* family drop out, and the real-home tool carries survive.
    #[test]
    fn env_table_sets_roots_unsets_identity_and_carries_tools() {
        let _env = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let old_home = set_home(temp.path());
        fake_real_home(temp.path());
        let rc = run(&args(&["create", "demo"]));
        std::env::set_var("ANTHROPIC_AUTH_TOKEN", "leak");
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "live-session");
        let manifest = read_manifest(&root_for("demo"));
        let table = manifest.as_ref().map(env_table);
        let keychain = root_for("demo").join("home/Library/Keychains");
        let codex_cfg = root_for("demo").join("home/.codex/config.toml");
        restore_env("ANTHROPIC_AUTH_TOKEN", None);
        restore_env("CLAUDE_CODE_SESSION_ID", None);
        restore_env("HOME", old_home);

        assert_eq!(rc, 0, "create succeeds before the table is read");
        let manifest = manifest.expect("a stored manifest");
        let (sets, unsets) = table.expect("an env table");
        let root = manifest.root.clone();
        let set = |key: &str| {
            sets.iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        assert_eq!(set("FNO_CONFIG"), root.join("fno/config.toml"));
        assert_eq!(set("FNO_MUX_DIR"), root.join("fno/mux"));
        assert_eq!(set("FNO_AGENTS_HOME"), root.join("agents"));
        assert_eq!(set("FNO_CLAIMS_ROOT"), root.join("claims"));
        assert_eq!(set("FNO_SPACES_DIR"), root.join("fno/spaces"));
        assert_eq!(set("CLAUDE_CONFIG_DIR"), root.join("home/.claude"));
        assert_eq!(set("CODEX_HOME"), root.join("home/.codex"));
        assert_eq!(set("HOME"), root.join("home"));
        assert_eq!(set("FNO_SANDBOX"), "demo");
        assert_eq!(
            set("GIT_CONFIG_GLOBAL"),
            manifest.real_home.join(".gitconfig")
        );
        assert_eq!(set("GH_CONFIG_DIR"), manifest.real_home.join(".config/gh"));
        assert_eq!(set("CARGO_HOME"), manifest.real_home.join(".cargo"));
        assert_eq!(set("RUSTUP_HOME"), manifest.real_home.join(".rustup"));
        for key in [
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_SECURESTORAGE_CONFIG_DIR",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_DIR_OVERRIDE",
            "CLAUDE_CODE_SESSION_ID",
            "CODEX_THREAD_ID",
        ] {
            assert!(
                unsets.iter().any(|k| k == key),
                "{key} must be unset inside the sandbox"
            );
        }
        assert!(
            keychain.is_symlink(),
            "the keychain link is what makes the login reachable under a foreign HOME"
        );
        let pin = std::fs::read_to_string(codex_cfg).unwrap();
        assert!(
            pin.contains("cli_auth_credentials_store = \"file\""),
            "codex pins its credential store to the file inside CODEX_HOME"
        );
    }

    // Reset is the wipe-between-runs primitive: the fno world refreshes,
    // the harness homes (and the logins in them) stay byte-identical.
    #[test]
    fn reset_keeps_harness_homes_and_refreshes_fno_state() {
        let _env = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let old_home = set_home(temp.path());
        fake_real_home(temp.path());
        let rc_create = run(&args(&["create", "demo"]));
        let root = root_for("demo");
        let claude_marker = root.join("home/.claude/marker.txt");
        let codex_marker = root.join("home/.codex/marker.txt");
        std::fs::write(&claude_marker, "claude-login").unwrap();
        std::fs::write(&codex_marker, "codex-login").unwrap();
        let junk = root.join("fno/junk.txt");
        std::fs::write(&junk, "stale").unwrap();
        let rc_reset = run(&args(&["reset", "demo"]));
        let claude_after = std::fs::read_to_string(&claude_marker);
        let codex_after = std::fs::read_to_string(&codex_marker);
        let cfg_after = std::fs::read_to_string(root.join("fno/config.toml"));
        restore_env("HOME", old_home);

        assert_eq!(rc_create, 0);
        assert_eq!(rc_reset, 0, "reset on a quiet sandbox succeeds");
        assert_eq!(claude_after.as_deref().ok(), Some("claude-login"));
        assert_eq!(codex_after.as_deref().ok(), Some("codex-login"));
        assert!(!junk.exists(), "reset wipes the old fno world");
        let cfg = cfg_after.expect("reset recreates a fresh config");
        assert!(cfg.contains("schema_version = 1"), "{cfg}");
        assert!(cfg.contains("state_dir"), "{cfg}");
    }
}
