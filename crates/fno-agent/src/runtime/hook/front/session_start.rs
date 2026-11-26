//! The SessionStart legs: the using-fno preamble, the context producers'
//! cached pass, the daemon liveness heal, the claude supervisor bootstrap
//! check, and the unread auto-memory retire pass.

use super::{Leg, Payload};
use crate::hook::emit_line;
use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

pub(super) fn legs() -> Vec<Leg> {
    vec![
        Leg::new(
            "session-start-using-fno",
            super::CLAUDE_DEFAULT_SECS,
            using_fno_leg,
        ),
        Leg::new("context-run", 50, context_run_leg),
        Leg::new(
            "daemon-liveness-session-start",
            super::CLAUDE_DEFAULT_SECS,
            daemon_liveness_leg,
        ),
        Leg::new(
            "bootstrap-session-start",
            super::CLAUDE_DEFAULT_SECS,
            bootstrap_leg,
        ),
        Leg::new("retire-unread-pass", 10, retire_unread_leg),
    ]
}

/// An env value, unset and empty alike reading as none (the shell's `${X:-}`).
fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_default()
}

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// session-start-using-fno: the using-fno stub as context, for a session that
// joined fno only. A session joins at its first fno skill or command
// (crates/fno-agent/src/backlog/join_marker.rs). An unjoined session gets no
// preamble once any session on the machine has joined; before that, the
// install itself is the opt-in. A joined one gets it once, and again after
// each compaction.
// ---------------------------------------------------------------------------

/// `[EVENT] [join] [trigger]`: EVENT is the hookEventName to answer with
/// (default SessionStart), `join` joins the session first, and the trigger
/// names the join cause (`skill:<name>`).
fn using_fno_leg(args: &[String]) -> i32 {
    let event = args
        .first()
        .filter(|e| !e.is_empty())
        .map_or("SessionStart", String::as_str);
    let join = args.get(1).is_some_and(|a| a == "join");
    let trigger = args.get(2).map_or("", String::as_str);
    using_fno(event, join, trigger, &crate::hook::read_stdin())
}

const PREAMBLE_HEAD: &str = "<EXTREMELY_IMPORTANT>\nYou are in a footnote-enabled project.\n\n**The 'using-fno' stub - when to reach for fno and its two surfaces. The version-matched full guide prints on demand: `fno whoami guide` (whole guide + topic index) or `fno whoami guide <topic>`. Re-read it when in doubt about which surface to use:**\n\n";

/// The preamble answer, or `{}`. Hooks never block: every failure is `{}`.
pub(super) fn using_fno(event: &str, join: bool, trigger: &str, raw: &str) -> i32 {
    let root = using_fno_root();
    // Before any early exit: this leg noops whenever the preamble is not
    // due, and a prime behind that gate would run once per machine instead
    // of every session start.
    if let Some(home) = fno_home() {
        prime_plugin_root_pointer(&root, &home);
    }
    let skill = root.join("skills/using-fno/SKILL.md");
    let Ok(body) = std::fs::read_to_string(&skill) else {
        eprintln!(
            "session-start-using-fno: SKILL.md missing or unreadable at {}",
            skill.display()
        );
        return noop();
    };
    let payload = Payload::parse(raw);
    let Some((harness, sid)) = harness_session(&env_nonempty, &payload) else {
        return noop();
    };
    let compacted = payload.source.as_deref() == Some("compact") || event == "PostCompact";
    let transcript = payload.transcript_path.as_deref().filter(|t| !t.is_empty());
    let due = if join {
        // The join act refuses in hook context, so a deliberate trigger
        // clears FNO_HOOK_CONTEXT for this one child.
        let Some(bin) = super::fno_agent_bin() else {
            return noop();
        };
        let args = join_report_args(&harness, &sid, transcript, trigger, compacted);
        let mut cmd = Command::new(bin);
        cmd.args(&args)
            .env_remove(crate::join_marker::HOOK_CONTEXT_ENV);
        super::bounded(cmd, None, join_budget_secs())
            .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n') == "due")
    } else {
        check_due(&harness, &sid, transcript, compacted)
    };
    if !due {
        return noop();
    }
    let text = preamble(&body);
    let answer = render(
        &harness,
        event,
        &text,
        env_nonempty("CURSOR_PLUGIN_ROOT").is_some(),
        env_nonempty("COPILOT_CLI").is_some(),
    );
    emit_line(&answer.to_string());
    0
}

fn noop() -> i32 {
    emit_line("{}");
    0
}

/// The plugin tree the preamble reads: the harness's own root first, then
/// the shim's tree.
fn using_fno_root() -> PathBuf {
    env_nonempty("CLAUDE_PLUGIN_ROOT")
        .or_else(|| env_nonempty("CODEX_PLUGIN_ROOT"))
        .map(PathBuf::from)
        .unwrap_or_else(super::plugin_root)
}

fn fno_home() -> Option<PathBuf> {
    env_nonempty("FNO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".fno")))
}

fn join_budget_secs() -> u64 {
    env_nonempty("FNO_JOIN_BUDGET_SECS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3)
}

/// The harness and session id register-session-start reads. The opencode,
/// pi and agy carriers name their harness in FNO_HOOK_HARNESS and pass the
/// session id in the payload (agy: camelCase conversationId).
fn harness_session(
    env: &dyn Fn(&str) -> Option<String>,
    payload: &Payload,
) -> Option<(String, String)> {
    let (harness, mut sid) = if let Some(harness) = env("FNO_HOOK_HARNESS") {
        (harness, String::new())
    } else if env("FNO_PLATFORM").as_deref() == Some("codex") || env("CODEX_PLUGIN_ROOT").is_some()
    {
        let (thread, session) = (env("CODEX_THREAD_ID"), env("CODEX_SESSION_ID"));
        let subagent = thread.is_some() && session.is_some() && thread != session;
        let sid = if subagent {
            String::new()
        } else {
            thread.or(session).unwrap_or_default()
        };
        ("codex".to_string(), sid)
    } else if env("CLAUDE_PLUGIN_ROOT").is_some() {
        let sid = env("CLAUDE_CODE_SESSION_ID").unwrap_or_default();
        ("claude".to_string(), sid)
    } else {
        return None;
    };
    for fallback in [&payload.session_id, &payload.conversation_id] {
        if sid.is_empty() {
            sid = fallback.clone().unwrap_or_default();
        }
    }
    (!sid.is_empty()).then_some((harness, sid))
}

/// `fno-agent report --kind join ... --preamble-due --mark` for the join act.
fn join_report_args(
    harness: &str,
    sid: &str,
    transcript: Option<&str>,
    trigger: &str,
    compacted: bool,
) -> Vec<String> {
    let mut args: Vec<String> = [
        "report",
        "--kind",
        "join",
        "--harness",
        harness,
        "--session-id",
        sid,
        "--preamble-due",
        "--mark",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(t) = transcript {
        args.extend(["--transcript".to_string(), t.to_string()]);
    }
    if !trigger.is_empty() {
        args.extend(["--trigger".to_string(), trigger.to_string()]);
    }
    if compacted {
        args.push("--compacted".to_string());
    }
    args
}

/// `report --kind join ... --preamble-due --mark --check` in process, under
/// the same join budget the child ran under: a slow registry read answers
/// not due.
fn check_due(harness: &str, sid: &str, transcript: Option<&str>, compacted: bool) -> bool {
    let (tx, rx) = mpsc::channel();
    let (harness, sid) = (harness.to_string(), sid.to_string());
    let transcript = transcript.map(PathBuf::from);
    let spawned = std::thread::Builder::new()
        .name("hook-front-using-fno-due".into())
        .spawn(move || {
            let home = crate::paths::AgentsHome::from_env();
            let due = crate::join_marker::preamble_due_now(
                home.root(),
                &harness,
                &sid,
                transcript.as_deref(),
                true,
                compacted,
            );
            let _ = tx.send(due);
        });
    spawned.is_ok()
        && rx
            .recv_timeout(Duration::from_secs(join_budget_secs()))
            .unwrap_or(false)
}

/// The skill body framed so it reads as load-bearing at the top of the
/// conversation. The body is the stub; the framing names the full guide's
/// door.
fn preamble(body: &str) -> String {
    format!(
        "{PREAMBLE_HEAD}{}\n</EXTREMELY_IMPORTANT>",
        body.trim_end_matches('\n')
    )
}

/// The answer shape each carrier reads: injectSteps (agy), `additional_context`
/// (Cursor), top-level `additionalContext` (Copilot), else hookSpecificOutput.
fn render(
    harness: &str,
    event: &str,
    text: &str,
    cursor: bool,
    copilot: bool,
) -> serde_json::Value {
    use serde_json::json;
    if harness == "agy" {
        json!({"injectSteps": [{"ephemeralMessage": text}]})
    } else if cursor {
        json!({"additional_context": text})
    } else if copilot {
        json!({"additionalContext": text})
    } else {
        json!({"hookSpecificOutput": {"hookEventName": event, "additionalContext": text}})
    }
}

/// session-start.sh's `prime_plugin_root_pointer`: persist the plugin root
/// to `<fno home>/install/plugin-root`, the only env-less source for
/// `fno do target init`. A linked worktree (a `.git` FILE) never becomes the
/// machine-global value; an equal pointer is left alone.
fn prime_plugin_root_pointer(root: &Path, home: &Path) {
    if !root.join(".claude-plugin/plugin.json").is_file() || root.join(".git").is_file() {
        return;
    }
    let ptr = home.join("install/plugin-root");
    let want = root.to_string_lossy();
    if std::fs::read_to_string(&ptr).is_ok_and(|have| have.trim_end_matches('\n') == want) {
        return;
    }
    if std::fs::create_dir_all(home.join("install")).is_err() {
        return;
    }
    let _ = std::fs::write(&ptr, format!("{want}\n"));
}

// ---------------------------------------------------------------------------
// context-run: one runner for every fno context producer (the groups live in
// hooks/context-hooks.json). Law D1839 pins the SessionStart budget near 2 s,
// but a full pass measures tens of seconds on a loaded machine, so the leg is
// stale-while-revalidate: it answers at once from the project's cached pass
// and refreshes that cache in a detached pass for the next session (D1838:
// background work never sits on the foreground path).
// ---------------------------------------------------------------------------

/// The detached refresh. The values travel as environment, never
/// interpolated, so a quote in a path cannot break it. The cache is
/// published by atomic mv only on a clean pass.
const REVALIDATE: &str = r#"
    if "$FNO_RUNNER" context-run --group "$FNO_RUN_GROUP" --plugin-root "$FNO_RUN_ROOT" < "$FNO_RUN_PAYLOAD" > "$FNO_RUN_TMP" 2> "$FNO_RUN_LOG"; then
        mv -f "$FNO_RUN_TMP" "$FNO_RUN_CACHE"
        date +%s > "$FNO_RUN_CACHE.ts"
    else
        rm -f "$FNO_RUN_TMP"
    fi
    rm -f "$FNO_RUN_PAYLOAD"
"#;

/// `[group]`. The claude front passes no args: its hooks.json entry named
/// `claude-session-start`, so that is the default here.
fn context_run_leg(args: &[String]) -> i32 {
    let group = args.first().map_or("claude-session-start", String::as_str);
    let raw = crate::hook::read_stdin();
    let Some(bin) = super::fno_agent_bin() else {
        return 0;
    };
    let root = super::plugin_root();
    let payload = if group.is_empty() {
        None
    } else {
        write_payload(&raw)
    };
    let Some(payload) = payload else {
        // Nothing to cache against: the synchronous pass is the fallback.
        run_sync(&bin, group, &root, &raw);
        return 0;
    };
    let cache_dir = context_cache_dir(&env_nonempty);
    let _ = std::fs::create_dir_all(&cache_dir);
    let key = cwd_key(&super::cwd());
    let cache = cache_dir.join(format!("{key}-{group}.json"));

    // A cache written within the TTL replays the previous pass's output;
    // anything else answers the no-op object.
    let ts = std::fs::read_to_string(suffixed(&cache, ".ts"))
        .ok()
        .map(|t| t.trim_end_matches('\n').to_string())
        .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|t| t.parse::<i64>().ok())
        .unwrap_or(0);
    let ttl = match env_nonempty("FNO_CONTEXT_CACHE_TTL_SECONDS") {
        None => Some(86400),
        Some(v) => v.parse::<i64>().ok(),
    };
    let filled = std::fs::metadata(&cache).is_ok_and(|m| m.len() > 0);
    let fresh = filled && ttl.is_some_and(|ttl| now_epoch() - ts < ttl);
    match fresh.then(|| std::fs::read(&cache).ok()).flatten() {
        Some(bytes) => super::emit_raw(&String::from_utf8_lossy(&bytes)),
        None => emit_line("{}"),
    }

    let mut cmd = Command::new("nice");
    cmd.args(["-n", "10", "bash", "-c", REVALIDATE])
        .env("FNO_HOOK_CONTEXT", "1")
        .env("FNO_RUNNER", &bin)
        .env("FNO_RUN_GROUP", group)
        .env("FNO_RUN_ROOT", &root)
        .env("FNO_RUN_PAYLOAD", &payload)
        .env("FNO_RUN_CACHE", &cache)
        .env(
            "FNO_RUN_TMP",
            suffixed(&cache, &format!(".tmp.{}", std::process::id())),
        )
        .env("FNO_RUN_LOG", cache_dir.join(format!("{key}-{group}.log")));
    super::detach(cmd);
    0
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// The no-cache pass: the runner's own answer, its producers kept out of
/// the session's join.
fn run_sync(bin: &Path, group: &str, root: &Path, raw: &str) {
    use std::os::unix::process::ExitStatusExt;
    let mut cmd = Command::new(bin);
    cmd.args(["context-run", "--group", group, "--plugin-root"])
        .arg(root)
        .env("FNO_HOOK_CONTEXT", "1");
    let Some(out) = super::bounded(cmd, Some(raw), 45) else {
        return;
    };
    super::emit_raw(&String::from_utf8_lossy(&out.stdout));
    if !out.status.success() {
        // The runner exits 0 on its own error paths; a nonzero here is an
        // old binary that does not know the command.
        let rc = out
            .status
            .code()
            .or_else(|| out.status.signal().map(|s| 128 + s))
            .unwrap_or(1);
        eprintln!("fno: context-run exited {rc} (stale fno-agent binary?); run fno doctor --fix");
    }
}

/// The payload file the detached pass reads its stdin from.
fn write_payload(raw: &str) -> Option<PathBuf> {
    let dir = env_nonempty("TMPDIR").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    let mut file = tempfile::Builder::new()
        .prefix("fno-context-payload.")
        .tempfile_in(dir)
        .ok()?;
    file.write_all(raw.as_bytes()).ok()?;
    file.into_temp_path().keep().ok()
}

/// The cache home, env then platform only (hook hot path: no fno fork).
fn context_cache_dir(env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    let home = env("HOME").unwrap_or_else(|| "/tmp".to_string());
    if let Some(dir) = env("FNO_CONTEXT_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(cache) = env("FNO_CACHE_HOME") {
        let cache = match cache.strip_prefix('~') {
            Some(rest) => format!("{home}{rest}"),
            None => cache,
        };
        return PathBuf::from(cache).join("context");
    }
    if cfg!(target_os = "macos") {
        return PathBuf::from(home).join("Library/Caches/fno/context");
    }
    let xdg = env("XDG_CACHE_HOME").unwrap_or_else(|| format!("{home}/.cache"));
    PathBuf::from(xdg).join("fno/context")
}

/// The per-project cache key: the first 16 hex of the sha256 of
/// `pwd -P` and its newline, as the shell pass keyed it.
fn cwd_key(cwd: &Path) -> String {
    let real = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let mut line = real.as_os_str().as_bytes().to_vec();
    line.push(b'\n');
    Sha256::digest(&line)
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---------------------------------------------------------------------------
// daemon-liveness-session-start: with the daemon down nothing wakes a lead on
// a daemon beat until someone types, and SessionStart is the one surface that
// fires anyway. The probe is a raw socket connect with the client's own down
// predicate (a missing socket, or one that refuses), not `fno-agent status`,
// which costs ~1.9 s. The heal is `fno-agent restart --if-down`, whose gate
// re-measures the down state before any swap.
// ---------------------------------------------------------------------------

const DAEMON_HEALED: &str = "[fno] the fno-agent daemon is down: fired fno-agent restart --if-down in the background to start it; it serves within a few seconds. Run that command by hand if it stays down.";

fn daemon_liveness_leg(_args: &[String]) -> i32 {
    let Some(bin) = super::fno_agent_bin() else {
        return 0;
    };
    let home =
        env_nonempty("FNO_AGENT_HOME").unwrap_or_else(|| format!("{}/.fno/agents", home_dir()));
    if !daemon_down(&Path::new(&home).join("supervisor.sock")) {
        return 0;
    }
    // Detached, never waited on: a slow boot finishes in the background.
    let mut cmd = Command::new(bin);
    cmd.args(["restart", "--if-down"]);
    super::detach(cmd);
    super::emit_context("SessionStart", DAEMON_HEALED);
    0
}

/// A missing socket reads down with no connect. A socket that refuses, or
/// does not accept within 1 s, is one a dead daemon left behind. Anything
/// that is not a socket says nothing.
fn daemon_down(sock: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    let Ok(meta) = std::fs::metadata(sock) else {
        return true;
    };
    if !meta.file_type().is_socket() {
        return false;
    }
    let (tx, rx) = mpsc::channel();
    let path = sock.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("hook-front-daemon-probe".into())
        .spawn(move || {
            let _ = tx.send(std::os::unix::net::UnixStream::connect(&path).is_ok());
        });
    spawned.is_ok() && !rx.recv_timeout(Duration::from_secs(1)).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// bootstrap-session-start: a claude supervisor born outside the user's Aqua
// GUI session hosts sessions that can lose DNS, gh, ps and the keychain for
// its whole life. The in-Rust birth guard only sees births fno mediates, so
// SessionStart names the state where a hosted session can see it. Evidence,
// never a self-heal: at most one line.
// ---------------------------------------------------------------------------

fn bootstrap_leg(_args: &[String]) -> i32 {
    // macOS only: launchctl and scutil do not exist elsewhere.
    if !cfg!(target_os = "macos") {
        return 0;
    }
    // A wedged launchctl or configd must not wedge session start.
    let probe = |program: &str, arg: &str| {
        let mut cmd = Command::new(program);
        cmd.arg(arg);
        super::bounded(cmd, None, 1)
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let manager = probe("launchctl", "managername");
    let dns = probe("scutil", "--dns");
    if let Some(reason) = bootstrap_reason(&manager, &dns) {
        super::emit_context(
            "SessionStart",
            &format!("[fno] {reason} Cure: from a GUI terminal, run `claude daemon stop --any`, then start any `claude --bg` command; the recipe is the claude supervisor entry in docs/fleet-faq.md."),
        );
    }
    0
}

/// Arm 1, the birth context: a manager other than Aqua (an empty answer is
/// unknown, never a warning). Arm 2, the services: a supervisor that lost
/// its bootstrap prints no DNS configuration at all, even under Aqua.
fn bootstrap_reason(manager_out: &str, dns_out: &str) -> Option<String> {
    let manager: String = manager_out
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let dns_ok = dns_out.lines().any(|l| l.starts_with("DNS configuration"));
    if !manager.is_empty() && manager != "Aqua" {
        Some(format!("this session runs under a claude supervisor born outside the GUI session (launchctl managername reads {manager}): DNS, gh, ps and the keychain can fail here, and every session the supervisor hosts inherits the same."))
    } else if !dns_ok {
        Some("this session's claude supervisor lost its GUI bootstrap: scutil --dns names no DNS configuration, so DNS, gh, ps and the keychain fail here.".to_string())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// retire-unread-pass: retire unread auto_generated memory entries by move.
// An entry moves to .archived/ only when its frontmatter carries
// `auto_generated: true`, its created_at parses and is past probation, and no
// sidecar line names it at or after its created_at. The pass never deletes or
// overwrites, and prunes only the writer-owned MEMORY.md index.
// ---------------------------------------------------------------------------

const RETIRE_HELP: &str = "retire-unread-pass: retire unread auto_generated memory entries by move.
Args:
  --memory-dir DIR     The memory dir. Without it, the hook payload's
                       (.cwd) is derived exactly as the Read counter does.
  --probation-days N   Minimum age before a zero-read entry is eligible.
                       Default 30. 0 makes every past entry eligible.
  --dry-run            Name the would-move entries; move and prune nothing.";

/// The files the operator or the index owns: never candidates.
const RETIRE_KEEP: &[&str] = &["MEMORY.md", "CLAUDE.md", "ARCHIVE-INDEX.md"];

/// Optional, fail open: 0 on every routine path, 1 on a bad argument. One
/// stdout line only when an entry moved (SessionStart stdout is context).
fn retire_unread_leg(args: &[String]) -> i32 {
    let mut mem_dir: Option<PathBuf> = None;
    let mut probation = "30".to_string();
    let mut dry_run = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--memory-dir" | "--probation-days" => {
                let Some(value) = it.next() else {
                    eprintln!("retire-unread-pass: {arg} needs a value");
                    return 1;
                };
                if arg == "--memory-dir" {
                    mem_dir = Some(PathBuf::from(value)).filter(|d| !d.as_os_str().is_empty());
                } else {
                    probation = value.clone();
                }
            }
            "--dry-run" => dry_run = true,
            "-h" | "--help" => {
                RETIRE_HELP.lines().for_each(emit_line);
                return 0;
            }
            other => {
                eprintln!("retire-unread-pass: unknown arg: {other}");
                return 1;
            }
        }
    }
    let probation = match probation.parse::<i64>() {
        Ok(days) if probation.bytes().all(|b| b.is_ascii_digit()) => days,
        _ => {
            eprintln!("retire-unread-pass: --probation-days needs a number");
            return 1;
        }
    };
    let mem_dir = match mem_dir {
        Some(dir) => dir,
        None => {
            // Hook form: the dir derives from the payload's cwd, as Claude
            // Code keys its own per-project store.
            let payload = Payload::parse(&crate::hook::read_stdin());
            let Some(cwd) = payload.cwd.filter(|c| !c.is_empty()) else {
                return 0;
            };
            let root = env_nonempty("CLAUDE_CONFIG_DIR")
                .unwrap_or_else(|| format!("{}/.claude", home_dir()));
            Path::new(&root)
                .join("projects")
                .join(memory_slug(&cwd))
                .join("memory")
        }
    };
    if !mem_dir.is_dir() {
        return 0;
    }
    for line in retire_pass(&mem_dir, probation, dry_run, now_epoch()) {
        emit_line(&line);
    }
    0
}

/// Claude Code's project slug: every byte that is not ASCII alphanumeric
/// becomes `-`.
fn memory_slug(cwd: &str) -> String {
    cwd.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                b as char
            } else {
                '-'
            }
        })
        .collect()
}

/// The frontmatter's `auto_generated` and `created_at` values. Keys are
/// trimmed: one live entry carries two-space indented keys.
fn frontmatter(text: &str) -> (String, String) {
    let (mut auto, mut created) = (String::new(), String::new());
    let (mut in_fm, mut seen) = (false, false);
    for line in text.split('\n') {
        if line == "---" {
            if !seen {
                (in_fm, seen) = (true, true);
                continue;
            } else if in_fm {
                break;
            }
        }
        if !in_fm {
            continue;
        }
        let t = line.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
        let value = |v: &str| v.strip_prefix(' ').unwrap_or(v).to_string();
        if let Some(v) = t.strip_prefix("auto_generated:") {
            auto = value(v);
        } else if let Some(v) = t.strip_prefix("created_at:") {
            created = value(v);
        }
    }
    (auto, created)
}

/// created_at as UTC epoch seconds; None never archives (unknown age).
fn created_epoch(created: &str) -> Option<i64> {
    let naive = created.strip_suffix('Z').unwrap_or(created);
    chrono::NaiveDateTime::parse_from_str(naive, "%Y-%m-%dT%H:%M:%S")
        .map(|t| t.and_utc().timestamp())
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(created).map(|t| t.timestamp()))
        .ok()
}

/// Whether the sidecar holds a line naming `name` whose ts is at least the
/// entry's created_at. Reads and writer updates both count as use; older
/// lines are ghosts of a prior life of that name. ISO-8601 UTC timestamps
/// compare as strings. An unreadable sidecar counts as use: never archive
/// on a read that failed.
fn used_since(sidecar: &Path, name: &str, created: &str) -> bool {
    if !sidecar.is_file() {
        return false;
    }
    let Ok(bytes) = std::fs::read(sidecar) else {
        return true;
    };
    let pat = format!("\"file\":\"{name}\"");
    String::from_utf8_lossy(&bytes).lines().any(|line| {
        line.contains(&pat)
            && line
                .split_once("\"ts\":\"")
                .and_then(|(_, rest)| rest.split_once('"'))
                .is_some_and(|(ts, _)| ts >= created)
    })
}

/// One pass over the top-level entries; answers the stdout lines.
fn retire_pass(mem_dir: &Path, probation_days: i64, dry_run: bool, now: i64) -> Vec<String> {
    let Ok(dir) = std::fs::read_dir(mem_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = dir
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".md") && !n.starts_with('.') && !RETIRE_KEEP.contains(&n.as_str()))
        .collect();
    names.sort();
    let sidecar = mem_dir.join(".reads.jsonl");
    let archive = mem_dir.join(".archived");
    let mut moved: Vec<String> = Vec::new();
    let mut would: Vec<String> = Vec::new();
    for name in names {
        let entry = mem_dir.join(&name);
        let Ok(bytes) = std::fs::read(&entry) else {
            continue;
        };
        let (auto, created) = frontmatter(&String::from_utf8_lossy(&bytes));
        if auto != "true" || created.is_empty() {
            continue;
        }
        let Some(epoch) = created_epoch(&created) else {
            continue;
        };
        if (now - epoch) / 86400 < probation_days {
            continue;
        }
        // Read against the live sidecar: a read landing while the pass
        // runs still protects the entry.
        if used_since(&sidecar, &name, &created) {
            continue;
        }
        if dry_run {
            would.push(name);
            continue;
        }
        if std::fs::create_dir_all(&archive).is_err() {
            eprintln!("retire-unread-pass: cannot create {}", archive.display());
            return Vec::new();
        }
        // A move never overwrites: a prior retire of the same name is history.
        let mut target = archive.join(&name);
        if target.exists() {
            target = archive.join(format!("{name}.{}", now_epoch()));
        }
        if std::fs::rename(&entry, &target).is_err() {
            eprintln!("retire-unread-pass: move failed for {name}");
            continue;
        }
        moved.push(name);
    }
    if dry_run {
        if would.is_empty() {
            return Vec::new();
        }
        let mut out = vec!["memory-retire (dry run): would archive:".to_string()];
        out.extend(would.iter().map(|n| format!("  {n}")));
        return out;
    }
    if moved.is_empty() {
        return Vec::new();
    }
    prune_index(mem_dir, &moved);
    let word = if moved.len() == 1 { "entry" } else { "entries" };
    vec![format!(
        "memory-retire: archived {} unread auto {word}: {}",
        moved.len(),
        moved.join(" ")
    )]
}

/// Drop the MEMORY.md lines whose link form `(name.md)` names a moved
/// entry; every other line stays byte for byte.
fn prune_index(mem_dir: &Path, moved: &[String]) {
    let index = mem_dir.join("MEMORY.md");
    let Ok(old) = std::fs::read(&index) else {
        return;
    };
    let patterns: Vec<Vec<u8>> = moved
        .iter()
        .map(|n| format!("({n})").into_bytes())
        .collect();
    let body = old.strip_suffix(b"\n").unwrap_or(&old[..]);
    let mut new: Vec<u8> = Vec::with_capacity(old.len());
    if !old.is_empty() {
        for line in body.split(|b| *b == b'\n') {
            let named = patterns
                .iter()
                .any(|p| line.windows(p.len()).any(|w| w == p.as_slice()));
            if !named {
                new.extend_from_slice(line);
                new.push(b'\n');
            }
        }
    }
    if new == old {
        return;
    }
    let Ok(mut tmp) = tempfile::Builder::new()
        .prefix(".tmp.retire.")
        .tempfile_in(mem_dir)
    else {
        return;
    };
    if tmp.write_all(&new).is_ok() {
        let _ = tmp.persist(&index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn using_fno_carriers_pick_the_harness_and_the_answer_shape() {
        let payload = Payload::parse(r#"{"source":"compact","session_id":"s1"}"#);
        let with = |pairs: &[(&str, &str)]| {
            let map: HashMap<String, String> = pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            move |k: &str| map.get(k).cloned()
        };
        // opencode's runner sets CLAUDE_PLUGIN_ROOT; FNO_HOOK_HARNESS still wins.
        let env = with(&[
            ("CLAUDE_PLUGIN_ROOT", "/p"),
            ("FNO_HOOK_HARNESS", "opencode"),
        ]);
        assert_eq!(
            harness_session(&env, &payload),
            Some(("opencode".into(), "s1".into()))
        );
        let env = with(&[
            ("CLAUDE_PLUGIN_ROOT", "/p"),
            ("CLAUDE_CODE_SESSION_ID", "c9"),
        ]);
        assert_eq!(
            harness_session(&env, &payload),
            Some(("claude".into(), "c9".into()))
        );
        // A codex subagent thread (thread id differs) reads the payload id.
        let env = with(&[
            ("FNO_PLATFORM", "codex"),
            ("CODEX_THREAD_ID", "t"),
            ("CODEX_SESSION_ID", "u"),
        ]);
        assert_eq!(
            harness_session(&env, &payload),
            Some(("codex".into(), "s1".into()))
        );
        let agy = Payload::parse(r#"{"conversationId":"a1"}"#);
        let env = with(&[("FNO_HOOK_HARNESS", "agy")]);
        assert_eq!(
            harness_session(&env, &agy),
            Some(("agy".into(), "a1".into()))
        );
        assert_eq!(harness_session(&with(&[]), &payload), None);
        assert_eq!(harness_session(&env, &Payload::default()), None);

        let args = join_report_args("pi", "s1", Some("/t.jsonl"), "skill:x", true);
        assert_eq!(
            args.join(" "),
            "report --kind join --harness pi --session-id s1 --preamble-due --mark --transcript /t.jsonl --trigger skill:x --compacted"
        );

        let text = preamble("BODY\n\n");
        assert!(text.starts_with("<EXTREMELY_IMPORTANT>\nYou are in a footnote-enabled project."));
        assert!(text.ends_with("\n\nBODY\n</EXTREMELY_IMPORTANT>"));
        assert_eq!(
            render("agy", "PreInvocation", "C", false, false).to_string(),
            r#"{"injectSteps":[{"ephemeralMessage":"C"}]}"#
        );
        assert_eq!(
            render("claude", "S", "C", true, true).to_string(),
            r#"{"additional_context":"C"}"#
        );
        assert_eq!(
            render("claude", "S", "C", false, true).to_string(),
            r#"{"additionalContext":"C"}"#
        );
        assert_eq!(
            render("claude", "UserPromptSubmit", "C", false, false)["hookSpecificOutput"]
                ["hookEventName"],
            "UserPromptSubmit"
        );
    }

    #[test]
    fn plugin_root_pointer_primes_a_stage_or_checkout_never_a_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let ptr = home.join("install/plugin-root");
        let plugin = |name: &str, git: Option<bool>| {
            let root = tmp.path().join(name);
            std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
            std::fs::write(root.join(".claude-plugin/plugin.json"), "{}").unwrap();
            match git {
                Some(true) => std::fs::create_dir_all(root.join(".git")).unwrap(),
                Some(false) => std::fs::write(root.join(".git"), "gitdir: /x\n").unwrap(),
                None => {}
            }
            root
        };
        let stage = plugin("stage", None);
        std::fs::create_dir_all(home.join("install")).unwrap();
        std::fs::write(&ptr, "/stale/copy\n").unwrap();
        prime_plugin_root_pointer(&stage, &home);
        let want = format!("{}\n", stage.display());
        assert_eq!(std::fs::read_to_string(&ptr).unwrap(), want);

        prime_plugin_root_pointer(&plugin("wt", Some(false)), &home);
        assert_eq!(std::fs::read_to_string(&ptr).unwrap(), want);
        prime_plugin_root_pointer(&tmp.path().join("bare"), &home);
        assert_eq!(std::fs::read_to_string(&ptr).unwrap(), want);

        let canon = plugin("canon", Some(true));
        prime_plugin_root_pointer(&canon, &home);
        assert_eq!(
            std::fs::read_to_string(&ptr).unwrap(),
            format!("{}\n", canon.display())
        );
    }

    #[test]
    fn bootstrap_reason_names_a_foreign_manager_or_lost_dns_and_is_silent_when_healthy() {
        let dns_ok = "DNS configuration\n\nresolver #1\n  nameserver[0] : 1.1.1.1\n";
        let foreign = bootstrap_reason("Background\n", dns_ok).unwrap();
        assert!(
            foreign.contains("launchctl managername reads Background"),
            "{foreign}"
        );
        let lost = bootstrap_reason("Aqua\n", "No DNS configuration available\n").unwrap();
        assert!(lost.contains("lost its GUI bootstrap"), "{lost}");
        assert_eq!(bootstrap_reason("Aqua\n", dns_ok), None);
        assert_eq!(bootstrap_reason("", dns_ok), None);
    }
}
