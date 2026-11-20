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
    /// One offscreen demo shot: render --shot-mode in --shot-theme and
    /// write --shot-out, then exit. Never opens a window on any display.
    #[arg(long, requires = "shot_out")]
    shot: bool,
    /// png path for --shot.
    #[arg(long = "shot-out")]
    shot_out: Option<String>,
    /// Palette --shot paints: dark (default) or light.
    #[arg(long = "shot-theme", value_parser = ["dark", "light"], default_value = "dark")]
    shot_theme: String,
    /// View --shot renders (default: the command center board).
    #[arg(long = "shot-mode", value_enum)]
    shot_mode: Option<ShotView>,
    /// Hold the composited shot window up for a foreign capturer
    /// instead of capturing. Diagnostic.
    #[arg(long = "shot-hold", hide = true)]
    shot_hold: bool,
    /// The chat pane keeps its empty start view for the frame, whatever
    /// the world's focus carries. Shot-only.
    #[arg(long = "shot-empty-chat", hide = true)]
    shot_empty_chat: bool,
    /// Cmd held: the rows and the start view show their cmd hints.
    /// Shot-only.
    #[arg(long = "shot-cmd", hide = true)]
    shot_cmd: bool,
}

/// The --shot-mode values. The kebab spellings mirror MainView.
#[derive(clap::ValueEnum, Clone)]
enum ShotView {
    /// The focused agent's conversation
    Chat,
    /// The server's panes
    Terminal,
    /// The command center board
    Orchestration,
    /// The mods and layouts feed
    Mods,
}

impl ShotView {
    fn into_main(self) -> fno_app::MainView {
        match self {
            ShotView::Chat => fno_app::MainView::Chat,
            ShotView::Terminal => fno_app::MainView::Terminal,
            ShotView::Orchestration => fno_app::MainView::Orchestration,
            ShotView::Mods => fno_app::MainView::Mods,
        }
    }
}

fn main() {
    let args = Args::parse();
    let shot = if args.shot {
        let Some(out) = args.shot_out else {
            eprintln!("fno-app: --shot needs --shot-out <png path>");
            std::process::exit(2);
        };
        Some(fno_app::ShotOptions {
            session: args.session.clone(),
            chat: args.chat.clone(),
            font: args.font.clone(),
            font_size: args.font_size,
            out: std::path::PathBuf::from(out),
            theme: args.shot_theme,
            mode: args.shot_mode.map(ShotView::into_main),
            hold: args.shot_hold,
            empty_chat: args.shot_empty_chat,
            cmd: args.shot_cmd,
        })
    } else {
        None
    };
    std::process::exit(fno_app::run(fno_app::Options {
        session: args.session,
        font: args.font,
        font_size: args.font_size,
        probe: args.probe,
        shot_hold: args.shot_hold,
        mode: args.mode,
        chat: args.chat,
        shot,
    }));
}
