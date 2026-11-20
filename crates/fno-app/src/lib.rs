//! `fno-app`: the macOS app. A thin client of the mux server: it draws
//! the `Frame` cells the server sends over the session socket and runs no
//! terminal emulator of its own.

mod alerts;
mod app_state;
mod boxdraw;
mod chat;
mod control;
mod find;
mod goto;
mod grid;
mod hq;
pub mod keys;
pub(crate) mod launch;
mod left_pane;
pub mod link;
mod mods;
mod mouse;
mod orchestration;
pub mod palette;
mod sheet;
mod shot;
mod tabs;
mod ui;
mod window;

use std::path::PathBuf;
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
    /// The view the window opens on. None restores the saved view (the
    /// default for a plain launch); an explicit `--mode` wins over the save.
    pub mode: Option<MainView>,
    /// A claude session id, or `newest`: the chat view shows it instead of
    /// the focused agent's. With `probe`, count its events instead.
    pub chat: Option<String>,
    /// One offscreen demo shot: render and exit, writing a png.
    pub shot: Option<ShotOptions>,
    /// Hold the composited shot window up instead of capturing.
    /// Diagnostic; the shot door never sets it.
    pub shot_hold: bool,
}

/// The `--shot` flag set: one offscreen render into a png.
pub struct ShotOptions {
    /// Hold the composited window up for a foreign capturer instead of
    /// capturing. Diagnostic; the shot door never sets it.
    pub hold: bool,
    /// The mux session to attach (the demo world names its own).
    pub session: Option<String>,
    /// A claude session id, or `newest`: what --shot-mode chat renders.
    pub chat: Option<String>,
    /// The terminal font family.
    pub font: String,
    /// The terminal font size in points.
    pub font_size: f32,
    /// png path to write.
    pub out: PathBuf,
    /// Palette the frame paints: dark or light.
    pub theme: String,
    /// View to render (default: the command center board).
    pub mode: Option<MainView>,
    /// Paint the frame as if cmd alone is held: the pick-gated hints
    /// reveal. The proof frame for the badge rule.
    pub picking: bool,
}

/// Run the app (or the headless probe) and return the process exit code.

/// `path` with the directories fno installs into appended when missing. A
/// Finder launch hands the app `/usr/bin:/bin:/usr/sbin:/sbin`, which holds
/// neither `fno` nor `fno-agent`, so the fleet and question reads fail.
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
    if let Some(shot) = opts.shot {
        let out = shot.out;
        let theme = shot.theme;
        let mode = shot.mode.or(opts.mode);
        return crate::shot::run(ShotOptions {
            hold: opts.shot_hold,
            out,
            theme,
            mode,
            session: opts.session,
            chat: opts.chat,
            font: opts.font,
            font_size: opts.font_size,
            picking: shot.picking,
        });
    }
    if let Some(home) = std::env::var_os("HOME").and_then(|h| h.into_string().ok()) {
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", widened_path(&path, &home));
    }
    let session = resolve_session(opts.session.as_deref(), env_server().as_deref());
    let restored = app_state::state_path().and_then(|p| app_state::load(&p));
    let pinned = match opts.chat.as_deref().map(chat::resolve_key) {
        Some(None) => {
            eprintln!(
                "fno-app: no transcript found for --chat {}",
                opts.chat.as_deref().unwrap_or_default()
            );
            return 1;
        }
        // An explicit --chat beats the saved stream.
        Some(key) => key,
        None => restored.as_ref().and_then(|s| s.pinned.clone()),
    };
    // An explicit --mode beats the saved view; Chat stays the no-state default.
    let view = opts
        .mode
        .or(restored.as_ref().map(|s| s.view))
        .unwrap_or(MainView::Chat);
    match (opts.probe, pinned) {
        (true, Some(key)) => return chat::probe(&session, key, CHAT_PROBE_IDLE),
        (true, None) => return probe(&session),
        (false, pinned) => window::run(session, view, pinned, opts.font, opts.font_size, restored),
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
