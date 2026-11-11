//! The app window: one root view that lays out the server's pane rects and
//! routes keys to the focused pane.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    div, font, prelude::*, px, rgb, size, AnyView, App, Application, Bounds, Context, Entity,
    FocusHandle, KeyDownEvent, MouseButton, Pixels, Size, Task, Window, WindowBounds,
    WindowOptions,
};

use super::grid::{Metrics, PaneView};
use super::keys::{self, Key, Modes, Shortcut};
use super::link::{Event, Layout, Link};
use super::palette::Palette;
use crate::proto::{ClientMsg, Command, Frame};
use crate::theme::Theme;

/// What fills the main area. The Extensions view adds its variant here.
pub enum MainView {
    Terminal,
}

struct Root {
    main: MainView,
    /// The native left pane. Empty until the left pane lands.
    left: Option<AnyView>,
    link: Option<Link>,
    layout: Layout,
    frames: HashMap<u64, Arc<Frame>>,
    panes: HashMap<u64, Entity<PaneView>>,
    modes: Modes,
    notice: Option<String>,
    closed: Option<String>,
    metrics: Metrics,
    palette: Palette,
    focus: FocusHandle,
    /// The content size (rows, cols) last sent to the server.
    sent_size: (u16, u16),
    _reader: Option<Task<()>>,
}

fn grid_size(viewport: Size<Pixels>, cell: Size<Pixels>) -> (u16, u16) {
    let rows = (viewport.height / cell.height).floor().max(1.0);
    let cols = (viewport.width / cell.width).floor().max(1.0);
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
            left: None,
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
            _reader: None,
        };
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
            Err(e) => root.closed = Some(e),
        }
        root
    }

    fn send(&mut self, msg: ClientMsg) {
        if let Some(link) = &self.link {
            if let Err(e) = link.send(&msg) {
                self.closed = Some(e);
                self.link = None;
            }
        }
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
                    relayout = true;
                }
                Event::Modes(bytes) => self.modes.apply(&bytes),
                Event::Notice(text) => {
                    self.notice = Some(text);
                    relayout = true;
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
            let focus = self.layout.focus;
            for (id, view) in &self.panes {
                let focused = *id == focus;
                view.update(cx, |p, cx| {
                    if p.focused != focused {
                        p.focused = focused;
                        cx.notify();
                    }
                });
            }
            cx.notify();
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
            }
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

    fn terminal(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let (cw, ch) = (self.metrics.cell.width, self.metrics.cell.height);
        // flex_1, not size_full: the status line below must not push the
        // grid's last row out of the window.
        let mut area = div().relative().flex_1().w_full().overflow_hidden();
        let rects = self.layout.panes.clone();
        for (id, rect) in rects {
            let view = self.pane_view(id, cx);
            let border = if id == self.layout.focus {
                self.palette.accent
            } else {
                self.palette.border
            };
            // The border sits in the one-cell gap the server leaves
            // between rects: half a cell out on every side.
            let (hx, hy) = (cw / 2.0, ch / 2.0);
            area = area.child(
                div()
                    .absolute()
                    .left(cw * rect.x as f32 - hx)
                    .top(ch * rect.y as f32 - hy)
                    .w(cw * rect.cols as f32 + cw)
                    .h(ch * rect.rows as f32 + ch)
                    .border_1()
                    .border_color(rgb(border))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |root, _, _, _| {
                            root.send(ClientMsg::Command(Command::FocusPane(id)))
                        }),
                    )
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
        area
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
            MainView::Terminal => self.terminal(cx),
        };
        let status = self.closed.clone().or_else(|| self.notice.clone());
        let mut root = div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .flex()
            .flex_row()
            .bg(rgb(self.palette.bg))
            .text_color(rgb(self.palette.fg))
            .font_family(self.metrics.font.family.clone())
            .text_size(self.metrics.font_size);
        if let Some(left) = self.left.clone() {
            root = root.child(left);
        }
        let mut column = div().flex_1().flex().flex_col().child(main);
        if let Some(text) = status {
            column = column.child(div().px_2().bg(rgb(self.palette.border)).child(text));
        }
        root.child(column)
    }
}

/// Open the window and run the app until it quits.
pub fn run(session: String, family: String, font_size: f32) {
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1100.0), px(720.0)), cx);
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
                let metrics = Metrics {
                    font,
                    font_size,
                    cell: size(width, (font_size * 1.25).round()),
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
