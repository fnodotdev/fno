//! The app window: one root view that lays out the left pane, the tab strip
//! and the server's pane rects, routes keys and mouse to the focused pane,
//! and hosts the chat view and the orchestration board.

use std::cell::Cell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    actions, canvas, div, font, point, prelude::*, px, rgb, rgba, size, AnyView, App, Application,
    Bounds, ClipboardItem, Context, ElementInputHandler, Entity, EntityInputHandler, FocusHandle,
    Global, InputHandler, KeyBinding, KeyDownEvent, Menu, MenuItem, ModifiersChangedEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollWheelEvent,
    Size, Subscription, Task, TitlebarOptions, UTF16Selection, Window, WindowBounds, WindowOptions,
};

use serde::{Deserialize, Serialize};

use super::alerts;
use super::alerts::Signal;
use super::chat::{self, Chat};
use super::goto;
use super::grid::{Metrics, PaneView};
use super::hq::{Poller, Reply};
use super::keys::{self, Key, Modes, Shortcut};
use super::launch::{self, Launch};
use super::link::{Event, Layout, Link};
use super::mouse::{self, Hit};
use super::palette::Palette;
use super::{find, left_pane, mods, orchestration, sheet, tabs, ui};
use fno::app_api::web_hq::Board;
use fno::proto::chat_event::SessionKey;
use fno::proto::{self, ClientMsg, Command, Frame, MouseEvent, MouseKind};
use fno::tree::Rect;

actions!(fno_app, [KeyboardShortcuts, GoTo]);

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
}

/// What a click in the left pane, the strip or the board does.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Send(Command),
    Notice(String),
    Answer {
        id: String,
        option: u32,
    },
    Show(MainView),
    /// Show every agent in the left pane, or only the first few.
    ToggleAgents,
    /// Open or close the key sheet, as ⌘/ and the palette's command row do.
    ToggleKeySheet,
    /// Open or close one chat thinking row.
    ToggleChat(usize),
    /// Open the sheet of one chat tool call.
    OpenSheet(usize),
    CloseSheet,
    Copy(String),
    SendChat,
    /// Reopen a chat stream that closed.
    Reconnect,
    /// Open the launch composer, with a kind pre-picked when the opener
    /// named one; `None` keeps the retained draft's kind.
    OpenLaunch(Option<launch::Kind>),
}

/// The stable control id a click's [`Action`] ships to the usage events arm
/// (`fno::ui_usage`). One constant per control, never a label.
fn click_id(action: &Action) -> &'static str {
    match action {
        Action::Send(_) => "send_command",
        Action::Notice(_) => "notice",
        Action::Answer { .. } => "answer",
        Action::Show(_) => "show_view",
        Action::ToggleAgents => "toggle_agents",
        Action::ToggleKeySheet => "toggle_key_sheet",
        Action::ToggleChat(_) => "toggle_chat",
        Action::OpenSheet(_) => "open_sheet",
        Action::CloseSheet => "close_sheet",
        Action::Copy(_) => "copy",
        Action::SendChat => "send_chat",
        Action::Reconnect => "reconnect",
        Action::OpenLaunch(_) => "open_launch",
    }
}

/// Where an answer the user clicked stands. The row shows it in place of its
/// options until the next read drops the question.
#[derive(Debug, Clone, PartialEq)]
pub enum AnswerState {
    Sending(u32),
    Done(u32),
    Refused(String),
}

/// How long a notice stays in the corner.
const NOTICE_FOR: Duration = Duration::from_secs(4);

/// The most wheel lines one event sends, so a fling cannot flood the socket.
const WHEEL_CAP: i32 = 10;

pub struct Root {
    pub(super) main: MainView,
    link: Option<Link>,
    pub(super) layout: Layout,
    frames: HashMap<u64, Arc<Frame>>,
    panes: HashMap<u64, Entity<PaneView>>,
    modes: Modes,
    notice: Option<String>,
    /// The notice reports a failure: it paints in the error color.
    notice_failed: bool,
    /// Bumped by each notice, so only the newest one's timer clears it.
    notice_gen: u64,
    pub(super) closed: Option<String>,
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
    poller: Option<Poller>,
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
    pub(super) agents_open: bool,
    pub(super) chat: Chat,
    /// `--chat <id>`: the session chat mode shows, whatever has focus.
    pub(super) pinned: Option<SessionKey>,
    /// Cmd alone is held: rows and tabs show their cmd-digit badges.
    pub(super) picking: bool,
    /// The key sheet (cmd-/) covers the window.
    pub(super) sheet: bool,
    /// The go-to palette (cmd-k) covers the window.
    pub(super) goto: Option<goto::GoTo>,
    /// The find bar (cmd-f) over the focused pane or the chat.
    pub(super) find: Option<find::Find>,
    /// The launch composer: retained state, open flag, id mint, and the
    /// row the last launch named. Esc closes and keeps the draft.
    pub(super) launch: launch::Panel,
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
        agents_open: bool,
        metrics: Metrics,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let sent_size = grid_size(window.viewport_size(), metrics.cell);
        let light = super::palette::appearance_is_light(window.appearance());
        let theme_cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
        let (palette, theme_warning) = super::palette::for_appearance(light, &theme_cwd);
        let mut root = Root {
            main,
            link: None,
            layout: Layout::default(),
            frames: HashMap::new(),
            panes: HashMap::new(),
            modes: Modes::default(),
            notice: theme_warning,
            notice_failed: false,
            notice_gen: 0,
            closed: None,
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
            agents_open,
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
            goto: None,
            find: None,
            launch: launch::Panel::default(),
            preedit: None,
            _reader: None,
            _chat: None,
            _chat_wait: None,
            _hq: None,
            _plugins: None,
            _activation: None,
            _appearance: None,
            _notice: None,
            _alerts: None,
            _alerts_tick: None,
            alerts: alerts::Alerts::new(),
            // A terminal launch leaves the new window behind the frontmost
            // app; the activation observer only reports changes, so the
            // first read happens here.
            window_active: window.is_window_active(),
        };
        wire_window(&mut root, window, cx);
        root.connect_link(&session, sent_size, cx);
        root.wire_alerts(cx);
        root.start_board_poll(cx);
        root.start_plugins(cx);
        // `--mode mods` opens on the view without a switch, so read here.
        if root.main == MainView::Mods {
            root.ext.read();
        }
        root
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

    /// Connect the link and spawn its batch reader; a refused connect
    /// lands the reason in `closed`.
    fn connect_link(&mut self, session: &str, sent_size: (u16, u16), cx: &mut Context<Self>) {
        match Link::connect(session, sent_size.0, sent_size.1) {
            Ok((link, mut rx)) => {
                self.link = Some(link);
                self.send(ClientMsg::PaneInset {
                    rows: mouse::INSET.0,
                    cols: mouse::INSET.1,
                });
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
            Err(e) => self.closed = Some(e.to_string()),
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

    pub(super) fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        fno::ui_usage::click("app", click_id(&action));
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
                // A row already sent or answered takes no second click.
                Some(_) if self.answers.contains_key(&id) => {}
                Some(poller) => {
                    poller.answer(id.clone(), option);
                    self.answers.insert(id, AnswerState::Sending(option));
                }
                None => self.say("no board poll: cannot answer".into(), cx),
            },
            Action::Show(view) => self.show(view, cx),
            Action::ToggleAgents => {
                self.agents_open = !self.agents_open;
                record_ui_state(self, cx);
            }
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
            Action::Reconnect => self.open_chat(cx),
            Action::OpenLaunch(kind) => self.open_launch(kind, cx),
        }
        cx.notify();
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

    /// In chat mode, stream the focused pane's agent session. A new focus
    /// parks the shown conversation and resumes the new one where it left.
    fn sync_chat(&mut self, cx: &mut Context<Self>) {
        let key = chat::focused_key(self);
        if self.main != MainView::Chat || key == self.chat.key {
            return;
        }
        self.chat.switch(key);
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
        if self.chat.draft.text().trim().is_empty() || self.chat.closed.is_some() {
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
        let mut text = self.chat.draft.take();
        // Unbracketed, each newline would type Enter and submit early.
        if !self.modes.bracketed_paste {
            text = text.replace('\n', " ");
        }
        let id = self.chat.next_request;
        self.chat.next_request += 2;
        let input = |request_id, bytes| {
            ClientMsg::PaneInput(proto::pane_input::PaneInputRequest {
                request_id,
                pane,
                expected_identity: identity.clone(),
                bytes,
            })
        };
        self.send(input(id, keys::paste(&text, self.modes)));
        let enter = input(id + 1, b"\r".to_vec());
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(chat::SUBMIT_SETTLE).await;
            let _ = link.send(&enter);
        })
        .detach();
        self.chat.pending.push(text);
    }

    pub(super) fn has_focus(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    fn on_hq(&mut self, reply: Reply, cx: &mut Context<Self>) {
        match reply {
            Reply::Board(board) => {
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
                if let Some(AnswerState::Sending(option)) = self.answers.get(&id).cloned() {
                    self.answers.insert(id, AnswerState::Done(option));
                }
                self.say(receipt, cx);
            }
            Reply::Refused { id, why } => {
                self.answers.insert(id, AnswerState::Refused(why));
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
                self.chat.switch(Some(key));
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
                Event::Desktop { title, body } => {
                    let known = self.layout.agents.iter().any(|a| a.name == title);
                    if let Some(sig) =
                        self.alerts
                            .on_desktop(&title, &body, known, self.window_active)
                    {
                        self.consume_alert(sig, cx);
                    }
                }
                // A plain click selects nothing; it never wipes the clipboard.
                Event::Copy(text) if text.is_empty() => {}
                Event::Copy(text) => {
                    let n = text.chars().count();
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    self.say(format!("copied {n} characters"), cx);
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
                Event::Launch(u) => {
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
    /// first Enter opens the server search and later Enters and Shift+Enters
    /// step it; the chat steps its own scan. Esc always closes, clearing the
    /// pane's server highlight even when no reply ever arrived.
    fn find_key(&mut self, k: find::FindKey, cx: &mut Context<Self>) {
        let mut send = None;
        let mut rescan = false;
        let mut escape = false;
        let mut reveal = None;
        {
            let Some(f) = &mut self.find else { return };
            match k {
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
                find::FindKey::Enter { shift } => match f.target {
                    find::Target::Pane(p) => {
                        // (Re)open on a zero reply too: a no-match search_open
                        // already dropped the server's search state, so a step
                        // would only draw its "no active search" Notice.
                        if (!f.searched || f.total == 0) && !f.query.is_empty() {
                            send = Some(ClientMsg::SearchOpen {
                                pane: p,
                                query: f.query.clone(),
                            });
                            f.searched = true;
                        } else if f.searched {
                            send = Some(ClientMsg::SearchStep {
                                pane: p,
                                dir: find::step_dir(shift),
                            });
                        }
                    }
                    find::Target::Chat => {
                        if !f.hits.is_empty() {
                            let cur = f.current.max(1) as usize;
                            let next = if shift {
                                (cur + 1).min(f.hits.len())
                            } else {
                                (cur - 1).max(1)
                            };
                            f.current = next as u32;
                            if let Some(h) = f.hits.get(next - 1) {
                                reveal = Some(h.item);
                            }
                        }
                    }
                },
                find::FindKey::Escape => escape = true,
            }
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
        window.focus(&self.focus);
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
        window.focus(&self.focus);
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
        let (shift, alt, cmd) = (key.shift, key.alt, key.cmd);
        let vertical = matches!(key.key, "up" | "down");
        if !vertical {
            self.chat.field.goal_x.set(None);
        }
        match key.key {
            "enter" if !shift => self.submit_chat(cx),
            "enter" => self.chat.draft.replace_selection("\n"),
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
        // The palette takes every key while it shows: escape closes, enter
        // goes, up and down move, other cmd keys close it and run as today.
        if self.goto.is_some() && goto::key(self, &key, cx) {
            cx.stop_propagation();
            return;
        }
        // Esc under a seam drag puts the split back where it was grabbed.
        // After chord: with the key sheet open, esc closes the sheet first.
        if key.key == "escape" {
            if let Some(drag) = self.seam_drag.take() {
                cx.stop_propagation();
                if drag.last_pos != drag.start_pos {
                    self.send(ClientMsg::Command(Command::ResizeSeam {
                        a: drag.seam.a,
                        b: drag.seam.b,
                        pos: drag.start_pos,
                    }));
                }
                return;
            }
        }
        // The composer is modal: its keys never reach the pane behind it,
        // and its text keys still compose through the input handler.
        if self.launch_key(&key, cx) {
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
        if self.main == MainView::Mods && mods::key(self, &key, cx) {
            cx.stop_propagation();
            return;
        }
        // The board takes no typing, and text keys reach the pane through the
        // input handler, which composes dead keys and IME drafts first.
        if self.main != MainView::Terminal || keys::is_text(&key) {
            return;
        }
        if let Some(bytes) = keys::encode(&key, self.modes) {
            cx.stop_propagation();
            if self.notice.take().is_some() {
                cx.notify();
            }
            self.send(ClientMsg::Input(bytes));
        }
    }

    /// One cmd shortcut lands.
    fn cmd_action(&mut self, action: Shortcut, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Shortcut::Command(Command::CopySelection)
                if self.main == MainView::Chat && self.chat.draft.has_selection() =>
            {
                let text = self.chat.draft.selected_text().to_string();
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            Shortcut::Command(c) => self.send(ClientMsg::Command(c)),
            Shortcut::Cut => {
                if self.main == MainView::Chat && self.chat.draft.has_selection() {
                    let text = self.chat.draft.selected_text().to_string();
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    self.chat.draft.replace_selection("");
                    self.chat_moved(cx);
                }
            }
            Shortcut::SelectAll => {
                if self.main == MainView::Chat {
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
            // (agent, lead, shell) instead of picking a tab or a row.
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
            Shortcut::NewAgent => self.act(Action::OpenLaunch(None), cx),
            Shortcut::Find => self.find_open(cx),
        }
    }

    /// The paste shortcut: into the open find bar first, then the mods
    /// sheet, the chat draft, or the focused pane.
    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) else {
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
                self.chat.draft.replace_selection(&text);
                self.chat_moved(cx);
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

    fn on_down(&mut self, ev: &MouseDownEvent, window: &mut Window, _cx: &mut Context<Self>) {
        window.focus(&self.focus);
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

    fn on_wheel(&mut self, ev: &ScrollWheelEvent, _window: &mut Window, _cx: &mut Context<Self>) {
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
            .on_scroll_wheel(cx.listener(Self::on_wheel))
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
        if self.launch.open {
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
        if self.launch.open {
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
        if self.launch.open {
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
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The palette's query owns the keyboard while it shows.
        if self.goto.is_some() {
            return;
        }
        if self.launch.open {
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
        if self.launch.open {
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
        if self.launch.open {
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
        self.width = window.viewport_size().width;
        let main = match self.main {
            MainView::Chat => self.chat_view(window, cx).into_any_element(),
            MainView::Terminal => self.terminal(cx).into_any_element(),
            MainView::Orchestration => orchestration::render(self, cx)
                .font_family(ui::MONO)
                .text_size(px(ui::BODY))
                .into_any_element(),
            MainView::Mods => mods::render(self, cx)
                .font_family(ui::MONO)
                .text_size(px(ui::BODY))
                .into_any_element(),
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
                .child(find::bar(f, self.palette))
                .into_any_element(),
            _ => main,
        };
        let now = fno::app_api::digest_overlay::now_secs();
        let left = left_pane::render(self, now, self.adv, cx).text_size(px(ui::BODY));
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
        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_modifiers_changed(cx.listener(Self::on_modifiers))
            .on_action(cx.listener(Self::toggle_sheet))
            .on_action(cx.listener(Self::toggle_goto))
            // A click on a row, a tab or a view name lands on no focusable
            // element and would leave the window with no key focus.
            .on_any_mouse_down(cx.listener(|root, _, window, _| window.focus(&root.focus)))
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
                    .children(toast),
            )
            .when(self.launch.open, |d| {
                d.child(launch::render(self, window, cx))
            })
            .when(self.sheet, |d| d.child(sheet::render(self, cx)))
            .when(self.goto.is_some(), |d| d.child(goto::render(self, cx)))
    }
}

/// The UI state the exit paths persist. The mutation sites below keep it
/// current, and both quit paths, the quit shortcut and the red close
/// button, write it beside the window's live frame to the state file.
struct UiState {
    view: MainView,
    agents_open: bool,
    chat_key: Option<SessionKey>,
    path: Option<PathBuf>,
}

impl Global for UiState {}

/// Copy the live UI state into the global the exit paths read.
fn record_ui_state(root: &Root, cx: &mut App) {
    // The global is set when the window opens; every mutation site runs
    // after that, so the panicking read is unreachable in practice.
    let ui = cx.global_mut::<UiState>();
    ui.view = root.main;
    ui.agents_open = root.agents_open;
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
        agents_open: ui.agents_open,
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
    Application::new().run(move |cx: &mut App| {
        cx.bind_keys([KeyBinding::new(keys::SHEET_KEY, KeyboardShortcuts, None)]);
        cx.bind_keys([KeyBinding::new(keys::GOTO_KEY, GoTo, None)]);
        // The first menu is the app menu; Help lists the keys.
        cx.set_menus(vec![
            Menu {
                name: "fno".into(),
                items: Vec::new(),
            },
            Menu {
                name: "Help".into(),
                items: vec![
                    MenuItem::action("Go to…", GoTo),
                    MenuItem::action("Keyboard Shortcuts", KeyboardShortcuts),
                ],
            },
        ]);
        let window_bounds = initial_window_bounds(restored.as_ref(), cx);
        // The fold a quit left behind seeds the left pane; the exit paths
        // read the rest from the UiState global.
        let agents_open = restored.as_ref().map(|s| s.agents_open).unwrap_or(false);
        // Where the exit paths write; a HOME-less environment saves nothing.
        let state_path = super::app_state::state_path();
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
                    let root = Root::new(
                        session,
                        main,
                        pinned.clone(),
                        agents_open,
                        metrics,
                        window,
                        cx,
                    );
                    // The exit paths read this snapshot; the mutation sites
                    // keep it current while the window lives.
                    cx.set_global(UiState {
                        view: main,
                        agents_open,
                        chat_key: pinned,
                        path: state_path,
                    });
                    window.on_window_should_close(cx, |window, cx| {
                        persist_window_state(window, cx);
                        true
                    });
                    window.focus(&root.focus);
                    root
                })
            },
        );
        if let Err(e) = opened {
            eprintln!("fno-app: cannot open a window: {e}");
            cx.quit();
            return;
        }
        cx.on_window_closed(|cx| cx.quit()).detach();
        cx.activate(true);
    });
}
