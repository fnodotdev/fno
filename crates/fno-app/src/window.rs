//! The app window: one root view that lays out the left pane, the tab strip
//! and the server's pane rects, routes keys and mouse to the focused pane,
//! and hosts the chat view and the orchestration board.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    actions, canvas, div, font, point, prelude::*, px, rgb, rgba, size, AnyView, App, Bounds,
    ClipboardEntry, ClipboardItem, Context, ElementInputHandler, Entity, EntityInputHandler,
    FocusHandle, Global, InputHandler, KeyBinding, KeyDownEvent, Menu, MenuItem,
    ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, ScrollEvent, Size, Subscription, Task, TitlebarOptions, UTF16Selection, Window,
    WindowBounds, WindowOptions,
};

use serde::{Deserialize, Serialize};

use super::alerts;
use super::alerts::Signal;
use super::answer;
use super::app_state::GroupBy;
use super::chat::{
    self,
    composer::{Attachment, Draft},
    Chat,
};
use super::goto;
use super::grid::{Metrics, PaneView};
use super::hq::{Poller, Reply};
use super::keys::{self, Key, Modes, Shortcut};
use super::launch::{self, Launch};
use super::link::{Event, Layout, Link};
use super::mouse::{self, Hit};
use super::mux_modes::{self, MenuAct, MenuKey, Step};
use super::palette::Palette;
use super::{find, left_pane, mods, orchestration, settings, sheet, tabs, ui};
use fno::app_api::web_hq::Board;
use fno::proto::chat_event::SessionKey;
use fno::proto::{self, ClientMsg, Command, Frame, MouseEvent, MouseKind};
use fno::tree::Rect;

actions!(fno_app, [KeyboardShortcuts, GoTo, OpenSettings]);

/// What fills the main area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
pub enum MainView {
    /// The focused agent's conversation, the mode the app opens in.
    Chat,
    /// The server's panes.
    Terminal,
    /// The command center board.
    Orchestration,
    /// The mods and layouts from `fno config plugins ls`.
    Mods,
    /// The navigation for questions and decisions: Open and Answered.
    Inbox,
}

/// What a click or a key in the left pane, the strip, the board or the
/// sheets does.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Send(Command),
    Notice(String),
    Answer {
        id: String,
        option: u32,
    },
    /// Open the answer sheet on one question.
    OpenAnswer(String),
    /// Close the answer sheet without answering.
    CloseAnswer,
    /// Record the sheet's picked choice; Answer sends it.
    PickAnswer(u32),
    /// The sheet's Answer button and Enter: the typed words win over the
    /// picked choice.
    SubmitAnswer,
    Show(MainView),
    /// Open the inline field that names a new workspace.
    NewWorkspace,
    /// Open one workspace's context menu (a right-click on its header).
    OpenWorkspaceMenu(u64),
    /// Open the header +'s two-item menu: a new worker or a new shell.
    OpenAddMenu(u64),
    /// Cycle the Workspaces section's grouping: workspaces, state, machine.
    CycleGrouping,
    /// Close the open context menu without acting.
    CloseWorkspaceMenu,
    /// Select a workspace, then open a shell tab in it.
    NewTabIn(u64),
    /// Open the inline field seeded with the workspace's current name.
    RenameWorkspace(u64),
    /// Reorder one workspace by delta, as the sideline order does.
    MoveWorkspace(u64, i32),
    /// Open the close-workspace confirm for one workspace.
    RemoveWorkspace(u64),
    /// The confirm's own Close button: the command fires, the sheet goes.
    ConfirmCloseWorkspace(u64),
    /// The confirm's Keep button: nothing is sent.
    KeepWorkspace,
    /// Open the launch composer with the workspace's cwd preset.
    NewWorker(u64),
    /// A new shell tab under the + menu's workspace.
    NewShell(u64),
    /// Fold or open one workspace: the id resolves to the persisted fold
    /// key (its canonical cwd, else its name) at act time.
    ToggleWorkspace(u64),
    /// Fold or open the trailing Elsewhere group.
    ToggleElsewhere,
    /// Open or close the key sheet, as ⌘/ and the palette's command row do.
    ToggleKeySheet,
    /// Open or close one chat thinking row.
    ToggleChat(usize),
    /// Open the sheet of one chat tool call.
    OpenSheet(usize),
    CloseSheet,
    Copy(String),
    SendChat,
    /// Move the open question's pick to one row.
    PickChoice(usize),
    /// Answer the open question with the picked row.
    AnswerChoice,
    /// The start view's send: the launch panel's own submit door.
    SubmitLaunch,
    /// Reopen a chat stream that closed.
    Reconnect,
    /// Route one HQ composer message to the owning lead.
    Mail(String),
    /// Open the launch composer, with a kind pre-picked when the opener
    /// named one; `None` keeps the retained draft's kind.
    OpenLaunch(Option<launch::Kind>),
    /// The update card's Restart: swap in the staged build and relaunch.
    RestartForUpdate,
    /// The update card's Later or Dismiss: drop the card.
    DismissUpdate,
}

/// The stable control id a click's [`Action`] ships to the usage events arm
/// (`fno::ui_usage`). One constant per control, never a label.
fn click_id(action: &Action) -> &'static str {
    match action {
        Action::Send(_) => "send_command",
        Action::Notice(_) => "notice",
        Action::Answer { .. } => "answer",
        Action::OpenAnswer(_) => "open_answer",
        Action::CloseAnswer => "close_answer",
        Action::PickAnswer(_) => "pick_answer",
        Action::SubmitAnswer => "submit_answer",
        Action::Show(_) => "show_view",
        Action::NewWorkspace => "new_workspace",
        Action::ToggleWorkspace(_) => "toggle_workspace",
        Action::ToggleElsewhere => "toggle_elsewhere",
        Action::OpenWorkspaceMenu(_) => "open_workspace_menu",
        Action::OpenAddMenu(_) => "open_add_menu",
        Action::CycleGrouping => "cycle_grouping",
        Action::CloseWorkspaceMenu => "close_workspace_menu",
        Action::NewTabIn(_) => "new_tab_in",
        Action::RenameWorkspace(_) => "rename_workspace",
        Action::MoveWorkspace(_, _) => "move_workspace",
        Action::RemoveWorkspace(_) => "remove_workspace",
        Action::ConfirmCloseWorkspace(_) => "confirm_close_workspace",
        Action::KeepWorkspace => "keep_workspace",
        Action::NewWorker(_) => "new_worker",
        Action::NewShell(_) => "new_shell",
        Action::ToggleKeySheet => "toggle_key_sheet",
        Action::ToggleChat(_) => "toggle_chat",
        Action::OpenSheet(_) => "open_sheet",
        Action::CloseSheet => "close_sheet",
        Action::Copy(_) => "copy",
        Action::SendChat => "send_chat",
        Action::PickChoice(_) => "pick_choice",
        Action::AnswerChoice => "answer_choice",
        Action::SubmitLaunch => "submit_launch",
        Action::Reconnect => "reconnect",
        Action::Mail(_) => "mail_lead",
        Action::OpenLaunch(_) => "open_launch",
        Action::RestartForUpdate => "restart_for_update",
        Action::DismissUpdate => "dismiss_update",
    }
}

/// What the update card says.
#[derive(Debug, Clone, PartialEq)]
enum UpdateCard {
    /// A proven build is staged; Restart swaps it in.
    Ready(super::update::Ready),
    /// This launch is the new build: its release notes, shown once.
    Installed { version: String, notes: String },
}

/// Where an answer the user clicked stands. The row shows it in place of its
/// options until the next read drops the question.
#[derive(Debug, Clone, PartialEq)]
pub enum AnswerState {
    Sending(u32),
    Done(u32),
    /// A typed answer in flight or recorded; the door took the words.
    SendingWords,
    DoneWords,
    Refused(String),
}

/// Where the HQ composer's send stands.
#[derive(Debug, Clone, PartialEq)]
pub enum MailState {
    Sending,
    /// The porcelain's own verdict line.
    Sent(String),
    Refused(String),
}

/// How long a notice stays in the corner.
const NOTICE_FOR: Duration = Duration::from_secs(4);
/// How long a question arrival toast stays top-right.
const QUESTION_TOAST_FOR: Duration = Duration::from_secs(8);
/// The terminal client's quiet windows, mirrored: 40ms releases a held Esc
/// candidate, 400ms keeps a typed tab number together before it resolves
/// on its own.
const CHORD_FLUSH_MS: u64 = 40;
const DIGIT_FLUSH_MS: u64 = 400;

/// The most wheel lines one event sends, so a fling cannot flood the socket.
const WHEEL_CAP: i32 = 10;

/// The close-workspace confirm's facts: the wording the TUI uses, plus
/// the count it names.
pub(super) struct WorkspaceConfirm {
    pub squad: u64,
    pub name: String,
    pub panes: usize,
    /// The last workspace: closing it ends the session.
    pub last: bool,
}

/// The inline workspace-name field's own state: the draft the input
/// handler routes into, the field's layout state, and the workspace being
/// renamed when the field edits one (`None` names a new workspace).
pub(super) struct SquadField {
    pub draft: Draft,
    pub field: Rc<chat::field::FieldState>,
    /// The workspace being renamed; `None` when the field names a new one.
    pub squad: Option<u64>,
}

pub struct Root {
    pub(super) main: MainView,
    link: Option<Link>,
    pub(super) layout: Layout,
    frames: HashMap<u64, Arc<Frame>>,
    panes: HashMap<u64, Entity<PaneView>>,
    modes: Modes,
    /// The mux's own key scanner: claims the prefix (Ctrl+B by default)
    /// before any pane input, the same layer the terminal client claims it.
    scanner: fno::ui::keys::Scanner,
    notice: Option<String>,
    /// The notice reports a failure: it paints in the error color.
    notice_failed: bool,
    /// Bumped by each notice, so only the newest one's timer clears it.
    notice_gen: u64,
    /// A shot window paints the durable surface only: server notices are a
    /// transient corner toast that would photobomb a still frame.
    pub(super) shot: bool,
    /// The empty-pane shot: sync_chat never hands the pane a session, so
    /// the start view holds for the frame.
    shot_empty_chat: bool,
    pub(super) closed: Option<String>,
    /// The no-server offer: set when the attach found no socket listening.
    pub(super) offer: Option<crate::first_run::Offer>,
    /// A launch with no saved state: the start view adds the key card.
    pub(super) first_run: bool,
    pub(super) metrics: Metrics,
    pub(super) palette: Palette,
    /// Whether the window appearance is light, and the project directory the
    /// palette was last resolved from.
    light: bool,
    theme_cwd: PathBuf,
    focus: FocusHandle,
    /// The content size (rows, cols) last sent to the server.
    sent_size: (u16, u16),
    pub(super) board: Board,
    pub(super) poller: Option<Poller>,
    /// The terminal area's bounds at the last paint, for mouse hit tests.
    area: Rc<Cell<Bounds<Pixels>>>,
    /// The pane a left-button drag started in.
    drag: Option<(u64, Rect)>,
    /// A split drag: the pane pair under the grabbed hairline and the last
    /// position sent, so a move sends only on a cell crossing.
    seam_drag: Option<mouse::SeamDrag>,
    wheel: f32,
    /// The pane the wheel remainder belongs to.
    wheel_pane: Option<u64>,
    pub(super) answers: HashMap<String, AnswerState>,
    /// The composer's draft and its send state.
    pub(super) board_draft: Draft,
    pub(super) mail: Option<MailState>,
    /// The persisted fold keys (workspace cwds, or names when the cwd is
    /// empty); absence means open.
    pub(super) folds: HashSet<String>,
    /// The Workspaces section's grouping.
    pub(super) group_by: GroupBy,
    /// The inline workspace-name field: it names a new workspace, or
    /// renames the workspace it carries.
    pub(super) squad_field: Option<SquadField>,
    /// The workspace whose menu shows, by squad id, with the menu's kind:
    /// `true` is the header +'s add menu, `false` the context menu.
    pub(super) workspace_menu: Option<(u64, bool)>,
    /// The close-workspace confirm: the wording facts the sheet reads.
    pub(super) workspace_confirm: Option<WorkspaceConfirm>,
    pub(super) chat: Chat,
    /// `--chat <id>`: the session chat mode shows, whatever has focus.
    pub(super) pinned: Option<SessionKey>,
    /// Cmd alone is held: rows and tabs show their cmd-digit badges.
    pub(super) picking: bool,
    /// The key sheet (cmd-/) covers the window.
    pub(super) sheet: bool,
    /// The answer sheet: the one question it holds, the choice the user
    /// picked, and the typed draft. Esc and Later close; Answer submits the
    /// typed words, else the pick.
    pub(super) answer: Option<answer::Sheet>,
    /// The arrival toast: the question it names and the timer generation,
    /// so only the newest toast's expiry clears it.
    question_toast: Option<(String, u64)>,
    question_toast_gen: u64,
    /// The update card: a staged build waiting for Restart, or the notes of
    /// the build this launch just installed (shown once).
    update_card: Option<UpdateCard>,
    /// A staged build found while the notes card was up; Dismiss offers it.
    held_update: Option<super::update::Ready>,
    /// Every question id any board read has listed. Arrival = an id the set
    /// has not met; the first read seeds it silently, unless the shot flag
    /// below is set, so an app open never toasts a backlog.
    seen_questions: HashSet<String>,
    /// A good question read has seeded `seen_questions`.
    questions_seeded: bool,
    /// The shot window toasts on its first read: the capture wants the
    /// arrival toast in frame.
    toast_on_first_read: bool,
    /// The shot window opens the answer sheet on the newest question when
    /// the first board read lands: the sheet proof.
    sheet_on_first_read: bool,
    /// The go-to palette (cmd-k) covers the window.
    pub(super) goto: Option<goto::GoTo>,
    /// The find bar (cmd-f) over the focused pane or the chat.
    pub(super) find: Option<find::Find>,
    /// Keyboard scroll mode (prefix+PgUp): the pane it holds, until q or Esc.
    pub(super) scroll_mode: Option<u64>,
    /// The rename-tab prompt (prefix+,).
    pub(super) rename: Option<mux_modes::Rename>,
    /// The focused agent's row menu (prefix+P).
    pub(super) row_menu: Option<mux_modes::RowMenu>,
    /// The Peek reply on screen: the agent and its recent transcript.
    pub(super) peek: Option<(String, Vec<String>)>,
    /// The launch composer: retained state, open flag, id mint, and the
    /// row the last launch named. Esc closes and keeps the draft.
    pub(super) launch: launch::Panel,
    /// The Settings panel (cmd-,): rows, query, page; None while closed.
    pub(super) settings: Option<settings::SettingsState>,
    /// The font face and size the terminal metrics should carry: the
    /// launch values, then a Settings write. Render moves the real
    /// `metrics` onto it, where the text system can measure.
    pub(super) font_want: (String, f32),
    /// The mux session this window attached to; panes open on it.
    pub(super) session: String,
    pub(super) ext: mods::Page,
    /// One SF Mono advance at the body size, measured each frame.
    pub(super) adv: Pixels,
    /// The window's width, measured each frame.
    pub(super) width: Pixels,
    /// The input handler's uncommitted text.
    preedit: Option<String>,
    _reader: Option<Task<()>>,
    _chat: Option<Task<()>>,
    _chat_wait: Option<Task<()>>,
    _hq: Option<Task<()>>,
    _plugins: Option<Task<()>>,
    _activation: Option<Subscription>,
    _appearance: Option<Subscription>,
    _notice: Option<Task<()>>,
    _question_toast: Option<Task<()>>,
    _update: Option<Task<()>>,
    _alerts: Option<Task<()>>,
    _alerts_tick: Option<Task<()>>,
    /// The attention signals: the lattice diff, the switch cache, the badge.
    alerts: alerts::Alerts,
    /// Whether the window is frontmost, kept by the activation observer.
    window_active: bool,
}

/// The content size sent to the server. The left pane and the title bar are
/// always reserved. A notice floats over the grid and never resizes it, and
/// one row and one column are kept for the half-cell inset around the grid.
/// The window's live subscriptions: repaint every pane on (de)activation
/// (the cursor turns hollow; the board poll pauses while the window is
/// inactive) and follow the system appearance live, because macOS flips it
/// at dusk with the app open.
fn wire_window(root: &mut Root, window: &mut Window, cx: &mut Context<Root>) {
    root._activation = Some(cx.observe_window_activation(window, |root, window, cx| {
        root.window_active = window.is_window_active();
        if let Some(poller) = &root.poller {
            poller.set_active(window.is_window_active());
        }
        for view in root.panes.values() {
            view.update(cx, |_, cx| cx.notify());
        }
    }));
    root._appearance = Some(cx.observe_window_appearance(window, |root, window, cx| {
        root.light = super::palette::appearance_is_light(window.appearance());
        root.restyle(cx);
    }));
}

fn grid_size(viewport: Size<Pixels>, cell: Size<Pixels>) -> (u16, u16) {
    let height = viewport.height - px(ui::TITLE_H);
    let width = viewport.width - px(left_pane::WIDTH);
    let rows = ((height / cell.height).floor() - 1.0).max(1.0);
    let cols = ((width / cell.width).floor() - 1.0).max(1.0);
    (
        rows.min(u16::MAX as f32) as u16,
        cols.min(u16::MAX as f32) as u16,
    )
}

impl Root {
    fn new(
        session: String,
        main: MainView,
        pinned: Option<SessionKey>,
        folds: HashSet<String>,
        group_by: GroupBy,
        metrics: Metrics,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_link(
            session, main, pinned, folds, group_by, metrics, None, window, cx,
        )
    }

    /// The shot path's constructor: `prewarmed` hands a link already
    /// connected and drained to the world's first layout, so the one
    /// frame gpui paints synchronously at open carries the world's data.
    fn new_with_link(
        session: String,
        main: MainView,
        pinned: Option<SessionKey>,
        folds: HashSet<String>,
        group_by: GroupBy,
        metrics: Metrics,
        prewarmed: Option<super::link::WarmedLink>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::write_pid_file(&session);
        let sent_size = grid_size(window.viewport_size(), metrics.cell);
        let light = super::palette::appearance_is_light(window.appearance());
        let font_want = (
            metrics.font.family.to_string(),
            f32::from(metrics.font_size),
        );
        let theme_cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
        let (palette, theme_warning) = super::palette::for_appearance(light, &theme_cwd);
        // The key layer (config.mux.prefix, [mux.keys]), installed before the
        // scanner reads its first keystroke - the same loader the terminal
        // client uses. A refused rebind surfaces as the startup notice rather
        // than silently running the shipped default.
        let (keymap, key_warnings) = fno::ui::digest_overlay::keymap(&theme_cwd);
        fno::ui::keys::install(keymap);
        let key_warning = key_warnings.first().map(|w| w.0.clone());
        let mut root = Root {
            main,
            link: None,
            layout: Layout::default(),
            frames: HashMap::new(),
            panes: HashMap::new(),
            modes: Modes::default(),
            scanner: fno::ui::keys::Scanner::default(),
            notice: key_warning.or(theme_warning),
            notice_failed: false,
            notice_gen: 0,
            shot: false,
            shot_empty_chat: false,
            closed: None,
            offer: None,
            first_run: false,
            metrics,
            palette,
            focus: cx.focus_handle(),
            sent_size,
            board: Board::default(),
            poller: None,
            area: Rc::new(Cell::new(Bounds::default())),
            drag: None,
            seam_drag: None,
            wheel: 0.0,
            wheel_pane: None,
            answers: HashMap::new(),
            board_draft: Default::default(),
            mail: None,
            folds,
            group_by,
            squad_field: None,
            workspace_menu: None,
            workspace_confirm: None,
            light,
            theme_cwd: theme_cwd.clone(),
            session: session.clone(),
            ext: mods::Page::default(),
            adv: px(ui::BODY * 0.6),
            width: px(1360.0),
            chat: Chat::default(),
            pinned,
            picking: false,
            sheet: false,
            answer: None,
            question_toast: None,
            question_toast_gen: 0,
            update_card: None,
            held_update: None,
            seen_questions: HashSet::new(),
            questions_seeded: false,
            toast_on_first_read: false,
            sheet_on_first_read: false,
            goto: None,
            find: None,
            scroll_mode: None,
            rename: None,
            row_menu: None,
            peek: None,
            launch: launch::Panel::default(),
            settings: None,
            font_want,
            preedit: None,
            _reader: None,
            _chat: None,
            _chat_wait: None,
            _hq: None,
            _plugins: None,
            _activation: None,
            _appearance: None,
            _notice: None,
            _question_toast: None,
            _update: None,
            _alerts: None,
            _alerts_tick: None,
            alerts: alerts::Alerts::new(),
            // A terminal launch leaves the new window behind the frontmost
            // app; the activation observer only reports changes, so the
            // first read happens here.
            window_active: window.is_window_active(),
        };
        wire_window(&mut root, window, cx);
        Self::attach_link(&mut root, &session, sent_size, prewarmed, cx);
        root.wire_alerts(cx);
        root.start_board_poll(cx);
        root.start_plugins(cx);
        // The control channel: the drain loop applies rows through the
        // same paths clicks ride (focus, view switch, quit).
        crate::control::spawn_poll(session.clone(), cx);
        // `--mode mods` opens on the view without a switch, so read here.
        if root.main == MainView::Mods {
            root.ext.read();
        }
        root
    }

    /// The drive surface finds the live app through this pid file (the
    /// fleet law: stop builds by pid, never a sweep). The app names its
    /// own pid at open; a stale file cleans up on the next read.
    fn write_pid_file(session: &str) {
        if let Some(dir) = fno::app_control::support_dir() {
            std::fs::create_dir_all(&dir).ok();
            std::fs::write(
                dir.join(format!("fno-app-{session}.pid")),
                std::process::id().to_string(),
            )
            .ok();
        }
    }

    /// Connect the root to its session: a prewarmed link adopts the
    /// stream the shot world already drained; otherwise the root
    /// connects fresh, and a refusal lands in the notice, not a panic.
    fn attach_link(
        root: &mut Root,
        session: &str,
        sent_size: (u16, u16),
        prewarmed: Option<super::link::WarmedLink>,
        cx: &mut Context<Self>,
    ) {
        match prewarmed {
            Some(warm) => {
                root.link = Some(warm.link);
                root.send(ClientMsg::PaneInset {
                    rows: mouse::INSET.0,
                    cols: mouse::INSET.1,
                });
                root.spawn_reader(warm.early, warm.rest, cx);
            }
            None => match Link::connect(session, sent_size.0, sent_size.1) {
                Ok((link, rx)) => {
                    root.link = Some(link);
                    root.send(ClientMsg::PaneInset {
                        rows: mouse::INSET.0,
                        cols: mouse::INSET.1,
                    });
                    root.spawn_reader(Vec::new(), rx, cx);
                }
                Err(e) => {
                    if crate::first_run::offers_start(&e) {
                        root.offer = Some(crate::first_run::Offer::Ask);
                    } else {
                        root.closed = Some(e.to_string());
                    }
                }
            },
        }
    }

    /// The main pane while no server answers: the offer card.
    fn offer_view(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let offer = self.offer.unwrap_or(crate::first_run::Offer::Ask);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(crate::first_run::offer_card(self, offer, cx))
            .into_any_element()
    }

    /// The offer's answer: spawn the server off the UI thread, then attach
    /// as a fresh launch would. A refusal lands in the closed notice and
    /// the card asks again.
    pub(super) fn start_mux(&mut self, cx: &mut Context<Self>) {
        if self.offer != Some(crate::first_run::Offer::Ask) {
            return;
        }
        self.offer = Some(crate::first_run::Offer::Starting);
        cx.notify();
        let session = self.session.clone();
        cx.spawn(async move |this, cx| {
            let started = {
                let session = session.clone();
                cx.background_executor()
                    .spawn(async move { fno::app_api::start_server(&session) })
                    .await
            };
            this.update(cx, |root, cx| {
                match started {
                    Ok(()) => {
                        root.offer = None;
                        let sent = root.sent_size;
                        Self::attach_link(root, &session, sent, None, cx);
                    }
                    Err(e) => {
                        root.offer = Some(crate::first_run::Offer::Ask);
                        root.closed = Some(e.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The attention signals: notification taps land here and the task
    /// routes them through the same click path a sidebar row takes.
    fn wire_alerts(&mut self, cx: &mut Context<Self>) {
        let wants_auth = self.alerts.notification_on();
        if let Some(mut click_rx) = alerts::install_click(wants_auth) {
            self._alerts = Some(cx.spawn(async move |this, cx| {
                while let Some(pane) = click_rx.recv().await {
                    if this
                        .update(cx, |root, cx| root.on_alert_click(pane, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            }));
        }
        // The switches live in a settings file no event announces: a one
        // second tick re-reads them, so turning the badge off clears it at
        // once even on an idle session that pushes no layouts.
        self._alerts_tick = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(1))
                .await;
            if this.update(cx, |root, cx| root.on_alerts_tick(cx)).is_err() {
                break;
            }
        }));
    }

    /// The board poll: fleet, questions and seats arrive here.
    fn start_board_poll(&mut self, cx: &mut Context<Self>) {
        match Poller::start() {
            Ok((poller, mut rx)) => {
                self.poller = Some(poller);
                self._hq = Some(cx.spawn(async move |this, cx| {
                    while let Some(reply) = rx.recv().await {
                        if this.update(cx, |root, cx| root.on_hq(reply, cx)).is_err() {
                            break;
                        }
                    }
                }));
            }
            Err(e) => self.notice = Some(format!("the board poll did not start: {e}")),
        }
    }

    /// Adopt the link's event stream: apply the pre-drained events first,
    /// then spawn the reader task over the live receiver. One body for
    /// the cold path and the shot's warmed path, so the two cannot drift.
    fn spawn_reader(
        &mut self,
        early: Vec<Event>,
        mut rx: tokio::sync::mpsc::UnboundedReceiver<Event>,
        cx: &mut Context<Self>,
    ) {
        if !early.is_empty() {
            self.apply(early, cx);
        }
        self._reader = Some(cx.spawn(async move |this, cx| {
            while let Some(first) = rx.recv().await {
                let mut batch = vec![first];
                while let Ok(more) = rx.try_recv() {
                    batch.push(more);
                }
                if this.update(cx, |root, cx| root.apply(batch, cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// The control-channel rows the poll loop drained: one path per row,
    /// riding the same rows a click rides. Quit persists the UI state
    /// global (the frame saves on the interactive quit paths) and quits.
    pub(super) fn apply_control(
        &mut self,
        cmds: &[fno::app_control::AppCommand],
        cx: &mut Context<Self>,
    ) {
        for cmd in cmds {
            match cmd {
                fno::app_control::AppCommand::Quit => {
                    record_ui_state(self, cx);
                    cx.quit();
                }
                fno::app_control::AppCommand::Answer { option } => {
                    let open = self
                        .chat
                        .conv
                        .open_question()
                        .map(|(_, ask)| ask.choices.len());
                    match open {
                        Some(n) if (1..=n).contains(option) => {
                            self.chat.pick = option - 1;
                            self.answer_choice(cx);
                        }
                        Some(n) => self.say(
                            format!("fno mux app answer: the question has {n} options"),
                            cx,
                        ),
                        None => self.say(
                            "fno mux app answer: no question waits in this chat".into(),
                            cx,
                        ),
                    }
                }
                fno::app_control::AppCommand::View { view } => match view.as_str() {
                    "chat" => self.show(MainView::Chat, cx),
                    "terminal" => self.show(MainView::Terminal, cx),
                    "orchestration" => self.show(MainView::Orchestration, cx),
                    "mods" => self.show(MainView::Mods, cx),
                    "inbox" => self.show(MainView::Inbox, cx),
                    other => {
                        self.notice = Some(format!("fno mux app: unknown view {other:?}"));
                        self.notice_failed = true;
                    }
                },
                fno::app_control::AppCommand::Focus { agent } => {
                    let now = fno::app_api::digest_overlay::now_secs();
                    // The lookup reads the same rows the pane paints, so a
                    // focus naming the launched row's task label resolves.
                    let streamed = match self.main {
                        MainView::Chat => self.chat.key.as_ref().map(|k| k.session_id.as_str()),
                        _ => None,
                    };
                    let launched = self
                        .launch
                        .show
                        .as_deref()
                        .zip(self.launch.show_task.as_deref());
                    let hit = super::left_pane::rows(
                        &self.layout,
                        &self.board,
                        now,
                        &self.folds,
                        self.group_by,
                        streamed,
                        launched,
                    )
                    .into_iter()
                    .find_map(|r| match r {
                        super::left_pane::Row::Item {
                            label,
                            action: Some(a),
                            ..
                        } if label == *agent => Some(a),
                        _ => None,
                    });
                    match hit {
                        Some(a) => self.act(a, cx),
                        None => {
                            self.notice =
                                Some(format!("fno mux app focus: no agent row names {agent:?}"));
                            self.notice_failed = true;
                        }
                    }
                }
            }
        }
    }

    /// Start the plugins thread and its reply reader. The list reads when
    /// the view shows, after a switch and when an install pane closes.
    fn start_plugins(&mut self, cx: &mut Context<Self>) {
        match mods::start() {
            Ok((asks, mut rx)) => {
                self.ext = mods::Page::new(asks);
                self._plugins = Some(cx.spawn(async move |this, cx| {
                    while let Some(reply) = rx.recv().await {
                        let landed = this.update(cx, |root, cx| {
                            let did = root.ext.apply(reply);
                            if did.terminal {
                                root.main = MainView::Terminal;
                            }
                            if let Some(text) = did.notice {
                                root.say_as(text, did.failed, cx);
                            }
                            cx.notify();
                        });
                        if landed.is_err() {
                            break;
                        }
                    }
                }));
            }
            Err(e) => self.notice = Some(format!("The plugins read did not start: {e}")),
        }
    }

    fn send(&mut self, msg: ClientMsg) {
        if let Some(link) = &self.link {
            if let Err(e) = link.send(&msg) {
                self.closed = Some(e.to_string());
                self.link = None;
            }
        }
    }

    /// Show `text` in the corner for [`NOTICE_FOR`].
    pub(super) fn say(&mut self, text: String, cx: &mut Context<Self>) {
        self.say_as(text, false, cx);
    }

    /// [`Self::say`], in the error color when `failed`.
    fn say_as(&mut self, text: String, failed: bool, cx: &mut Context<Self>) {
        if self.shot {
            return;
        }
        self.notice = Some(text);
        self.notice_failed = failed;
        self.notice_gen += 1;
        // Without this the toast paints only when an unrelated event redraws;
        // on an idle board it stayed invisible for its whole lifetime.
        cx.notify();
        let gen = self.notice_gen;
        self._notice = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTICE_FOR).await;
            let _ = this.update(cx, |root, cx| {
                if root.notice_gen == gen {
                    root.notice = None;
                    cx.notify();
                }
            });
        }));
    }

    /// Read the release feed now and every [`update::RECHECK`] until a build
    /// is staged. A failed read waits for the next round, and a build that
    /// already names the running version never shows a card.
    pub(super) fn start_update(&mut self, cx: &mut Context<Self>) {
        if !super::update::bundled() {
            return;
        }
        self.update_card = super::update::take_pending_notes(super::update::RUNNING)
            .map(|(version, notes)| UpdateCard::Installed { version, notes });
        self._update = Some(cx.spawn(async move |this, cx| loop {
            let read = cx
                .background_executor()
                .spawn(async { super::update::check(super::update::RUNNING) })
                .await;
            match read {
                Ok(Some(ready)) => {
                    let _ = this.update(cx, |root, cx| {
                        match root.update_card {
                            // The notes card of the build just installed is
                            // up: hold the staged build for its Dismiss.
                            Some(UpdateCard::Installed { .. }) => root.held_update = Some(ready),
                            _ => root.update_card = Some(UpdateCard::Ready(ready)),
                        }
                        cx.notify();
                    });
                }
                Ok(None) => {}
                Err(e) => eprintln!("fno-app: update check: {e}"),
            }
            cx.background_executor().timer(super::update::RECHECK).await;
        }));
    }

    /// The update card's two buttons. Later and Dismiss offer a build held
    /// behind the notes card; Restart swaps in the staged one and quits.
    fn update_act(&mut self, action: &Action, cx: &mut Context<Self>) {
        if *action == Action::DismissUpdate {
            self.update_card = self.held_update.take().map(UpdateCard::Ready);
            return;
        }
        let Some(UpdateCard::Ready(ready)) = self.update_card.clone() else {
            return;
        };
        match super::update::install_and_relaunch(&ready) {
            Ok(()) => {
                self.send(ClientMsg::Detach);
                record_ui_state(self, cx);
                cx.quit();
            }
            Err(e) => self.say_as(format!("update: {e}"), true, cx),
        }
    }

    /// The update card: a raised card bottom-left. A staged build offers Restart; a fresh install shows its notes
    /// once and drops on Dismiss.
    fn update_card_view(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        // The first-run offer owns the main pane: the card stays set and
        // returns when the mux attaches.
        if self.offer.is_some() {
            return None;
        }
        let card = self.update_card.as_ref()?;
        let (head, body, buttons) = match card {
            UpdateCard::Ready(r) => (
                format!("fno-app {} is ready", r.version),
                "Restart to finish the update.".to_string(),
                vec![
                    ui::button(
                        "update-restart",
                        "Restart",
                        ui::Button::Primary,
                        &self.palette,
                    )
                    .on_click(cx.listener(|root, _, _, cx| root.act(Action::RestartForUpdate, cx))),
                    ui::button("update-later", "Later", ui::Button::Quiet, &self.palette).on_click(
                        cx.listener(|root, _, _, cx| root.act(Action::DismissUpdate, cx)),
                    ),
                ],
            ),
            UpdateCard::Installed { version, notes } => (
                format!("Updated to fno-app {version}"),
                ui::cut(notes.trim(), 400),
                vec![ui::button(
                    "update-dismiss",
                    "Dismiss",
                    ui::Button::Primary,
                    &self.palette,
                )
                .on_click(cx.listener(|root, _, _, cx| root.act(Action::DismissUpdate, cx)))],
            ),
        };
        Some(
            div()
                .absolute()
                .bottom(px(88.0))
                .left(px(12.0))
                .w(px(420.0))
                .flex()
                .flex_col()
                .gap(px(6.0))
                .px_4()
                .py_3()
                .rounded(px(6.0))
                .border_1()
                .border_color(rgb(self.palette.faint))
                .bg(rgb(self.palette.raised))
                .font_family(ui::MONO)
                .text_size(px(ui::BODY))
                .child(ui::role(div(), ui::Role::Name, &self.palette).child(head))
                .child(div().text_color(rgb(self.palette.muted)).child(body))
                .child(div().flex().flex_row().gap(px(6.0)).children(buttons)),
        )
    }

    /// Raise the arrival toast for one question. The element reads the
    /// board live at paint, so a question the board drops hides the toast
    /// even before the timer does. The shot holds it: the capture wants
    /// the surface in frame, not a timed one.
    fn say_question(&mut self, id: String, cx: &mut Context<Self>) {
        self.question_toast_gen += 1;
        let gen = self.question_toast_gen;
        self.question_toast = Some((id, gen));
        cx.notify();
        if self.shot {
            return;
        }
        self._question_toast = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(QUESTION_TOAST_FOR).await;
            let _ = this.update(cx, |root, cx| {
                if root.question_toast.as_ref().is_some_and(|(_, g)| *g == gen) {
                    root.question_toast = None;
                    cx.notify();
                }
            });
        }));
    }

    /// The arrival toast: a raised card top-right, the diamond, who
    /// asked, a preview of the question, and the Answer door. It reads
    /// the board live, so a dropped question hides it at once.
    /// The Inbox lists the question itself, so it never toasts there.
    fn question_toast_card(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        self.question_toast
            .as_ref()
            .filter(|_| self.main != MainView::Inbox)
            .and_then(|(id, _)| self.board.questions().iter().find(|q| q.item.id == *id))
            .map(|q| {
                let item = &q.item;
                let ask = item
                    .asker
                    .as_ref()
                    .map(|a| a.handle.as_str())
                    .unwrap_or("an agent");
                let open = Action::OpenAnswer(item.id.clone());
                div()
                    .absolute()
                    // Below the Questions heading on the HQ board, never over it.
                    .top(px(ui::TITLE_H + 44.0))
                    .right(px(12.0))
                    .w(px(420.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .px_3()
                    .py_2()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(self.palette.faint))
                    .bg(rgb(self.palette.raised))
                    .font_family(ui::MONO)
                    .text_size(px(ui::BODY))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(rgb(self.palette.warn))
                                    .child("◆"),
                            )
                            .child(
                                ui::role(div().flex_1().min_w_0(), ui::Role::Name, &self.palette)
                                    .child(format!("New question from {ask}")),
                            )
                            .child(
                                ui::button(
                                    "toast-answer",
                                    "Answer",
                                    ui::Button::Primary,
                                    &self.palette,
                                )
                                .on_click(
                                    cx.listener(move |root, _, _, cx| root.act(open.clone(), cx)),
                                ),
                            ),
                    )
                    .child(ui::cut(&item.title, 52))
            })
    }

    pub(super) fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        fno::ui_usage::click("app", click_id(&action));
        // Any other click ends an in-tab rename without sending it.
        if self.rename.as_ref().is_some_and(|r| r.inline) {
            self.rename = None;
        }
        match action {
            Action::Send(cmd) => {
                // A click on the board shows what it picked; chat and the
                // terminal keep their view and follow the new focus.
                if self.main == MainView::Orchestration {
                    self.main = MainView::Chat;
                }
                self.send(ClientMsg::Command(cmd));
            }
            Action::Notice(text) => self.say(text, cx),
            Action::Answer { id, option } => match &self.poller {
                // A row already sent or answered takes no second click; a
                // refused one takes a retry.
                Some(_) if answer::in_flight_or_done(self.answers.get(&id)) => {}
                Some(poller) => {
                    if poller.answer(id.clone(), fno::needs_overlay::AnswerPick::Option(option)) {
                        self.answers.insert(id, AnswerState::Sending(option));
                    } else {
                        self.say("the board poll stopped: cannot answer".into(), cx);
                    }
                }
                None => self.say("no board poll: cannot answer".into(), cx),
            },
            Action::OpenAnswer(id) => {
                // The sheet answers what the toast announced.
                if self.question_toast.as_ref().is_some_and(|(t, _)| *t == id) {
                    self.question_toast = None;
                }
                self.answer = Some(answer::Sheet::new(id));
            }
            Action::CloseAnswer => self.answer = None,
            Action::DismissUpdate | Action::RestartForUpdate => self.update_act(&action, cx),
            Action::PickAnswer(n) => {
                if let Some(s) = self.answer.as_mut() {
                    s.pick = Some(n);
                }
            }
            Action::SubmitAnswer => answer::submit(self, cx),
            Action::Show(view) => self.show(view, cx),
            Action::NewWorkspace
            | Action::ToggleWorkspace(_)
            | Action::ToggleElsewhere
            | Action::OpenWorkspaceMenu(_)
            | Action::OpenAddMenu(_)
            | Action::CycleGrouping
            | Action::CloseWorkspaceMenu
            | Action::NewTabIn(_)
            | Action::NewShell(_)
            | Action::RenameWorkspace(_)
            | Action::MoveWorkspace(_, _)
            | Action::RemoveWorkspace(_)
            | Action::ConfirmCloseWorkspace(_)
            | Action::KeepWorkspace
            | Action::NewWorker(_) => self.act_workspace(action, cx),
            Action::ToggleKeySheet => {
                self.sheet = !self.sheet;
                if self.sheet {
                    self.goto = None;
                }
            }
            Action::ToggleChat(i) => {
                let open = &mut self.chat.conv.expanded;
                if !open.remove(&i) {
                    open.insert(i);
                }
                self.chat.list.splice(i..i + 1, 1);
            }
            Action::OpenSheet(i) => self.chat.sheet = Some(i),
            Action::CloseSheet => self.chat.sheet = None,
            Action::Copy(text) => {
                let n = text.chars().count();
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.say(format!("copied {n} characters"), cx);
            }
            Action::SendChat => self.submit_chat(cx),
            Action::PickChoice(i) => {
                // A second click on the picked row answers with it.
                if self.chat.pick == i {
                    self.answer_choice(cx);
                } else {
                    self.chat.pick = i;
                }
            }
            Action::AnswerChoice => self.answer_choice(cx),
            Action::SubmitLaunch => self.submit_launch(cx),
            Action::Reconnect => self.open_chat(cx),
            Action::Mail(text) => match &self.poller {
                Some(poller) => {
                    if poller.mail(text) {
                        self.mail = Some(MailState::Sending);
                    } else {
                        self.mail = Some(MailState::Refused("the board poll stopped".into()));
                    }
                    cx.notify();
                }
                None => self.say("no board poll: cannot mail".into(), cx),
            },
            Action::OpenLaunch(kind) => self.open_launch(kind, cx),
        }
        cx.notify();
    }

    /// The workspace actions: the menus, the fold, the rename field and the
    /// close confirm.
    fn act_workspace(&mut self, action: Action, cx: &mut Context<Self>) {
        match action {
            Action::NewWorkspace => {
                self.squad_field = Some(SquadField {
                    draft: Draft::default(),
                    field: Rc::new(chat::field::FieldState::default()),
                    squad: None,
                });
            }
            Action::ToggleWorkspace(id) => {
                let key = self
                    .layout
                    .squads
                    .iter()
                    .find(|s| s.id == id)
                    .map(super::left_pane::fold_key);
                if let Some(key) = key {
                    if !self.folds.remove(&key) {
                        self.folds.insert(key);
                    }
                    record_ui_state(self, cx);
                }
            }
            Action::ToggleElsewhere => {
                let key = super::left_pane::ELSEWHERE_KEY.to_string();
                if !self.folds.remove(&key) {
                    self.folds.insert(key);
                }
                record_ui_state(self, cx);
            }
            Action::OpenWorkspaceMenu(id) => {
                // One menu at a time; a second right-click moves it.
                self.workspace_menu = Some((id, false));
            }
            Action::OpenAddMenu(id) => self.workspace_menu = Some((id, true)),
            Action::CycleGrouping => {
                self.group_by = match self.group_by {
                    GroupBy::Workspaces => GroupBy::State,
                    GroupBy::State => GroupBy::Machine,
                    GroupBy::Machine => GroupBy::Workspaces,
                };
                record_ui_state(self, cx);
            }
            Action::CloseWorkspaceMenu => self.workspace_menu = None,
            Action::NewTabIn(id) => {
                self.workspace_menu = None;
                self.send(ClientMsg::Command(Command::SelectSquad(id)));
                self.send(ClientMsg::Command(Command::NewTab));
            }
            Action::NewShell(id) => self.act(Action::NewTabIn(id), cx),
            Action::RenameWorkspace(id) => {
                self.workspace_menu = None;
                let name = self
                    .layout
                    .squads
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                let mut draft = Draft::default();
                draft.replace_selection(&name);
                self.squad_field = Some(SquadField {
                    draft,
                    field: Rc::new(chat::field::FieldState::default()),
                    squad: Some(id),
                });
            }
            Action::MoveWorkspace(id, delta) => {
                self.workspace_menu = None;
                self.send(ClientMsg::Command(Command::MoveSquad { squad: id, delta }));
            }
            Action::RemoveWorkspace(id) => self.ask_close_workspace(id, cx),
            Action::ConfirmCloseWorkspace(id) => {
                self.workspace_confirm = None;
                self.send(ClientMsg::Command(Command::RemoveSquad(id)));
            }
            Action::KeepWorkspace => self.workspace_confirm = None,
            Action::NewWorker(id) => {
                self.workspace_menu = None;
                let cwd = self
                    .layout
                    .squads
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| s.canonical_cwd.clone())
                    .unwrap_or_default();
                self.open_launch(Some(crate::launch::Kind::Worker), cx);
                self.launch.state.preset_project(&cwd);
            }
            _ => {}
        }
    }

    /// The Close menu item: ask first, naming the pane count.
    fn ask_close_workspace(&mut self, id: u64, cx: &mut Context<Self>) {
        self.workspace_menu = None;
        let last = self.layout.squads.len() == 1;
        match self.layout.squads.iter().find(|s| s.id == id) {
            Some(s) => {
                self.workspace_confirm = Some(WorkspaceConfirm {
                    squad: s.id,
                    name: s.name.clone(),
                    panes: s.panes,
                    last,
                });
            }
            None => self.say(format!("no workspace {id} here"), cx),
        }
    }

    /// Open the composer over whatever view shows. A fresh open re-reads
    /// the harness catalog; a reopened one keeps its picks and draft.
    pub(super) fn open_launch(&mut self, kind: Option<launch::Kind>, cx: &mut Context<Self>) {
        if !self.launch.open {
            self.launch.state.refresh_harnesses();
        }
        self.launch.state.open(kind);
        self.launch.open = true;
        cx.notify();
    }

    /// Close the composer; the draft and picks survive on the root.
    pub(super) fn close_launch(&mut self, cx: &mut Context<Self>) {
        self.launch.open = false;
        cx.notify();
    }

    /// The composer's shell escape: close the panel, open a terminal pane,
    /// and send the command into it. The pane primitives already exist
    /// (NewTab, then Input); the ordered link lands the bytes after the tab
    /// switch, so the command runs in the new pane.
    fn run_shell_from_composer(&mut self, cmd: String, cx: &mut Context<Self>) {
        self.close_launch(cx);
        self.send(ClientMsg::Command(Command::NewTab));
        let mut bytes = cmd.into_bytes();
        bytes.push(b'\n');
        self.send(ClientMsg::Input(bytes));
    }

    /// Submit one launch attempt: the shell escape first (a leading `!`, or
    /// the shell kind, runs the command in a new terminal pane instead of a
    /// launch), then the panel's in-place refusals, then the door. The
    /// draft stays until the verdict lands.
    fn submit_launch(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = launch::typed_project(&self.launch.state.draft) {
            self.add_project_path(path, cx);
            return;
        }
        if let Some(cmd) = launch::shell_command(&self.launch.state.draft) {
            self.run_shell_from_composer(cmd, cx);
            return;
        }
        if self.launch.state.kind == launch::Kind::Shell {
            // The shell kind without the bang still runs as a shell command.
            let text = self.launch.state.draft.text().trim().to_string();
            if !text.is_empty() {
                self.run_shell_from_composer(text, cx);
                return;
            }
        }
        if let Some(link) = &self.link {
            if let Some(why) = self.launch.state.block() {
                self.launch.state.phase = launch::Phase::Refused(why);
                cx.notify();
                return;
            }
            let req = self.launch.state.request(self.launch.next_id);
            self.launch.next_id += 1;
            self.launch.state.phase = launch::Phase::Starting;
            if let Err(e) = link.send(&ClientMsg::AgentLaunch(req)) {
                self.closed = Some(e.to_string());
                self.link = None;
            }
            cx.notify();
            return;
        }
        self.launch.state.phase = launch::Phase::Refused("not attached to a mux session".into());
        cx.notify();
    }

    /// The attention signals on one layout push: the enter-only lattice diff
    /// (module docs in `alerts.rs` carry the route reasoning), plus the Dock
    /// badge, written only on change. `window_active` picks the toast form.
    fn on_layout_alerts(&mut self, cx: &mut Context<Self>) {
        for sig in self
            .alerts
            .on_layout(&self.layout.agents, self.window_active)
        {
            self.consume_alert(sig, cx);
        }
        if let Some(b) = self.alerts.badge(&self.layout.agents) {
            alerts::set_badge(b);
        }
    }

    /// Route one alert signal to the toast, the OS notification or the
    /// sound.
    fn consume_alert(&mut self, sig: Signal, cx: &mut Context<Self>) {
        match sig {
            Signal::Toast(text) => self.say(text, cx),
            Signal::Notify {
                title,
                body,
                pane_id,
            } => alerts::notify(&title, &body, pane_id),
            Signal::Sound(kind) => alerts::play(kind),
        }
    }

    /// The switch tick: re-read the prefs and re-apply the badge, so a
    /// settings write lands without waiting for a server event.
    fn on_alerts_tick(&mut self, _cx: &mut Context<Self>) {
        if let Some(b) = self.alerts.badge(&self.layout.agents) {
            alerts::set_badge(b);
        }
    }

    /// A tapped notification: front the app, open Chat, then send the same
    /// command a sidebar-row click sends. An exited or gone agent still
    /// fronts the app but says so in the corner toast (AC5's edge).
    fn on_alert_click(&mut self, pane: u64, cx: &mut Context<Self>) {
        cx.activate(true);
        self.main = MainView::Chat;
        let row = self
            .layout
            .agents
            .iter()
            .find(|a| a.pane_id == Some(pane) && !a.exited);
        match row {
            Some(a) => match fno::app_api::client::agent_group::click_command(a) {
                fno::app_api::client::agent_group::RowClick::Send(cmd) => {
                    self.send(ClientMsg::Command(cmd))
                }
                fno::app_api::client::agent_group::RowClick::Notice(text) => self.say(text, cx),
            },
            None => self.say(format!("pane {pane}: the agent is gone"), cx),
        }
        self.sync_chat(cx);
        cx.notify();
    }

    fn show(&mut self, view: MainView, cx: &mut Context<Self>) {
        self.main = view;
        if view == MainView::Mods {
            self.ext.read();
        }
        self.sync_chat(cx);
        self.find_gc(cx);
        // sync_chat early-returns when the chat key is unchanged, which is
        // every switch between non-chat views; record here so the global
        // the close button reads never carries a stale view.
        record_ui_state(self, cx);
    }

    /// The project path a session runs in, for its project skills: the
    /// launch panel's known project whose basename is the row's cwd.
    fn chat_project(&self, key: Option<&SessionKey>) -> String {
        let base = key.and_then(|k| {
            self.layout
                .agents
                .iter()
                .find(|a| a.harness_session_id.as_deref() == Some(k.session_id.as_str()))
                .and_then(|a| a.cwd_base.clone())
        });
        base.and_then(|b| self.launch.state.project_for_base(&b))
            .unwrap_or_default()
    }

    /// In chat mode, stream the focused pane's agent session. A new focus
    /// parks the shown conversation and resumes the new one where it left.
    fn sync_chat(&mut self, cx: &mut Context<Self>) {
        // The empty-pane shot pins the start view: no layout event may
        // hand the pane a session.
        if self.shot_empty_chat {
            return;
        }
        let key = chat::focused_key(self);
        if self.main != MainView::Chat || key == self.chat.key {
            return;
        }
        let project = self.chat_project(key.as_ref());
        self.chat.switch(key, &project);
        self.open_chat(cx);
        // The shown conversation changed: chat find re-scans the new rows.
        self.find_rescan(cx);
        record_ui_state(self, cx);
    }

    /// (Re)open the shown session's stream from the next unread event.
    fn open_chat(&mut self, cx: &mut Context<Self>) {
        self._chat = None;
        self.chat.stream = None;
        self.chat.closed = None;
        self.chat.opened_at = std::time::Instant::now();
        let Some(key) = self.chat.key.clone() else {
            return;
        };
        match chat::open(&self.session, key, self.chat.conv.events) {
            Ok((stream, mut rx)) => {
                self.chat.stream = Some(stream);
                self._chat = Some(cx.spawn(async move |this, cx| {
                    while let Some(first) = rx.recv().await {
                        let mut batch = vec![first];
                        while let Ok(more) = rx.try_recv() {
                            batch.push(more);
                        }
                        if this.update(cx, |root, cx| root.on_chat(batch, cx)).is_err() {
                            break;
                        }
                    }
                }));
                // An empty session sends nothing; repaint once its wait is up.
                self._chat_wait = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(chat::EMPTY_AFTER).await;
                    let _ = this.update(cx, |_, cx| cx.notify());
                }));
            }
            Err(e) => self.chat.closed = Some(chat::Closed::Failed(e.to_string())),
        }
    }

    fn on_chat(&mut self, batch: Vec<chat::Msg>, cx: &mut Context<Self>) {
        self.chat.apply(batch);
        // The agent may still be streaming with the bar open: re-scan, keep
        // the current match by identity.
        self.find_rescan(cx);
        cx.notify();
    }

    /// Send the draft as the TUI reply path does: a paste addressed to the
    /// session's own pane, then Enter to that same pane once the harness has
    /// taken it. Focus moving in between changes nothing.
    fn submit_chat(&mut self, cx: &mut Context<Self>) {
        if self.chat.closed.is_some() {
            return;
        }
        let attach_now = self.chat.draft.attachment().cloned();
        if self.chat.draft.text().trim().is_empty() && attach_now.is_none() {
            return;
        }
        let target = self
            .chat
            .key
            .as_ref()
            .and_then(|key| chat::target_pane(self, key));
        let (Some((pane, identity)), Some(link)) = (target, self.link.clone()) else {
            self.say(
                "no pane on screen hosts this session: open its pane to send".into(),
                cx,
            );
            return;
        };
        let attachment = self.chat.draft.take_attachment();
        let mut text = self.chat.draft.take();
        // Unbracketed, each newline would type Enter and submit early.
        if !self.modes.bracketed_paste {
            text = text.replace('\n', " ");
        }
        let harness = self
            .chat
            .key
            .as_ref()
            .map(|k| k.harness.clone())
            .unwrap_or_default();
        let id = self.chat.next_request;
        let mut next = id + 1;
        self.chat.next_request += if attachment.is_some() { 3 } else { 2 };
        let input = |request_id, bytes| {
            ClientMsg::PaneInput(proto::pane_input::PaneInputRequest {
                request_id,
                pane,
                expected_identity: identity.clone(),
                bytes,
            })
        };
        if !text.is_empty() {
            self.send(input(id, keys::paste(&text, self.modes)));
        }
        // The image rides as a keystroke, never as pane bytes: the clipboard
        // is restored on the main thread first, so the user's next copy never
        // clobbers it mid-send. The stroke precedes Enter with its own settle,
        // so the harness reads the clipboard before the submit lands.
        let stroke = match (attachment.as_ref(), Self::image_paste_key(&harness)) {
            (Some(att), Some(bytes)) => {
                let image = gpui::Image::from_bytes(att.format, (*att.bytes).clone());
                cx.write_to_clipboard(ClipboardItem::new_image(&image));
                let msg = input(next, bytes.to_vec());
                next += 1;
                Some(msg)
            }
            (Some(_), None) => {
                self.say(
                    format!("no verified image paste for {harness}; the text went alone"),
                    cx,
                );
                None
            }
            (None, _) => None,
        };
        // Enter only when a turn exists to submit: text typed, or an image
        // pasted onto the harness. An image-only draft on a harness with no
        // verified stroke sends nothing, so a bare Enter never submits the
        // pane's stale input.
        let enter = (!text.is_empty() || stroke.is_some()).then(|| input(next, b"\r".to_vec()));
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(chat::SUBMIT_SETTLE).await;
            if let Some(msg) = stroke {
                if link.send(&msg).is_err() {
                    return; // the link died: an Enter would type into a dead pane
                }
                cx.background_executor().timer(chat::SUBMIT_SETTLE).await;
            }
            if let Some(enter) = enter {
                link.send(&enter).ok();
            }
        })
        .detach();
        if !text.is_empty() {
            self.chat.pending.push(text);
        }
    }

    /// Answer the open question with the picked row. Where the mux already
    /// read the harness's own menu off the pane and pinned a keystroke per
    /// row, the answer goes through `PaneAnswer`, which the server checks
    /// against the live grid before it types. Claude's question screen has
    /// no extracted menu, but its rows take their own number as a key, so a
    /// pick there types that digit into the pane.
    fn answer_choice(&mut self, cx: &mut Context<Self>) {
        let Some((_, ask)) = self.chat.conv.open_question() else {
            return;
        };
        let pick = self.chat.pick.min(ask.choices.len().saturating_sub(1));
        let Some(choice) = ask.choices.get(pick) else {
            return;
        };
        let want = choice.label.trim().to_lowercase();
        let Some(key) = self.chat.key.clone() else {
            return;
        };
        // The row is addressed by pane id and the server re-checks the pane
        // before it types, so the pane need not be on screen.
        let row = self.layout.agents.iter().find(|a| {
            !a.exited
                && a.pane_id.is_some()
                && a.harness_session_id.as_deref() == Some(key.session_id.as_str())
        });
        let Some((pane, menu)) = row.and_then(|a| a.pane_id.map(|p| (p, a.answerable.clone())))
        else {
            self.say("no pane hosts this session: answer in the pane".into(), cx);
            return;
        };
        let digit = u8::try_from(pick).ok().filter(|p| *p < 9).map(|p| b'1' + p);
        match (menu, digit) {
            (Some(menu), _) => {
                // The pane's menu must be this question's: the row whose
                // label holds the picked option's text. Any other prompt in
                // the pane (a permission ask, a stale question) fails closed
                // instead of taking a keystroke meant for another menu.
                let Some(option) = menu
                    .options
                    .iter()
                    .find(|o| o.label.to_lowercase().contains(&want))
                else {
                    self.say(
                        "the pane's menu is not this question: answer in the pane".into(),
                        cx,
                    );
                    return;
                };
                self.send(ClientMsg::PaneAnswer {
                    pane,
                    fingerprint: menu.fingerprint,
                    region_lines: u16::try_from(menu.region_lines).unwrap_or(u16::MAX),
                    keystroke: option.keystroke.clone(),
                });
            }
            (None, Some(digit)) if key.harness == "claude" => {
                let request_id = self.chat.next_request;
                self.chat.next_request += 1;
                // The server types only into a pane the client views, so
                // bring the question's pane into view first.
                self.send(ClientMsg::Command(Command::FocusPane(pane)));
                self.send(ClientMsg::PaneInput(proto::pane_input::PaneInputRequest {
                    request_id,
                    pane,
                    expected_identity: key.session_id.clone(),
                    bytes: vec![digit],
                }));
            }
            _ => self.say(
                "the pane shows no menu to answer: answer in the pane".into(),
                cx,
            ),
        }
    }

    /// One clipboard paste: text routes by view; a clipboard image with no
    /// text attaches to the chat draft, and the send re-puts it and types
    /// the harness's image-paste keystroke (submit_chat).
    fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        let item = cx.read_from_clipboard();
        let Some(text) = item.as_ref().and_then(|i| i.text()) else {
            if self.main == MainView::Chat {
                if let Some(image) = item.as_ref().and_then(|i| {
                    i.entries().iter().find_map(|e| match e {
                        ClipboardEntry::Image(img) => Some(img.clone()),
                        _ => None,
                    })
                }) {
                    self.chat.draft.attach(Attachment {
                        format: image.format,
                        bytes: image.bytes.into(),
                    });
                    self.chat_moved(cx);
                }
            }
            return;
        };
        // The open bar takes the paste into its query, ahead of the draft
        // and the pane.
        if self.find.is_some() {
            self.find_key(find::FindKey::Edit(text.to_owned()), cx);
            return;
        }
        match self.main {
            MainView::Mods => mods::paste(self, &text, cx),
            MainView::Chat => {
                // The start view's field: the paste lands in the launch draft.
                if crate::start::visible(self) {
                    self.launch.state.draft.replace_selection(&text);
                    self.launch.state.field.reveal.set(true);
                    cx.notify();
                    return;
                }
                self.chat.draft.replace_selection(&text);
                self.chat_moved(cx);
            }
            _ => {
                let bytes = keys::paste(&text, self.modes);
                self.send(ClientMsg::Input(bytes));
            }
        }
    }

    /// The keystroke each harness TUI reads the clipboard image with, where
    /// verified. All three read Ctrl+V: claude's `chat:imagePaste`, and
    /// codex (0.166.0) plus opencode measured live on 2026-11-18 in a tmux
    /// probe: the byte inserts the clipboard image into their composers.
    fn image_paste_key(harness: &str) -> Option<&'static [u8]> {
        match harness {
            "claude" | "codex" | "opencode" => Some(b"\x16"),
            _ => None,
        }
    }

    pub(super) fn has_focus(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    fn on_hq(&mut self, reply: Reply, cx: &mut Context<Self>) {
        match reply {
            Reply::Board(board) => {
                // Arrival: an id the seen set has not met. The first read
                // seeds the set silently - an app open never toasts a
                // backlog - unless the shot wants the toast in frame.
                let first_read = !self.questions_seeded;
                let ids: Vec<String> = board
                    .questions()
                    .iter()
                    .map(|q| q.item.id.clone())
                    .collect();
                let arrivals: Vec<String> = ids
                    .iter()
                    .filter(|i| !self.seen_questions.contains(*i))
                    .cloned()
                    .collect();
                self.seen_questions.extend(ids);
                // A good read seeds the set, even an empty one, so the first
                // question after an empty open still toasts.
                if board.read_yet() && board.questions_error().is_none() {
                    self.questions_seeded = true;
                }
                if board.questions_error().is_none() && (!first_read || self.toast_on_first_read) {
                    // Newest first, so the first hit is the newest arrival.
                    let newest = board
                        .questions_newest_first()
                        .into_iter()
                        .find_map(|q| arrivals.contains(&q.item.id).then(|| q.item.id.clone()));
                    if let Some(id) = newest {
                        self.say_question(id, cx);
                    }
                }
                if first_read && self.sheet_on_first_read {
                    if let Some(q) = board.questions_newest_first().first() {
                        self.question_toast = None;
                        self.answer = Some(answer::Sheet::new(q.item.id.clone()));
                    }
                }
                self.board = *board;
                // A question the read no longer lists needs no answer state;
                // a refusal stays so the row keeps its reason.
                let open: Vec<&str> = self
                    .board
                    .questions()
                    .iter()
                    .map(|q| q.item.id.as_str())
                    .collect();
                self.answers.retain(|id, _| open.contains(&id.as_str()));
            }
            Reply::Answered { id, receipt } => {
                match self.answers.get(&id).cloned() {
                    Some(AnswerState::Sending(option)) => {
                        self.answers.insert(id.clone(), AnswerState::Done(option));
                    }
                    Some(AnswerState::SendingWords) => {
                        self.answers.insert(id.clone(), AnswerState::DoneWords);
                    }
                    _ => {}
                }
                if self.answer.as_ref().is_some_and(|s| s.id == id) {
                    self.answer = None;
                }
                self.say(receipt, cx);
            }
            Reply::Refused { id, why } => {
                self.answers.insert(id, AnswerState::Refused(why));
            }
            Reply::Mailed { ok, text } => {
                self.mail = Some(match ok {
                    true => MailState::Sent(text.clone()),
                    false => MailState::Refused(text.clone()),
                });
                self.say(text, cx);
            }
        }
        cx.notify();
    }

    /// Resolve the palette again for the current appearance and project, and
    /// say what the theme ladder warned about.
    fn restyle(&mut self, cx: &mut Context<Self>) {
        let (palette, warning) = super::palette::for_appearance(self.light, &self.theme_cwd);
        self.set_palette(palette, cx);
        if let Some(text) = warning {
            self.say(text, cx);
        }
    }

    /// Repaint every pane and the chrome in `palette`.
    fn set_palette(&mut self, palette: Palette, cx: &mut Context<Self>) {
        self.palette = palette;
        for view in self.panes.values() {
            view.update(cx, |v, cx| {
                v.palette = palette;
                cx.notify();
            });
        }
        cx.notify();
    }

    fn pane_view(&mut self, id: u64, cx: &mut Context<Self>) -> Entity<PaneView> {
        let frame = self.frames.get(&id).cloned();
        let focused = id == self.layout.focus;
        let (metrics, palette) = (self.metrics.clone(), self.palette);
        self.panes
            .entry(id)
            .or_insert_with(|| {
                cx.new(|_| PaneView {
                    frame,
                    focused,
                    preedit: None,
                    find: None,
                    metrics,
                    palette,
                })
            })
            .clone()
    }

    /// A launch's new row steers twice, once per roster fact: the named
    /// row's click action opens a thread through its portal like a click,
    /// and the named row's session id, once it exists, moves the shown
    /// conversation onto the launched agent. The pin moves too, or
    /// sync_chat would hand the view straight back to the startup pin on
    /// the next layout.
    fn steer_launch(&mut self, cx: &mut Context<Self>) {
        if let Some(name) = self.launch.focus.clone() {
            let row = self.layout.agents.iter().find(|a| a.name == name);
            if let Some(a) = row {
                self.launch.focus = None;
                let action = match fno::app_api::client::agent_group::click_command(a) {
                    fno::app_api::client::agent_group::RowClick::Send(cmd) => {
                        Some(Action::Send(cmd))
                    }
                    fno::app_api::client::agent_group::RowClick::Notice(text) => {
                        Some(Action::Notice(text))
                    }
                };
                if let Some(action) = action {
                    self.act(action, cx);
                }
            }
        }
        if let Some(name) = self.launch.show.clone() {
            if let Some(key) = launch::stream_key(&self.layout.agents, &name) {
                self.launch.show = None;
                self.pinned = Some(key.clone());
                let project = self.chat_project(Some(&key));
                self.chat.switch(Some(key), &project);
                self.open_chat(cx);
            }
        }
    }

    /// True when a server notice is the launch's own portal receipt: the
    /// list already marks and names that row, so the toast would only sit
    /// over it.
    fn own_launch_receipt(&self, text: &str) -> bool {
        self.launch
            .show
            .as_ref()
            .or(self.launch.focus.as_ref())
            .is_some_and(|n| text.contains(n) && text.starts_with("thread pane "))
    }

    fn apply(&mut self, batch: Vec<Event>, cx: &mut Context<Self>) {
        // Only the newest frame per pane is drawn.
        let mut newest: HashMap<u64, Frame> = HashMap::new();
        let mut relayout = false;
        for event in batch {
            match event {
                Event::Frame(id, frame) => {
                    newest.insert(id, frame);
                }
                Event::Layout(l) => {
                    self.layout = l;
                    self.launch.state.refresh_projects(&self.layout);
                    self.steer_launch(cx);
                    self.on_layout_alerts(cx);
                    // A Finder launch has cwd `/`: the project's `mux.theme`
                    // pin is read from the active session's own directory.
                    if let Some(dir) = super::palette::project_dir(&self.layout) {
                        if dir != self.theme_cwd {
                            self.theme_cwd = dir;
                            self.restyle(cx);
                        }
                    }
                    if let Some(poller) = &self.poller {
                        poller.set_roster(&self.layout.agents);
                    }
                    self.find_gc(cx);
                    relayout = true;
                }
                Event::Modes(bytes) => self.modes.apply(&bytes),
                Event::Notice(text) => {
                    if !self.own_launch_receipt(&text) {
                        self.say(text, cx);
                    }
                }
                Event::Desktop { title, body } => self.handle_desktop(&title, &body, cx),
                // A plain click selects nothing; it never wipes the clipboard.
                Event::Copy(text) if text.is_empty() => {}
                Event::Copy(text) => {
                    let n = text.chars().count();
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    self.say(format!("copied {n} characters"), cx);
                }
                Event::Peek { name, lines } => {
                    self.peek = Some((name, lines));
                    cx.notify();
                }
                Event::OpenLink(url) => {
                    if fno::link::is_openable(&url) {
                        cx.open_url(&url);
                    }
                }
                Event::SearchResult {
                    pane,
                    total,
                    current,
                } => {
                    // The initiator-only reply lands in the bar while its
                    // pane is still the target; a stale reply drops.
                    if let Some(f) = &mut self.find {
                        if matches!(f.target, find::Target::Pane(p) if p == pane) {
                            f.total = total;
                            f.current = current;
                            cx.notify();
                        }
                    }
                }
                Event::Closed(reason) => {
                    self.closed = Some(reason);
                    self.link = None;
                    self.find = None;
                    if let Some(b) = self.alerts.closed() {
                        alerts::set_badge(b);
                    }
                    relayout = true;
                }
                Event::Launch(u) => self.apply_launch_update(u, cx),
            }
        }
        for (id, frame) in newest {
            let frame = Arc::new(frame);
            self.frames.insert(id, frame.clone());
            if let Some(view) = self.panes.get(&id) {
                view.update(cx, |p, cx| {
                    p.frame = Some(frame);
                    cx.notify();
                });
            }
        }
        if relayout {
            let live: Vec<u64> = self.layout.panes.iter().map(|(id, _)| *id).collect();
            self.panes.retain(|id, _| live.contains(id));
            self.frames.retain(|id, _| live.contains(id));
            self.ext.layout(&live);
            self.sync_panes(cx);
            self.sync_chat(cx);
            cx.notify();
        }
    }

    /// One desktop event: a known agent's tap routes through the alert
    /// gate, and the gate picks the toast or notification form.
    fn handle_desktop(&mut self, title: &str, body: &str, cx: &mut Context<Self>) {
        let known = self.layout.agents.iter().any(|a| a.name == title);
        if let Some(sig) = self
            .alerts
            .on_desktop(title, body, known, self.window_active)
        {
            self.consume_alert(sig, cx);
        }
    }

    /// One launch update: the panel applies it, and a `Launched` closes the
    /// panel, freshens its state for the next open, and marks the new row
    /// to show the task as its label.
    fn apply_launch_update(&mut self, u: fno::proto::AgentLaunchUpdate, cx: &mut Context<Self>) {
        if self.launch.state.apply(&u) && self.launch.open {
            if let fno::proto::LaunchState::Launched { name, .. } = &u.state {
                let name = name.clone();
                let task = self.launch.state.draft.text().trim().to_string();
                self.launch.open = false;
                self.launch.state = Launch::fresh();
                self.launch.state.refresh_projects(&self.layout);
                self.launch.focus = Some(name.clone());
                self.launch.show = Some(name.clone());
                self.launch.show_task = (!task.is_empty()).then_some(task);
                self.say(format!("started {name}"), cx);
            }
            cx.notify();
        }
    }

    /// Push the focus, the input handler's draft and the find tint to every
    /// pane view.
    fn sync_panes(&mut self, cx: &mut Context<Self>) {
        let focus = self.layout.focus;
        let needle = match self.find.as_ref() {
            Some(f) => match f.target {
                find::Target::Pane(p) if p == focus && self.main == MainView::Terminal => {
                    Some(f.query.clone())
                }
                _ => None,
            },
            None => None,
        };
        for (id, view) in &self.panes {
            let focused = *id == focus;
            let preedit = self.preedit.clone().filter(|_| focused);
            let find = needle.as_ref().filter(|_| focused).cloned();
            view.update(cx, |p, cx| {
                if p.focused != focused || p.preedit != preedit || p.find != find {
                    p.focused = focused;
                    p.preedit = preedit;
                    p.find = find;
                    cx.notify();
                }
            });
        }
    }

    /// Cmd-f: open the bar over the focused pane (terminal view) or the
    /// chat (chat view); another view takes nothing. Already open on this
    /// target keeps its query; a target switch closes the old bar first.
    fn find_open(&mut self, cx: &mut Context<Self>) {
        let target = match self.main {
            MainView::Terminal => find::Target::Pane(self.layout.focus),
            MainView::Chat => find::Target::Chat,
            _ => return,
        };
        let same = self.find.as_ref().is_some_and(|f| f.target == target);
        if same {
            return;
        }
        self.find_close(cx);
        let chat_target = matches!(target, find::Target::Chat);
        self.find = Some(find::Find::new(target));
        if chat_target {
            self.find_rescan(cx);
        } else {
            self.sync_panes(cx);
            cx.notify();
        }
    }

    /// The bar's keys: text and backspace edit the query; the terminal's
    /// Enter opens the server search, the arrows and the cmd-g chords step
    /// its matches, the chat steps its own scan. Esc always closes, clearing
    /// the pane's server highlight even when no reply ever arrived.
    fn find_key(&mut self, k: find::FindKey, cx: &mut Context<Self>) {
        let mut send = None;
        let mut rescan = false;
        let mut escape = false;
        let reveal = None;
        let mut step = None;
        {
            let Some(f) = &mut self.find else { return };
            match k {
                find::FindKey::Step { older } => step = Some(older),
                find::FindKey::Edit(text) => {
                    f.edit(&text);
                    if matches!(f.target, find::Target::Chat) {
                        rescan = true;
                    }
                }
                find::FindKey::Backspace => {
                    f.backspace();
                    if matches!(f.target, find::Target::Chat) {
                        rescan = true;
                    }
                }
                find::FindKey::Enter => {
                    if let find::Target::Pane(p) = f.target {
                        // (Re)open on a zero reply too: a no-match search_open
                        // already dropped the server's search state, so a step
                        // would only draw its "no active search" Notice.
                        if (!f.searched || f.total == 0) && !f.query.is_empty() {
                            send = Some(ClientMsg::SearchOpen {
                                pane: p,
                                query: f.query.clone(),
                            });
                            f.searched = true;
                        }
                    }
                }
                find::FindKey::Escape => escape = true,
            }
        }
        // The step runs outside the bar borrow above: find_step borrows
        // self again.
        if let Some(older) = step {
            self.find_step(older, cx);
            return;
        }
        if let Some(msg) = send {
            self.send(msg);
        }
        if escape {
            self.find_close(cx);
            return;
        }
        if rescan {
            self.find_rescan(cx);
        } else {
            self.sync_panes(cx);
        }
        if let Some(item) = reveal {
            self.chat.list.scroll_to_reveal_item(item);
        }
        cx.notify();
    }

    /// One step for the arrows and the cmd-g chords: the pane sends
    /// SearchStep only while the server holds a live match list (an arrow
    /// before a search, or on a no-match one, is a no-op: a no-match
    /// search_open dropped it), the chat walks its hit index.
    fn find_step(&mut self, older: bool, cx: &mut Context<Self>) {
        let mut reveal = None;
        let mut pane_step = None;
        {
            let Some(f) = &mut self.find else { return };
            match f.target {
                find::Target::Pane(p) => {
                    if f.searched && f.total > 0 {
                        pane_step = Some((p, find::step_dir(older)));
                    }
                }
                find::Target::Chat => {
                    if !f.hits.is_empty() {
                        let cur = f.current.max(1) as usize;
                        let next = if older {
                            (cur - 1).max(1)
                        } else {
                            (cur + 1).min(f.hits.len())
                        };
                        f.current = next as u32;
                        if let Some(h) = f.hits.get(next - 1) {
                            reveal = Some(h.item);
                        }
                    }
                }
            }
        }
        if let Some((p, dir)) = pane_step {
            self.send(ClientMsg::SearchStep { pane: p, dir });
        }
        self.sync_panes(cx);
        if let Some(item) = reveal {
            self.chat.list.scroll_to_reveal_item(item);
        }
        cx.notify();
    }

    /// Close the bar: the pane target clears its server highlight, and
    /// sync_panes drops the tint with it.
    fn find_close(&mut self, cx: &mut Context<Self>) {
        let old = self.find.take();
        if let Some(f) = old {
            if let find::Target::Pane(p) = f.target {
                self.send(ClientMsg::SearchClear { pane: p });
            }
        }
        self.sync_panes(cx);
        cx.notify();
    }

    /// Close the bar when its target stopped showing: a focus change or a
    /// vanished pane in the terminal view, a view switch away from the chat.
    fn find_gc(&mut self, cx: &mut Context<Self>) {
        let stale = match self.find.as_ref().map(|f| &f.target) {
            None => return,
            Some(find::Target::Pane(p)) => {
                self.main != MainView::Terminal
                    || *p != self.layout.focus
                    || !self.layout.panes.iter().any(|(id, _)| id == p)
            }
            Some(find::Target::Chat) => self.main != MainView::Chat,
        };
        if stale {
            self.find_close(cx);
        }
    }

    /// A command-block action on the focused pane. The server answers a pane
    /// with no blocks, or one still busy on a rerun, with its own notice.
    fn block_op(&mut self, op: keys::BlockOp) {
        let pane = self.layout.focus;
        self.send(match op {
            keys::BlockOp::Jump(dir) => ClientMsg::BlockJump { pane, dir },
            keys::BlockOp::Select(dir) => ClientMsg::BlockSelect { pane, dir },
            keys::BlockOp::Rerun => ClientMsg::BlockRerun { pane },
        });
    }

    /// The focused pane back to its live screen.
    pub(super) fn back_to_live(&mut self, cx: &mut Context<Self>) {
        let pane = self.layout.focus;
        self.send(ClientMsg::PaneScrollEdge { pane, top: false });
        cx.notify();
    }

    /// Whether the focused pane shows history, from its newest frame.
    fn focus_scrolled(&self) -> bool {
        self.frames
            .get(&self.layout.focus)
            .is_some_and(|f| f.scroll_offset > 0)
    }

    /// Keyboard history scroll with no mode held: true when the key scrolled.
    fn history_key(&mut self, key: &Key, cx: &mut Context<Self>) -> bool {
        let pane = self.layout.focus;
        let rows = self.frames.get(&pane).map_or(24, |f| i32::from(f.rows));
        let Some(msg) = mux_modes::history_step(key, pane, rows, self.focus_scrolled()) else {
            return false;
        };
        cx.stop_propagation();
        self.send(msg);
        true
    }

    /// `--shot-ui`: open one overlay the way its click opens it.
    pub(super) fn shot_ui(&mut self, ui: crate::UiShot, window: &Window, cx: &mut Context<Self>) {
        match ui {
            crate::UiShot::RowMenu | crate::UiShot::RowMenuNode => {
                // The row with a session id offers the full menu.
                let name = self
                    .layout
                    .agents
                    .iter()
                    .find(|a| a.harness_session_id.is_some())
                    .or_else(|| self.layout.agents.first())
                    .map(|a| a.name.clone());
                if let Some(name) = name {
                    // The node variant binds one first, so the menu shows the
                    // row a worker with a node gets.
                    if ui == crate::UiShot::RowMenuNode {
                        if let Some(a) = self.layout.agents.iter_mut().find(|a| a.name == name) {
                            a.node = Some("node-demo".into());
                        }
                    }
                    let at = gpui::point(gpui::px(150.0), gpui::px(150.0));
                    self.agent_menu_open(&name, at, window, cx);
                }
            }
            crate::UiShot::TabRename => {
                let tab = self
                    .layout
                    .squads
                    .iter()
                    .find(|s| s.id == self.layout.active_squad)
                    .and_then(|s| s.tabs.first())
                    .map(|t| t.id);
                if let Some(tab) = tab {
                    self.rename_tab(tab, cx);
                    // A named tab mid-edit: the field holds the name being typed.
                    if let Some(r) = self.rename.as_mut() {
                        r.field =
                            crate::ui::LineField::seeded("api-server", fno::proto::MAX_TAB_NAME);
                    }
                }
            }
            // The shell prints enough lines to fill its history; the second
            // phase scrolls the pane back through a real frame.
            crate::UiShot::Scrolled => self.pane_input(b"seq 1 120\r".to_vec(), cx),
        }
    }

    /// `--shot-ui`, after the first phase had time to land.
    pub(super) fn shot_ui_after(&mut self, ui: crate::UiShot) {
        if ui == crate::UiShot::Scrolled {
            let pane = self.layout.focus;
            self.send(ClientMsg::PaneScroll { pane, delta: 30 });
        }
    }

    /// prefix+PgUp: hold the keyboard for the focused pane's shared scroll.
    fn scroll_open(&mut self, cx: &mut Context<Self>) {
        self.scroll_mode = Some(self.layout.focus);
        cx.notify();
    }

    /// A double-click on a tab: its name becomes a field in the tab itself.
    pub(super) fn rename_tab(&mut self, tab: u64, cx: &mut Context<Self>) {
        match mux_modes::Rename::open_tab(&self.layout, tab) {
            Some(rename) => {
                self.rename = Some(rename);
                cx.notify();
            }
            None => self.say("that tab is gone".into(), cx),
        }
    }

    /// prefix+`,`: the rename prompt for the active tab.
    fn rename_open(&mut self, cx: &mut Context<Self>) {
        match mux_modes::Rename::open(&self.layout) {
            Some(rename) => {
                self.rename = Some(rename);
                cx.notify();
            }
            None => {
                alerts::beep();
                self.say("no active tab to rename".into(), cx);
            }
        }
    }

    /// prefix+P: the row menu of the agent in the focused pane.
    fn row_menu_open(&mut self, cx: &mut Context<Self>) {
        self.row_menu = Some(mux_modes::RowMenu::open(
            &self.layout.agents,
            self.layout.focus,
        ));
        cx.notify();
    }

    /// Run one row-menu item and close the menu.
    pub(super) fn row_menu_run(&mut self, at: usize, cx: &mut Context<Self>) {
        let item = self
            .row_menu
            .take()
            .and_then(|m| m.items.into_iter().nth(at))
            .map(|(_, act, _)| act);
        match item {
            Some(MenuAct::Run(actions)) => {
                for action in actions {
                    self.act(action, cx);
                }
            }
            Some(MenuAct::Rename(None)) => self.rename_open(cx),
            Some(MenuAct::RenameTab(tab)) => self.rename_tab(tab, cx),
            Some(MenuAct::Rename(Some(name))) => {
                self.rename = Some(mux_modes::Rename::for_agent(name));
            }
            Some(MenuAct::Peek(name)) => {
                self.send(ClientMsg::Command(Command::PeekAgent { name, seq: 0 }))
            }
            Some(MenuAct::Shell(argv, done)) => self.run_agent_tool(argv, done, cx),
            None => {}
        }
        cx.notify();
    }

    /// A right-click on an agent row: its menu at the pointer.
    pub(super) fn agent_menu_open(
        &mut self,
        name: &str,
        at: gpui::Point<gpui::Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        // A just-launched row shows its task as the label until the next
        // layout, so the name can miss: say so rather than do nothing.
        let Some(agent) = self.layout.agents.iter().find(|a| a.name == name).cloned() else {
            self.say("this row has no menu yet".into(), cx);
            return;
        };
        let mut menu = mux_modes::RowMenu::of(Some(&agent));
        // Keep the menu on screen: a long one opens upward from the bottom.
        let room = f32::from(window.viewport_size().height) - 420.0;
        menu.anchor = Some((f32::from(at.x), f32::from(at.y).min(room.max(0.0))));
        self.row_menu = Some(menu);
        cx.notify();
    }

    /// A right-click on a tab: rename, move and close.
    pub(super) fn tab_menu_open(
        &mut self,
        tab: u64,
        at: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let mut menu = mux_modes::RowMenu::of_tab(self.layout.active_squad, tab);
        menu.anchor = Some((f32::from(at.x), f32::from(at.y)));
        self.row_menu = Some(menu);
        cx.notify();
    }

    /// The composer's add-project row: register the picked folder in the
    /// config off the UI loop, then open a workspace for it unless one already
    /// roots there.
    pub(super) fn add_project_path(&mut self, path: String, cx: &mut Context<Self>) {
        let typed =
            launch::typed_project(&self.launch.state.draft).as_deref() == Some(path.as_str());
        let rooted = self.layout.squads.iter().any(|s| s.canonical_cwd == path);
        cx.spawn(async move |this, cx| {
            let added = cx
                .background_executor()
                .spawn({
                    let path = path.clone();
                    async move { fno::app_api::client::agent_launcher::add_project(&path) }
                })
                .await;
            this.update(cx, |root, cx| match added {
                Ok(project) => {
                    // A refused add leaves the typed path in place to fix.
                    if typed {
                        root.launch.state.draft.select_all();
                        root.launch.state.draft.replace_selection("");
                    }
                    if !rooted {
                        root.send(ClientMsg::Command(Command::NewSquad {
                            name: project.name.clone(),
                            origin: Some(project.path.clone()),
                            project: Some(project.id.clone()),
                        }));
                    }
                    root.launch.state.refresh_config();
                    root.launch.state.refresh_projects(&root.layout);
                    root.say(format!("added project {}", project.name), cx);
                }
                Err(why) => root.say(format!("refused: {why}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Run `fno-agent` off the UI loop and say how it ended.
    fn run_agent_tool(&mut self, argv: Vec<String>, done: String, cx: &mut Context<Self>) {
        let bin = fno::app_api::digest_overlay::fno_agent_bin();
        cx.spawn(async move |this, cx| {
            let out = cx
                .background_executor()
                .spawn(async move {
                    std::process::Command::new(bin)
                        .args(&argv)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::piped())
                        .spawn()
                        .and_then(|child| child.wait_with_output())
                })
                .await;
            let text = match out {
                Ok(o) if o.status.success() => done,
                Ok(o) => {
                    let why = String::from_utf8_lossy(&o.stderr);
                    format!(
                        "refused: {}",
                        why.lines().next().unwrap_or("no reason given")
                    )
                }
                Err(e) => format!("did not start: {e}"),
            };
            this.update(cx, |root, cx| root.say(text, cx)).ok();
        })
        .detach();
    }

    /// prefix+Z: focus the next pane whose agent waits on the user, wrapping
    /// after the last. Nothing waiting beeps and says so.
    pub(super) fn focus_next_waiting(&mut self, cx: &mut Context<Self>) {
        let now = fno::app_api::digest_overlay::now_secs();
        match mux_modes::next_waiting(&self.layout.agents, self.layout.focus, now) {
            Some((pane, name, at, of)) => {
                self.send(ClientMsg::Command(Command::FocusPane(pane)));
                self.say(format!("{name} ({at} of {of} waiting)"), cx);
            }
            None => {
                alerts::beep();
                self.say("no agent is waiting on you".into(), cx);
            }
        }
    }

    /// True when the row menu, the rename prompt or scroll mode consumed the
    /// key. Cmd keys fall through to their shortcuts.
    fn mux_modes_take(&mut self, key: &Key, cx: &mut Context<Self>) -> bool {
        if self.peek.is_some() && key.key == "escape" {
            self.peek = None;
            cx.stop_propagation();
            cx.notify();
            return true;
        }
        if let Some(menu) = self.row_menu.as_ref() {
            if key.cmd {
                return false;
            }
            let pick = menu.key(key);
            cx.stop_propagation();
            match pick {
                MenuKey::Move(by) => {
                    if let Some(m) = self.row_menu.as_mut() {
                        m.step(by);
                    }
                    cx.notify();
                }
                MenuKey::Run(at) => self.row_menu_run(at, cx),
                MenuKey::Close => {
                    self.row_menu = None;
                    cx.notify();
                }
                MenuKey::Swallow => {}
            }
            return true;
        }
        if let Some(r) = self.rename.as_mut() {
            if key.cmd {
                return false;
            }
            cx.stop_propagation();
            let text = match (key.key_char, key.key) {
                (None, "space") => Some(" "),
                (text, _) => text.filter(|_| !key.ctrl),
            };
            match r.field.edit(key.key, text) {
                ui::Edit::Typed => cx.notify(),
                ui::Edit::Close => {
                    self.rename = None;
                    cx.notify();
                }
                ui::Edit::Submit => {
                    let cmd = r.command();
                    self.rename = None;
                    self.send(ClientMsg::Command(cmd));
                    cx.notify();
                }
            }
            return true;
        }
        let Some(pane) = self.scroll_mode else {
            return false;
        };
        if self.main != MainView::Terminal || pane != self.layout.focus {
            self.scroll_mode = None;
            cx.notify();
            return false;
        }
        let rows = self.frames.get(&pane).map_or(24, |f| i32::from(f.rows));
        match mux_modes::scroll_step(key, pane, rows) {
            Step::Pass => return false,
            Step::Send(msg) => self.send(msg),
            Step::Leave => self.scroll_mode = None,
            Step::Swallow => {}
        }
        cx.stop_propagation();
        cx.notify();
        true
    }

    /// Scan the conversation for the query, keeping the current match by its
    /// (item, block, range start) so a streaming row appends without moving
    /// the highlight; a replaced row falls back to the newest match.
    fn find_rescan(&mut self, cx: &mut Context<Self>) {
        let Some(f) = &mut self.find else { return };
        if !matches!(f.target, find::Target::Chat) {
            return;
        }
        if f.query.is_empty() {
            f.hits.clear();
            f.total = 0;
            f.current = 0;
            f.searched = false;
            cx.notify();
            return;
        }
        let keep = f
            .current
            .checked_sub(1)
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| f.hits.get(i))
            .map(|h| (h.item, h.block, h.range.start));
        let hits =
            chat::find::chat_matches(&self.chat.conv.items, &self.chat.conv.expanded, &f.query);
        let current = keep
            .and_then(|k| {
                hits.iter()
                    .position(|h| (h.item, h.block, h.range.start) == k)
            })
            .map_or(hits.len(), |p| p + 1);
        f.hits = hits;
        f.total = f.hits.len() as u32;
        f.current = current as u32;
        f.searched = true;
        if current > 0 {
            if let Some(h) = f.hits.get(current - 1) {
                self.chat.list.scroll_to_reveal_item(h.item);
            }
        }
        cx.notify();
    }

    /// The caret moved or the text changed: repaint and scroll the caret in.
    fn chat_moved(&mut self, cx: &mut Context<Self>) {
        self.chat.field.reveal.set(true);
        // The draft lives here, not in the pane, until submit_chat pastes
        // it: while text sits unsent, the hint opens the pane's USER draft
        // arm (v132) so a live mail inject queues instead of landing
        // mid-draft. It carries no text. A draft cleared without Enter
        // releases the way the pane lane does: the arm rides out its hold
        // window.
        if !self.chat.draft.text().trim().is_empty() {
            let hint = self
                .chat
                .key
                .as_ref()
                .and_then(|key| chat::target_pane(self, key))
                .map(|(pane, _)| ClientMsg::DraftHint { pane });
            if let Some(msg) = hint {
                self.send(msg);
            }
        }
        cx.notify();
    }

    /// Run `f` on the composer's layout, re-shaped first when the draft has
    /// changed since the last paint.
    fn with_composer_layout<R>(
        &self,
        window: &Window,
        f: impl FnOnce(&chat::field::Layout) -> R,
    ) -> Option<R> {
        let slot = self.chat.field.layout.borrow();
        let layout = slot.as_ref()?;
        if layout.is_current(&self.chat.draft) {
            return Some(f(layout));
        }
        Some(f(&layout.reshaped(&self.chat.draft, window)))
    }

    fn chat_vertical(&mut self, up: bool, extend: bool, window: &Window) {
        let from = self.chat.draft.caret();
        let len = self.chat.draft.text().len();
        let goal = self.chat.field.goal_x.get();
        let moved = self.with_composer_layout(window, |l| {
            let goal = goal.unwrap_or_else(|| l.position_for_index(from).x);
            (l.vertical(from, up, goal, len), goal)
        });
        if let Some((to, goal)) = moved {
            self.chat.field.goal_x.set(Some(goal));
            self.chat.draft.move_to(to, extend);
        }
    }

    /// The start view's field keys: the chat draft's movement set on the
    /// launch draft, Enter through the launch panel's submit door. An empty
    /// agent draft starts nothing: the placeholder is the contract.
    fn start_key(&mut self, key: &Key, window: &Window, cx: &mut Context<Self>) {
        if self.launch.state.draft.marked().is_some() {
            return;
        }
        let (shift, cmd) = (key.shift, key.cmd);
        let vertical = matches!(key.key, "up" | "down");
        if !vertical {
            self.launch.state.field.goal_x.set(None);
        }
        match key.key {
            "enter" if !shift => {
                cx.stop_propagation();
                let empty = self.launch.state.draft.text().trim().is_empty();
                let worker = self.launch.state.kind == crate::launch::Kind::Worker;
                if !(worker && empty) {
                    self.submit_launch(cx);
                }
            }
            "enter" => self.launch.state.draft.replace_selection("\n"),
            "tab"
                if self.launch.state.complete_skill()
                    || self.launch.state.complete_typed_path() => {}
            "escape" if self.launch.state.skill_close() => {}
            "up" | "down" if self.launch.state.skill_step(key.key == "down") => {}
            "backspace" => self.launch.state.draft.backspace(),
            "delete" => self.launch.state.draft.delete(),
            "left" if cmd => self.launch.state.draft.home(shift),
            "left" if key.alt => self.launch.state.draft.word_left(shift),
            "left" => self.launch.state.draft.left(shift),
            "right" if cmd => self.launch.state.draft.end(shift),
            "right" if key.alt => self.launch.state.draft.word_right(shift),
            "right" => self.launch.state.draft.right(shift),
            "home" => self.launch.state.draft.home(shift),
            "end" => self.launch.state.draft.end(shift),
            "up" if cmd => self.launch.state.draft.doc_start(shift),
            "down" if cmd => self.launch.state.draft.doc_end(shift),
            "up" | "down" => self.start_vertical(key.key == "up", shift, window),
            _ => return,
        }
        self.launch.state.field.reveal.set(true);
        cx.stop_propagation();
        cx.notify();
    }

    /// Vertical caret movement on the start field, the chat composer's own
    /// goal-column walk against the launch field's layout.
    fn start_vertical(&mut self, up: bool, extend: bool, window: &Window) {
        let from = self.launch.state.draft.caret();
        let len = self.launch.state.draft.text().len();
        let goal = self.launch.state.field.goal_x.get();
        let moved = launch::with_layout(self, window, |l| {
            let goal = goal.unwrap_or_else(|| l.position_for_index(from).x);
            (l.vertical(from, up, goal, len), goal)
        });
        if let Some((to, new_goal)) = moved {
            self.launch.state.field.goal_x.set(Some(new_goal));
            self.launch.state.draft.move_to(to, extend);
        }
    }

    fn composer_point(&self, at: Point<Pixels>, window: &Window) -> Option<usize> {
        let len = self.chat.draft.text().len();
        self.with_composer_layout(window, |l| l.index_for_point(at - l.origin, len))
    }

    pub(super) fn composer_down(
        &mut self,
        ev: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        let Some(at) = self.composer_point(ev.position, window) else {
            return;
        };
        match ev.click_count {
            1 => self.chat.draft.move_to(at, ev.modifiers.shift),
            2 => self.chat.draft.select_word_at(at),
            _ => self.chat.draft.select_all(),
        }
        self.chat.field.dragging.set(true);
        self.chat.field.goal_x.set(None);
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn composer_drag(
        &mut self,
        ev: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.chat.field.dragging.get() || ev.pressed_button != Some(MouseButton::Left) {
            return;
        }
        if let Some(at) = self.composer_point(ev.position, window) {
            self.chat.draft.move_to(at, true);
            self.chat_moved(cx);
        }
    }

    pub(super) fn composer_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.chat.field.dragging.set(false);
    }

    /// The launch composer's task box: the same caret placement the chat
    /// composer runs, against the panel's draft.
    pub(super) fn launch_down(
        &mut self,
        ev: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        let Some(at) = launch::caret_at(self, ev.position, window) else {
            return;
        };
        match ev.click_count {
            1 => self.launch.state.draft.move_to(at, ev.modifiers.shift),
            2 => self.launch.state.draft.select_word_at(at),
            _ => self.launch.state.draft.select_all(),
        }
        self.launch.state.field.dragging.set(true);
        self.launch.state.field.goal_x.set(None);
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn launch_drag(
        &mut self,
        ev: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.launch.state.field.dragging.get() || ev.pressed_button != Some(MouseButton::Left) {
            return;
        }
        if let Some(at) = launch::caret_at(self, ev.position, window) {
            self.launch.state.draft.move_to(at, true);
            self.launch.state.field.reveal.set(true);
            cx.notify();
        }
    }

    pub(super) fn launch_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.launch.state.field.dragging.set(false);
    }

    /// A key in chat mode: an open sheet takes Escape, otherwise it edits the draft.
    fn chat_key(&mut self, key: &Key, window: &Window, cx: &mut Context<Self>) {
        // The start view's field takes the keys first: its own movement set,
        // and Enter through the launch panel's submit door.
        if crate::start::visible(self) {
            self.start_key(key, window, cx);
            return;
        }
        // An open sheet takes only Escape; an IME draft takes the keys.
        if self.chat.sheet.is_some() {
            if key.key == "escape" {
                self.chat.sheet = None;
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if self.chat.draft.marked().is_some() {
            return;
        }
        // An open question takes the movement keys and Enter in the
        // composer's place.
        if let Some((_, ask)) = self.chat.conv.open_question() {
            let last = ask.choices.len().saturating_sub(1);
            match key.key {
                "up" => self.chat.pick = self.chat.pick.saturating_sub(1),
                "down" => self.chat.pick = (self.chat.pick + 1).min(last),
                "enter" => self.answer_choice(cx),
                _ => return,
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let (shift, alt, cmd) = (key.shift, key.alt, key.cmd);
        let vertical = matches!(key.key, "up" | "down");
        if !vertical {
            self.chat.field.goal_x.set(None);
        }
        match key.key {
            "enter" if !shift => self.submit_chat(cx),
            "enter" => self.chat.draft.replace_selection("\n"),
            "tab" if self.chat.complete_skill() => {}
            "escape" if self.chat.skill_close() => {}
            "up" | "down" if self.chat.skill_step(key.key == "down") => {}
            "backspace" => self.chat.draft.backspace(),
            "delete" => self.chat.draft.delete(),
            "left" if cmd => self.chat.draft.home(shift),
            "left" if alt => self.chat.draft.word_left(shift),
            "left" => self.chat.draft.left(shift),
            "right" if cmd => self.chat.draft.end(shift),
            "right" if alt => self.chat.draft.word_right(shift),
            "right" => self.chat.draft.right(shift),
            "home" => self.chat.draft.home(shift),
            "end" => self.chat.draft.end(shift),
            "up" if cmd => self.chat.draft.doc_start(shift),
            "down" if cmd => self.chat.draft.doc_end(shift),
            "up" | "down" => self.chat_vertical(key.key == "up", shift, window),
            _ => return,
        }
        cx.stop_propagation();
        self.chat_moved(cx);
    }

    /// cmd-`n`: in the terminal view the session's `n`th tab, elsewhere the
    /// sidebar's `n`th agent row. A number past the last one does nothing.
    fn pick(&mut self, n: u8, cx: &mut Context<Self>) {
        let at = usize::from(n).saturating_sub(1);
        let action = if self.main == MainView::Terminal {
            super::tabs::strip(&self.layout)
                .into_iter()
                .nth(at)
                .map(|t| t.action)
        } else {
            super::left_pane::agent_actions(self).into_iter().nth(at)
        };
        if let Some(action) = action {
            self.act(action, cx);
        }
    }

    /// The open sheet takes every key first: esc or `q` closes it, and a
    /// cmd key closes it and runs. ⌘/ never gets here; its action toggles.
    fn chord(&mut self, key: &Key, cx: &mut Context<Self>) -> bool {
        if !self.sheet {
            return false;
        }
        if key.cmd || matches!(key.key, "escape" | "q") {
            self.sheet = false;
            cx.notify();
        }
        !key.cmd
    }

    fn toggle_sheet(&mut self, _: &KeyboardShortcuts, _: &mut Window, cx: &mut Context<Self>) {
        self.sheet = !self.sheet;
        if self.sheet {
            self.goto = None;
        }
        cx.notify();
    }

    /// ⌘k: toggle the go-to palette. Opening one closes the key sheet.
    fn toggle_goto(&mut self, _: &GoTo, _: &mut Window, cx: &mut Context<Self>) {
        if self.goto.take().is_none() {
            fno::ui_usage::click("app", "goto_open");
            self.sheet = false;
            self.goto = Some(goto::GoTo {
                query: ui::LineField::new(goto::MAX_QUERY),
                selected: 0,
            });
        }
        cx.notify();
    }

    pub(super) fn close_goto(&mut self, cx: &mut Context<Self>) {
        self.goto = None;
        cx.notify();
    }

    pub(super) fn close_sheet(&mut self, cx: &mut Context<Self>) {
        self.sheet = false;
        cx.notify();
    }

    /// A Settings font write moved `font_want`: re-measure the cell here,
    /// where the text system can measure, and hand every pane the new
    /// metrics - the grid resize follows on the next paint.
    fn apply_font_want(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.font_want
            == (
                self.metrics.font.family.to_string(),
                f32::from(self.metrics.font_size),
            )
        {
            return;
        }
        let (family, want) = self.font_want.clone();
        let font = font(family);
        let size_px = px(want);
        let text = window.text_system();
        let id = text.resolve_font(&font);
        let width = text
            .advance(id, size_px, 'm')
            .map(|s| s.width)
            .unwrap_or(size_px * 0.6);
        let scale = window.scale_factor();
        let snap = |v: Pixels| px((f32::from(v) * scale).round() / scale);
        let metrics = Metrics {
            font,
            font_size: size_px,
            cell: size(snap(width), snap(size_px * 1.25)),
        };
        self.metrics = metrics.clone();
        for view in self.panes.values() {
            view.update(cx, |v, _| v.metrics = metrics.clone());
        }
        cx.notify();
    }

    /// ⌘,: toggle the Settings panel. Opening it closes the key sheet and
    /// the palette.
    fn toggle_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.take().is_some() {
            cx.notify();
            return;
        }
        let fonts = settings::mono_fonts(window);
        self.open_settings_with(fonts, cx);
    }

    /// The open half: rows from the live config, fleet and the given
    /// faces. The shot path calls it with pre-measured faces.
    pub(super) fn open_settings_with(&mut self, fonts: Vec<String>, cx: &mut Context<Self>) {
        fno::ui_usage::click("app", "settings_open");
        self.sheet = false;
        self.goto = None;
        let config = crate::global_config_raw();
        let machines = self
            .board
            .fleet()
            .map(|f| f.machines.clone())
            .unwrap_or_default();
        let rows = settings::build_rows(config.as_ref(), &self.theme_cwd, &machines, &fonts);
        self.settings = Some(settings::SettingsState {
            query: ui::LineField::new(64),
            selected: 0,
            page: None,
            note: None,
            rows,
            fonts,
            scroll: gpui::ScrollHandle::new(),
            follow: std::cell::Cell::new(false),
        });
        cx.notify();
    }
    /// The panel's click-outside close (settings.rs's backdrop listener).
    pub(super) fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.settings = None;
        cx.notify();
    }

    /// One resolved settings write: spawn the bounded CLI door on a
    /// background task. The apply half runs on the result.
    fn spawn_settings_write(
        &mut self,
        set: bool,
        id: String,
        value: String,
        cx: &mut Context<Self>,
    ) {
        // Detached: a second Enter must not drop the write still in flight.
        cx.spawn(async move |this, cx| {
            let id2 = id.clone();
            let v2 = value.clone();
            let out = cx
                .background_executor()
                .spawn(async move {
                    settings::bounded_write(set, &id, if set { Some(&value) } else { None })
                })
                .await;
            this.update(cx, |root, cx| root.on_settings_write(out, id2, v2, cx))
                .ok();
        })
        .detach();
    }

    /// Rebuild the panel's rows from the live file and fleet, and set the
    /// refusal line under them (None clears it).
    fn rebuild_settings_rows(&mut self, note: Option<String>) {
        let machines = self
            .board
            .fleet()
            .map(|f| f.machines.clone())
            .unwrap_or_default();
        if let Some(st) = self.settings.as_mut() {
            let config = crate::global_config_raw();
            st.note = note;
            st.rows = settings::build_rows(config.as_ref(), &self.theme_cwd, &machines, &st.fonts);
        }
    }

    /// A settings write landed: repaint the rows from the fresh global
    /// file and run the apply half - the theme restyles, the font keys
    /// move `font_want`, and render re-measures the panes there.
    fn on_settings_write(
        &mut self,
        out: Result<(), settings::Refusal>,
        id: String,
        value: String,
        cx: &mut Context<Self>,
    ) {
        match out {
            Ok(()) => {
                self.rebuild_settings_rows(None);
                if id == "mux.theme" {
                    self.restyle(cx);
                }
                if id == "app.font" {
                    self.font_want.0 = value.clone();
                }
                if id == "app.font_size" {
                    if let Ok(f) = value.parse::<f32>() {
                        self.font_want.1 = f;
                    }
                }
            }
            Err(settings::Refusal(line)) => self.rebuild_settings_rows(Some(line)),
        }
        cx.notify();
    }

    /// Badges show while cmd alone is down, as in the Claude desktop app.
    fn on_modifiers(&mut self, ev: &ModifiersChangedEvent, _: &mut Window, cx: &mut Context<Self>) {
        let m = &ev.modifiers;
        let picking = m.platform && !m.alt && !m.shift && !m.control;
        if picking != self.picking {
            self.picking = picking;
            cx.notify();
        }
    }

    /// The open composer takes the key: true when it died in the panel.
    /// Text keys fall through unstopped, so the input handler still feeds
    /// the panel's draft.
    fn launch_key(&mut self, key: &Key, cx: &mut Context<Self>) -> bool {
        if !self.launch.open {
            return false;
        }
        match self.launch.state.key(key) {
            launch::KeyOut::Close => self.close_launch(cx),
            launch::KeyOut::Submit => self.submit_launch(cx),
            launch::KeyOut::Taken => {
                cx.notify();
                cx.stop_propagation();
            }
            launch::KeyOut::Ignored => return false,
        }
        true
    }

    /// The workspace-name field's keys. Enter commits and never inserts a
    /// newline (the launch panel's own shape): a new workspace sends
    /// NewSquad, a rename sends RenameSquad. Esc cancels; a blank or
    /// whitespace name is sent as-is, because the server's notice is the
    /// honest answer (nothing created, or the name reverts to its derived
    /// label).
    fn squad_field_key(&mut self, key: &Key, cx: &mut Context<Self>) -> bool {
        let Some(f) = self.squad_field.as_mut() else {
            return false;
        };
        if key.cmd || key.ctrl {
            return false;
        }
        match key.key {
            "enter" if !key.shift => {
                let name = f.draft.text().trim().to_string();
                let squad = f.squad;
                self.squad_field = None;
                match squad {
                    Some(id) => {
                        // A name-keyed workspace's fold entry moves to the
                        // new name, so a folded workspace stays folded; a
                        // cwd-keyed one keeps its key.
                        let moved = self
                            .layout
                            .squads
                            .iter()
                            .find(|s| s.id == id)
                            .and_then(|s| super::left_pane::rename_fold_move(s, &name));
                        if let Some((old, new)) = moved {
                            if self.folds.remove(&old) {
                                self.folds.insert(new);
                                record_ui_state(self, cx);
                            }
                        }
                        self.send(ClientMsg::Command(Command::RenameSquad { squad: id, name }))
                    }
                    None => self.send(ClientMsg::Command(Command::NewSquad {
                        name,
                        origin: None,
                        project: None,
                    })),
                }
            }
            "escape" => self.squad_field = None,
            "backspace" => f.draft.backspace(),
            "delete" => f.draft.delete(),
            "left" => f.draft.left(key.shift),
            "right" => f.draft.right(key.shift),
            "home" => f.draft.home(key.shift),
            "end" => f.draft.end(key.shift),
            _ => return false,
        }
        cx.stop_propagation();
        cx.notify();
        true
    }

    /// True when the open find bar consumed the key: an edit, a step, close.
    fn find_takes(&mut self, key: &Key, cx: &mut Context<Self>) -> bool {
        if self.find.is_none() {
            return false;
        }
        let Some(k) = find::key(key) else {
            return false;
        };
        cx.stop_propagation();
        self.find_key(k, cx);
        true
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let key = Key {
            key: &ks.key,
            key_char: ks.key_char.as_deref(),
            ctrl: ks.modifiers.control,
            alt: ks.modifiers.alt,
            shift: ks.modifiers.shift,
            cmd: ks.modifiers.platform,
        };
        if self.chord(&key, cx) {
            cx.stop_propagation();
            return;
        }
        // With no server to talk to, Enter answers the offer.
        if self.offer.is_some() && !key.cmd && !key.ctrl && key.key == "enter" {
            self.start_mux(cx);
            cx.stop_propagation();
            return;
        }
        // The palette takes every key while it shows: escape closes, enter
        // goes, up and down move, other cmd keys close it and run as today.
        if self.goto.is_some() && goto::key(self, &key, cx) {
            cx.stop_propagation();
            return;
        }
        if self.settings_key(&key, cx) {
            return;
        }
        // After chord: with the key sheet open, esc closes the sheet first.
        if key.key == "escape" && self.seam_escape(cx) {
            return;
        }
        // The composer is modal: its keys never reach the pane behind it,
        // and its text keys still compose through the input handler.
        if self.launch_key(&key, cx) {
            return;
        }
        // The workspace-name field is modal the same way: Enter names the
        // workspace, Esc cancels, the editing keys edit its draft. A cmd or
        // ctrl key falls through to the shortcuts.
        if self.squad_field_key(&key, cx) {
            return;
        }
        // The workspace menu closes on Esc; a key that is not Esc falls
        // through, so the shortcuts keep running under the menu.
        if self.workspace_menu.is_some() && key.key == "escape" {
            self.workspace_menu = None;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        // The answer sheet is modal the same way: esc closes, enter
        // submits, the editing keys and text land in its draft. A cmd or
        // ctrl key falls through, so the shortcuts keep running under it.
        if answer::key(self, &key, cx) {
            cx.stop_propagation();
            return;
        }
        if self.mux_modes_take(&key, cx) {
            return;
        }
        if let Some(action) = keys::shortcut(&key) {
            cx.stop_propagation();
            self.cmd_action(action, window, cx);
            return;
        }
        // The find bar takes the keys it edits before chat and pane routing;
        // cmd keys kept their shortcuts above.
        if self.find_takes(&key, cx) {
            return;
        }
        if self.main == MainView::Chat {
            self.chat_key(&key, window, cx);
            return;
        }
        if self.main == MainView::Orchestration {
            orchestration::key(self, &key, cx);
            return;
        }
        if self.main == MainView::Mods && mods::key(self, &key, cx) {
            cx.stop_propagation();
            return;
        }
        // The board takes no typing, and text keys reach the pane through the
        // input handler, which composes dead keys and IME drafts first.
        if self.main != MainView::Terminal || keys::is_text(&key) {
            return;
        }
        if self.history_key(&key, cx) {
            return;
        }
        if let Some(bytes) = keys::encode(&key, self.modes) {
            cx.stop_propagation();
            self.mux_key(bytes, window, cx);
        }
    }

    /// The Settings panel takes every key while it shows; a cmd key closes it
    /// and runs as today (the key sheet's grammar). True when it took the key.
    fn settings_key(&mut self, key: &Key, cx: &mut Context<Self>) -> bool {
        if self.settings.is_none() {
            return false;
        }
        if key.cmd {
            self.settings = None;
            cx.notify();
            return false;
        }
        let ev = self.settings.as_mut().and_then(|st| settings::key(st, key));
        cx.stop_propagation();
        match ev {
            Some(settings::Event::Close) => {
                self.settings = None;
                cx.notify();
            }
            Some(settings::Event::Set { id, value }) => {
                if let Some(st) = self.settings.as_mut() {
                    settings::apply_local(&mut st.rows, &id, &value);
                }
                self.spawn_settings_write(true, id, value, cx)
            }
            Some(settings::Event::Unset { id }) => {
                self.spawn_settings_write(false, id, String::new(), cx)
            }
            None => {}
        }
        true
    }

    /// One encoded keystroke through the mux scanner: the prefix claims its
    /// chords before any pane input, exactly like the terminal client's
    /// scan loop. A pending prefix announces itself once; a held chord
    /// candidate or tab number arms the quiet-window flush.
    fn mux_key(&mut self, bytes: Vec<u8>, window: &mut Window, cx: &mut Context<Self>) {
        let was_pending = self.scanner.prefix_pending();
        for event in self.scanner.scan(&bytes, Instant::now()) {
            self.mux_event(event, &bytes, window, cx);
        }
        if self.scanner.prefix_pending() && !was_pending {
            self.say(
                "prefix pending: the next key runs a mux action (cmd-/ shows the sheet)".into(),
                cx,
            );
        }
        if self.scanner.chord_pending() || self.scanner.digits_pending() {
            let ms = if self.scanner.chord_pending() {
                CHORD_FLUSH_MS
            } else {
                DIGIT_FLUSH_MS
            };
            self.arm_quiet_flush(ms, cx);
        }
    }

    /// Dispatch one resolved mux event; the same map the terminal client's
    /// dispatch runs, through [`keys::translate`].
    fn mux_event(
        &mut self,
        event: fno::ui::keys::Event,
        key: &[u8],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match keys::translate(event) {
            keys::MuxAction::Input(bytes) => self.pane_input(bytes, cx),
            keys::MuxAction::Command(c) => self.send(ClientMsg::Command(c)),
            keys::MuxAction::Tab(n) => self.mux_tab(n, cx),
            keys::MuxAction::Quit => {
                self.send(ClientMsg::Detach);
                record_ui_state(self, cx);
                persist_window_state(window, cx);
                cx.quit();
            }
            keys::MuxAction::Notice(text) => self.say(text, cx),
            keys::MuxAction::Block(op) => self.block_op(op),
            keys::MuxAction::ScrollMode => self.scroll_open(cx),
            keys::MuxAction::Rename => self.rename_open(cx),
            keys::MuxAction::RowMenu => self.row_menu_open(cx),
            keys::MuxAction::NextWaiting => self.focus_next_waiting(cx),
            keys::MuxAction::Overlay(overlay) => self.open_overlay(overlay, window, cx),
            keys::MuxAction::Bell => {
                alerts::beep();
                let name = String::from_utf8_lossy(key).escape_debug().to_string();
                self.say(format!("prefix {name}: no mux action is bound"), cx);
            }
        }
    }

    /// A fleet overlay chord: close the key sheet and palette, then open the
    /// app's surface. A second go-to chord leaves the palette open.
    fn open_overlay(
        &mut self,
        overlay: keys::Overlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.notice = None;
        match overlay {
            keys::Overlay::GoTo => {
                cx.notify();
                if self.goto.is_none() {
                    self.toggle_goto(&GoTo, window, cx);
                }
            }
            keys::Overlay::Inbox | keys::Overlay::Board => {
                self.goto = None;
                self.sheet = false;
                let view = match overlay {
                    keys::Overlay::Inbox => MainView::Inbox,
                    _ => MainView::Orchestration,
                };
                self.show(view, cx);
                cx.notify();
            }
        }
    }

    /// Bytes that reached no mux arm: pane input, and the notice a typing
    /// keystroke always dismissed before, exactly as the pre-prefix path did.
    fn pane_input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if self.notice.take().is_some() {
            cx.notify();
        }
        self.send(ClientMsg::Input(bytes));
    }

    /// A 1-based tab ordinal resolved against the active workspace's strip,
    /// the same rows the cmd-digit pick walks; a miss answers with a notice,
    /// never a silent dead key.
    fn mux_tab(&mut self, n: usize, cx: &mut Context<Self>) {
        let at = n.saturating_sub(1);
        match super::tabs::strip(&self.layout).into_iter().nth(at) {
            Some(tab) => self.act(tab.action, cx),
            None => self.say(format!("no tab {n} in this workspace"), cx),
        }
    }

    /// Arm the quiet window: a held chord candidate or tab number releases
    /// to the pane when no keystroke lands. Each pending keystroke re-arms;
    /// a flush that finds nothing held is a no-op, so stacked arms are
    /// harmless.
    fn arm_quiet_flush(&mut self, ms: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(ms))
                .await;
            // A window closed mid-timer has nothing left to flush.
            this.update(cx, |root, cx| root.flush_quiet(cx)).ok();
        })
        .detach();
    }

    /// Release whatever the quiet window ends. A flush releases only
    /// Forward chunks (a lone Esc) and held tab numbers, per the scanner's
    /// flush contract, so the other arms cannot fire here and no Window
    /// handle is needed.
    fn flush_quiet(&mut self, cx: &mut Context<Self>) {
        let events = [self.scanner.flush_chord(), self.scanner.flush_digits()];
        for event in events.into_iter().flatten() {
            match keys::translate(event) {
                keys::MuxAction::Input(bytes) => self.pane_input(bytes, cx),
                keys::MuxAction::Tab(n) => self.mux_tab(n, cx),
                _ => {}
            }
        }
    }

    /// Esc under a seam drag puts the split back where it was grabbed.
    /// True when a live drag consumed the escape.
    fn seam_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(drag) = self.seam_drag.take() {
            cx.stop_propagation();
            if drag.last_pos != drag.start_pos {
                self.send(ClientMsg::Command(Command::ResizeSeam {
                    a: drag.seam.a,
                    b: drag.seam.b,
                    pos: drag.start_pos,
                }));
            }
            return true;
        }
        false
    }

    /// One cmd shortcut lands.
    fn cmd_action(&mut self, action: Shortcut, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Shortcut::Command(Command::CopySelection)
                if crate::start::visible(self) && self.launch.state.draft.has_selection() =>
            {
                let text = self.launch.state.draft.selected_text().to_string();
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            Shortcut::Command(Command::CopySelection)
                if self.main == MainView::Chat && self.chat.draft.has_selection() =>
            {
                let text = self.chat.draft.selected_text().to_string();
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            Shortcut::Command(c) => self.send(ClientMsg::Command(c)),
            Shortcut::Cut => {
                if let Some(f) = self.squad_field.as_mut() {
                    if f.draft.has_selection() {
                        let text = f.draft.selected_text().to_string();
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        f.draft.replace_selection("");
                        cx.notify();
                    }
                } else if crate::start::visible(self) && self.launch.state.draft.has_selection() {
                    let text = self.launch.state.draft.selected_text().to_string();
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    self.launch.state.draft.replace_selection("");
                    self.launch.state.field.reveal.set(true);
                    cx.notify();
                } else if self.main == MainView::Chat && self.chat.draft.has_selection() {
                    let text = self.chat.draft.selected_text().to_string();
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    self.chat.draft.replace_selection("");
                    self.chat_moved(cx);
                }
            }
            Shortcut::SelectAll => {
                if let Some(f) = self.squad_field.as_mut() {
                    f.draft.select_all();
                    cx.notify();
                } else if crate::start::visible(self) {
                    self.launch.state.draft.select_all();
                    self.launch.state.field.reveal.set(true);
                    cx.notify();
                } else if self.main == MainView::Chat {
                    self.chat.draft.select_all();
                    self.chat_moved(cx);
                }
            }
            Shortcut::Paste => self.paste(cx),
            Shortcut::Quit => {
                self.send(ClientMsg::Detach);
                record_ui_state(self, cx);
                persist_window_state(window, cx);
                cx.quit();
            }
            // While the composer is open, cmd-1/2/3 switch its kind
            // (worker, lead, shell) instead of picking a tab or a row.
            Shortcut::Pick(n) => {
                if self.launch.open {
                    if let Some(kind) = launch::Kind::from_index(n) {
                        self.launch.state.set_kind(kind);
                        self.launch.state.row = launch::row_for_kind(kind);
                        cx.notify();
                        return;
                    }
                }
                self.pick(n, cx);
            }
            Shortcut::NewAgent => {
                self.squad_field = None;
                self.act(Action::OpenLaunch(None), cx);
            }
            Shortcut::Find => self.find_open(cx),
            Shortcut::FindStep { older } => self.find_step(older, cx),
        }
    }

    /// The paste shortcut: into the open answer sheet's draft, the open
    /// find bar, then the mods sheet, the chat draft, the board composer,
    /// or the focused pane.
    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) else {
            return;
        };
        // The open answer sheet takes the paste into its draft.
        if let Some(s) = self.answer.as_mut() {
            s.draft.replace_selection(&text);
            cx.notify();
            return;
        }
        if let Some(f) = self.squad_field.as_mut() {
            f.draft.replace_selection(&text);
            cx.notify();
            return;
        }
        // The open bar takes the paste into its query, ahead of the draft
        // and the pane.
        if self.find.is_some() {
            self.find_key(find::FindKey::Edit(text.to_owned()), cx);
            return;
        }
        match self.main {
            MainView::Mods => mods::paste(self, &text, cx),
            MainView::Chat => {
                // The start view's field: the paste lands in the launch draft.
                if crate::start::visible(self) {
                    self.launch.state.draft.replace_selection(&text);
                    self.launch.state.field.reveal.set(true);
                    cx.notify();
                    return;
                }
                self.chat.draft.replace_selection(&text);
                self.chat_moved(cx);
            }
            MainView::Orchestration => {
                self.board_draft.replace_selection(&text);
                cx.notify();
            }
            _ => {
                let bytes = keys::paste(&text, self.modes);
                self.send(ClientMsg::Input(bytes));
            }
        }
    }

    /// A window point relative to the terminal area.
    fn local(&self, at: Point<Pixels>) -> (f32, f32) {
        let o = self.area.get().origin;
        (f32::from(at.x - o.x), f32::from(at.y - o.y))
    }

    /// Each pane's grid: its rect minus the inset the server sized the PTY to.
    fn grids(&self) -> Vec<(u64, Rect)> {
        self.layout
            .panes
            .iter()
            .map(|&(id, r)| (id, mouse::inset(r)))
            .collect()
    }

    fn cell(&self) -> (f32, f32) {
        let c = self.metrics.cell;
        (f32::from(c.width), f32::from(c.height))
    }

    fn mouse(&mut self, hit: Hit, kind: MouseKind, alt: bool) {
        self.send(ClientMsg::Mouse {
            pane: hit.pane,
            event: MouseEvent {
                row: hit.row,
                col: hit.col,
                kind,
                alt,
            },
        });
    }

    fn on_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if self.rename.as_ref().is_some_and(|r| r.inline) {
            self.rename = None;
            cx.notify();
        }
        let (x, y) = self.local(ev.position);
        // A drag whose release never reached the window is closed first, so
        // the server never keeps a half-made selection.
        if let Some((pane, rect)) = self.drag.take() {
            let end = mouse::clamped(x, y, self.cell(), pane, rect);
            self.mouse(end, MouseKind::Release(proto::MouseButton::Left), false);
        }
        // A seam drag whose release never reached the window ends here too;
        // its last crossed cell already holds on the server.
        self.seam_drag = None;
        // A grab on a split's gap cell starts a seam drag, before any pane
        // hit test: the gap belongs to no pane's grid.
        if let Some(seam) = mouse::seam_at(x, y, self.cell(), &self.layout.panes) {
            fno::ui_usage::click("app", "seam");
            if let Some(start) = mouse::seam_pos(&seam, &self.layout.panes) {
                self.seam_drag = Some(mouse::SeamDrag {
                    seam,
                    start_pos: start,
                    last_pos: start,
                });
            }
            return;
        }
        let panes = self.grids();
        if let Some(hit) = mouse::hit(x, y, self.cell(), &panes) {
            fno::ui_usage::click("app", "pane");
            let rect = panes.iter().find(|(id, _)| *id == hit.pane).map(|p| p.1);
            self.drag = rect.map(|r| (hit.pane, r));
            // The server's mouse path selects and forwards; it never moves
            // focus. A click on another pane focuses it first, as the TUI does.
            if hit.pane != self.layout.focus {
                self.send(ClientMsg::Command(Command::FocusPane(hit.pane)));
            }
            self.mouse(
                hit,
                MouseKind::Press(proto::MouseButton::Left),
                ev.modifiers.alt,
            );
        }
    }

    fn on_move(&mut self, ev: &MouseMoveEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        // A seam drag streams the cursor's own coordinate along the seam's
        // axis; the cell crossing is what keeps the wire quiet.
        if let Some(drag) = self.seam_drag {
            if ev.pressed_button != Some(MouseButton::Left) {
                return;
            }
            let (x, y) = self.local(ev.position);
            if let Some(pos) = mouse::seam_pos_at(drag.seam.axis, x, y, self.cell()) {
                if pos != drag.last_pos {
                    self.seam_drag = Some(mouse::SeamDrag {
                        last_pos: pos,
                        ..drag
                    });
                    self.send(ClientMsg::Command(Command::ResizeSeam {
                        a: drag.seam.a,
                        b: drag.seam.b,
                        pos,
                    }));
                }
            }
            return;
        }
        let Some((pane, rect)) = self.drag else {
            return;
        };
        if ev.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let (x, y) = self.local(ev.position);
        let hit = mouse::clamped(x, y, self.cell(), pane, rect);
        self.mouse(
            hit,
            MouseKind::Drag(proto::MouseButton::Left),
            ev.modifiers.alt,
        );
    }

    fn on_up(&mut self, ev: &MouseUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        // A seam drag ends with no message: the server applied every cell
        // the drag crossed, and a dragged size holds for the session.
        if self.seam_drag.take().is_some() {
            return;
        }
        let Some((pane, rect)) = self.drag.take() else {
            return;
        };
        let (x, y) = self.local(ev.position);
        let hit = mouse::clamped(x, y, self.cell(), pane, rect);
        self.mouse(
            hit,
            MouseKind::Release(proto::MouseButton::Left),
            ev.modifiers.alt,
        );
    }

    fn on_wheel(&mut self, ev: &ScrollEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        let (x, y) = self.local(ev.position);
        let Some(hit) = mouse::hit(x, y, self.cell(), &self.grids()) else {
            return;
        };
        // A remainder left over from another pane never scrolls this one.
        if self.wheel_pane != Some(hit.pane) {
            self.wheel_pane = Some(hit.pane);
            self.wheel = 0.0;
        }
        let line = self.metrics.cell.height;
        let dy = f32::from(ev.delta.pixel_delta(line).y);
        let lines = mouse::wheel_lines(&mut self.wheel, dy, f32::from(line));
        let kind = if lines > 0 {
            MouseKind::WheelUp
        } else {
            MouseKind::WheelDown
        };
        for _ in 0..lines.abs().min(WHEEL_CAP) {
            self.mouse(hit, kind, ev.modifiers.alt);
        }
    }

    fn terminal(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let (cw, ch) = (self.metrics.cell.width, self.metrics.cell.height);
        let area = self.area.clone();
        let focus = self.focus.clone();
        let me = cx.entity();
        // flex_1, not size_full: the status line below must not push the
        // grid's last row out of the window.
        let mut out = div()
            .relative()
            .flex_1()
            .w_full()
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_down))
            .on_scroll(cx.listener(Self::on_wheel))
            .child(
                canvas(
                    move |bounds, _, _| area.set(bounds),
                    move |bounds, (), window, cx| {
                        let handler = TermInput(ElementInputHandler::new(bounds, me));
                        window.handle_input(&focus, handler, cx);
                    },
                )
                .absolute()
                .size_full(),
            );
        let rects = self.layout.panes.clone();
        let rects_len = rects.len();
        for (id, rect) in rects {
            let view = self.pane_view(id, cx);
            let p = self.palette;
            // The grid starts half a cell in, and the server leaves one cell
            // between rects: a split's divider is one hairline down the middle
            // of that gap, drawn on the pane right of it or below it. Focus is
            // no box: the panes that do not have it dim.
            let (hx, hy) = (cw / 2.0, ch / 2.0);
            let (x, y) = (cw * rect.x as f32 + hx, ch * rect.y as f32 + hy);
            let (w, h) = (cw * rect.cols as f32, ch * rect.rows as f32);
            if rect.x > 0 {
                out = out.child(
                    div()
                        .absolute()
                        .left(x - hx)
                        .top(y - hy)
                        .w(px(1.0))
                        .h(h + ch)
                        .bg(rgb(p.border)),
                );
            }
            if rect.y > 0 {
                out = out.child(
                    div()
                        .absolute()
                        .left(x - hx)
                        .top(y - hy)
                        .w(w + cw)
                        .h(px(1.0))
                        .bg(rgb(p.border)),
                );
            }
            // The server sized the PTY to the rect minus the inset; the grid
            // sits half the inset in from each side.
            let (padx, pady) = mouse::pad((f32::from(cw), f32::from(ch)));
            let (padx, pady) = (px(padx), px(pady));
            let grid = mouse::inset(rect);
            let (gw, gh) = (cw * grid.cols as f32, ch * grid.rows as f32);
            let place = |d: gpui::Div| d.absolute().left(x + padx).top(y + pady).w(gw).h(gh);
            out =
                out.child(place(div()).child(
                    AnyView::from(view).cached(gpui::StyleRefinement::default().size_full()),
                ));
            // A sibling painted after the pane, so the cached view cannot
            // paint over it.
            if id != self.layout.focus && rects_len > 1 {
                out = out.child(place(div()).bg(rgba(p.bg << 8 | 0x73)));
            }
        }
        out
    }

    /// The chat view over the input handler that feeds its composer.
    fn chat_view(&mut self, window: &Window, cx: &mut Context<Self>) -> gpui::Div {
        let focus = self.focus.clone();
        let me = cx.entity();
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .font_family(ui::PROSE)
            .text_size(px(13.0))
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, (), window, cx| {
                        let handler = TermInput(ElementInputHandler::new(bounds, me));
                        window.handle_input(&focus, handler, cx);
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(chat::render(self, window, cx))
    }

    /// The focused pane's cursor cell in window coordinates, where the IME
    /// candidate window opens.
    fn cursor_bounds(&self, area: Bounds<Pixels>) -> Option<Bounds<Pixels>> {
        let focus = self.layout.focus;
        let rect = self.layout.panes.iter().find(|(id, _)| *id == focus)?.1;
        let frame = self.frames.get(&focus)?;
        let c = self.metrics.cell;
        let col = rect.x as f32 + 0.5 + frame.cursor_col as f32;
        let row = rect.y as f32 + 0.5 + frame.cursor_row as f32;
        Some(Bounds::new(
            point(
                area.origin.x + c.width * col,
                area.origin.y + c.height * row,
            ),
            c,
        ))
    }

    fn set_preedit(&mut self, text: Option<String>, cx: &mut Context<Self>) {
        self.preedit = text.filter(|t| !t.is_empty());
        self.sync_panes(cx);
        if self.main == MainView::Chat {
            cx.notify();
        }
    }
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// The pane takes typed text through here, so dead keys, option-compose and
/// IME drafts commit before the bytes reach the PTY. The draft is never sent:
/// only committed text is.
impl EntityInputHandler for Root {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        if let Some(f) = self.squad_field.as_ref() {
            let d = &f.draft;
            let r = d.range_from_utf16(&range);
            *adjusted = Some(d.range_to_utf16(&r));
            return Some(d.text()[r].to_string());
        }
        if self.launch.open {
            let d = &self.launch.state.draft;
            let r = d.range_from_utf16(&range);
            *adjusted = Some(d.range_to_utf16(&r));
            return Some(d.text()[r].to_string());
        }
        if self.main == MainView::Chat {
            let d = &self.chat.draft;
            let r = d.range_from_utf16(&range);
            *adjusted = Some(d.range_to_utf16(&r));
            return Some(d.text()[r].to_string());
        }
        let text = self.preedit.clone().unwrap_or_default();
        *adjusted = Some(0..utf16_len(&text));
        Some(text)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if let Some(f) = self.squad_field.as_ref() {
            let d = &f.draft;
            let range = d.range_to_utf16(&d.selection());
            return Some(UTF16Selection {
                range,
                reversed: d.caret() < d.selection().end,
            });
        }
        if self.launch.open || crate::start::visible(self) {
            let d = &self.launch.state.draft;
            let range = d.range_to_utf16(&d.selection());
            return Some(UTF16Selection {
                range,
                reversed: d.caret() < d.selection().end,
            });
        }
        if self.main == MainView::Chat {
            let d = &self.chat.draft;
            let range = d.range_to_utf16(&d.selection());
            return Some(UTF16Selection {
                range,
                reversed: d.caret() < d.selection().end,
            });
        }
        let end = self.preedit.as_deref().map_or(0, utf16_len);
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        if let Some(f) = self.squad_field.as_ref() {
            let d = &f.draft;
            return d.marked().map(|m| d.range_to_utf16(&m));
        }
        if self.launch.open || crate::start::visible(self) {
            let d = &self.launch.state.draft;
            return d.marked().map(|m| d.range_to_utf16(&m));
        }
        if self.main == MainView::Chat {
            let d = &self.chat.draft;
            return d.marked().map(|m| d.range_to_utf16(&m));
        }
        self.preedit.as_deref().map(|t| 0..utf16_len(t))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(f) = self.squad_field.as_mut() {
            f.draft.unmark();
            cx.notify();
            return;
        }
        if self.launch.open || crate::start::visible(self) {
            self.launch.state.draft.unmark();
            cx.notify();
            return;
        }
        if self.main == MainView::Chat {
            self.chat.draft.unmark();
            cx.notify();
            return;
        }
        self.set_preedit(None, cx);
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The palette's query owns the keyboard while it shows.
        if self.goto.is_some() {
            return;
        }
        if let Some(f) = self.squad_field.as_mut() {
            f.draft.replace_utf16(range, text, false, None);
            cx.notify();
            return;
        }
        if self.launch.open || crate::start::visible(self) {
            self.launch
                .state
                .draft
                .replace_utf16(range, text, false, None);
            cx.notify();
            return;
        }
        if self.main == MainView::Chat {
            if self.chat.sheet.is_none() && self.chat.closed.is_none() {
                self.chat.draft.replace_utf16(range, text, false, None);
                self.chat_moved(cx);
            }
            return;
        }
        self.set_preedit(None, cx);
        if self.main != MainView::Terminal || text.is_empty() {
            return;
        }
        // A prefix chord mid-flight owns the next text key: `c`, `%`, `d` and
        // the tab digits arrive here, not through on_key, so they must reach
        // the scanner or the prefix claims nothing.
        if keys::chord_in_flight(&self.scanner) {
            self.mux_key(text.as_bytes().to_vec(), window, cx);
            return;
        }
        if self.notice.take().is_some() {
            cx.notify();
        }
        self.send(ClientMsg::Input(text.as_bytes().to_vec()));
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The palette's query owns the keyboard while it shows.
        if self.goto.is_some() {
            return;
        }
        if let Some(f) = self.squad_field.as_mut() {
            f.draft
                .replace_utf16(range, new_text, true, new_selected_range);
            cx.notify();
            return;
        }
        if self.launch.open {
            self.launch
                .state
                .draft
                .replace_utf16(range, new_text, true, new_selected_range);
            cx.notify();
            return;
        }
        if self.main == MainView::Chat {
            if self.chat.sheet.is_none() && self.chat.closed.is_none() {
                self.chat
                    .draft
                    .replace_utf16(range, new_text, true, new_selected_range);
                self.chat_moved(cx);
            }
            return;
        }
        self.set_preedit(Some(new_text.to_string()), cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if self.launch.open || crate::start::visible(self) {
            let from = self.launch.state.draft.offset_from_utf16(range_utf16.start);
            let found = launch::with_layout(self, window, |l| {
                let at = l.position_for_index(from);
                Bounds::new(l.origin + at, size(px(1.0), l.line_height()))
            });
            // Before the first paint, open at the task box's last line.
            return found.or_else(|| {
                let at = self.launch.state.composer.get();
                let line = self.metrics.cell.height;
                let origin = point(at.origin.x, at.origin.y + at.size.height - line);
                Some(Bounds::new(origin, size(px(1.0), line)))
            });
        }
        if self.main == MainView::Chat {
            let from = self.chat.draft.offset_from_utf16(range_utf16.start);
            let found = self.with_composer_layout(window, |l| {
                let at = l.position_for_index(from);
                Bounds::new(l.origin + at, size(px(1.0), l.line_height()))
            });
            // Before the first paint, open at the composer's last line.
            return found.or_else(|| {
                let at = self.chat.composer.get();
                let line = self.metrics.cell.height;
                let origin = point(at.origin.x, at.origin.y + at.size.height - line);
                Some(Bounds::new(origin, size(px(1.0), line)))
            });
        }
        self.cursor_bounds(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        at: Point<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        if self.launch.open || crate::start::visible(self) {
            let offset = launch::caret_at(self, at, window)?;
            return Some(self.launch.state.draft.offset_to_utf16(offset));
        }
        if self.main == MainView::Chat {
            let offset = self.composer_point(at, window)?;
            return Some(self.chat.draft.offset_to_utf16(offset));
        }
        None
    }
}

/// The element handler with macOS press-and-hold off: a held key repeats,
/// as in every terminal, and never opens the accent menu.
struct TermInput(ElementInputHandler<Root>);

impl InputHandler for TermInput {
    fn selected_text_range(
        &mut self,
        ignore: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        self.0.selected_text_range(ignore, window, cx)
    }

    fn marked_text_range(&mut self, window: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        self.0.marked_text_range(window, cx)
    }

    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        self.0.text_for_range(range, adjusted, window, cx)
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.0.replace_text_in_range(range, text, window, cx)
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.0
            .replace_and_mark_text_in_range(range, text, selected, window, cx)
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut App) {
        self.0.unmark_text(window, cx)
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        self.0.bounds_for_range(range, window, cx)
    }

    fn character_index_for_point(
        &mut self,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<usize> {
        self.0.character_index_for_point(at, window, cx)
    }

    fn apple_press_and_hold_enabled(&mut self) -> bool {
        false
    }
}

/// A board-style view in the mono face at body size.
fn mono_view(view: impl IntoElement + Styled) -> gpui::AnyElement {
    view.font_family(ui::MONO)
        .text_size(px(ui::BODY))
        .into_any_element()
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size_now = grid_size(window.viewport_size(), self.metrics.cell);
        if size_now != self.sent_size && self.link.is_some() {
            self.sent_size = size_now;
            self.send(ClientMsg::Resize {
                rows: size_now.0,
                cols: size_now.1,
            });
        }
        self.adv = ui::advance(window, ui::BODY);
        self.apply_font_want(window, cx);
        self.width = window.viewport_size().width;

        let main = match self.main {
            _ if self.offer.is_some() => self.offer_view(cx),
            MainView::Chat => self.chat_view(window, cx).into_any_element(),
            MainView::Terminal => self.terminal(cx).into_any_element(),
            MainView::Orchestration => mono_view(orchestration::render(self, cx)),
            MainView::Mods => mono_view(mods::render(self, cx)),
            MainView::Inbox => mono_view(crate::inbox::render(self, cx)),
        };
        // The bar floats over the target view's top right; find_gc keeps the
        // target and the view in step, so a live bar always sits over its
        // own view here.
        let main = match self.find.as_ref() {
            Some(f) if find::over_view(f, self.main) => div()
                .relative()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(main)
                .child(find::bar(f, self.palette, self.picking))
                .into_any_element(),
            _ => main,
        };
        let now = fno::app_api::digest_overlay::now_secs();
        let left = left_pane::render(self, now, self.adv, window, cx).text_size(px(ui::BODY));
        let strip = tabs::render(self, cx)
            .font_family(ui::MONO)
            .text_size(px(ui::BODY));
        // A closed link and a failed notice use the error color; a notice
        // is muted and expires. Neither takes a row: the toast floats over the corner.
        // In chat mode a closed stream's bar already carries the exit
        // reason, so the corner toast would repeat it - unless a sheet
        // covers that bar, where the toast is the only visible signal.
        let bar_shows_reason =
            self.main == MainView::Chat && self.chat.closed.is_some() && self.chat.sheet.is_none();
        let (status, status_color) = match (&self.closed, &self.notice) {
            (Some(c), _) if !bar_shows_reason => (Some(c.clone()), self.palette.error),
            (_, n) if bar_shows_reason && !self.notice_failed => (n.clone(), self.palette.muted),
            (_, n) if self.notice_failed => (n.clone(), self.palette.error),
            (_, n) => (n.clone(), self.palette.muted),
        };
        let toast = status.map(|text| {
            div()
                .absolute()
                .right(px(12.0))
                // Stacked above the chat status row (the empty-state line sits
                // in the bottom 40px); a corner pin cut that line off.
                .bottom(px(44.0))
                .max_w(px(520.0))
                .px_3()
                .py_1()
                .rounded(px(4.0))
                .bg(rgb(self.palette.raised))
                .text_color(rgb(status_color))
                .font_family(ui::MONO)
                .text_size(px(ui::BODY))
                .child(text)
        });
        let qtoast = self.question_toast_card(cx);
        let utoast = self.update_card_view(cx);
        let page = div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_modifiers_changed(cx.listener(Self::on_modifiers))
            .on_action(cx.listener(Self::toggle_sheet))
            .on_action(cx.listener(Self::toggle_goto))
            .on_action(cx.listener(Self::toggle_settings))
            // A click on a row, a tab or a view name lands on no focusable
            // element and would leave the window with no key focus.
            .on_any_mouse_down(cx.listener(|root, _, window, cx| window.focus(&root.focus, cx)))
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_up))
            .relative()
            .size_full()
            .flex()
            .flex_row()
            .bg(rgb(self.palette.bg))
            .text_color(rgb(self.palette.fg))
            .font_family(self.metrics.font.family.clone())
            .text_size(self.metrics.font_size)
            .child(left)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(strip)
                    .child(main)
                    .children(toast)
                    .children(qtoast)
                    .children(utoast),
            );
        self.overlays(page, window, cx)
    }
}

impl Root {
    /// The overlays over the window, in draw order: the launch composer,
    /// the key sheet, the go-to palette, the Settings panel.
    fn overlays(
        &mut self,
        page: gpui::Div,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        page.when(self.launch.open, |d| {
            d.child(launch::render(self, window, cx))
        })
        .when(self.sheet, |d| d.child(sheet::render(self, cx)))
        .when(self.answer.is_some(), |d| d.child(answer::render(self, cx)))
        .when(self.goto.is_some(), |d| d.child(goto::render(self, cx)))
        .children(self.row_menu.as_ref().map(|m| m.view(self, cx)))
        .children(
            self.peek
                .as_ref()
                .map(|(n, l)| mux_modes::peek_view(n, l, &self.palette, cx)),
        )
        .children(
            self.rename
                .as_ref()
                .filter(|r| !r.inline)
                .map(|r| r.bar(&self.palette)),
        )
        .children(
            self.scroll_mode
                .filter(|p| *p == self.layout.focus && self.main == MainView::Terminal)
                .map(|_| mux_modes::scroll_bar(&self.palette)),
        )
        .when(
            self.scroll_mode.is_none() && self.main == MainView::Terminal && self.focus_scrolled(),
            |d| d.child(mux_modes::back_to_live(&self.palette, cx)),
        )
        .when(self.settings.is_some(), |d| {
            d.child(settings::render(self, cx))
        })
        .when(self.workspace_confirm.is_some(), |d| {
            d.child(left_pane::confirm_sheet(self, cx))
        })
    }
}

/// The UI state the exit paths persist. The mutation sites below keep it
/// current, and both quit paths, the quit shortcut and the red close
/// button, write it beside the window's live frame to the state file.
struct UiState {
    view: MainView,
    folds: HashSet<String>,
    group_by: GroupBy,
    chat_key: Option<SessionKey>,
    path: Option<PathBuf>,
}

impl Global for UiState {}

/// Copy the live UI state into the global the exit paths read.
fn record_ui_state(root: &Root, cx: &mut App) {
    // The global is set when the window opens in the attach path, and
    // every mutation site runs after that. The shot window sets none,
    // so its chat mutations skip the snapshot: nothing persists a
    // shot's state.
    if cx.try_global::<UiState>().is_none() {
        return;
    }
    let ui = cx.global_mut::<UiState>();
    ui.view = root.main;
    ui.folds = root.folds.clone();
    ui.group_by = root.group_by;
    ui.chat_key = root.chat.key.clone();
}

/// Write the state file from the global plus the window's live frame.
/// Best-effort: a failed write never blocks quitting.
fn persist_window_state(window: &Window, cx: &mut App) {
    let Some(ui) = cx.try_global::<UiState>() else {
        return;
    };
    let Some(path) = ui.path.as_ref() else {
        return;
    };
    let (mode, bounds) = match window.window_bounds() {
        WindowBounds::Windowed(b) => (super::app_state::SavedMode::Windowed, b),
        WindowBounds::Maximized(b) => (super::app_state::SavedMode::Maximized, b),
        WindowBounds::Fullscreen(b) => (super::app_state::SavedMode::Fullscreen, b),
    };
    let state = super::app_state::AppState {
        version: super::app_state::STATE_VERSION,
        view: ui.view,
        bounds: super::app_state::SavedBounds {
            mode,
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            w: f32::from(bounds.size.width),
            h: f32::from(bounds.size.height),
        },
        pinned: ui.chat_key.clone(),
        folds: ui.folds.clone(),
        group_by: ui.group_by,
    };
    if let Err(e) = super::app_state::save(path, &state) {
        eprintln!("fno-app: window state save failed: {e}");
    }
}

/// The frame the window opens at: the saved frame when it still intersects
/// a display, else the centered default. A monitor unplugged since the last
/// quit cannot park the window off-screen.
fn initial_window_bounds(
    restored: Option<&super::app_state::AppState>,
    cx: &mut App,
) -> WindowBounds {
    let displays: Vec<(f32, f32, f32, f32)> = cx
        .displays()
        .iter()
        .map(|d| {
            let b = d.bounds();
            (
                f32::from(b.origin.x),
                f32::from(b.origin.y),
                f32::from(b.size.width),
                f32::from(b.size.height),
            )
        })
        .collect();
    let restored_frame =
        restored.and_then(|s| super::app_state::clamp_to_displays(&s.bounds, &displays));
    let bounds = match restored_frame {
        Some(b) => Bounds {
            origin: point(px(b.x), px(b.y)),
            size: size(px(b.w), px(b.h)),
        },
        None => Bounds::centered(None, size(px(1360.0), px(840.0)), cx),
    };
    match restored_frame.map(|b| b.mode) {
        Some(super::app_state::SavedMode::Maximized) => WindowBounds::Maximized(bounds),
        Some(super::app_state::SavedMode::Fullscreen) => WindowBounds::Fullscreen(bounds),
        _ => WindowBounds::Windowed(bounds),
    }
}

/// Open the window and run the app until it quits.
pub fn run(
    session: String,
    main: MainView,
    pinned: Option<SessionKey>,
    family: String,
    font_size: f32,
    restored: Option<super::app_state::AppState>,
) {
    gpui_platform::application().run(move |cx: &mut App| {
        cx.bind_keys([KeyBinding::new(keys::SHEET_KEY, KeyboardShortcuts, None)]);
        cx.bind_keys([KeyBinding::new(keys::GOTO_KEY, GoTo, None)]);
        cx.bind_keys([KeyBinding::new(keys::SETTINGS_KEY, OpenSettings, None)]);
        // The first menu is the app menu; Help lists the keys.
        cx.set_menus(vec![
            Menu {
                name: "fno".into(),
                items: vec![MenuItem::action("Settings…", OpenSettings)],
                disabled: false,
            },
            Menu {
                name: "Help".into(),
                items: vec![
                    MenuItem::action("Go to…", GoTo),
                    MenuItem::action("Keyboard Shortcuts", KeyboardShortcuts),
                ],
                disabled: false,
            },
        ]);
        let window_bounds = initial_window_bounds(restored.as_ref(), cx);
        // The fold a quit left behind seeds the left pane; the exit paths
        // read the rest from the UiState global.
        let folds = restored
            .as_ref()
            .map(|s| s.folds.clone())
            .unwrap_or_default();
        let group_by = restored.as_ref().map(|s| s.group_by).unwrap_or_default();
        // Where the exit paths write; a HOME-less environment saves nothing.
        let state_path = super::app_state::state_path();
        let first_run = restored.is_none();
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(window_bounds),
                // The content runs under the title bar; the traffic lights
                // sit over the left pane, which pads its top for them.
                titlebar: Some(TitlebarOptions {
                    title: Some("fno".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(12.0), px(12.0))),
                }),
                ..Default::default()
            },
            move |window, cx| {
                let font = font(family);
                let font_size = px(font_size);
                let text = window.text_system();
                let id = text.resolve_font(&font);
                let width = text
                    .advance(id, font_size, 'm')
                    .map(|s| s.width)
                    .unwrap_or(font_size * 0.6);
                // Snap the cell to whole device pixels, so cell edges never
                // land between pixels and drift across a wide row.
                let scale = window.scale_factor();
                let snap = |v: Pixels| px((f32::from(v) * scale).round() / scale);
                let metrics = Metrics {
                    font,
                    font_size,
                    cell: size(snap(width), snap(font_size * 1.25)),
                };
                cx.new(|cx| {
                    let mut root = Root::new(
                        session,
                        main,
                        pinned.clone(),
                        folds.clone(),
                        group_by,
                        metrics,
                        window,
                        cx,
                    );
                    // The exit paths read this snapshot; the mutation sites
                    // keep it current while the window lives.
                    cx.set_global(UiState {
                        view: main,
                        folds,
                        group_by,
                        chat_key: pinned,
                        path: state_path,
                    });
                    window.on_window_should_close(cx, |window, cx| {
                        persist_window_state(window, cx);
                        true
                    });
                    root.first_run = first_run;
                    window.focus(&root.focus, cx);
                    root.start_update(cx);
                    root
                })
            },
        );
        if let Err(e) = opened {
            eprintln!("fno-app: cannot open a window: {e}");
            cx.quit();
            return;
        }
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}

/// The shot path's root builder: the same metrics run() derives, with
/// the caller's warmed link adopted so the one frame the window paints
/// synchronously at open carries the world's data. The link was already
/// connected before gpui started, so this runs fast and the frame is
/// the shot.
pub(super) fn root_for_shot(
    session: &str,
    mode: MainView,
    pinned: Option<SessionKey>,
    warmed: Option<super::link::WarmedLink>,
    family: String,
    font_size: f32,
    find: Option<crate::FindShot>,
    settings: Option<crate::SettingsShot>,
    update: Option<crate::UpdateShot>,
    empty_chat: bool,
    cmd: bool,
    sheet: bool,
    draft: Option<String>,
    harness: Option<String>,
    first_run: Option<crate::FirstRunShot>,
    window: &mut Window,
    cx: &mut App,
) -> gpui::Entity<Root> {
    let font = gpui::font(family);
    let font_size = px(font_size);
    let text = window.text_system();
    let id = text.resolve_font(&font);
    let width = text
        .advance(id, font_size, 'm')
        .map(|s| s.width)
        .unwrap_or(font_size * 0.6);
    let scale = window.scale_factor();
    let snap = |v: Pixels| px((f32::from(v) * scale).round() / scale);
    let metrics = Metrics {
        font,
        font_size,
        cell: gpui::size(snap(width), snap(font_size * 1.25)),
    };
    let root = cx.new(|cx| {
        Root::new_with_link(
            session.to_string(),
            mode,
            pinned,
            HashSet::new(),
            GroupBy::Workspaces,
            metrics,
            warmed,
            window,
            cx,
        )
    });
    root.update(cx, |root, _| {
        root.shot = true;
        root.toast_on_first_read = true;
        root.sheet_on_first_read = sheet;
        root.update_card = update.map(|u| match u {
            crate::UpdateShot::Ready => UpdateCard::Ready(super::update::Ready {
                version: "0.0.2".to_string(),
                notes: String::new(),
                staged: std::path::PathBuf::new(),
            }),
            crate::UpdateShot::Notes => UpdateCard::Installed {
                version: "0.0.2".to_string(),
                notes: "The app now checks for new releases in the background and downloads a signed build. When one is ready, a card offers Restart.".to_string(),
            },
        });
        root.first_run = first_run == Some(crate::FirstRunShot::KeyCard);
        if empty_chat {
            root.shot_empty_chat = true;
            root.chat.switch(None, "");
        }
        if cmd {
            root.picking = true;
        }
        if let Some(h) = harness {
            root.launch.state.set_harness_for_shot(h);
        }
        if let Some(text) = draft {
            if root.shot_empty_chat {
                root.launch.state.refresh_skills();
                root.launch.state.draft.replace_selection(&text);
            } else {
                root.chat.draft.replace_selection(&text);
            }
        }
    });
    if let Some(find) = find {
        root.update(cx, |root, cx| {
            // The real paths: the bar opens and the query types exactly
            // as the user's keystrokes would land.
            root.find_open(cx);
            root.find_key(find::FindKey::Edit("the".to_owned()), cx);
            if find == crate::FindShot::Chords {
                root.picking = true;
            }
        });
    }
    if let Some(shot) = settings {
        let fonts = settings::mono_fonts(window);
        // The demo world pins its theme in a project file; the panel's
        // rows read the global file alone, so the Theme row stays editable.
        root.update(cx, |root, cx| {
            root.theme_cwd = PathBuf::from("/");
            root.open_settings_with(fonts, cx)
        });
        root.update(cx, |root, _| {
            if let Some(st) = root.settings.as_mut() {
                settings::apply_shot(st, shot);
            }
        });
    }
    // The shot window never activates, and the board poll pauses while
    // the window is inactive; the shot needs every feed settled before
    // the capture, so it polls regardless.
    if let Some(poller) = &root.read(cx).poller {
        poller.set_active(true);
    }
    root
}
