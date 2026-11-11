//! The app window: one root view that lays out the left pane, the tab strip
//! and the server's pane rects, routes keys and mouse to the focused pane,
//! and hosts the orchestration board.

use std::cell::Cell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    canvas, div, font, point, prelude::*, px, rgb, size, AnyView, App, Application, Bounds,
    ClipboardItem, Context, ElementInputHandler, Entity, EntityInputHandler, FocusHandle,
    InputHandler, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, ScrollWheelEvent, Size, Subscription, Task, UTF16Selection, Window, WindowBounds,
    WindowOptions,
};

use super::grid::{Metrics, PaneView};
use super::hq::{Poller, Reply};
use super::keys::{self, Key, Modes, Shortcut};
use super::link::{Event, Layout, Link};
use super::mouse::{self, Hit};
use super::palette::Palette;
use super::{left_pane, orchestration, tabs};
use crate::proto::{self, ClientMsg, Command, Frame, MouseEvent, MouseKind};
use crate::theme::Theme;
use crate::tree::Rect;
use crate::web_hq::Board;

/// What fills the main area. The Extensions view adds its variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainView {
    Terminal,
    /// The command center board (cmd-2).
    Orchestration,
}

/// What a click in the left pane, the strip or the board does.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Send(Command),
    Notice(String),
    Answer { id: String, option: u32 },
    Show(MainView),
}

/// The most wheel lines one event sends, so a fling cannot flood the socket.
const WHEEL_CAP: i32 = 10;

pub struct Root {
    main: MainView,
    link: Option<Link>,
    pub(super) layout: Layout,
    frames: HashMap<u64, Arc<Frame>>,
    panes: HashMap<u64, Entity<PaneView>>,
    modes: Modes,
    notice: Option<String>,
    closed: Option<String>,
    metrics: Metrics,
    pub(super) palette: Palette,
    focus: FocusHandle,
    /// The content size (rows, cols) last sent to the server.
    sent_size: (u16, u16),
    pub(super) board: Board,
    poller: Option<Poller>,
    /// The terminal area's bounds at the last paint, for mouse hit tests.
    area: Rc<Cell<Bounds<Pixels>>>,
    /// The pane a left-button drag started in.
    drag: Option<(u64, Rect)>,
    wheel: f32,
    /// The input handler's uncommitted text.
    preedit: Option<String>,
    _reader: Option<Task<()>>,
    _hq: Option<Task<()>>,
    _activation: Option<Subscription>,
}

/// The status line's and the tab strip's height: one cell plus padding.
fn bar_height(cell: Size<Pixels>) -> Pixels {
    cell.height + px(8.0)
}

/// The content size sent to the server. The left pane, the strip and the
/// status line are always reserved, so a notice never resizes the panes, and
/// one row and one column are kept for the outer borders, which sit half a
/// cell outside the grid.
fn grid_size(viewport: Size<Pixels>, cell: Size<Pixels>) -> (u16, u16) {
    let height = viewport.height - bar_height(cell) * 2.0;
    let width = viewport.width - px(left_pane::WIDTH);
    let rows = ((height / cell.height).floor() - 1.0).max(1.0);
    let cols = ((width / cell.width).floor() - 1.0).max(1.0);
    (
        rows.min(u16::MAX as f32) as u16,
        cols.min(u16::MAX as f32) as u16,
    )
}

impl Root {
    fn new(session: String, metrics: Metrics, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sent_size = grid_size(window.viewport_size(), metrics.cell);
        let mut root = Root {
            main: MainView::Terminal,
            link: None,
            layout: Layout::default(),
            frames: HashMap::new(),
            panes: HashMap::new(),
            modes: Modes::default(),
            notice: None,
            closed: None,
            metrics,
            palette: Palette::from_theme(&Theme::default_theme()),
            focus: cx.focus_handle(),
            sent_size,
            board: Board::default(),
            poller: None,
            area: Rc::new(Cell::new(Bounds::default())),
            drag: None,
            wheel: 0.0,
            preedit: None,
            _reader: None,
            _hq: None,
            _activation: None,
        };
        // Repaint every pane on (de)activation: the cursor turns hollow.
        root._activation = Some(cx.observe_window_activation(window, |root, _, cx| {
            for view in root.panes.values() {
                view.update(cx, |_, cx| cx.notify());
            }
        }));
        match Link::connect(&session, sent_size.0, sent_size.1) {
            Ok((link, mut rx)) => {
                root.link = Some(link);
                root._reader = Some(cx.spawn(async move |this, cx| {
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
            Err(e) => root.closed = Some(e.to_string()),
        }
        match Poller::start() {
            Ok((poller, mut rx)) => {
                root.poller = Some(poller);
                root._hq = Some(cx.spawn(async move |this, cx| {
                    while let Some(reply) = rx.recv().await {
                        if this.update(cx, |root, cx| root.on_hq(reply, cx)).is_err() {
                            break;
                        }
                    }
                }));
            }
            Err(e) => root.notice = Some(format!("the board poll did not start: {e}")),
        }
        root
    }

    fn send(&mut self, msg: ClientMsg) {
        if let Some(link) = &self.link {
            if let Err(e) = link.send(&msg) {
                self.closed = Some(e.to_string());
                self.link = None;
            }
        }
    }

    pub(super) fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        match action {
            Action::Send(cmd) => {
                self.main = MainView::Terminal;
                self.send(ClientMsg::Command(cmd));
            }
            Action::Notice(text) => self.notice = Some(text),
            Action::Answer { id, option } => match &self.poller {
                Some(poller) => {
                    poller.answer(id, option);
                    self.notice = Some(format!("answering option {option}"));
                }
                None => self.notice = Some("no board poll: cannot answer".into()),
            },
            Action::Show(view) => self.main = view,
        }
        cx.notify();
    }

    fn on_hq(&mut self, reply: Reply, cx: &mut Context<Self>) {
        match reply {
            Reply::Reading(r) => {
                self.board.record(
                    r.fleet,
                    r.questions,
                    &r.backlog,
                    r.measured_at,
                    &self.layout.agents,
                );
            }
            Reply::Answered(Ok(receipt)) => self.notice = Some(receipt),
            Reply::Answered(Err(why)) => self.notice = Some(format!("answer refused: {why}")),
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
                    metrics,
                    palette,
                })
            })
            .clone()
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
                    if let Some(poller) = &self.poller {
                        poller.set_roster(&self.layout.agents);
                    }
                    relayout = true;
                }
                Event::Modes(bytes) => self.modes.apply(&bytes),
                Event::Notice(text) => {
                    self.notice = Some(text);
                    relayout = true;
                }
                Event::Copy(text) => {
                    let n = text.chars().count();
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    self.notice = Some(format!("copied {n} characters"));
                    relayout = true;
                }
                Event::OpenLink(url) => {
                    if crate::link::is_openable(&url) {
                        cx.open_url(&url);
                    }
                }
                Event::Closed(reason) => {
                    self.closed = Some(reason);
                    self.link = None;
                    relayout = true;
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
            self.sync_panes(cx);
            cx.notify();
        }
    }

    /// Push the focus and the input handler's draft to every pane view.
    fn sync_panes(&mut self, cx: &mut Context<Self>) {
        let focus = self.layout.focus;
        for (id, view) in &self.panes {
            let focused = *id == focus;
            let preedit = self.preedit.clone().filter(|_| focused);
            view.update(cx, |p, cx| {
                if p.focused != focused || p.preedit != preedit {
                    p.focused = focused;
                    p.preedit = preedit;
                    cx.notify();
                }
            });
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let key = Key {
            key: &ks.key,
            key_char: ks.key_char.as_deref(),
            ctrl: ks.modifiers.control,
            alt: ks.modifiers.alt,
            shift: ks.modifiers.shift,
            cmd: ks.modifiers.platform,
        };
        if let Some(action) = keys::shortcut(&key) {
            cx.stop_propagation();
            match action {
                Shortcut::Command(c) => self.send(ClientMsg::Command(c)),
                Shortcut::Paste => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                        let bytes = keys::paste(&text, self.modes);
                        self.send(ClientMsg::Input(bytes));
                    }
                }
                Shortcut::Quit => {
                    self.send(ClientMsg::Detach);
                    cx.quit();
                }
                Shortcut::View(n) => {
                    self.main = if n == 2 {
                        MainView::Orchestration
                    } else {
                        MainView::Terminal
                    };
                    cx.notify();
                }
            }
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

    /// A window point relative to the terminal area.
    fn local(&self, at: Point<Pixels>) -> (f32, f32) {
        let o = self.area.get().origin;
        (f32::from(at.x - o.x), f32::from(at.y - o.y))
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
        if let Some(hit) = mouse::hit(x, y, self.cell(), &self.layout.panes) {
            let rect = self
                .layout
                .panes
                .iter()
                .find(|(id, _)| *id == hit.pane)
                .map(|p| p.1);
            self.drag = rect.map(|r| (hit.pane, r));
            self.mouse(
                hit,
                MouseKind::Press(proto::MouseButton::Left),
                ev.modifiers.alt,
            );
        }
    }

    fn on_move(&mut self, ev: &MouseMoveEvent, _window: &mut Window, _cx: &mut Context<Self>) {
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
        let Some(hit) = mouse::hit(x, y, self.cell(), &self.layout.panes) else {
            return;
        };
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
        for (id, rect) in rects {
            let view = self.pane_view(id, cx);
            let border = if id == self.layout.focus {
                self.palette.accent
            } else {
                self.palette.border
            };
            // The border sits in the one-cell gap the server leaves
            // between rects: half a cell out on every side. The grid itself
            // starts half a cell in, so the outer borders are never clipped.
            let (hx, hy) = (cw / 2.0, ch / 2.0);
            out = out.child(
                div()
                    .absolute()
                    .left(cw * rect.x as f32)
                    .top(ch * rect.y as f32)
                    .w(cw * rect.cols as f32 + cw)
                    .h(ch * rect.rows as f32 + ch)
                    .border_1()
                    .border_color(rgb(border))
                    .child(
                        div()
                            .absolute()
                            .left(hx - px(1.0))
                            .top(hy - px(1.0))
                            .w(cw * rect.cols as f32)
                            .h(ch * rect.rows as f32)
                            .child(
                                AnyView::from(view)
                                    .cached(gpui::StyleRefinement::default().size_full()),
                            ),
                    ),
            );
        }
        out
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
        _range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
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
        self.preedit.as_deref().map(|t| 0..utf16_len(t))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_preedit(None, cx);
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_preedit(Some(new_text.to_string()), cx);
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.cursor_bounds(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
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
        let main = match self.main {
            MainView::Terminal => self.terminal(cx).into_any_element(),
            MainView::Orchestration => orchestration::render(self, cx)
                .font_family(".SystemUIFont")
                .text_sm()
                .into_any_element(),
        };
        let now = crate::digest_overlay::now_secs();
        let left = left_pane::render(self, now, cx)
            .font_family(".SystemUIFont")
            .text_sm();
        let strip = tabs::render(self, cx)
            .h(bar_height(self.metrics.cell))
            .items_center()
            .font_family(".SystemUIFont")
            .text_sm();
        let status = self.closed.clone().or_else(|| self.notice.clone());
        let status_color = if self.closed.is_some() {
            self.palette.error
        } else {
            self.palette.fg
        };
        let status_line = div()
            .h(bar_height(self.metrics.cell))
            .flex_none()
            .px_2()
            .py_1()
            .bg(rgb(self.palette.border))
            .text_color(rgb(status_color))
            .children(status);
        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_up))
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
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(strip)
                    .child(main)
                    .child(status_line),
            )
    }
}

/// Open the window and run the app until it quits.
pub fn run(session: String, family: String, font_size: f32) {
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1360.0), px(800.0)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
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
                    let root = Root::new(session, metrics, window, cx);
                    window.focus(&root.focus);
                    root
                })
            },
        );
        if let Err(e) = opened {
            eprintln!("fno-desktop: cannot open a window: {e}");
            cx.quit();
            return;
        }
        cx.on_window_closed(|cx| cx.quit()).detach();
        cx.activate(true);
    });
}
