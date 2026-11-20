//! The `fno mux app` doors: an agent's drive surface for the fno-app
//! desktop app. The CLI resolves the binary the way `resolve_daemon_bin`
//! resolves the daemon (env override, sibling of the front door, PATH),
//! talks to a running app through the control file in the app's own
//! Application Support dir, and never launches a build: a missing binary
//! is a named refusal, not a compile.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cli_args::{AppOp, AppView};

/// The bare-group refusal: clap's missing-subcommand error names no operation,
/// and this group's guide lives behind the `fno whoami guide app` topic,
/// so the refusal is the one self-teaching surface.
pub fn bare_usage() -> String {
    "fno mux app: an operation word is required: open, show, focus, shot, state or close. \
     The full guide prints at `fno whoami guide app`."
        .to_string()
}

/// Where the fno-app binary is: `FNO_APP_BIN` first (the demo rig's
/// variable, kept as the override), then a sibling of the front door
/// named `fno-app`, then PATH. `None` means every door refuses naming
/// the lookup order.
pub fn resolve_app_bin() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("FNO_APP_BIN") {
        let p = PathBuf::from(v);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(exe) = std::env::current_exe().ok() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("fno-app");
            if sibling.is_file() {
                return Some(sibling);
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("fno-app"))
        .find(|p| p.is_file())
}

/// The named refusal when no binary answers the lookup. Points at the
/// installer door and the one legal way to produce a local binary (a
/// build needs a go; the door never builds).
fn missing_binary() -> i32 {
    eprintln!(
        "fno mux app: the fno-app binary was not found (a runnable $FNO_APP_BIN, \
         beside the front door, or PATH). fno-app ships with fno from \
         `fno doctor update`; a local build needs an explicit go."
    );
    127
}

/// The macOS gate: the desktop app crate is macOS-only, so every door
/// refuses with that reason elsewhere.
fn require_macos(op: &str) -> bool {
    if cfg!(target_os = "macos") {
        return true;
    }
    eprintln!("fno mux app {op}: fno-app is a macOS app; this host is not macOS.");
    false
}

/// The session the doors address: the flag, then the app's own default
/// chain (`FNO_SERVER`, then `default`), which the app resolves itself.
pub fn session_or_default(server: Option<&String>) -> String {
    server
        .cloned()
        .or_else(|| std::env::var("FNO_SERVER").ok())
        .unwrap_or_else(|| "default".to_string())
}

/// Dispatch one `mux app` operation. Each operation returns the process exit
/// code, so the caller is one `exit` away from the shell contract.
pub fn run(op: AppOp) -> i32 {
    match op {
        AppOp::Open(args) => open(&args.mode, args.focus.as_deref(), &args.common),
        AppOp::Show(args) => show(args.view, &args.common),
        AppOp::Focus(args) => focus(&args.agent, &args.common),
        AppOp::Shot(args) => shot(&args.out, &args.theme, args.mode, args.picking),
        AppOp::State { json } => state(json.json),
        AppOp::Close { common } => close(&common),
    }
}

/// `mux app open`: launch fno-app attached to the session and print the
/// child pid, so the script that opened it can close it by pid (the
/// fleet rule: stop builds by pid, never by pattern).
fn open(mode: &Option<AppView>, focus: Option<&str>, common: &crate::cli_args::MuxCommon) -> i32 {
    if !require_macos("open") {
        return 2;
    }
    let Some(bin) = resolve_app_bin() else {
        return missing_binary();
    };
    let session = session_or_default(common.server.as_ref());
    if let Some(agent) = focus {
        // The focus row lands before the app starts, so its first poll
        // drains it; no launch flag to keep in sync with the app's argv.
        if let Err(e) = crate::app_control::append(
            &session,
            &crate::app_control::AppCommand::Focus {
                agent: agent.to_string(),
            },
        ) {
            eprintln!("fno mux app open: focus row failed: {e}");
        }
    }
    let mut cmd = Command::new(&bin);
    cmd.arg("--session").arg(&session);
    if let Some(view) = mode {
        cmd.arg("--mode").arg(view.as_spelling());
    }
    match cmd.spawn() {
        Ok(child) => {
            println!("pid={} session={}", child.id(), session);
            0
        }
        Err(e) => {
            eprintln!("fno mux app open: {} failed to launch: {e}", bin.display());
            1
        }
    }
}

/// `mux app show <view>`: switch the running app to the view over the
/// control channel. Refuses naming the session when no live app answers
/// the pid file: a row with nothing to read it would sit and fire at the
/// next launch instead.
fn show(view: AppView, common: &crate::cli_args::MuxCommon) -> i32 {
    if !require_macos("show") {
        return 2;
    }
    let session = session_or_default(common.server.as_ref());
    if running_app_pid(&session).is_none() {
        eprintln!(
            "fno mux app show: no fno-app runs for session {session}; open one with fno mux app open --server {session}"
        );
        return 3;
    }
    let cmd = crate::app_control::AppCommand::View {
        view: view.as_spelling().to_string(),
    };
    match crate::app_control::append(&session, &cmd) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("fno mux app show: {e}");
            1
        }
    }
}

/// `mux app focus <agent>`: focus one agent row in the running app.
/// Refuses naming the session when no live app answers the pid file,
/// so a typo never silently lands a row nothing will read.
fn focus(agent: &str, common: &crate::cli_args::MuxCommon) -> i32 {
    if !require_macos("focus") {
        return 2;
    }
    let session = session_or_default(common.server.as_ref());
    let Some(_pid) = running_app_pid(&session) else {
        eprintln!(
            "fno mux app focus: no fno-app runs for session {session}; open one with fno mux app open --server {session}"
        );
        return 3;
    };
    let cmd = crate::app_control::AppCommand::Focus {
        agent: agent.to_string(),
    };
    match crate::app_control::append(&session, &cmd) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("fno mux app focus: {e}");
            1
        }
    }
}

/// The pid of a live app instance for the session, from the app's own
/// pid file. `None` when the file is absent or its pid is dead; the
/// dead-pid file is cleaned up so the next open can write a fresh one.
fn running_app_pid(session: &str) -> Option<u32> {
    let mut path = crate::app_control::support_dir()?;
    path.push(format!("fno-app-{session}.pid"));
    let text = std::fs::read_to_string(&path).ok()?;
    let pid: u32 = text.trim().parse().ok()?;
    if pid_is_alive(pid) {
        Some(pid)
    } else {
        std::fs::remove_file(&path).ok();
        None
    }
}

/// Liveness by signal-0 probe: no pattern sweeps, only a pid the app
/// itself wrote to disk.
#[cfg(target_os = "macos")]
fn pid_is_alive(pid: u32) -> bool {
    // kill(pid, 0) via libc: signals nothing, reports existence.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(target_os = "macos"))]
fn pid_is_alive(_pid: u32) -> bool {
    false
}

/// The lean demo board the shot world seeds: priority lanes only, no
/// project scopes. The rig's registry rows stay rig-side (they read the
/// rig's invented usage and login stubs), so the app's shot paints the
/// board alone; a populated registry needs an envelope writer and is
/// recorded as the known v1 gap.
const SHOT_SEEDS: &[(&str, &str)] = &[
    ("p1", "Rate limit the checkout api per key"),
    ("p1", "Retry webhooks with backoff and a dead-letter queue"),
    ("p2", "Add an audit log for refunds"),
    ("p2", "Split the payments module out of the monolith"),
    ("p3", "Cache product search for 60 seconds"),
    ("p3", "Support Apple Pay on the checkout page"),
    ("p3", "Move image resizing to a queue worker"),
    ("p3", "Document the refund audit flow"),
];

/// Identity keys the caller carried: left in, a child would join the
/// world's fresh store as the real session, or read the caller's own
/// config and bin overrides into the demo. Each world child strips them.
const WORLD_STRIP_KEYS: &[&str] = &[
    "FNO_CONFIG",
    "FNO_MUX_DIR",
    "FNO_AGENT_HOME",
    "FNO_CLAIMS_ROOT",
    "FNO_SERVER",
    "FNO_SESSION",
    "FNO_PANE",
    "FNO_OWNER_BIRTH",
    "FNO_BOARD_SCOPE",
    "FNO_APP_BIN",
    "FNO_AGENT_BIN",
    "FNO_BIN",
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
];

/// One throwaway world for the shot: the temp root and the world server
/// the app attaches to. Dropping kills the server by its captured pid
/// and removes the root, so every early return tears the world down.
struct ShotWorld {
    root: PathBuf,
    server: Option<std::process::Child>,
}

impl Drop for ShotWorld {
    fn drop(&mut self) {
        if let Some(mut child) = self.server.take() {
            // The pid was captured at spawn: kill by pid, never a sweep.
            child.kill().ok();
            child.wait().ok();
        }
        std::fs::remove_dir_all(&self.root).ok();
    }
}

/// One child of the shot world: the front door or the app binary under
/// the world env. HOME and every state root point inside the temp root,
/// the frozen demo clock and host pin the frame's volatile strings, and
/// the caller's identity keys are stripped so nothing real joins the
/// demo store.
fn world_cmd(root: &Path, exe: &Path) -> Command {
    let mut cmd = Command::new(exe);
    // The strip runs BEFORE the world pins: an env_remove overrides an
    // earlier env for the same key, so the reverse order would strip the
    // world's own FNO_MUX_DIR and the server would fall back to the
    // state-layout default ($HOME/.fno/mux), one directory off from the
    // socket this door waits on.
    for key in WORLD_STRIP_KEYS {
        cmd.env_remove(key);
    }
    cmd.env("HOME", root)
        .env("FNO_MUX_DIR", root.join("mux"))
        .env("FNO_AGENT_HOME", root.join(".fno/agents"))
        .env("FNO_CLAIMS_ROOT", root.join("claimroot"))
        .env("FNO_DEMO_EPOCH", "1794096000")
        .env("FNO_DEMO_HOST", "demo-host")
        .env("FNO_CLAUDE_PROJECTS_DIR", root.join("transcripts"))
        .env("FNO_OWNER_PID", std::process::id().to_string());
    cmd
}

/// Wait for the world server's socket, 10 s budget, 100 ms poll.
fn wait_for_socket(sock: &Path) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if sock.exists() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

/// The backlog idea door's JSON stdout, typed so the id read stays a
/// struct field (R1: parse once, no string-key reads).
#[derive(serde::Deserialize)]
struct IdeaSeedOut {
    id: Option<String>,
}

/// The demo chat session: the transcript file the seed writes and the
/// adopt seed registers, one uuid both sides name.
const DEMO_CHAT_SESSION: &str = "00000000-0000-4000-8000-000000000001";

/// Seed the world's graph through the front door's own backlog, one
/// deterministic mint per card. Any refusal tears the world down, so a
/// half-seeded board never shoots.
fn seed_world(root: &Path, front: &Path) -> Option<String> {
    let mut first_id = None;
    for (i, (prio, title)) in SHOT_SEEDS.iter().enumerate() {
        let mut cmd = world_cmd(root, front);
        cmd.args([
            "backlog",
            "idea",
            title,
            "-p",
            prio,
            "--difficulty",
            "medium",
        ])
        .env("FNO_DEMO_ID_SEED", format!("node-{}", i + 1))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
        match cmd.spawn().and_then(|child| child.wait_with_output()) {
            Ok(out) if out.status.success() => {
                if i == 0 {
                    first_id = serde_json::from_slice::<IdeaSeedOut>(&out.stdout)
                        .ok()
                        .and_then(|seed| seed.id);
                }
            }
            Ok(out) => {
                eprintln!(
                    "fno mux app shot: the seed refused: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                return None;
            }
            Err(e) => {
                eprintln!("fno mux app shot: the seed failed to run: {e}");
                return None;
            }
        }
    }
    first_id
}

/// Claim the first seeded node as a demo seat, so the board's seats
/// panel carries a real row with work. The transcript is durable
/// evidence, so the adopt door then writes the registry row the
/// sideline and seat readers consume.
fn seed_fleet(root: &Path, front: &Path, node_id: &str) -> bool {
    let mut cmd = world_cmd(root, front);
    cmd.args([
        "agent",
        "claim",
        "acquire",
        &format!("node:{node_id}"),
        "--holder",
        "demo-worker",
    ])
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped());
    let claim = match cmd.spawn().and_then(|child| child.wait_with_output()) {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            eprintln!(
                "fno mux app shot: the claim seed refused: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            false
        }
        Err(e) => {
            eprintln!("fno mux app shot: the claim seed failed to run: {e}");
            false
        }
    };
    let mut cmd = world_cmd(root, front);
    cmd.args(["agent", "adopt", DEMO_CHAT_SESSION])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let adopt = match cmd.spawn().and_then(|child| child.wait_with_output()) {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            eprintln!(
                "fno mux app shot: the adopt seed refused: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            false
        }
        Err(e) => {
            eprintln!("fno mux app shot: the adopt seed failed to run: {e}");
            false
        }
    };
    claim && adopt
}

/// One demo question in the world's journal, so the board's questions
/// panel carries a real row. The FILE is the contract: the shape the
/// event store writes.
fn seed_question(root: &Path) -> bool {
    let ts = chrono::Utc::now().to_rfc3339();
    let row = format!(
        r#"{{"ts":"{ts}","type":"operator_question","source":"rust","data":{{"question_id":"q-demo-0001","question":"Which machine should host the demo deploy?","ask":"Pick the demo host; the deploy preview follows the pick.","asker":"demo-worker","node":"none","context":{{"blocked_because":"The demo host choice has not been recorded.","unknowns":"Machine capacity and tailnet reachability have not been compared."}},"subject":"demo-deploy-host","blocks":[]}}}}"#
    );
    match std::fs::write(root.join(".fno/questions.jsonl"), format!("{row}\n")) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("fno mux app shot: the question seed failed to write: {e}");
            false
        }
    }
}

/// A claude-shaped transcript in the world's projects dir, so the chat
/// view renders a conversation. The FILE is the contract: rows carry
/// type + message.content like the real transcripts the server reads.
fn seed_transcript(root: &Path) -> bool {
    let dir = root.join("transcripts/demo-project");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("fno mux app shot: the transcript seed failed to create its dir: {e}");
        return false;
    }
    let session = DEMO_CHAT_SESSION;
    let rows = concat!(
        r#"{"parentUuid":null,"isSidechain":false,"type":"user","uuid":"u1","message":{"role":"user","content":"What does the shot door build?"},"sessionId":"00000000-0000-4000-8000-000000000001"}"#,
        "\n",
        r#"{"parentUuid":null,"isSidechain":false,"type":"assistant","uuid":"a1","message":{"id":"m1","role":"assistant","content":[{"type":"text","text":"It builds a throwaway mux world, seeds it, opens a one-percent-alpha window above every normal window, and hands the buffer to the capture tool."}]},"sessionId":"00000000-0000-4000-8000-000000000001"}"#,
        "\n",
        r#"{"parentUuid":null,"isSidechain":false,"type":"user","uuid":"u2","message":{"role":"user","content":"And the offscreen proof?"},"sessionId":"00000000-0000-4000-8000-000000000001"}"#,
        "\n",
        r#"{"parentUuid":null,"isSidechain":false,"type":"assistant","uuid":"a2","message":{"id":"m2","role":"assistant","content":[{"type":"text","text":"Frontmost identical across the three reads, no keystrokes, no Dock bounce, and the frame lands. The window composites invisible the whole time."}]},"sessionId":"00000000-0000-4000-8000-000000000001"}"#,
        "\n",
    );
    match std::fs::write(dir.join(format!("{session}.jsonl")), rows) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("fno mux app shot: the transcript seed failed to write: {e}");
            false
        }
    }
}

/// `mux app shot`: one offscreen demo frame as a png, from a world this
/// door builds, seeds, shoots and tears down. The world never touches
/// the caller's own state: its own HOME, mux dir, agents home and graph
/// store, all under /tmp (a socket path must stay under 104 bytes on
/// macOS), and every child strips the caller's identity keys.
fn shot(out: &str, theme: &str, mode: Option<AppView>, picking: bool) -> i32 {
    if !require_macos("shot") {
        return 2;
    }
    let Some(bin) = resolve_app_bin() else {
        return missing_binary();
    };
    let Ok(front) = std::env::current_exe() else {
        eprintln!("fno mux app shot: the front door's own path is unreadable");
        return 1;
    };
    // Absolutize the out path before the app chdirs into its theme pin:
    // a relative --out would resolve inside the pin dir and die with it.
    let out_abs = std::path::absolute(out).unwrap_or_else(|_| PathBuf::from(out));
    let root = std::path::Path::new("/tmp").join(format!("fno-shot-world-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let mut world = ShotWorld { root, server: None };
    for dir in [
        "mux",
        ".fno/agents",
        "code/checkout",
        "claimroot/.fno/claims",
        "transcripts",
    ] {
        if let Err(e) = std::fs::create_dir_all(world.root.join(dir)) {
            eprintln!("fno mux app shot: the world dir failed: {e}");
            return 1;
        }
    }
    if let Err(e) = std::fs::write(world.root.join(".fno/config.toml"), "") {
        eprintln!("fno mux app shot: the world config failed: {e}");
        return 1;
    }
    let mut server_cmd = world_cmd(&world.root, &front);
    server_cmd
        .args(["mux", "server", "--server", "default"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    world.server = match server_cmd.spawn() {
        Ok(child) => Some(child),
        Err(e) => {
            eprintln!("fno mux app shot: the world server failed to start: {e}");
            return 1;
        }
    };
    if !wait_for_socket(&world.root.join("mux/default.sock")) {
        eprintln!("fno mux app shot: the world server never opened its socket");
        return 1;
    }
    let Some(first_node) = seed_world(&world.root, &front) else {
        return 1;
    };
    // The transcript lands first: the adopt seed reads it as evidence.
    let transcript = seed_transcript(&world.root);
    let fleet = seed_fleet(&world.root, &front, &first_node);
    let question = seed_question(&world.root);
    if !(transcript && fleet && question) {
        return 1;
    }
    let mut cmd = world_cmd(&world.root, &bin);
    cmd.arg("--shot")
        .arg("--shot-out")
        .arg(&out_abs)
        .arg("--shot-theme")
        .arg(theme)
        .arg("--session")
        .arg("default");
    if let Some(view) = mode {
        cmd.arg("--shot-mode").arg(view.as_spelling());
        if matches!(view, AppView::Chat) {
            cmd.arg("--chat").arg("newest");
        }
    }
    if picking {
        cmd.arg("--shot-picking");
    }
    match cmd.status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("fno mux app shot: {} failed to run: {e}", bin.display());
            1
        }
    }
}

/// The state file mirror: what the app writes on quit, typed so the door
/// prints a validated doc instead of echoing file bytes. Unknown or extra
/// fields stay out of the printed doc; `pinned` prints verbatim because
/// the session-key shape is the app crate's to evolve.
#[derive(serde::Deserialize, serde::Serialize)]
struct SavedStateMirror {
    version: u32,
    view: String,
    bounds: SavedBoundsMirror,
    #[serde(default)]
    pinned: Option<serde_json::Value>,
    agents_open: bool,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct SavedBoundsMirror {
    mode: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// `mux app state`: read the app's saved window state as JSON. A missing
/// or corrupt file prints a JSON null and exits 0, mirroring the app's
/// own read of damaged state (a damaged file opens the default window,
/// it never refuses to launch).
fn state(_json: bool) -> i32 {
    // The flag is accepted for script compatibility; JSON is the one
    // output shape either way.
    let Some(path) = crate::app_control::state_path() else {
        println!("null");
        return 0;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            println!("null");
            return 0;
        }
    };
    match serde_json::from_str::<SavedStateMirror>(&text) {
        Ok(s) => match serde_json::to_string_pretty(&s) {
            Ok(out) => {
                println!("{out}");
                0
            }
            Err(e) => {
                eprintln!("fno mux app state: encode failed: {e}");
                1
            }
        },
        Err(_) => {
            println!("null");
            0
        }
    }
}

/// `mux app close`: ask the running app to quit; exits 0 without
/// writing when none runs, so a stale Quit can never fire at the next
/// launch.
fn close(common: &crate::cli_args::MuxCommon) -> i32 {
    let session = session_or_default(common.server.as_ref());
    if running_app_pid(&session).is_none() {
        return 0;
    }
    match crate::app_control::append(&session, &crate::app_control::AppCommand::Quit) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("fno mux app close: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_usage_names_the_six_commands() {
        let line = bare_usage();
        assert!(
            line.contains("open, show, focus, shot, state or close"),
            "{line}"
        );
        assert!(line.contains("fno whoami guide app"), "{line}");
    }

    #[test]
    fn view_spellings_match_the_app_parse() {
        assert_eq!(AppView::Chat.as_spelling(), "chat");
        assert_eq!(AppView::Terminal.as_spelling(), "terminal");
        assert_eq!(AppView::Orchestration.as_spelling(), "orchestration");
        assert_eq!(AppView::Mods.as_spelling(), "mods");
    }

    #[test]
    fn session_resolution_prefers_flag_then_env_then_default() {
        // No env, no flag: the app's own default chain answers.
        unsafe { std::env::remove_var("FNO_SERVER") };
        assert_eq!(session_or_default(None), "default");
        assert_eq!(session_or_default(Some(&"demo".to_string())), "demo");
    }
}
