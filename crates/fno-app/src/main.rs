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
    /// Palette --shot paints: dark (default), light, or an inverse-You twin of either.
    #[arg(long = "shot-theme", value_parser = ["dark", "light", "dark-inverse", "light-inverse"], default_value = "dark")]
    shot_theme: String,
    /// View --shot renders (default: the command center board).
    #[arg(long = "shot-mode", value_enum)]
    shot_mode: Option<ShotView>,
    /// Find-bar state --shot paints over the chat view: `bar` opens the
    /// bar with a query typed through the real key path; `chords` also
    /// forces the cmd-held hint.
    #[arg(long = "shot-find", value_enum)]
    shot_find: Option<ShotFind>,
    /// Window-relative X,Y in logical points the shot clicks through a
    /// posted mouse event before the capture.
    #[arg(long = "shot-tap", value_name = "X,Y")]
    shot_tap: Option<String>,
    /// Overlay --shot opens through its real code path before the capture.
    #[arg(long = "shot-ui", value_enum)]
    shot_ui: Option<ShotUi>,
    /// Settings-panel state --shot paints: `panel` opens the panel,
    /// `filter` types `alert` through the real field, `page` opens the
    /// Theme choice page through real Enter.
    #[arg(long = "shot-settings", value_enum)]
    shot_settings: Option<ShotSettings>,
    /// Update card state --shot paints: `ready` is the Restart card,
    /// `notes` the release notes of a build just installed.
    #[arg(long = "shot-update", value_enum, hide = true)]
    shot_update: Option<ShotUpdate>,
    /// Hold the composited shot window up for a foreign capturer
    /// instead of capturing. Diagnostic.
    #[arg(long = "shot-hold", hide = true)]
    shot_hold: bool,
    /// Open the answer sheet on the newest question: the sheet proof.
    #[arg(long = "shot-sheet", hide = true)]
    shot_sheet: bool,
    /// The chat pane keeps its empty start view for the frame, whatever
    /// the world's focus carries. Shot-only.
    #[arg(long = "shot-empty-chat", hide = true)]
    shot_empty_chat: bool,
    /// Cmd held: the rows and the start view show their cmd hints.
    /// Shot-only.
    #[arg(long = "shot-cmd", hide = true)]
    shot_cmd: bool,
    /// Text typed into the start composer (or the chat composer when the
    /// pane has a session) before the frame. Shot-only.
    #[arg(long = "shot-draft", hide = true)]
    shot_draft: Option<String>,
    /// Harness the start composer shows picked. Shot-only.
    #[arg(long = "shot-harness", hide = true)]
    shot_harness: Option<String>,
    /// First-run state --shot paints: `key-card` is the empty start view
    /// on a first launch, `no-server` is the offer to start the mux.
    #[arg(long = "shot-first-run", value_enum, hide = true)]
    shot_first_run: Option<ShotFirstRun>,
    /// Let --shot drive the session: its attach births a workspace with a
    /// shell, and its keystrokes reach it. Only for a world the caller built
    /// and tears down; any other shot is an observer.
    #[arg(long = "shot-own-world", hide = true)]
    shot_own_world: bool,
}

/// The --shot-first-run values.
#[derive(clap::ValueEnum, Clone)]
enum ShotFirstRun {
    /// The empty start view with the splash mark and the key card.
    KeyCard,
    /// No mux server answers: the offer to start one.
    NoServer,
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
    /// The inbox
    Inbox,
}

impl ShotView {
    fn into_main(self) -> fno_app::MainView {
        match self {
            ShotView::Chat => fno_app::MainView::Chat,
            ShotView::Terminal => fno_app::MainView::Terminal,
            ShotView::Orchestration => fno_app::MainView::Orchestration,
            ShotView::Mods => fno_app::MainView::Mods,
            ShotView::Inbox => fno_app::MainView::Inbox,
        }
    }
}

/// The --shot-update values.
#[derive(clap::ValueEnum, Clone)]
enum ShotUpdate {
    /// The Restart card for a staged build.
    Ready,
    /// The release notes card of a build just installed.
    Notes,
}

/// The --shot-find values.
#[derive(clap::ValueEnum, Clone)]
enum ShotFind {
    /// The bar open, hint at rest.
    Bar,
    /// The bar open, cmd-held hint forced.
    Chords,
}

/// Parse the --shot-tap X,Y pair.
fn parse_point(s: &str) -> Option<(f64, f64)> {
    let (x, y) = s.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// The --shot-ui values.
#[derive(clap::ValueEnum, Clone)]
enum ShotUi {
    /// The first agent's row menu, opened as a right-click opens it.
    RowMenu,
    /// The same menu for an agent that carries a node.
    RowMenuNode,
    /// The first tab's in-place rename field.
    TabRename,
    /// The back-to-live pill, as a scrolled-back pane shows it.
    Scrolled,
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
            own_world: args.shot_own_world,
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
            tap: args.shot_tap.as_deref().and_then(parse_point),
            ui: args.shot_ui.map(|u| match u {
                ShotUi::RowMenu => fno_app::UiShot::RowMenu,
                ShotUi::RowMenuNode => fno_app::UiShot::RowMenuNode,
                ShotUi::TabRename => fno_app::UiShot::TabRename,
                ShotUi::Scrolled => fno_app::UiShot::Scrolled,
            }),
            settings: args.shot_settings.map(|s| match s {
                ShotSettings::Panel => fno_app::SettingsShot::Panel,
                ShotSettings::Filter => fno_app::SettingsShot::Filter,
                ShotSettings::Page => fno_app::SettingsShot::Page,
            }),
            update: args.shot_update.map(|u| match u {
                ShotUpdate::Ready => fno_app::UpdateShot::Ready,
                ShotUpdate::Notes => fno_app::UpdateShot::Notes,
            }),
            hold: args.shot_hold,
            sheet: args.shot_sheet,
            empty_chat: args.shot_empty_chat,
            cmd: args.shot_cmd,
            draft: args.shot_draft,
            harness: args.shot_harness,
            first_run: args.shot_first_run.map(|f| match f {
                ShotFirstRun::KeyCard => fno_app::FirstRunShot::KeyCard,
                ShotFirstRun::NoServer => fno_app::FirstRunShot::NoServer,
            }),
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
