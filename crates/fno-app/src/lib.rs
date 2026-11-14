//! `fno-app`: the macOS app. A thin client of the mux server: it draws
//! the `Frame` cells the server sends over the session socket and runs no
//! terminal emulator of its own.

mod boxdraw;
mod chat;
mod grid;
mod hq;
pub mod keys;
mod left_pane;
pub mod link;
mod mods;
mod mouse;
mod orchestration;
pub mod palette;
mod tabs;
mod ui;
mod window;

use std::time::Duration;

use fno::app_api::web_hq::Board;
use fno::mux_cli::{env_server, resolve_session};
pub use window::MainView;

/// How long `--probe` waits for a layout and a frame per pane.
const PROBE_BUDGET: Duration = Duration::from_secs(5);

/// How long `--probe --chat` waits for the next batch before it counts.
const CHAT_PROBE_IDLE: Duration = Duration::from_secs(2);

pub struct Options {
    pub session: Option<String>,
    pub font: String,
    pub font_size: f32,
    pub probe: bool,
    /// The view the window opens on.
    pub mode: MainView,
    /// A claude session id, or `newest`: the chat view shows it instead of
    /// the focused agent's. With `probe`, count its events instead.
    pub chat: Option<String>,
}

/// `path` with the directories fno installs into appended when missing. A
/// Finder launch hands the app `/usr/bin:/bin:/usr/sbin:/sbin`, which holds
/// neither `fno` nor `fno-agents`, so the fleet and question reads fail.
fn widened_path(path: &str, home: &str) -> String {
    let wanted = [
        format!("{home}/.local/bin"),
        format!("{home}/.fno/install/bin"),
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".to_string(),
    ];
    let mut dirs: Vec<String> = path
        .split(':')
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        .collect();
    for dir in wanted {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs.join(":")
}

/// Run the app (or the headless probe) and return the process exit code.
pub fn run(opts: Options) -> i32 {
    if let Some(home) = std::env::var_os("HOME").and_then(|h| h.into_string().ok()) {
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", widened_path(&path, &home));
    }
    let session = resolve_session(opts.session.as_deref(), env_server().as_deref());
    let pinned = match opts.chat.as_deref().map(chat::claude_key) {
        Some(None) => {
            eprintln!("fno-app: no claude transcript to read");
            return 1;
        }
        Some(key) => key,
        None => None,
    };
    match (opts.probe, pinned) {
        (true, Some(key)) => return chat::probe(&session, key, CHAT_PROBE_IDLE),
        (true, None) => return probe(&session),
        (false, pinned) => window::run(session, opts.mode, pinned, opts.font, opts.font_size),
    }
    0
}

/// `--probe`: the attach line, then `agents=<n> questions=<m> seats=<k>`
/// from the same reads and the same board builder the window draws.
fn probe(session: &str) -> i32 {
    let (code, layout) = link::probe(session, PROBE_BUDGET);
    let agents = layout.map(|l| l.agents).unwrap_or_default();
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("fno-app: cannot start the board read: {e}");
            return 1;
        }
    };
    let mut board = Board::default();
    rt.block_on(hq::read(&mut board, &agents, None));
    let seats = board.fleet().map_or(0, |f| f.seats().count());
    println!(
        "agents={} questions={} seats={seats}",
        agents.len(),
        board.questions().len()
    );
    for e in [board.fleet_error(), board.questions_error()]
        .into_iter()
        .flatten()
    {
        eprintln!("fno-app: {e}");
    }
    code
}

#[cfg(test)]
mod path_tests {
    use super::widened_path;

    #[test]
    fn widened_path_adds_the_install_dirs_once() {
        let path = widened_path("/usr/bin:/bin", "/Users/u");
        assert_eq!(
            path,
            "/usr/bin:/bin:/Users/u/.local/bin:/Users/u/.fno/install/bin:/opt/homebrew/bin:/usr/local/bin"
        );
        assert_eq!(widened_path(&path, "/Users/u"), path);
    }
}
