use std::path::Path;
use std::process::Command;

/// Spawn `fno --server <socket>` detached: its own session (setsid) so the
/// server never receives the terminal's SIGHUP, stderr to a per-session log.
/// Two clients racing here both spawn; the bind is the lock, the losing
/// server exits 0, and both clients attach to the winner (AC4-EDGE).
pub(super) fn spawn_server(path: &Path, attaching: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find own binary: {e}"))?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(super::log_path(path))
        .map_err(|e| format!("cannot open server log: {e}"))?;
    let mut cmd = crate::process_admission::std_command(exe);
    cmd.arg("--server")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log);
    stamp_sandbox_owner(&mut cmd, attaching);
    // The pure-Rust server does not read config.toml. Resolve this once on the
    // client, which already pays for the bounded config lookup.
    if std::env::var_os("FNO_MUX_SHELL_INTEGRATION").is_none() && shell_integration_off() {
        cmd.env("FNO_MUX_SHELL_INTEGRATION", "off");
    }
    if std::env::var_os("FNO_BOARD_SCOPE").is_none() {
        // The server must not shell out for config on its SIGTERM-critical
        // startup path; the client resolves the scope and passes it by env.
        let (scope, _why) =
            crate::backlog_view::resolve_board_scope(crate::config_defaults::lookup_key);
        cmd.env(
            "FNO_BOARD_SCOPE",
            crate::backlog_view::board_scope_wire(&scope),
        );
    }
    if std::env::var_os("FNO_MUX_WINDOW_SIZE").is_none() {
        // Same latch as the scope above: `mux.window_size` rides the env, an
        // unset or unknown value leaves the server on its `latest` default.
        if let Some(word) = crate::config_defaults::lookup_key("mux.window_size")
            .as_deref()
            .and_then(window_size_word)
        {
            cmd.env("FNO_MUX_WINDOW_SIZE", word);
        }
    }
    if std::env::var_os("FNO_MUX_SCROLLBACK_LINES").is_none() {
        // `mux.scrollback_lines` rides the env the same way.
        if let Some(lines) = crate::config_defaults::lookup_key("mux.scrollback_lines")
            .as_deref()
            .and_then(scrollback_lines_value)
        {
            cmd.env("FNO_MUX_SCROLLBACK_LINES", lines.to_string());
        }
    }
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    crate::process_admission::std_spawn(&mut cmd)
        .map(|_| ())
        .map_err(|e| format!("cannot spawn the mux server: {e}"))
}

/// Spawn a detached server at `path` and wait for its socket to accept: the
/// desktop app's "start the mux" door. A server already there is not
/// replaced; the bind is the lock, so a lost race still ends connected.
pub(crate) fn start_server_at(path: &Path) -> std::io::Result<()> {
    use super::{log_path, ATTACH_CONNECT_TIMEOUT, SPAWN_CONNECT_TIMEOUT};
    use crate::proto;
    use std::time::{Duration, Instant};
    proto::ensure_mux_dir()?;
    if proto::connect_unix_timeout(path, ATTACH_CONNECT_TIMEOUT).is_ok() {
        return Ok(());
    }
    spawn_server(path, false).map_err(std::io::Error::other)?;
    let deadline = Instant::now() + SPAWN_CONNECT_TIMEOUT;
    loop {
        match proto::connect_unix_timeout(path, ATTACH_CONNECT_TIMEOUT) {
            Ok(_) => return Ok(()),
            Err(e) if Instant::now() >= deadline => {
                return Err(std::io::Error::other(format!(
                    "server did not come up at {} ({e}); check {}",
                    path.display(),
                    log_path(path).display()
                )));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// An owner-bound mux server. An explicit `FNO_OWNER_SESSION` is the sandbox
/// marker; without it the attaching client's own ambient harness session
/// names the owner, so a lazy server binds in any harness (claude, codex,
/// opencode, gemini, pi) with no third-party plugin. `CODEX_COMPANION_SESSION_ID`
/// - the openai-codex Claude plugin's stamp, which carries the Claude session
/// id - stays as the last fallback. A caller may pass an outer script's pid;
/// otherwise an attaching client owns the detached server for the duration
/// of its attach.
pub(super) fn stamp_sandbox_owner(cmd: &mut Command, attaching: bool) {
    const OWNER_KEYS: [&str; 3] = ["FNO_OWNER_PID", "FNO_OWNER_BIRTH", "FNO_OWNER_SESSION"];
    for key in OWNER_KEYS {
        cmd.env_remove(key);
    }
    let session = resolve_owner_session();
    let lease = owner_lease(
        session,
        std::env::var("FNO_OWNER_PID").ok(),
        std::env::var("FNO_OWNER_BIRTH").ok(),
        attaching,
    );
    if let Some((owner_pid, birth, session)) = lease {
        cmd.env("FNO_OWNER_PID", owner_pid.to_string())
            .env("FNO_OWNER_BIRTH", birth.to_string())
            .env("FNO_OWNER_SESSION", session);
    }
}

/// The owner a spawned server binds to, or `None` for a shared ownerless one.
/// A one-shot client such as `pane run` exits right after the spawn, so with
/// no declared `FNO_OWNER_PID` it must not name itself: the server would shut
/// down owner-gone one tick later and take every pane with it.
fn owner_lease(
    session: Option<String>,
    pid_raw: Option<String>,
    birth_raw: Option<String>,
    attaching: bool,
) -> Option<(u32, u64, String)> {
    let session = session?;
    let owner_pid = match pid_raw {
        Some(raw) => raw.parse::<u32>().ok().filter(|pid| *pid > 1)?,
        None if attaching => std::process::id(),
        None => return None,
    };
    let birth = match birth_raw {
        Some(raw) => raw.parse::<u64>().ok().filter(|birth| *birth > 0)?,
        None => crate::proto::pid_start_time(owner_pid)?,
    };
    (crate::proto::pid_start_time(owner_pid) == Some(birth)).then_some((owner_pid, birth, session))
}

/// The session id an owner stamp carries, or `None` for an ownerless server.
/// Highest precedence first: the explicit `FNO_OWNER_SESSION` sandbox marker,
/// then fno's own ambient resolution - the same [`crate::backlog::join_marker::join_candidate`]
/// walk the front-door join uses, so a plain harness session needs no plugin -
/// and finally `CODEX_COMPANION_SESSION_ID` for callers predating the walk.
fn resolve_owner_session() -> Option<String> {
    owner_session_choice(
        std::env::var("FNO_OWNER_SESSION").ok(),
        crate::backlog::join_marker::join_candidate(&|k| std::env::var(k).ok()).map(|(_, sid)| sid),
        std::env::var("CODEX_COMPANION_SESSION_ID").ok(),
    )
}

/// The pure precedence: an unusable id falls through to the next arm, so a
/// slash-bearing ambient id never blocks the plugin export.
fn owner_session_choice(
    fno_owner: Option<String>,
    ambient: Option<String>,
    codex: Option<String>,
) -> Option<String> {
    [fno_owner, ambient, codex]
        .into_iter()
        .filter(|v| valid_owner_session(v))
        .next()
}

fn valid_owner_session(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= 128
        && session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Whether the interactive path must disable OSC 133 injection. Fail-open
/// through the native config read ([`crate::config_defaults::lookup_key`]):
/// an absent file, an unreadable one, or a non-`off` value all leave
/// injection on (the default). Runs synchronously inside `spawn_server`,
/// *before* the client's spawn-connect wait loop exists - the native read is
/// one bounded file parse, where the retired `fno config get` subprocess
/// cold-started the Python shim and could freeze `fno` startup for seconds.
fn shell_integration_off() -> bool {
    crate::config_defaults::lookup_key("mux.shell_integration")
        .as_deref()
        .map(config_says_off)
        .unwrap_or(false)
}

/// A `mux.window_size` value the server understands, else `None`.
pub(crate) fn window_size_word(raw: &str) -> Option<&'static str> {
    ["smallest", "latest", "largest"]
        .into_iter()
        .find(|word| *word == raw.trim())
}

/// A `mux.scrollback_lines` value the server accepts: a positive count.
pub(crate) fn scrollback_lines_value(raw: &str) -> Option<usize> {
    raw.trim().parse().ok().filter(|&n: &usize| n > 0)
}

/// The one off-switch, matched exactly like the Rust pane-spawn side
/// (`pty::integration_disabled`): only a trimmed `off` disables injection.
pub(crate) fn config_says_off(stdout: &str) -> bool {
    stdout.trim() == "off"
}

#[cfg(test)]
mod spawn_config_tests {
    use super::{
        config_says_off, owner_lease, owner_session_choice, scrollback_lines_value,
        window_size_word,
    };

    /// The env bridge from config.toml to the interactive server's
    /// latched env: must mirror `pty::integration_disabled` - exactly
    /// `off`, never case-loose, never the default.
    #[test]
    fn config_says_off_matches_only_trimmed_off() {
        assert!(config_says_off("off"));
        assert!(config_says_off("off\n"));
        assert!(config_says_off("  off  "));
        assert!(!config_says_off("mux-panes\n"));
        assert!(!config_says_off("OFF"));
        assert!(!config_says_off(""));
        // The window-size latch passes only the three policy words.
        assert_eq!(window_size_word(" smallest\n"), Some("smallest"));
        assert_eq!(window_size_word("largest"), Some("largest"));
        assert_eq!(window_size_word("Latest"), None);
        assert_eq!(window_size_word(""), None);
        // The scrollback latch passes only a positive count.
        assert_eq!(scrollback_lines_value(" 20000\n"), Some(20_000));
        assert_eq!(scrollback_lines_value("0"), None);
        assert_eq!(scrollback_lines_value("20k"), None);
    }

    /// A one-shot client never owns the server it leaves behind; an
    /// attaching client and a declared owner pid still bind it.
    #[test]
    fn only_an_attaching_client_or_a_declared_pid_owns_the_server() {
        let session = || Some("agent-session".to_string());
        let me = std::process::id();
        assert_eq!(owner_lease(session(), None, None, false), None);
        let attached = owner_lease(session(), None, None, true).expect("attach binds");
        assert_eq!(attached.0, me);
        let declared = owner_lease(session(), Some(me.to_string()), None, false);
        assert_eq!(declared.map(|lease| lease.0), Some(me));
        assert_eq!(owner_lease(None, None, None, true), None);
    }

    /// A session without the codex plugin still stamps: the ambient harness
    /// identity resolves. The explicit sandbox marker wins, the companion id
    /// drops to last fallback, and an unusable id falls through.
    #[test]
    fn owner_session_resolves_from_ambient_identity_without_the_codex_plugin() {
        let choice = |fno_owner, ambient, codex| owner_session_choice(fno_owner, ambient, codex);
        // The node's case: plain opencode and pi sessions, no plugin env.
        assert_eq!(
            choice(None, Some("oc-1".to_string()), None),
            Some("oc-1".to_string())
        );
        assert_eq!(
            choice(None, Some("pi.1".to_string()), None),
            Some("pi.1".to_string())
        );
        // Precedence: explicit marker > ambient > companion.
        assert_eq!(
            choice(
                Some("sandbox-1".to_string()),
                Some("claude-1".to_string()),
                Some("companion-1".to_string())
            ),
            Some("sandbox-1".to_string())
        );
        assert_eq!(
            choice(
                None,
                Some("claude-1".to_string()),
                Some("companion-1".to_string())
            ),
            Some("claude-1".to_string())
        );
        // Last fallback: the companion alone still binds, today's path kept.
        assert_eq!(
            choice(None, None, Some("companion-1".to_string())),
            Some("companion-1".to_string())
        );
        // An ambient id the stamp cannot carry falls through to the companion.
        assert_eq!(
            choice(
                None,
                Some("has/slash".to_string()),
                Some("companion-1".to_string())
            ),
            Some("companion-1".to_string())
        );
        // No identity at all: the ownerless shared server a bare terminal
        // gets today.
        assert_eq!(choice(None, None, None), None);
    }
}
