//! The terminal guard a full-screen watch holds: raw mode and the
//! alternate screen, undone when it drops.

use std::io::Write;

/// Raw mode and the alternate screen for the life of the watch, undone on
/// every exit path.
pub(crate) struct Screen;

impl Screen {
    pub(crate) fn enter() -> Result<Screen, String> {
        crossterm::terminal::enable_raw_mode().map_err(|e| format!("terminal raw mode: {e}"))?;
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[?1049h\x1b[?25l\x1b[2J");
        let _ = out.flush();
        Ok(Screen)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[0m\x1b[?25h\x1b[?1049l");
        let _ = out.flush();
        let _ = crossterm::terminal::disable_raw_mode();
    }
}
