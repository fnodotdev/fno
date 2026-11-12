//! Chat mode (cmd-1, the mode the app opens in): the focused pane's agent
//! session as a conversation. The events come from the mux's `ChatTail`
//! stream, so the app parses no transcript of its own. The composer types
//! into the session's pane through the addressed pane-input path.

mod markdown;
mod model;
mod view;

use std::cell::Cell;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{px, Bounds, ListAlignment, ListState, Pixels};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use super::window::Root;
use fno::chat_events::{tail_sync, ChatReadError, ChatTail};
use fno::proto::chat_event::{ChatEvent, SessionKey};
use fno::proto::ProtoError;
pub use model::Conversation;
pub(super) use view::render;

/// The pause between the pasted text and the Enter that submits it, the
/// same settle `fno mux pane send --submit` uses.
pub const SUBMIT_SETTLE: Duration =
    Duration::from_millis(fno::app_api::CR_SETTLE_MS);

/// How long a stream may send nothing before the view says it is empty.
pub const EMPTY_AFTER: Duration = Duration::from_secs(2);

/// What the stream thread hands the window.
pub enum Msg {
    Events(Vec<ChatEvent>),
    /// The server ended the stream normally.
    Ended,
    Failed(String),
}

/// One open stream. Dropping it closes the socket, which ends the thread.
pub struct Stream {
    closer: UnixStream,
}

impl Drop for Stream {
    fn drop(&mut self) {
        let _ = self.closer.shutdown(std::net::Shutdown::Both);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("{0}")]
    Session(String),
    #[error(transparent)]
    Read(#[from] ChatReadError),
    #[error("chat stream: {0}")]
    Io(#[from] std::io::Error),
}

fn connect(session: &str, key: SessionKey, from_seq: u64) -> Result<ChatTail, OpenError> {
    let socket = fno::proto::socket_path(session).map_err(OpenError::Session)?;
    Ok(tail_sync(&socket, key, from_seq)?)
}

/// Open `key`'s chat stream on the `session` server from `from_seq`.
pub fn open(
    session: &str,
    key: SessionKey,
    from_seq: u64,
) -> Result<(Stream, UnboundedReceiver<Msg>), OpenError> {
    let tail = connect(session, key, from_seq)?;
    let closer = tail.closer()?;
    let (tx, rx) = unbounded_channel();
    std::thread::Builder::new()
        .name("fno-app-chat".into())
        .spawn(move || {
            for batch in tail {
                let msg = match batch {
                    Ok(events) => Msg::Events(events),
                    Err(e) => Msg::Failed(e.to_string()),
                };
                let failed = matches!(msg, Msg::Failed(_));
                if tx.send(msg).is_err() || failed {
                    return;
                }
            }
            let _ = tx.send(Msg::Ended);
        })?;
    Ok((Stream { closer }, rx))
}

/// The session chat mode shows: the pinned one, else the focused pane's.
pub fn focused_key(root: &Root) -> Option<SessionKey> {
    if let Some(key) = &root.pinned {
        return Some(key.clone());
    }
    let row = root
        .layout
        .agents
        .iter()
        .find(|a| a.pane_id == Some(root.layout.focus))?;
    Some(SessionKey {
        harness: row.harness.clone()?,
        session_id: row.harness_session_id.clone()?,
        subagent: None,
    })
}

/// The pane on screen that hosts `key`'s session, with the identity the
/// server checks before it types into it.
pub fn target_pane(root: &Root, key: &SessionKey) -> Option<(u64, String)> {
    root.layout.agents.iter().find_map(|a| {
        let pane = a.pane_id?;
        let live = !a.exited && a.harness_session_id.as_deref() == Some(key.session_id.as_str());
        let shown = root.layout.panes.iter().any(|(id, _)| *id == pane);
        (live && shown).then(|| (pane, key.session_id.clone()))
    })
}

/// Why the shown stream is not open.
#[derive(Debug, Clone, PartialEq)]
pub enum Closed {
    Ended,
    Failed(String),
}

/// The chat state the window keeps.
pub struct Chat {
    /// The session shown, `None` when the focused pane hosts no agent.
    pub key: Option<SessionKey>,
    pub conv: Conversation,
    /// Every other session read this run, so a refocus resumes, never replays.
    pub parked: HashMap<SessionKey, Conversation>,
    pub stream: Option<Stream>,
    pub closed: Option<Closed>,
    pub opened_at: Instant,
    pub list: ListState,
    /// The tool call whose sheet is open, by row index.
    pub sheet: Option<usize>,
    pub draft: String,
    /// Sent turns the transcript has not recorded yet.
    pub pending: Vec<String>,
    pub next_request: u64,
    /// The composer's bounds at the last paint, where the IME window opens.
    pub composer: Rc<Cell<Bounds<Pixels>>>,
}

impl Default for Chat {
    fn default() -> Self {
        Chat {
            key: None,
            conv: Conversation::default(),
            parked: HashMap::new(),
            stream: None,
            closed: None,
            opened_at: Instant::now(),
            list: ListState::new(0, ListAlignment::Bottom, px(800.0)),
            sheet: None,
            draft: String::new(),
            pending: Vec::new(),
            next_request: 1,
            composer: Rc::new(Cell::new(Bounds::default())),
        }
    }
}

impl Chat {
    /// Show `key`, parking the conversation shown now.
    pub fn switch(&mut self, key: Option<SessionKey>) {
        let conv = key
            .as_ref()
            .and_then(|k| self.parked.remove(k))
            .unwrap_or_default();
        let old = std::mem::replace(&mut self.conv, conv);
        if let Some(old_key) = self.key.take() {
            self.parked.insert(old_key, old);
        }
        self.key = key;
        self.stream = None;
        self.closed = None;
        self.sheet = None;
        self.pending.clear();
        self.opened_at = Instant::now();
        self.list.reset(self.conv.items.len());
    }

    /// Fold a batch. Rows that changed in place are measured again; rows
    /// past them are new.
    pub fn apply(&mut self, msgs: Vec<Msg>) {
        let before = self.conv.items.len();
        let mut first = before;
        for msg in msgs {
            match msg {
                Msg::Events(events) => {
                    for e in events {
                        if let Some(i) = self.conv.push(e) {
                            first = first.min(i);
                        }
                    }
                }
                Msg::Ended => self.closed = Some(Closed::Ended),
                Msg::Failed(why) => self.closed = Some(Closed::Failed(why)),
            }
        }
        self.drop_recorded_pending(first);
        let after = self.conv.items.len();
        if first < after {
            self.list.splice(first..before, after - first);
        }
    }

    /// A pending turn leaves once the transcript records the same text.
    fn drop_recorded_pending(&mut self, from: usize) {
        for item in self.conv.items.iter().skip(from) {
            if let model::Item::User {
                content,
                meta: false,
            } = item
            {
                let said = content.text.trim();
                if let Some(at) = self.pending.iter().position(|p| p.trim() == said) {
                    self.pending.remove(at);
                }
            }
        }
    }
}

/// The session id of the most recently written claude transcript.
fn newest_claude_session() -> Option<String> {
    let projects = std::fs::read_dir(fno::app_api::transcript_tail::claude_projects_dir()).ok()?;
    let files = projects
        .flatten()
        .filter_map(|d| std::fs::read_dir(d.path()).ok())
        .flat_map(|dir| dir.flatten());
    files
        .filter_map(|f| {
            let path = f.path();
            let id = path.file_stem()?.to_str()?.to_string();
            let transcript = path.extension().is_some_and(|e| e == "jsonl")
                && fno::app_api::transcript_tail::transcript_uuid_shaped(&id);
            if !transcript {
                return None;
            }
            Some((f.metadata().ok()?.modified().ok()?, id))
        })
        .max()
        .map(|(_, id)| id)
}

/// `--chat <id|newest>` as a claude session key.
pub fn claude_key(which: &str) -> Option<SessionKey> {
    let session_id = match which {
        "newest" => newest_claude_session()?,
        id => id.to_string(),
    };
    Some(SessionKey {
        harness: "claude".into(),
        session_id,
        subagent: None,
    })
}

/// `--probe --chat <id|newest>`: stream one claude session from the server,
/// count its events until no batch comes for `idle`, and print the count.
pub fn probe(session: &str, key: SessionKey, idle: Duration) -> i32 {
    let id = key.session_id.clone();
    let tail = match connect(session, key, 0) {
        Ok(tail) => tail,
        Err(e) => {
            eprintln!("fno-app: {e}");
            return 1;
        }
    };
    if let Ok(socket) = tail.closer() {
        let _ = socket.set_read_timeout(Some(idle));
    }
    let (mut n, mut failure) = (0usize, None);
    for batch in tail {
        match batch {
            Ok(events) => n += events.len(),
            Err(ChatReadError::Proto(ProtoError::Io(e)))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                break
            }
            Err(e) => {
                failure = Some(e.to_string());
                break;
            }
        }
    }
    println!("chat_events={n} session={id}");
    match failure {
        Some(why) => {
            eprintln!("fno-app: {why}");
            1
        }
        None if n == 0 => 1,
        None => 0,
    }
}
