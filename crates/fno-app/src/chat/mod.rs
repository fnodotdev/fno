//! Chat mode (the mode the app opens in): the focused pane's agent
//! session as a conversation. The events come from the mux's `ChatTail`
//! stream, so the app parses no transcript of its own. The composer types
//! into the session's pane through the addressed pane-input path.

pub mod composer;
pub mod field;
pub mod find;
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

use super::link::Layout;
use super::window::{MainView, Root};
use fno::chat_events::{tail_sync, ChatReadError, ChatTail};
use fno::proto::chat_event::{ChatEvent, SessionKey};
use fno::proto::{AgentRow, ProtoError};
pub use model::Conversation;
pub(super) use view::render;

/// The pause between the pasted text and the Enter that submits it, the
/// same settle `fno mux pane send --submit` uses.
pub const SUBMIT_SETTLE: Duration = Duration::from_millis(fno::app_api::CR_SETTLE_MS);

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
    #[error(transparent)]
    TooOld(#[from] crate::link::ServerTooOld),
}

fn connect(session: &str, key: SessionKey, from_seq: u64) -> Result<ChatTail, OpenError> {
    let socket = fno::proto::socket_path(session).map_err(OpenError::Session)?;
    // A throwaway connect: only a live server's missing stamp reads as old.
    if fno::proto::connect_unix_timeout(&socket, Duration::from_secs(3)).is_ok() {
        crate::link::check_server_wire(
            session,
            fno::app_api::proto::server_wire(session),
            crate::link::MIN_SERVER_PROTO,
        )?;
    }
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

/// The agent row the window is about. Chat names the row whose session
/// `chat` streams, which a pin can set apart from the focused pane; Chat
/// with no stream and Terminal name the focused pane's row; other views
/// name none.
pub fn focused_agent<'a>(
    layout: &'a Layout,
    view: MainView,
    chat: Option<&str>,
) -> Option<&'a AgentRow> {
    match (view, chat) {
        (MainView::Chat, Some(sid)) => layout
            .agents
            .iter()
            .find(|a| a.harness_session_id.as_deref() == Some(sid)),
        (MainView::Chat | MainView::Terminal, _) => layout
            .agents
            .iter()
            .find(|a| a.pane_id == Some(layout.focus)),
        _ => None,
    }
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
    /// The row picked in the open question's strip.
    pub pick: usize,
    pub draft: composer::Draft,
    /// The composer's last layout, scroll and drag state.
    pub field: Rc<field::FieldState>,
    /// Sent turns the transcript has not recorded yet.
    pub pending: Vec<String>,
    pub next_request: u64,
    /// The composer's bounds at the last paint, where the IME window opens.
    pub composer: Rc<Cell<Bounds<Pixels>>>,
    /// The shown session's harness skills, read on a switch.
    pub skills: Vec<crate::skills::Skill>,
    /// The popover's selected row and Esc state.
    pub skill_pick: crate::skills::Pick,
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
            pick: 0,
            draft: composer::Draft::default(),
            field: Rc::new(field::FieldState::default()),
            pending: Vec::new(),
            next_request: 1,
            composer: Rc::new(Cell::new(Bounds::default())),
            skills: Vec::new(),
            skill_pick: crate::skills::Pick::default(),
        }
    }
}

impl Chat {
    /// Show `key`, parking the conversation shown now.
    pub fn switch(&mut self, key: Option<SessionKey>, project: &str) {
        let conv = key
            .as_ref()
            .and_then(|k| self.parked.remove(k))
            .unwrap_or_default();
        let old = std::mem::replace(&mut self.conv, conv);
        if let Some(old_key) = self.key.take() {
            self.parked.insert(old_key, old);
        }
        self.skills = key
            .as_ref()
            .map(|k| crate::skills::list(&k.harness, project))
            .unwrap_or_default();
        self.key = key;
        self.stream = None;
        self.closed = None;
        self.sheet = None;
        self.pick = 0;
        self.pending.clear();
        self.opened_at = Instant::now();
        self.list.reset(self.conv.items.len());
    }

    /// The popover for the draft, grouped by source.
    pub fn skill_view(&self) -> Option<crate::skills::View> {
        let harness = &self.key.as_ref()?.harness;
        crate::skills::view(&self.skills, harness, self.draft.text(), &self.skill_pick)
    }

    /// Up or down moves the popover's row. False when none shows.
    pub fn skill_step(&mut self, down: bool) -> bool {
        let Some(v) = self.skill_view() else {
            return false;
        };
        self.skill_pick.step(self.draft.text(), down, v.rows.len());
        true
    }

    /// Esc closes the popover until the draft changes. False when none shows.
    pub fn skill_close(&mut self) -> bool {
        if self.skill_view().is_none() {
            return false;
        }
        self.skill_pick.close(self.draft.text());
        true
    }

    /// Tab fills the selected row as `<sigil>name `. False when none shows.
    pub fn complete_skill(&mut self) -> bool {
        let Some(v) = self.skill_view() else {
            return false;
        };
        let sigil = self
            .key
            .as_ref()
            .and_then(|k| crate::skills::sigil(&k.harness));
        let (Some(row), Some(sigil)) = (v.rows.get(v.sel), sigil) else {
            return false;
        };
        let text = format!("{sigil}{} ", row.name);
        self.draft.select_all();
        self.draft.replace_selection(&text);
        true
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
        if self.conv.open_question().is_none() {
            self.pick = 0;
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

/// The harness prefix names the key outright (`claude`, `codex`,
/// `opencode`). A bare id takes the harness of the transcript that carries
/// it; opencode sessions live in the store rather than the transcript tree,
/// so an unfound id stays claude and the server's NotFound names the truth.
/// `newest` alone stays the newest claude transcript, the shape the probe
/// and demo flows have always used.
pub fn resolve_key(which: &str) -> Option<SessionKey> {
    if let Some((harness, rest)) = which.split_once(':') {
        if matches!(harness, "claude" | "codex" | "opencode") {
            let session_id = match rest {
                "newest" => newest_for(harness)?,
                id => id.to_string(),
            };
            return Some(SessionKey {
                harness: harness.into(),
                session_id,
                subagent: None,
            });
        }
    }
    let harness = match which {
        "newest" => {
            return Some(SessionKey {
                harness: "claude".into(),
                session_id: newest_for("claude")?,
                subagent: None,
            })
        }
        _ => fno::app_api::transcript_tail::harness_of_session(which).unwrap_or("claude"),
    };
    Some(SessionKey {
        harness: harness.into(),
        session_id: which.to_string(),
        subagent: None,
    })
}

/// The newest session id a harness has on disk. All three harnesses read
/// through the shared `fno` helpers, so the ambient projects dir, the
/// isolated accounts and the codex tree follow one scan.
fn newest_for(harness: &str) -> Option<String> {
    match harness {
        "claude" => fno::app_api::transcript_tail::newest_transcript("claude"),
        "codex" => fno::app_api::transcript_tail::newest_transcript("codex"),
        "opencode" => fno::app_api::chat_events::newest_session_id(),
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_key_names_the_harness_prefix() {
        let key = resolve_key("codex:12345678-1234-4123-8123-123456789abc").unwrap();
        assert_eq!(key.harness, "codex");
        assert_eq!(key.session_id, "12345678-1234-4123-8123-123456789abc");
        let key = resolve_key("opencode:ses_123").unwrap();
        assert_eq!(key.harness, "opencode");
        assert_eq!(key.session_id, "ses_123");
    }

    #[test]
    fn resolve_key_bare_unfound_id_stays_claude() {
        // No transcript on this machine carries these ids, so the bare form
        // keeps the claude default and the server's NotFound names the truth.
        let key = resolve_key("00000000-0000-4000-8000-000000000000").unwrap();
        assert_eq!(key.harness, "claude");
        let key = resolve_key("agy:ses_x").unwrap();
        assert_eq!(key.harness, "claude");
        assert_eq!(key.session_id, "agy:ses_x");
    }
}
