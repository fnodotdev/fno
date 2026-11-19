//! The macOS desktop app. Needs a running mux server (`fno mux` starts one).

use clap::Parser;

/// Attach to a running fno mux session in a native window.
#[derive(Parser)]
#[command(name = "fno-app", version)]
struct Args {
    /// The mux session to attach to (default: FNO_SERVER, then `default`).
    #[arg(long)]
    session: Option<String>,
    /// The terminal font family.
    #[arg(long, default_value = "SF Mono")]
    font: String,
    /// The terminal font size in points.
    #[arg(long, default_value_t = 13.0)]
    font_size: f32,
    /// Attach with no window, print `attached session=<s> panes=<n> frames=<m>`
    /// and `agents=<n> questions=<m> seats=<k>`, and exit.
    #[arg(long)]
    probe: bool,
    /// The view the window opens on. Without it, the saved view restores.
    #[arg(long, value_enum)]
    mode: Option<fno_app::MainView>,
    /// Show this session's chat instead of the focused agent's:
    /// `<harness>:<id|newest>` (claude, codex or opencode), a bare session id
    /// (the transcript carrying it names the harness), or `newest` (claude:
    /// the newest transcript in the current project, ranked by session
    /// start; codex: the newest in its sessions tree). With --probe, print
    /// `chat_events=<n> session=<id>` and exit.
    #[arg(long)]
    chat: Option<String>,
}

fn main() {
    let args = Args::parse();
    std::process::exit(fno_app::run(fno_app::Options {
        session: args.session,
        font: args.font,
        font_size: args.font_size,
        probe: args.probe,
        mode: args.mode,
        chat: args.chat,
    }));
}
