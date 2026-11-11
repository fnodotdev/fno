//! The app's one connection to the mux: attach over the session socket, a
//! reader thread that forwards server messages, and one shared writer.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::proto::{self, ClientMsg, Frame, ProtoError, ServerMsg, BUILD_VERSION, PROTO_VERSION};
use crate::tree::Rect;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// The parts of a server `Layout` the app draws.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layout {
    pub panes: Vec<(u64, Rect)>,
    pub focus: u64,
    pub area: (u16, u16),
}

/// What the reader thread hands the UI.
#[derive(Debug)]
pub enum Event {
    Layout(Layout),
    Frame(u64, Frame),
    Modes(Vec<u8>),
    Notice(String),
    /// The server ended the connection (`Bye`) or the socket failed. The
    /// reader has stopped.
    Closed(String),
}

/// Map one server message to an app event. `None` for messages the app
/// does not use and for frames that break the `rows * cols` invariant: a
/// bad frame is dropped so the pane keeps its last good one.
fn to_event(msg: ServerMsg) -> Option<Event> {
    match msg {
        ServerMsg::Layout {
            panes, focus, area, ..
        } => Some(Event::Layout(Layout { panes, focus, area })),
        ServerMsg::Frame { pane_id, frame } if frame.geometry_ok() => {
            Some(Event::Frame(pane_id, frame))
        }
        ServerMsg::ModeSync { bytes } => Some(Event::Modes(bytes)),
        ServerMsg::Notice { text } => Some(Event::Notice(text)),
        ServerMsg::Bye { reason } => Some(Event::Closed(reason)),
        _ => None,
    }
}

/// Why an attach or a write failed. Each variant's text is what the window
/// shows and what `--probe` prints.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("{0}")]
    Session(String),
    #[error("cannot connect to session socket {}: {source}\n  is the mux server running? start one with `fno mux`.", path.display())]
    Connect {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{what} failed: {source}")]
    Io {
        what: &'static str,
        source: std::io::Error,
    },
    #[error("{what} failed: {source}")]
    Write {
        what: &'static str,
        source: ProtoError,
    },
    #[error("the socket writer lock was poisoned")]
    Poisoned,
}

/// Connect and send `Attach`. `rows`/`cols` (0, 0) attach as an observer,
/// which never resizes a pane: the probe uses that so it cannot disturb the
/// user's layout.
fn attach(session: &str, rows: u16, cols: u16) -> Result<UnixStream, LinkError> {
    let path = proto::socket_path(session).map_err(LinkError::Session)?;
    let mut stream = proto::connect_unix_timeout(&path, CONNECT_TIMEOUT)
        .map_err(|source| LinkError::Connect { path, source })?;
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    proto::write_msg_sync(
        &mut stream,
        &ClientMsg::Attach {
            proto: PROTO_VERSION,
            build: BUILD_VERSION.to_string(),
            rows,
            cols,
            cwd,
        },
    )
    .map_err(|source| LinkError::Write {
        what: "attach write",
        source,
    })?;
    Ok(stream)
}

/// A live attach. Every write goes through the one locked stream.
#[derive(Clone)]
pub struct Link {
    writer: Arc<Mutex<UnixStream>>,
}

impl Link {
    /// Attach with the window's content size and start the reader thread.
    pub fn connect(
        session: &str,
        rows: u16,
        cols: u16,
    ) -> Result<(Link, UnboundedReceiver<Event>), LinkError> {
        let stream = attach(session, rows, cols)?;
        let reader = stream.try_clone().map_err(|source| LinkError::Io {
            what: "socket clone",
            source,
        })?;
        let (tx, rx) = unbounded_channel();
        std::thread::Builder::new()
            .name("fno-desktop-reader".into())
            .spawn(move || read_loop(reader, tx))
            .map_err(|source| LinkError::Io {
                what: "reader thread start",
                source,
            })?;
        Ok((
            Link {
                writer: Arc::new(Mutex::new(stream)),
            },
            rx,
        ))
    }

    pub fn send(&self, msg: &ClientMsg) -> Result<(), LinkError> {
        let mut w = self.writer.lock().map_err(|_| LinkError::Poisoned)?;
        proto::write_msg_sync(&mut *w, msg).map_err(|source| LinkError::Write {
            what: "write",
            source,
        })
    }
}

fn read_loop(mut stream: UnixStream, tx: UnboundedSender<Event>) {
    loop {
        let event = match proto::read_msg_sync::<_, ServerMsg>(&mut stream) {
            Ok(msg) => match to_event(msg) {
                Some(e) => e,
                None => continue,
            },
            Err(ProtoError::Closed) => Event::Closed("the mux server closed the connection".into()),
            Err(e) => Event::Closed(format!("read failed: {e}")),
        };
        let closed = matches!(event, Event::Closed(_));
        if tx.send(event).is_err() || closed {
            return;
        }
    }
}

/// `--probe`: attach as an observer, wait up to `budget` for a `Layout`
/// and a frame for each of its panes, print one line, and return the exit
/// code. No window is opened.
pub fn probe(session: &str, budget: Duration) -> i32 {
    let mut stream = match attach(session, 0, 0) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fno-desktop: {e}");
            return 1;
        }
    };
    let deadline = Instant::now() + budget;
    let mut layout: Option<Layout> = None;
    let mut frames: BTreeMap<u64, usize> = BTreeMap::new();
    let done = |layout: &Option<Layout>, frames: &BTreeMap<u64, usize>| {
        layout
            .as_ref()
            .is_some_and(|l| l.panes.iter().all(|(id, _)| frames.contains_key(id)))
    };
    let mut failure: Option<String> = None;
    while !done(&layout, &frames) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            failure = Some("timed out waiting for a frame from every pane".into());
            break;
        }
        let _ = stream.set_read_timeout(Some(left));
        match proto::read_msg_sync::<_, ServerMsg>(&mut stream) {
            Ok(msg) => match to_event(msg) {
                Some(Event::Layout(l)) => layout = Some(l),
                Some(Event::Frame(id, _)) => *frames.entry(id).or_default() += 1,
                Some(Event::Closed(reason)) => {
                    failure = Some(reason);
                    break;
                }
                _ => {}
            },
            Err(ProtoError::Io(e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => {
                failure = Some(format!("read failed: {e}"));
                break;
            }
        }
    }
    let _ = proto::write_msg_sync(&mut stream, &ClientMsg::Detach);
    let Some(layout) = layout else {
        eprintln!(
            "fno-desktop: no layout from session {session}: {}",
            failure.unwrap_or_default()
        );
        return 1;
    };
    let counted: usize = layout
        .panes
        .iter()
        .map(|(id, _)| frames.get(id).copied().unwrap_or(0))
        .sum();
    println!(
        "attached session={session} panes={} frames={counted}",
        layout.panes.len()
    );
    match failure {
        Some(why) => {
            eprintln!("fno-desktop: {why}");
            1
        }
        None => 0,
    }
}
