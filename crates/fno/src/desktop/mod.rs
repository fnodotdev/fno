//! `fno-desktop`: the macOS app. A thin client of the mux server: it draws
//! the `Frame` cells the server sends over the session socket and runs no
//! terminal emulator of its own.

mod grid;
pub mod keys;
pub mod link;
pub mod palette;
mod window;

use std::time::Duration;

use crate::mux_cli::{env_server, resolve_session};

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
        return link::probe(&session, PROBE_BUDGET);
    }
    window::run(session, opts.font, opts.font_size);
    0
}
