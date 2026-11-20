//! The macOS desktop app. Needs a running mux server (`fno mux` starts one).

use clap::Parser;

/// Attach to a running fno mux session in a native window.
#[derive(Parser)]
#[command(name = "fno-app", version)]
struct Args {
    /// The mux session to attach to (default: FNO_SERVER, then `default`).
    #[arg(long)]
    session: Option<String>,
    /// The terminal font family. Overrides `app.font` in the global
    /// config, which the Settings page writes.
    #[arg(long)]
    font: Option<String>,
    /// The terminal font size in points. Overrides `app.font_size`.
    #[arg(long)]
    font_size: Option<f32>,
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
    /// Find-bar state --shot paints over the chat view: `bar` opens the
    /// bar with a query typed through the real key path; `chords` also
    /// forces the cmd-held hint.
    #[arg(long = "shot-find", value_enum)]
    shot_find: Option<ShotFind>,
    /// Settings-panel state --shot paints: `panel` opens the panel,
    /// `filter` types `alert` through the real field, `page` opens the
    /// Theme choice page through real Enter.
    #[arg(long = "shot-settings", value_enum)]
    shot_settings: Option<ShotSettings>,
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

/// The --shot-find values.
#[derive(clap::ValueEnum, Clone)]
enum ShotFind {
    /// The bar open, hint at rest.
    Bar,
    /// The bar open, cmd-held hint forced.
    Chords,
}

/// The --shot-settings values.
#[derive(clap::ValueEnum, Clone)]
enum ShotSettings {
    /// The open panel.
    Panel,
    /// The panel with `alert` typed.
    Filter,
    /// The Theme choice page open.
    Page,
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
            // A shot pins its own face: the shot path reads no config, so
            // the flag (or the default) decides and a run is repeatable.
            font: args.font.clone().unwrap_or_else(|| "SF Mono".to_string()),
            font_size: args.font_size.unwrap_or(13.0),
            out: std::path::PathBuf::from(out),
            theme: args.shot_theme,
            mode: args.shot_mode.map(ShotView::into_main),
            find: args.shot_find.map(|f| match f {
                ShotFind::Bar => fno_app::FindShot::Bar,
                ShotFind::Chords => fno_app::FindShot::Chords,
            }),
            settings: args.shot_settings.map(|s| match s {
                ShotSettings::Panel => fno_app::SettingsShot::Panel,
                ShotSettings::Filter => fno_app::SettingsShot::Filter,
                ShotSettings::Page => fno_app::SettingsShot::Page,
            }),
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
