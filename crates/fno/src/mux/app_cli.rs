//! The `fno mux app` doors: an agent's drive surface for the fno-app
//! desktop app. The CLI resolves the binary the way `resolve_daemon_bin`
//! resolves the daemon (env override, sibling of the front door, PATH),
//! talks to a running app through the control file in the app's own
//! Application Support dir, and never launches a build: a missing binary
//! is a named refusal, not a compile.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cli_args::{AppOp, AppView};

/// The bare-group refusal: clap's missing-subcommand error names no verb,
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
fn require_macos(verb: &str) -> bool {
    if cfg!(target_os = "macos") {
        return true;
    }
    eprintln!("fno mux app {verb}: fno-app is a macOS app; this host is not macOS.");
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

/// Dispatch one `mux app` operation. Each verb returns the process exit
/// code, so the caller is one `exit` away from the shell contract.
pub fn run(op: AppOp) -> i32 {
    match op {
        AppOp::Open(args) => open(&args.mode, args.focus.as_deref(), &args.common),
        AppOp::Show(args) => show(args.view, &args.common),
        AppOp::Focus(args) => focus(&args.agent, &args.common),
        AppOp::Shot(args) => shot(&args.out, &args.theme, args.mode),
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
    let mut cmd = Command::new(&bin);
    cmd.arg("--session").arg(&session);
    if let Some(view) = mode {
        cmd.arg("--mode").arg(view.as_spelling());
    }
    if let Some(agent) = focus {
        cmd.arg("--focus").arg(agent);
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
/// control channel, or open it there when none runs.
fn show(view: AppView, common: &crate::cli_args::MuxCommon) -> i32 {
    let _ = (view, common);
    eprintln!("fno mux app show: not wired yet in this branch");
    2
}

/// `mux app focus <agent>`: focus one agent row in the running app.
fn focus(agent: &str, common: &crate::cli_args::MuxCommon) -> i32 {
    let _ = (agent, common);
    eprintln!("fno mux app focus: not wired yet in this branch");
    2
}

/// `mux app shot`: one offscreen demo frame as a png.
fn shot(out: &str, theme: &str, mode: Option<AppView>) -> i32 {
    let _ = (out, theme, mode);
    eprintln!("fno mux app shot: not wired yet in this branch");
    2
}

/// `mux app state`: read the app's saved window state as JSON.
fn state(json: bool) -> i32 {
    let _ = json;
    eprintln!("fno mux app state: not wired yet in this branch");
    2
}

/// `mux app close`: ask the running app to quit; exits 0 when none runs.
fn close(common: &crate::cli_args::MuxCommon) -> i32 {
    let _ = common;
    eprintln!("fno mux app close: not wired yet in this branch");
    2
}

/// True when `path` exists as a file (a tiny helper the doors share).
#[cfg_attr(test, allow(dead_code))]
fn is_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_usage_names_verbs_and_guide_door() {
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
