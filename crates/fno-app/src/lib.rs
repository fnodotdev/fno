//! `fno-app`: the macOS app. A thin client of the mux server: it draws
//! the `Frame` cells the server sends over the session socket and runs no
//! terminal emulator of its own.

mod alerts;
mod answer;
mod app_state;
mod boxdraw;
mod chat;
mod control;
mod find;
mod first_run;
mod goto;
mod grid;
mod hq;
mod inbox;
pub mod keys;
pub(crate) mod launch;
mod left_pane;
pub mod link;
mod mods;
mod mouse;
mod mux_modes;
mod orchestration;
pub mod palette;
mod pane_seam;
mod settings;
mod sheet;
mod shot;
mod skills;
mod start;
mod tabs;
mod ui;
mod update;
mod window;

use std::path::{Path, PathBuf};
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
    /// None reads `app.font` from the global config, then the default.
    pub font: Option<String>,
    /// None reads `app.font_size`, then the default.
    pub font_size: Option<f32>,
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
    /// The mux session to attach. Required: the demo world names its own,
    /// and a shot never falls back to the user's default session.
    pub session: Option<String>,
    /// The caller built the world the session runs in and tears it down
    /// after, so the shot may drive it. Any other shot attaches as an
    /// observer and adds no workspace and no row.
    pub own_world: bool,
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
    /// Open the answer sheet on the newest question once the board lands.
    pub sheet: bool,
    /// The empty-pane state: the chat pane shows its start view no matter
    /// what the world's focus carries. The start-view shots set it.
    pub empty_chat: bool,
    /// Cmd held: the rows and the start view show their cmd hints. The
    /// cmd-held shots set it.
    pub cmd: bool,
    /// The find bar state the frame paints over the chat view.
    pub find: Option<FindShot>,
    /// Window-relative logical x,y the frame clicks through a posted
    /// mouse event before the capture: the proof frame for real
    /// interaction on the running demo window.
    pub tap: Option<(f64, f64)>,
    /// The overlay the frame opens through its real code path.
    pub ui: Option<UiShot>,
    /// Text the frame's composer holds.
    pub draft: Option<String>,
    /// Harness the start composer shows picked.
    pub harness: Option<String>,
    /// The first-run state the frame paints.
    pub first_run: Option<FirstRunShot>,
    /// The Settings panel state the frame paints.
    pub settings: Option<SettingsShot>,
    /// The update card the frame paints.
    pub update: Option<UpdateShot>,
}

/// The --shot-update values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UpdateShot {
    /// A staged build is ready: the Restart card.
    Ready,
    /// The first launch of a new build: the release notes card.
    Notes,
}

/// The `--shot-ui` states.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UiShot {
    RowMenu,
    RowMenuNode,
    TabRename,
    Scrolled,
}

/// The --shot-first-run values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FirstRunShot {
    /// The empty start view on a first launch: splash mark and key card.
    KeyCard,
    /// No mux server answers: the offer to start one.
    NoServer,
}

/// The --shot-settings values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SettingsShot {
    /// The open panel.
    Panel,
    /// The panel with `alert` typed through the real field.
    Filter,
    /// The panel with the Theme choice page open through real Enter.
    Page,
}

/// The find-bar states `--shot-find` paints.
#[derive(Clone, Copy, PartialEq)]
pub enum FindShot {
    /// The bar open with a query typed; the hint at rest: bare arrows.
    Bar,
    /// The bar open, and the cmd-held chord hint forced on.
    Chords,
}

/// The raw global settings dict: the file the WRITE door targets
/// (`~/.fno/config.toml`, preferred over the legacy yaml a migrated
/// install no longer has - `global_settings_path()` still names the yaml).
/// The same FNO_CONFIG / FNO_GLOBAL_SETTINGS_PATH pin rules the loader
/// walks, mirrored here because the reader twin (`digest_overlay.rs`
/// `global_config_toml`) is pub(crate).
pub fn global_config_raw() -> Option<serde_json::Value> {
    let path = global_config_path()?;
    fno::config_cli::parse_config_file(&path)
        .map(|parsed| fno::config_cli::unwrap_config_dict(&parsed))
}

/// The file `global_config_raw` reads: the pin rules above, one owner, so
/// the alert switches' mtime watch follows the file the read follows.
pub fn global_config_path() -> Option<PathBuf> {
    let dir = fno::config_cli::global_settings_path()
        .parent()?
        .to_path_buf();
    let pin = std::env::var("FNO_CONFIG").ok().filter(|p| !p.is_empty());
    let pin = pin.or_else(|| {
        std::env::var("FNO_GLOBAL_SETTINGS_PATH")
            .ok()
            .filter(|p| !p.is_empty())
    });
    match pin.as_deref() {
        // A yaml-shaped pin names the legacy file the write door converts;
        // read the toml sibling it leaves behind.
        Some(p) if Path::new(p).file_name() == Some(std::ffi::OsStr::new("settings.yaml")) => {
            Some(PathBuf::from(p).with_file_name("config.toml"))
        }
        Some(p) if Path::new(p).extension().is_some_and(|e| e == "toml") => Some(PathBuf::from(p)),
        Some(_) => None,
        // No pin: the write door targets config.toml and migrates the
        // legacy yaml beside it, so an existing config.toml wins.
        None => pick_global_file(&dir),
    }
}

/// The global config file inside `dir`: the write door targets
/// `config.toml` and converts the legacy yaml beside it, so an existing
/// config.toml wins; an unmigrated install reads the yaml.
fn pick_global_file(dir: &Path) -> Option<PathBuf> {
    let toml = dir.join("config.toml");
    if toml.is_file() {
        return Some(toml);
    }
    let yaml = dir.join("settings.yaml");
    yaml.is_file().then_some(yaml)
}

/// One dotted key's raw value out of the settings dict.
fn raw_get(config: Option<&serde_json::Value>, key: &str) -> Option<serde_json::Value> {
    config.and_then(|raw| fno::config_cli::raw_get(raw, key).cloned())
}

/// The launch ladder in one pure function: a flag wins, then `app.font` /
/// `app.font_size` / `mux.launch_screen` in the global config, then the
/// built-in defaults (`mux.launch_screen` lands before the saved view, so
/// the D3320 pin moves the first screen; `--mode` beats it). The config
/// read is the RAW value: the schema default `agents` must not pin Chat
/// over the saved view a quit left behind, so only a key the user wrote
/// decides.
pub fn launch_prefs(
    font_flag: Option<String>,
    size_flag: Option<f32>,
    mode_flag: Option<MainView>,
    saved_view: Option<MainView>,
    config: Option<&serde_json::Value>,
) -> (String, f32, MainView) {
    let font = font_flag
        .or_else(|| raw_get(config, "app.font").and_then(|v| v.as_str().map(str::to_string)))
        .unwrap_or_else(|| "SF Mono".to_string());
    let font_size = size_flag
        .or_else(|| {
            raw_get(config, "app.font_size")
                .and_then(|v| v.as_f64())
                .map(|f| f as f32)
        })
        .unwrap_or(13.0);
    let view = mode_flag
        .or_else(|| {
            match raw_get(config, "mux.launch_screen")
                .and_then(|v| v.as_str().map(str::to_string))
                .as_deref()
            {
                Some("hq") => Some(MainView::Orchestration),
                Some("agents") => Some(MainView::Chat),
                _ => None,
            }
        })
        .or(saved_view)
        .unwrap_or(MainView::Chat);
    (font, font_size, view)
}

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
            find: shot.find,
            session: opts.session,
            own_world: shot.own_world,
            chat: opts.chat,
            // A shot pins its own face: the shot path reads no config,
            // so the flag (or the default) decides and a run is repeatable.
            font: opts.font.unwrap_or_else(|| "SF Mono".to_string()),
            font_size: opts.font_size.unwrap_or(13.0),
            empty_chat: shot.empty_chat,
            cmd: shot.cmd,
            tap: shot.tap,
            ui: shot.ui,
            sheet: shot.sheet,
            draft: shot.draft,
            harness: shot.harness,
            first_run: shot.first_run,
            settings: shot.settings,
            update: shot.update,
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
    // The launch ladder: flag, then config, then the saved view, then Chat.
    let config = global_config_raw();
    let (font, font_size, view) = launch_prefs(
        opts.font,
        opts.font_size,
        opts.mode,
        restored.as_ref().map(|s| s.view),
        config.as_ref(),
    );
    match (opts.probe, pinned) {
        (true, Some(key)) => return chat::probe(&session, key, CHAT_PROBE_IDLE),
        (true, None) => return probe(&session),
        (false, pinned) => window::run(session, view, pinned, font, font_size, restored),
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

#[cfg(test)]
mod launch_prefs_tests {
    use super::launch_prefs;
    use crate::window::MainView;

    fn cfg(
        launch_screen: Option<&str>,
        font: Option<&str>,
        size: Option<f64>,
    ) -> serde_json::Value {
        let mut mux = serde_json::Map::new();
        if let Some(ls) = launch_screen {
            mux.insert("launch_screen".into(), serde_json::json!(ls));
        }
        let mut app = serde_json::Map::new();
        if let Some(f) = font {
            app.insert("font".into(), serde_json::json!(f));
        }
        if let Some(s) = size {
            app.insert("font_size".into(), serde_json::json!(s));
        }
        serde_json::json!({"mux": mux, "app": app})
    }

    #[test]
    fn config_fills_the_ladder_between_flags_and_saved_state() {
        // Config picks the face and the first view with no flags.
        let raw = cfg(Some("hq"), Some("JetBrains Mono"), Some(14.0));
        let (font, size, view) = launch_prefs(None, None, None, None, Some(&raw));
        assert_eq!(font, "JetBrains Mono");
        assert_eq!(size, 14.0);
        assert_eq!(view, MainView::Orchestration);
        // A flag beats the config pin.
        let (font, size, view) = launch_prefs(
            Some("Menlo".into()),
            Some(12.0),
            Some(MainView::Terminal),
            None,
            Some(&raw),
        );
        assert_eq!(font, "Menlo");
        assert_eq!(size, 12.0);
        assert_eq!(view, MainView::Terminal);
        // The saved view survives an unset key, and Chat stays the floor.
        let empty = cfg(None, None, None);
        let (font, size, view) = launch_prefs(None, None, None, Some(MainView::Mods), Some(&empty));
        assert_eq!(font, "SF Mono");
        assert_eq!(size, 13.0);
        assert_eq!(view, MainView::Mods);
    }

    #[test]
    fn the_schema_default_does_not_pin_chat_over_the_saved_view() {
        // The RAW read: an explicit "agents" pins Chat, but an absent key
        // falls through to the saved view - the resolved default would
        // strand the restore.
        let empty = cfg(None, None, None);
        let (_, _, view) = launch_prefs(
            None,
            None,
            None,
            Some(MainView::Orchestration),
            Some(&empty),
        );
        assert_eq!(view, MainView::Orchestration);
        let pinned = cfg(Some("agents"), None, None);
        let (_, _, view) = launch_prefs(
            None,
            None,
            None,
            Some(MainView::Orchestration),
            Some(&pinned),
        );
        assert_eq!(view, MainView::Chat);
        // An unknown word falls through too, never wedges the launch.
        let odd = cfg(Some("chaos"), None, None);
        let (_, _, view) =
            launch_prefs(None, None, None, Some(MainView::Orchestration), Some(&odd));
        assert_eq!(view, MainView::Orchestration);
    }
}
