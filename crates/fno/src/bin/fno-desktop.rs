//! The macOS desktop app. Needs a running mux server (`fno mux` starts one).

use clap::Parser;

/// Attach to a running fno mux session in a native window.
#[derive(Parser)]
#[command(name = "fno-desktop", version)]
struct Args {
    /// The mux session to attach to (default: FNO_SERVER, then `default`).
    #[arg(long)]
    session: Option<String>,
    /// The terminal font family.
    #[arg(long, default_value = "Menlo")]
    font: String,
    /// The terminal font size in points.
    #[arg(long, default_value_t = 13.0)]
    font_size: f32,
    /// Attach with no window, print `attached session=<s> panes=<n> frames=<m>`
    /// and `agents=<n> questions=<m> seats=<k>`, and exit.
    #[arg(long)]
    probe: bool,
    /// The view the window opens on.
    #[arg(long, value_enum, default_value_t = fno::desktop::MainView::Chat)]
    mode: fno::desktop::MainView,
    /// With --probe: stream this claude session's chat (a session id, or
    /// `newest`), print `chat_events=<n> session=<id>`, and exit.
    #[arg(long, requires = "probe")]
    chat: Option<String>,
}

fn main() {
    let args = Args::parse();
    std::process::exit(fno::desktop::run(fno::desktop::Options {
        session: args.session,
        font: args.font,
        font_size: args.font_size,
        probe: args.probe,
        mode: args.mode,
        chat: args.chat,
    }));
}
