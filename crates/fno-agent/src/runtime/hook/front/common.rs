//! What the front's legs share: the payload fields they read, the load
//! budget the shell legs took from scripts/lib/hook-budget.sh, bounded child
//! runs, and the answer shapes.

use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The payload fields the legs read. A field of the wrong type reads as an
/// empty payload, which every leg answers with nothing.
#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub(crate) struct Payload {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub transcript_path: Option<String>,
    pub hook_event_name: Option<String>,
    pub prompt: Option<String>,
    pub source: Option<String>,
    pub stop_hook_active: Option<bool>,
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
    #[serde(rename = "conversationId")]
    pub conversation_id: Option<String>,
}

impl Payload {
    pub(crate) fn parse(raw: &str) -> Self {
        serde_json::from_str(raw.trim()).unwrap_or_default()
    }

    pub(crate) fn session(&self) -> &str {
        self.session_id.as_deref().unwrap_or("")
    }
}

/// The stdin payload of this leg and its parsed fields.
pub(crate) fn payload() -> (String, Payload) {
    let raw = crate::hook::read_stdin();
    let parsed = Payload::parse(&raw);
    (raw, parsed)
}

/// The session id shape the shell legs accepted before handing it to a
/// path: `[A-Za-z0-9_-]`, at least 16 characters.
pub(crate) fn plain_session(sid: &str) -> bool {
    sid.len() >= 16
        && sid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The process cwd, as the hook was started in it.
pub(crate) fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// The plugin tree: the shim exports `FNO_HOOK_PLUGIN_ROOT`; Claude exports
/// `CLAUDE_PLUGIN_ROOT`. Not `FNO_REPO_ROOT`: the path and backlog readers
/// take that one as the PROJECT root, so a plugin path there would point
/// every leg and its children at the plugin tree.
pub(crate) fn plugin_root() -> PathBuf {
    [
        "FNO_HOOK_PLUGIN_ROOT",
        "CLAUDE_PLUGIN_ROOT",
        "CODEX_PLUGIN_ROOT",
    ]
    .iter()
    .find_map(|k| std::env::var_os(k).filter(|v| !v.is_empty()))
    .map(PathBuf::from)
    .unwrap_or_else(cwd)
}

/// `git rev-parse --show-toplevel` of `dir`, or `dir` itself.
pub(crate) fn repo_root(dir: &Path) -> PathBuf {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"]);
    crate::bounded_cmd::output_with_timeout(cmd, 3)
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.to_path_buf())
}

/// The one-minute load average and the online core count.
fn load_and_cores() -> Option<(f64, f64)> {
    let (one, _, _) = crate::machine_sample::load_average()?;
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    Some((one, cores as f64))
}

/// hook-budget.sh's `hook_overloaded`: load at or past
/// `FNO_HOOK_BUDGET_SKIP_PER_CORE` (default 8) per core skips an optional
/// leg outright. An unreadable load never skips.
pub(crate) fn overloaded() -> bool {
    let per_core = match std::env::var("FNO_HOOK_BUDGET_SKIP_PER_CORE") {
        Ok(v) if v.is_empty() => 8.0,
        Ok(v) => match v.parse::<u32>() {
            Ok(n) => f64::from(n),
            Err(_) => return false,
        },
        Err(_) => 8.0,
    };
    load_and_cores().is_some_and(|(load, cores)| load >= cores * per_core)
}

/// hook-budget.sh's `hook_budget_secs`: `FNO_HOOK_BUDGET_SECS` wins, an async
/// hook gets 10 s, a busy machine (load past one per core) 1 s, else 3 s.
pub(crate) fn budget_secs() -> u64 {
    if let Ok(v) = std::env::var("FNO_HOOK_BUDGET_SECS") {
        if let Ok(n) = v.parse::<u64>() {
            return n;
        }
    }
    if std::env::var("FNO_HOOK_ASYNC").as_deref() == Ok("1") {
        return 10;
    }
    match load_and_cores() {
        Some((load, cores)) if load > cores => 1,
        _ => 3,
    }
}

/// Run `cmd` with `stdin` (or none) under `secs`, stdout and stderr piped:
/// a leg's child never writes to the front's own stdout.
pub(crate) fn bounded(cmd: Command, stdin: Option<&str>, secs: u64) -> Option<Output> {
    if secs == 0 {
        return None;
    }
    match stdin {
        Some(input) => crate::bounded_cmd::output_with_timeout_stdin(cmd, secs, input).ok(),
        None => crate::bounded_cmd::output_with_timeout(cmd, secs),
    }
}

/// The stdout of a bounded run that exited 0, trimmed of its last newline.
pub(crate) fn bounded_stdout(cmd: Command, stdin: Option<&str>, secs: u64) -> Option<String> {
    bounded(cmd, stdin, secs)
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim_end_matches('\n')
                .to_string()
        })
}

/// Start `cmd` detached: no stdio, never waited on. It outlives the front.
pub(crate) fn detach(mut cmd: Command) {
    use std::os::unix::process::CommandExt;
    // A child that fails to start is a skipped leg.
    cmd.process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok();
}

/// The `fno` front door: PATH first, then the install homes fno-bin.sh read.
pub(crate) fn fno_bin() -> Option<PathBuf> {
    if let Some(p) = crate::loop_dispatch::which_binary("fno") {
        return Some(p);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("UV_TOOL_BIN_DIR").filter(|v| !v.is_empty()) {
        candidates.push(PathBuf::from(dir).join("fno"));
    }
    if let Some(home) = home {
        candidates.push(home.join(".local/bin/fno"));
        candidates.push(home.join(".cargo/bin/fno"));
    }
    candidates.into_iter().find(|p| is_executable(p))
}

/// This binary: the legs that once started `fno-agent <command>` start it.
pub(crate) fn fno_agent_bin() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

pub(crate) fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// `{"hookSpecificOutput":{"hookEventName":<event>,"additionalContext":<text>}}`.
pub(crate) fn emit_context(event: &str, text: &str) {
    crate::hook::emit_line(
        &json!({"hookSpecificOutput": {"hookEventName": event, "additionalContext": text}})
            .to_string(),
    );
}

/// `{"decision":"block","reason":<reason>}`.
pub(crate) fn emit_block(reason: &str) {
    crate::hook::emit_line(&json!({"decision": "block", "reason": reason}).to_string());
}

/// Hand a former leg's raw stdout on as its answer.
pub(crate) fn emit_raw(text: &str) {
    let text = text.trim_end_matches('\n');
    if !text.is_empty() {
        crate::hook::emit_line(text);
    }
}

/// The per-harness session-state report, the side leg every event runs:
/// session-state.sh's footnote-harness skip and zcode hand-off included.
pub(crate) fn session_state_leg(event: &str) -> i32 {
    if std::env::var("FNO_HARNESS_NAME").as_deref() == Ok("footnote") {
        return 0;
    }
    let raw = crate::hook::read_stdin();
    let zcode = [
        "ZCODE_BUILTIN_PROVIDER_CONFIG_FILE",
        "ZCODE_PERSONAL_PROVIDER_CONFIG_FILE",
    ]
    .iter()
    .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()));
    let claude_sid = std::env::var_os("CLAUDE_CODE_SESSION_ID").is_some_and(|v| !v.is_empty());
    if zcode && !claude_sid {
        let script = plugin_root().join("hooks/zcode-session-report.sh");
        let _ = bounded(Command::new(script), Some(&raw), 2);
        return 0;
    }
    if let Ok(whole) = serde_json::from_str::<Value>(raw.trim()) {
        crate::hook::session_state::process("claude", event, &whole);
    }
    0
}

/// A pack install drops every mux script, session-state.sh among them, and
/// with it the mux legs (gate.rs reads the same file).
pub(crate) fn mux_profile() -> bool {
    std::env::var_os("FNO_HOOK_PLUGIN_ROOT")
        .map(PathBuf::from)
        .is_none_or(|root| root.join("hooks/session-state.sh").is_file())
}
