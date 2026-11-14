//! Chat event readers: one harness session's transcript as
//! [`ChatEvent`]s, in source order, with a per-stream `seq`.
//!
//! Claude and Codex write JSONL, read line by line past a byte offset. The
//! opencode store is sqlite, read part by part past a watermark
//! (`opencode.rs`). The two reader kinds share one rule: `seq` is a pure
//! function of the source, so a reconnect re-reads from the start and skips
//! `seq < from_seq`. No seq is stored anywhere.

pub mod claude;
pub mod codex;
pub mod opencode;

use crate::harness::{AsHarness, Harness};
use crate::proto::chat_event::{ChatBody, ChatEvent, SessionKey, SourceRef};
use crate::proto::{read_msg_sync, write_msg_sync, ClientMsg, ProtoError, ServerMsg};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// An image from base64 text. Bytes that do not decode still yield an image:
/// its hash covers the text, and no data rides along.
pub(crate) fn image_from_base64(
    media_type: String,
    data: String,
) -> crate::proto::chat_event::ImageRef {
    use crate::proto::chat_event::{decode_base64, ImageRef, CHAT_IMAGE_INLINE_MAX};
    use sha2::{Digest, Sha256};
    let decoded = decode_base64(&data);
    let bytes = decoded.as_deref().unwrap_or(data.as_bytes());
    let sha256 = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let inline = decoded.is_some() && bytes.len() <= CHAT_IMAGE_INLINE_MAX;
    ImageRef::Inline {
        media_type,
        bytes_len: bytes.len() as u64,
        sha256,
        data: inline.then_some(data),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ChatReadError {
    #[error("chat events: harness {0:?} has no reader (claude, codex and opencode do)")]
    UnsupportedHarness(String),
    #[error("chat events: no {harness} transcript for session {session_id:?}")]
    NotFound { harness: String, session_id: String },
    #[error("chat events: {0}")]
    Io(#[from] std::io::Error),
    #[error("chat events: opencode store: {0}")]
    Store(String),
    #[error("chat events: {0}")]
    Proto(#[from] ProtoError),
    #[error("chat events: the server refused the stream: {0}")]
    Refused(String),
}

/// Where transcripts live. [`ChatRoots::host`] is this machine's; tests
/// point at scratch dirs.
#[derive(Debug, Clone, Default)]
pub struct ChatRoots {
    /// Claude projects dirs and the Codex sessions tree.
    pub transcripts: Vec<PathBuf>,
    pub opencode_stores: Vec<PathBuf>,
}

impl ChatRoots {
    pub fn host() -> Self {
        Self {
            transcripts: crate::transcript_tail::transcript_roots(),
            opencode_stores: opencode::host_stores(),
        }
    }
}

/// One mapped body and its source timestamp.
#[derive(Debug, Clone, PartialEq)]
pub struct Mapped {
    pub at_ms: Option<i64>,
    pub body: ChatBody,
}

impl Mapped {
    pub fn new(at_ms: Option<i64>, body: ChatBody) -> Self {
        Self { at_ms, body }
    }
}

/// One harness's JSONL row mapping.
pub trait RowMapper: Send {
    /// The bodies one complete line maps to. `None` holds the line: the
    /// mapper waits for source it needs (a file the harness has not written
    /// yet), and the reader retries the same line on the next poll. A
    /// mapper holds only when `more` is false (it is the last complete
    /// line), and it changes no state when it holds.
    fn map(&mut self, line: &str, more: bool) -> Option<Vec<Mapped>>;
}

enum Source {
    Lines {
        path: PathBuf,
        offset: u64,
        mapper: Box<dyn RowMapper>,
    },
    Store(opencode::StoreTail),
}

pub struct SessionReader {
    source: Source,
    next_seq: u64,
    from_seq: u64,
}

impl SessionReader {
    /// Locate `key`'s source under `roots`. Events below `from_seq` are read
    /// and dropped, never returned.
    pub fn open(key: &SessionKey, roots: &ChatRoots, from_seq: u64) -> Result<Self, ChatReadError> {
        let not_found = || ChatReadError::NotFound {
            harness: key.harness.clone(),
            session_id: key.session_id.clone(),
        };
        let source = match key.harness.as_str() {
            "claude" | "codex" => {
                let found = crate::transcript_tail::find_transcripts_in(
                    &roots.transcripts,
                    &[&key.session_id],
                );
                let path = found.get(&key.session_id).cloned().ok_or_else(not_found)?;
                let path = match (&key.subagent, key.harness.as_str()) {
                    (None, _) => path,
                    (Some(agent), "claude") => {
                        claude::subagent_path(&path, agent).ok_or_else(not_found)?
                    }
                    (Some(_), _) => return Err(not_found()),
                };
                let mapper: Box<dyn RowMapper> = if key.harness.as_harness() == Harness::Claude {
                    Box::new(claude::ClaudeMapper::new(&path))
                } else {
                    Box::new(codex::CodexMapper::default())
                };
                Source::Lines {
                    path,
                    offset: 0,
                    mapper,
                }
            }
            "opencode" => Source::Store(opencode::StoreTail::open(&roots.opencode_stores, key)?),
            other => return Err(ChatReadError::UnsupportedHarness(other.to_string())),
        };
        Ok(Self {
            source,
            next_seq: 0,
            from_seq,
        })
    }

    /// A reader over one JSONL file with an explicit mapper.
    pub fn lines(path: PathBuf, mapper: Box<dyn RowMapper>, from_seq: u64) -> Self {
        Self {
            source: Source::Lines {
                path,
                offset: 0,
                mapper,
            },
            next_seq: 0,
            from_seq,
        }
    }

    /// Every event the source gained since the last poll. A partial last
    /// line waits for its newline.
    pub fn poll(&mut self) -> Result<Vec<ChatEvent>, ChatReadError> {
        let mapped = match &mut self.source {
            Source::Lines {
                path,
                offset,
                mapper,
            } => poll_lines(path, offset, mapper.as_mut())?,
            Source::Store(store) => store.poll()?,
        };
        let mut out = Vec::with_capacity(mapped.len());
        for (source, item) in mapped {
            let seq = self.next_seq;
            self.next_seq += 1;
            if seq >= self.from_seq {
                out.push(ChatEvent {
                    seq,
                    at_ms: item.at_ms,
                    source,
                    body: item.body,
                });
            }
        }
        Ok(out)
    }
}

fn poll_lines(
    path: &Path,
    offset: &mut u64,
    mapper: &mut dyn RowMapper,
) -> Result<Vec<(SourceRef, Mapped)>, ChatReadError> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(*offset))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    // Only newline-terminated chunks are lines; a partial last one waits.
    let mut lines = buf
        .split_inclusive(|b| *b == b'\n')
        .filter(|chunk| chunk.ends_with(b"\n"))
        .peekable();
    let mut out = Vec::new();
    let mut consumed = 0usize;
    while let Some(chunk) = lines.next() {
        let more = lines.peek().is_some();
        let text = String::from_utf8_lossy(chunk.strip_suffix(b"\n").unwrap_or(chunk));
        let line = text.trim_end_matches('\r');
        if !line.trim().is_empty() {
            let Some(items) = mapper.map(line, more) else {
                break;
            };
            let source = SourceRef::Line {
                offset: *offset + consumed as u64,
            };
            out.extend(items.into_iter().map(|m| (source.clone(), m)));
        }
        consumed += chunk.len();
    }
    *offset += consumed as u64;
    Ok(out)
}

/// An ISO 8601 timestamp as epoch milliseconds.
pub(crate) fn iso_ms(text: Option<&str>) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text?)
        .ok()
        .map(|t| t.timestamp_millis())
}

/// Harness-injected user text: a tag-led block (`<system-reminder>`, hook
/// output) or the AGENTS.md preamble Codex sends as a user message.
pub(crate) fn injected_text(text: &str) -> bool {
    let text = text.trim_start();
    text.starts_with('<') || text.starts_with("# AGENTS.md instructions")
}

pub const IDLE_POLL: std::time::Duration = std::time::Duration::from_millis(250);
const BATCH_EVENTS: usize = 256;
/// Well under `MAX_MSG_BYTES`, so a batch of inline images still fits.
const BATCH_BYTES: usize = 8 * 1024 * 1024;

/// Split by count and by encoded size.
pub fn batches(events: Vec<ChatEvent>) -> Vec<Vec<ChatEvent>> {
    let mut out = Vec::new();
    let mut batch = Vec::new();
    let mut bytes = 0;
    for event in events {
        let size = serde_json::to_vec(&event).map(|v| v.len()).unwrap_or(0);
        if !batch.is_empty() && (batch.len() == BATCH_EVENTS || bytes + size > BATCH_BYTES) {
            out.push(std::mem::take(&mut batch));
            bytes = 0;
        }
        bytes += size;
        batch.push(event);
    }
    if !batch.is_empty() {
        out.push(batch);
    }
    out
}

/// Where [`pump`] sends batches: the mux socket and the app WebSocket each
/// implement it, so one loop answers when a chat stream polls and how big a
/// batch is.
pub trait BatchSink: Send {
    /// Deliver one batch. `false` means the client is gone.
    fn send(&mut self, batch: Vec<ChatEvent>) -> impl std::future::Future<Output = bool> + Send;
    /// Wait out one idle poll. `false` means the client is gone.
    fn idle(&mut self) -> impl std::future::Future<Output = bool> + Send;
}

/// Poll `reader` in `spawn_blocking` and feed `sink` until the client leaves
/// (`Ok`) or the source fails (`Err`).
pub async fn pump<S: BatchSink>(
    mut reader: SessionReader,
    sink: &mut S,
) -> Result<(), ChatReadError> {
    loop {
        let polled = tokio::task::spawn_blocking(move || {
            let result = reader.poll();
            (reader, result)
        })
        .await;
        let Ok((back, result)) = polled else {
            return Ok(());
        };
        reader = back;
        let events = result?;
        if events.is_empty() {
            if !sink.idle().await {
                return Ok(());
            }
            continue;
        }
        for batch in batches(events) {
            if !sink.send(batch).await {
                return Ok(());
            }
        }
    }
}

/// A live chat stream from a mux server, read without tokio.
pub struct ChatTail {
    stream: std::os::unix::net::UnixStream,
    done: bool,
}

/// Open `session`'s stream on the server at `socket`, from `from_seq` on.
pub fn tail_sync(
    socket: &Path,
    session: SessionKey,
    from_seq: u64,
) -> Result<ChatTail, ChatReadError> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    write_msg_sync(
        &mut stream,
        &ClientMsg::ChatTail {
            proto: crate::proto::PROTO_VERSION,
            build: crate::proto::BUILD_VERSION.to_string(),
            session,
            from_seq,
        },
    )?;
    Ok(ChatTail {
        stream,
        done: false,
    })
}

impl ChatTail {
    /// A second handle on the socket. Shutting it down ends a `next` that
    /// waits on an idle session, so a reader thread can be stopped.
    pub fn closer(&self) -> std::io::Result<std::os::unix::net::UnixStream> {
        self.stream.try_clone()
    }
}

impl Iterator for ChatTail {
    type Item = Result<Vec<ChatEvent>, ChatReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let refused = |reason: String| Some(Err(ChatReadError::Refused(reason)));
        match read_msg_sync::<_, ServerMsg>(&mut self.stream) {
            Ok(ServerMsg::ChatEvents { events, .. }) => Some(Ok(events)),
            Ok(ServerMsg::Err { msg, .. }) | Ok(ServerMsg::Bye { reason: msg }) => {
                self.done = true;
                refused(msg)
            }
            Ok(_) => {
                self.done = true;
                refused("an unexpected message on a chat stream".to_string())
            }
            Err(ProtoError::Closed) => {
                self.done = true;
                None
            }
            Err(error) => {
                self.done = true;
                Some(Err(error.into()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;
    use crate::proto::chat_event::{ChatBody, UserOrigin};
    use std::io::Write;

    #[test]
    fn chat_events_seq_is_stable_across_a_chunked_live_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let rows = [
            r#"{"type":"user","uuid":"u1","timestamp":"2026-11-11T10:00:00Z","message":{"role":"user","content":"hello"}}"#,
            r#"{"type":"assistant","uuid":"a1","message":{"id":"m1","content":[{"type":"text","text":"hi"}]}}"#,
            r#"{"type":"assistant","uuid":"a2","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#,
            r#"{"type":"user","uuid":"u2","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"a.txt"}]}}"#,
        ];
        let whole = rows.join("\n") + "\n";
        std::fs::write(&path, &whole).unwrap();
        let mapper = || Box::new(claude::ClaudeMapper::new(&path)) as Box<dyn RowMapper>;
        let replay = SessionReader::lines(path.clone(), mapper(), 0)
            .poll()
            .unwrap();

        let live_path = dir.path().join("live.jsonl");
        std::fs::write(&live_path, "").unwrap();
        let mut live = SessionReader::lines(live_path.clone(), mapper(), 0);
        let cut_a = whole.find("\n").unwrap() + 20;
        let cut_b = whole.rfind("{\"type\":\"user\"").unwrap() + 5;
        let mut got = Vec::new();
        for chunk in [&whole[..cut_a], &whole[cut_a..cut_b], &whole[cut_b..]] {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&live_path)
                .unwrap();
            f.write_all(chunk.as_bytes()).unwrap();
            got.extend(live.poll().unwrap());
        }
        let pairs = |events: &[ChatEvent]| {
            events
                .iter()
                .map(|e| (e.seq, e.body.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(pairs(&got), pairs(&replay));
        assert!(replay.iter().enumerate().all(|(i, e)| e.seq == i as u64));
        assert_eq!(replay.len(), 6, "{replay:#?}");
        assert!(matches!(
            &replay[0].body,
            ChatBody::UserMessage {
                origin: UserOrigin::Typed,
                ..
            }
        ));

        let resumed = SessionReader::lines(path.clone(), mapper(), 3)
            .poll()
            .unwrap();
        assert_eq!(pairs(&resumed), pairs(&replay[3..]));
    }
}
