//! `fno-agent-daemon` entrypoint (Wave 3). Argv parsing -> `daemon::run`.
//!
//! Usage:
//! ```text
//! fno-agent-daemon                    # start (foreground); lazy-exits when idle
//! fno-agent-daemon --home <dir>       # name the home in argv; must agree with
//!                                      # FNO_AGENT_HOME or the daemon refuses
//! ```
//! Any other argv refuses (exit 2) before the runtime builds: the daemon
//! takes no subcommands, so a caller that mis-resolved the daemon binary (a
//! hook probe hop that fell back to current_exe) fails loud instead
//! of booting a resident daemon wearing the hop's argv.
//! The client lazy-starts this detached on first need; running it directly is
//! for debugging.

use fno_agent::daemon::{run, DaemonOptions};
use fno_agent::paths::AgentsHome;
use fno_agent::pid::Pid;
use std::time::Duration;

fn main() {
    fno_agent::legacy_env::adopt();
    fno_agent::chokepoint_events::install();
    // `version [--json]`: report the baked-in build rev so `fno doctor update` can
    // verify this bin is the SAME build as its triad siblings, not just present.
    // Execs cheaply and returns without touching a running daemon or the runtime.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(
        args.first().map(String::as_str),
        Some("version" | "-V" | "--version")
    ) {
        fno_agent::version::print_version(fno_agent::json_output::requested(&args));
        return;
    }

    // Any argv the daemon does not own is a caller that resolved this binary
    // by mistake (a hook probe hop execed `fno-agent-daemon pr-worktree`;
    // the old silent ignore booted a full resident daemon with that argv).
    // Refuse before the runtime builds, with the usage beside the cause.
    let (home_arg, unknown) = parse_daemon_args(&args);
    if let Some(bad) = unknown {
        eprintln!("usage: fno-agent-daemon [--home <dir>] | version [--json]");
        eprintln!(
            "fno-agent-daemon: unknown argument {bad:?}; the daemon takes no subcommands \
             (client doors live in fno-agent) - refusing to start"
        );
        std::process::exit(2);
    }

    // A hold is a clean exit: launchd's KeepAlive relaunches only a failed one.
    if let Some(hold) = fno_agent::daemon_hold::read_hold(&fno_agent::paths::AgentsHome::from_env())
    {
        eprintln!("fno-agent-daemon: not starting; {}", hold.refusal());
        return;
    }

    // Test harnesses may launch children with SIGTERM blocked. The daemon owns
    // its SIGTERM listener below, so do not inherit a mask that makes graceful
    // shutdown permanently pending.
    #[cfg(unix)]
    unsafe {
        let mut set = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
    }

    // SIGHUP, SIGINT and SIGQUIT have no graceful arm and kill the process
    // with no trace. Name the signal on stderr, then die the way the default
    // action would. SIGKILL cannot be caught; SIGTERM logs from the loop.
    #[cfg(unix)]
    unsafe {
        for sig in [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT] {
            libc::signal(sig, log_fatal_signal as *const () as libc::sighandler_t);
        }
    }

    // Env mutation is only sound while the process is single-threaded: fill
    // the build-dir env before the runtime below spawns threads, so the heal
    // lane's `fno doctor update` and every other child inherits it even when
    // the daemon's parent passed no value. Only a footnote-checkout cwd
    // earns the fill; a daemon started from a foreign repo keeps its
    // children's build env untouched (the install path re-resolves its own
    // value). The daemon starts no sccache server: the rustc wrapper's own
    // client does, with the fleet cache env the wrapper exports.
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    fno_agent::cargo_build_dirs::fill_build_dir_env(&cwd);
    fno_agent::cargo_build_dirs::fill_sccache_env(&cwd);

    // A failed daemon must surface a non-zero exit and a clear stderr line; it
    // must never panic silently (Silent-Failure-Hunter posture).
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("fno-agent-daemon: cannot build tokio runtime: {e}");
            std::process::exit(1);
        }
    };

    // `--home <path>`: names the home in argv so a stray daemon can be
    // attributed in ps. The env stays the resolution source; a --home that
    // DISAGREES with the env is a daemon about to serve a home nobody expects,
    // so refuse it loudly rather than silently prefer one. No --home keeps the
    // env-only behavior (a daemon started by hand for debugging).
    if let Some(home_arg) = home_arg {
        let env_home = AgentsHome::from_env();
        if !fno_agent::paths::same_path(std::path::Path::new(&home_arg), env_home.root()) {
            eprintln!(
                "fno-agent-daemon: --home {} disagrees with FNO_AGENT_HOME {}; refusing to start",
                home_arg,
                env_home.root().display()
            );
            std::process::exit(2);
        }
    }

    if let Some((owner_pid, owner_birth, owner_session)) =
        fno_agent::test_run::daemon_owner_from_env()
    {
        if !fno_agent::test_run::owner_alive(owner_pid, owner_birth) {
            eprintln!(
                "fno-agent-daemon: declared owner pid={owner_pid} birth={owner_birth} is not alive; refusing to start"
            );
            std::process::exit(3);
        }
        let armed = fno_agent::test_run::spawn_owner_watchdog(
            owner_pid,
            owner_birth,
            "fno-daemon-owner",
            move || {
                if let Some(owner_session) = owner_session {
                    eprintln!(
                        "fno-agent-daemon: owner_reaped owner_pid={owner_pid} owner_birth={owner_birth} owner_session={owner_session}"
                    );
                } else {
                    eprintln!(
                        "fno-agent-daemon: test_owner_reaped owner_pid={owner_pid} owner_birth={owner_birth}"
                    );
                }
                // SIGTERM to self enters the existing graceful shutdown arm.
                Pid::current().signal(libc::SIGTERM);
            },
        );
        if !armed {
            eprintln!("fno-agent-daemon: refusing to start without an owner watchdog");
            std::process::exit(3);
        }
    } else if fno_agent::process_owner::OWNER_LEASE_KEYS
        .iter()
        .any(|key| std::env::var_os(key).is_some())
    {
        eprintln!("fno-agent-daemon: refusing incomplete FNO_OWNER_* lease");
        std::process::exit(3);
    }

    let mut home = AgentsHome::from_env();
    let mut opts = DaemonOptions::default();
    // Allow an idle-exit override (seconds) via env for tests / tuning.
    if let Ok(s) = std::env::var("FNO_AGENT_IDLE_EXIT_SECS") {
        if let Ok(secs) = s.parse::<u64>() {
            opts.idle_exit = Duration::from_secs(secs);
        }
    }
    // Retirement sweep config: resolve config.agents.retire_grace_s
    // and reap-receipt retention (env > FNO_CONFIG > project > global >
    // defaults) at sweep time -- the idle tick reads opts.agents_config_cwd
    // (the anchor pinned below), not a pre-resolved Duration. A
    // global ~/.fno knob is read via the global fallback regardless.
    // Pin the daemon's cwd for life, before any child can inherit it. The
    // daemon keeps whoever lazy-started it as its cwd; when that dir is a
    // worktree the merge reaper later deletes it, and every child spawned
    // without an explicit cwd then fails: roster reads exit 1 and
    // reconcile-style runs hit FileNotFoundError in os.getcwd, each cured
    // only by a manual restart. The canonical checkout is never reaped, so
    // anchor there; outside any repo, the agents home.
    let launch_dir = std::env::current_dir().ok();
    // A relative FNO_AGENT_HOME resolves against the launch cwd. Absolutize
    // it BEFORE the chdir below, or the daemon would bind its socket under
    // the anchor while the client still waits under the launch dir.
    home = AgentsHome::at(absolutize_home(launch_dir.as_deref(), home.root()));
    if let Err(error) = fno_agent::role_migration::run() {
        eprintln!("role migration: {error}");
        std::process::exit(2);
    }
    let _ = home.ensure_root();
    let anchor = daemon_anchor(launch_dir.as_deref(), home.root());
    match std::env::set_current_dir(&anchor) {
        Ok(()) => opts.agents_config_cwd = anchor,
        Err(e) => {
            eprintln!("fno-agent-daemon: cannot enter {}: {e}", anchor.display());
        }
    }
    // Badge -> OS notification knobs: config.mux.notify_on_blocked
    // (default ON) / notify_on_done (default OFF), read from the same cwd.
    opts.notify_on_blocked =
        fno_agent::agents_config::notify_on_blocked_enabled(&opts.agents_config_cwd);
    opts.notify_on_done = fno_agent::agents_config::notify_on_done_enabled(&opts.agents_config_cwd);
    // Opt out of the startup reconcile sweep for the fastest cold start
    // (Architecture B, plan). Any non-empty value disables it.
    if std::env::var("FNO_AGENT_NO_STARTUP_RECONCILE")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        opts.reconcile_on_start = false;
    }

    // State-root migration: move the layout-table entries into their
    // subfolders BEFORE the daemon opens a store. Best effort; a refusal
    // retries on the daemon's reclaim lane.
    fno_agent::state_layout::run_at_daemon_start(&home);

    let outcome = rt.block_on(run(home, opts));

    // Bound the wind-down instead of letting `rt` drop at the end of `main`.
    // Dropping a runtime WAITS for every already-started `spawn_blocking` task,
    // and the sweeps are exactly that: a gc sweep over a large roster shells one
    // child per row and runs for minutes. So a plain drop kept the process alive
    // long after SIGTERM had been received and `daemon_exited {"clean": true}`
    // had been written -- an event log claiming an exit that had not happened,
    // which is a worse signal for the watchdog than a slow exit. The sweeps own
    // nothing that must survive: each is a read plus an advisory-locked write
    // that either landed or did not, and the next daemon redoes it.
    rt.shutdown_timeout(std::time::Duration::from_secs(5));

    if let Err(e) = outcome {
        eprintln!("fno-agent-daemon: {e}");
        std::process::exit(1);
    }
}

/// One walk over the daemon's argv: the `--home` value when present, and the
/// first argument the daemon does not own. `version` and its spellings are
/// matched earlier and never reach here. A dangling `--home` stays legal
/// (env-only start, as before). The flag and its value travel through the
/// same walk, so the gate's accept set and the home extract cannot drift.
fn parse_daemon_args(args: &[String]) -> (Option<String>, Option<&str>) {
    let mut home = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--home" {
            home = it.next().cloned();
            continue;
        }
        if let Some(v) = a.strip_prefix("--home=") {
            home = Some(v.to_string());
            continue;
        }
        return (home, Some(a));
    }
    (home, None)
}

/// The dir the daemon runs in for life. A worktree gets reaped under a live
/// daemon; its canonical checkout does not. Outside a repo, or when the
/// launch dir cannot be read, the agents home is the anchor.
fn daemon_anchor(launch: Option<&std::path::Path>, home: &std::path::Path) -> std::path::PathBuf {
    if let Some(root) = launch.and_then(fno_agent::paths::canonical_repo_root) {
        return root;
    }
    if home.is_absolute() {
        home.to_path_buf()
    } else {
        launch.unwrap_or(std::path::Path::new(".")).to_path_buf()
    }
}

/// Absolute form of the agents home, resolved against the launch cwd. A
/// relative home must be pinned before the daemon enters its anchor, or
/// every later home path would resolve against the anchor instead and split
/// daemon and client onto two different homes.
fn absolutize_home(launch: Option<&std::path::Path>, home: &std::path::Path) -> std::path::PathBuf {
    if home.is_absolute() {
        return home.to_path_buf();
    }
    launch.unwrap_or(std::path::Path::new(".")).join(home)
}

/// Async-signal-safe: one `write(2)` of a fixed line, then the default action.
#[cfg(unix)]
extern "C" fn log_fatal_signal(sig: libc::c_int) {
    let line: &[u8] = match sig {
        libc::SIGHUP => b"fno-agent-daemon: exiting reason=sighup\n",
        libc::SIGINT => b"fno-agent-daemon: exiting reason=sigint\n",
        _ => b"fno-agent-daemon: exiting reason=sigquit\n",
    };
    unsafe {
        libc::write(2, line.as_ptr().cast(), line.len());
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "fno-daemon-anchor-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} in {dir:?} did not run: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} in {dir:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_ok()
    }

    #[test]
    fn daemon_anchor_resolves_the_canonical_root_from_a_linked_worktree() {
        if !git_available() {
            return;
        }
        let base = tmp_dir("wt");
        let main = base.join("main");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q"]);
        git(&main, &["config", "user.email", "t@t"]);
        git(&main, &["config", "user.name", "t"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let linked = base.join("wt");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                linked.to_str().unwrap(),
                "-b",
                "feat",
            ],
        );
        let home = base.join("home");
        assert_eq!(
            daemon_anchor(Some(&linked), &home),
            std::fs::canonicalize(&main).unwrap()
        );
        std::fs::remove_dir_all(&base).ok();
    }
}
