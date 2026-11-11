//! `fno-desktop`: the macOS app. A thin client of the mux server: it draws
//! the `Frame` cells the server sends over the session socket and runs no
//! terminal emulator of its own.

mod boxdraw;
mod grid;
mod hq;
pub mod keys;
mod left_pane;
pub mod link;
mod mouse;
mod orchestration;
pub mod palette;
mod tabs;
mod window;

use std::time::Duration;

use crate::mux_cli::{env_server, resolve_session};
use crate::web_hq::Board;

/// How long `--probe` waits for a layout and a frame per pane.
const PROBE_BUDGET: Duration = Duration::from_secs(5);

pub struct Options {
    pub session: Option<String>,
    pub font: String,
    pub font_size: f32,
    pub probe: bool,
}

/// Run the app (or the headless probe) and return the process exit code.
pub fn run(opts: Options) -> i32 {
    let session = resolve_session(opts.session.as_deref(), env_server().as_deref());
    if opts.probe {
        return probe(&session);
    }
    window::run(session, opts.font, opts.font_size);
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
            eprintln!("fno-desktop: cannot start the board read: {e}");
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
        eprintln!("fno-desktop: {e}");
    }
    code
}
