//! The opencode sqlite store as chat events. opencode rewrites a part row in
//! place while it streams, so the reader emits parts in store order and stops
//! at the first part that is not final: a text or reasoning part with no
//! `time.end`, or a tool that is still pending or running. A running tool
//! emits its call once and holds the watermark until it completes, so a live
//! tail and a replay emit the same sequence.

use super::{injected_text, ChatReadError, Mapped};
use crate::proto::chat_event::{
    ChatBody, ContentPart, ImageRef, SessionKey, SourceRef, ToolInput, TurnOutcome, UserOrigin,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// One session's parts in store order (message time, message id, part id),
/// the order fno-agents' opencode reader uses.
const PART_SQL: &str = "SELECT p.id, m.id, m.data, p.data, p.time_created FROM part p \
     JOIN message m ON m.id = p.message_id WHERE p.session_id = ?1 \
     ORDER BY m.time_created, m.id, p.id LIMIT -1 OFFSET ?2";

const CURSOR_SQL: &str = "SELECT max(time_updated), count(*) FROM part WHERE session_id = ?1";

/// `$OPENCODE_DB` when set, else every `opencode*.db` in
/// `${XDG_DATA_HOME:-~/.local/share}/opencode`, sorted.
pub fn host_stores() -> Vec<PathBuf> {
    if let Some(db) = std::env::var_os("OPENCODE_DB").filter(|v| !v.is_empty()) {
        return vec![PathBuf::from(db)];
    }
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .unwrap_or_else(|| PathBuf::from("."));
    let mut out: Vec<PathBuf> = std::fs::read_dir(data_home.join("opencode"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("opencode") && n.ends_with(".db"))
        })
        .collect();
    out.sort();
    out
}

#[derive(Deserialize)]
struct MessageData {
    #[serde(default)]
    role: String,
}

#[derive(Deserialize)]
struct Time {
    #[serde(default)]
    end: Option<i64>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum Part {
    Text {
        #[serde(default)]
        text: String,
        #[serde(default)]
        synthetic: bool,
        #[serde(default)]
        time: Option<Time>,
    },
    Reasoning {
        #[serde(default)]
        text: String,
        #[serde(default)]
        time: Option<Time>,
    },
    Tool {
        #[serde(rename = "callID")]
        call_id: String,
        tool: String,
        state: ToolState,
    },
    File {
        mime: String,
        url: String,
        #[serde(default)]
        filename: Option<String>,
    },
    Subtask {
        #[serde(default)]
        prompt: String,
    },
    Compaction {},
    StepStart {},
    StepFinish {},
    Agent {},
    Snapshot {},
    Patch {},
    Retry {},
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum ToolState {
    Pending {
        #[serde(default)]
        input: Value,
    },
    Running {
        #[serde(default)]
        input: Value,
        #[serde(default)]
        metadata: Option<ToolMeta>,
    },
    Completed {
        #[serde(default)]
        input: Value,
        #[serde(default)]
        output: String,
        #[serde(default)]
        metadata: Option<ToolMeta>,
    },
    Error {
        #[serde(default)]
        input: Value,
        #[serde(default)]
        error: String,
        #[serde(default)]
        metadata: Option<ToolMeta>,
    },
}

#[derive(Deserialize, Default)]
struct ToolMeta {
    /// The child session a `task` tool started.
    #[serde(rename = "sessionId", default)]
    session_id: Option<String>,
    /// A `question` tool's answers.
    #[serde(default)]
    answers: Option<Value>,
}

/// The bodies one part maps to, and whether the part is final. A part that
/// is not final maps to a prefix of what it maps to once final.
fn map_part(part_id: &str, message_id: &str, role: &str, part: Part) -> (Vec<ChatBody>, bool) {
    let finished = |time: &Option<Time>| time.as_ref().is_none_or(|t| t.end.is_some());
    match part {
        Part::Text {
            text,
            synthetic,
            time,
        } => {
            let done = finished(&time);
            let bodies = if role == "user" {
                let origin = if synthetic || injected_text(&text) {
                    UserOrigin::Meta
                } else {
                    UserOrigin::Typed
                };
                vec![ChatBody::UserMessage {
                    id: message_id.to_string(),
                    origin,
                    parts: vec![ContentPart::Text { text }],
                }]
            } else {
                let part = part_id.to_string();
                vec![
                    ChatBody::TextStarted { part: part.clone() },
                    ChatBody::TextDelta {
                        part: part.clone(),
                        text,
                    },
                    ChatBody::TextEnded { part },
                ]
            };
            (if done { bodies } else { Vec::new() }, done)
        }
        Part::Reasoning { text, time } => {
            let done = finished(&time);
            let part = part_id.to_string();
            let mut bodies = vec![ChatBody::ThinkingStarted {
                part: part.clone(),
                redacted: text.is_empty(),
            }];
            if !text.is_empty() {
                bodies.push(ChatBody::ThinkingDelta {
                    part: part.clone(),
                    text,
                });
            }
            bodies.push(ChatBody::ThinkingEnded { part });
            (if done { bodies } else { Vec::new() }, done)
        }
        Part::Tool {
            call_id,
            tool,
            state,
        } => map_tool(call_id, tool, state),
        Part::File {
            mime,
            url,
            filename,
        } => {
            let part = if mime.starts_with("image/") {
                let image = match url
                    .strip_prefix("data:")
                    .and_then(|r| r.split_once(";base64,"))
                {
                    Some((media_type, data)) => {
                        ImageRef::from_base64(media_type.to_string(), data.to_string())
                    }
                    None => ImageRef::Url { url },
                };
                ContentPart::Image { image }
            } else {
                ContentPart::File {
                    path: filename.unwrap_or(url),
                }
            };
            let origin = if role == "user" {
                UserOrigin::Typed
            } else {
                UserOrigin::Meta
            };
            (
                vec![ChatBody::UserMessage {
                    id: message_id.to_string(),
                    origin,
                    parts: vec![part],
                }],
                true,
            )
        }
        Part::Subtask { prompt } => (
            vec![ChatBody::UserMessage {
                id: message_id.to_string(),
                origin: UserOrigin::Meta,
                parts: vec![ContentPart::Text { text: prompt }],
            }],
            true,
        ),
        Part::Compaction {} => (
            vec![ChatBody::Compacted {
                summary: None,
                redacted: false,
            }],
            true,
        ),
        Part::StepStart {} => (
            vec![ChatBody::TurnStarted {
                turn_id: Some(message_id.to_string()),
            }],
            true,
        ),
        Part::StepFinish {} => (
            vec![ChatBody::TurnEnded {
                turn_id: Some(message_id.to_string()),
                outcome: TurnOutcome::Done,
            }],
            true,
        ),
        Part::Agent {} | Part::Snapshot {} | Part::Patch {} | Part::Retry {} => (Vec::new(), true),
    }
}

fn map_tool(call_id: String, tool: String, state: ToolState) -> (Vec<ChatBody>, bool) {
    let (input, metadata, end) = match state {
        ToolState::Pending { input } => (input, None, None),
        ToolState::Running { input, metadata } => (input, metadata, None),
        ToolState::Completed {
            input,
            output,
            metadata,
        } => (input, metadata, Some((true, output))),
        ToolState::Error {
            input,
            error,
            metadata,
        } => (input, metadata, Some((false, error))),
    };
    let metadata = metadata.unwrap_or_default();
    let mut bodies = Vec::new();
    if tool == "question" {
        bodies.push(ChatBody::QuestionAsked {
            call_id: call_id.clone(),
            prompt: input,
        });
        if let Some((ok, text)) = end {
            bodies.push(if ok {
                ChatBody::QuestionAnswered {
                    call_id,
                    answer: metadata.answers.unwrap_or(Value::String(text)),
                }
            } else {
                ChatBody::ToolFinished {
                    call_id,
                    ok,
                    output: vec![ContentPart::Text { text }],
                }
            });
            return (bodies, true);
        }
        return (bodies, false);
    }
    let task = (tool == "task")
        .then(|| {
            #[derive(Deserialize)]
            struct TaskInput {
                #[serde(default)]
                description: Option<String>,
            }
            TaskInput::deserialize(&input)
                .ok()
                .and_then(|t| t.description)
        })
        .flatten();
    bodies.push(ChatBody::ToolCalled {
        call_id: call_id.clone(),
        name: tool.clone(),
        input: ToolInput::from_value(input),
    });
    if let (true, Some(child)) = (tool == "task", metadata.session_id) {
        bodies.push(ChatBody::SubagentStarted {
            call_id: call_id.clone(),
            child: SessionKey {
                harness: "opencode".into(),
                session_id: child,
                subagent: None,
            },
            task,
        });
    }
    match end {
        Some((ok, text)) => {
            bodies.push(ChatBody::ToolFinished {
                call_id,
                ok,
                output: vec![ContentPart::Text { text }],
            });
            (bodies, true)
        }
        None => (bodies, false),
    }
}

fn store_error(path: &Path, error: impl std::fmt::Display) -> ChatReadError {
    ChatReadError::Store(format!("{}: {error}", path.display()))
}

pub struct StoreTail {
    path: PathBuf,
    conn: Connection,
    session_id: String,
    /// Parts fully emitted, in store order.
    done: usize,
    /// Bodies already emitted for the part at `done`, which is not final.
    held: usize,
    cursor: Option<(Option<i64>, i64)>,
}

impl StoreTail {
    /// The store whose `session` table holds `key`. A subagent key names the
    /// child session, which is a session of its own in the store.
    pub fn open(stores: &[PathBuf], key: &SessionKey) -> Result<Self, ChatReadError> {
        let session_id = key
            .subagent
            .clone()
            .unwrap_or_else(|| key.session_id.clone());
        let mut last_error = None;
        for path in stores {
            let conn = match crate::store_conn::open_read(path) {
                Ok(conn) => conn,
                Err(error) => {
                    last_error = Some(store_error(path, error));
                    continue;
                }
            };
            let found = conn
                .query_row(
                    "SELECT 1 FROM session WHERE id = ?1",
                    params![session_id],
                    |_| Ok(()),
                )
                .optional()
                .map_err(|e| store_error(path, e))?;
            if found.is_some() {
                return Ok(Self {
                    path: path.clone(),
                    conn,
                    session_id,
                    done: 0,
                    held: 0,
                    cursor: None,
                });
            }
        }
        Err(last_error.unwrap_or(ChatReadError::NotFound {
            harness: key.harness.clone(),
            session_id,
        }))
    }

    pub fn poll(&mut self) -> Result<Vec<(SourceRef, Mapped)>, ChatReadError> {
        let cursor = self
            .conn
            .query_row(CURSOR_SQL, params![self.session_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(|e| store_error(&self.path, e))?;
        if self.cursor == Some(cursor) {
            return Ok(Vec::new());
        }
        self.cursor = Some(cursor);
        let mut stmt = self
            .conn
            .prepare(PART_SQL)
            .map_err(|e| store_error(&self.path, e))?;
        let rows = stmt
            .query_map(params![self.session_id, self.done as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })
            .map_err(|e| store_error(&self.path, e))?;
        let mut out = Vec::new();
        for row in rows {
            let (part_id, message_id, message, data, created) =
                row.map_err(|e| store_error(&self.path, e))?;
            let role = serde_json::from_str::<MessageData>(&message)
                .map(|m| m.role)
                .unwrap_or_default();
            let (bodies, done) = match serde_json::from_str::<Part>(&data) {
                Ok(part) => map_part(&part_id, &message_id, &role, part),
                Err(_) => (vec![unknown_part(&data)], true),
            };
            let source = SourceRef::Row { id: part_id };
            let count = bodies.len();
            out.extend(
                bodies
                    .into_iter()
                    .skip(self.held)
                    .map(|body| (source.clone(), Mapped::new(Some(created), body))),
            );
            if !done {
                self.held = self.held.max(count);
                break;
            }
            self.done += 1;
            self.held = 0;
        }
        Ok(out)
    }
}

fn unknown_part(data: &str) -> ChatBody {
    #[derive(Deserialize)]
    struct Kind {
        #[serde(rename = "type", default)]
        kind: String,
    }
    let raw: Value = serde_json::from_str(data).unwrap_or(Value::Null);
    let kind = Kind::deserialize(&raw).map(|k| k.kind).unwrap_or_default();
    ChatBody::Unknown {
        name: format!("part/{kind}"),
        raw,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;
    use crate::chat_events::{ChatRoots, SessionReader};

    fn fixture(path: &Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, parent_id TEXT);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
             INSERT INTO session VALUES ('ses_a', NULL);
             INSERT INTO message VALUES ('msg_1', 'ses_a', 1, 1, '{\"role\":\"user\"}');
             INSERT INTO message VALUES ('msg_2', 'ses_a', 2, 2, '{\"role\":\"assistant\"}');",
        )
        .unwrap();
        let parts = [
            ("prt_01", "msg_1", r#"{"type":"text","text":"fix it"}"#),
            (
                "prt_02",
                "msg_1",
                r#"{"type":"file","mime":"image/png","url":"data:image/png;base64,aGVsbG8=","filename":"a.png"}"#,
            ),
            ("prt_03", "msg_2", r#"{"type":"step-start"}"#),
            (
                "prt_04",
                "msg_2",
                r#"{"type":"reasoning","text":"think","time":{"start":1,"end":2}}"#,
            ),
            (
                "prt_05",
                "msg_2",
                r#"{"type":"tool","tool":"read","callID":"c1","state":{"status":"completed","input":{"filePath":"/a"},"output":"body"}}"#,
            ),
            (
                "prt_06",
                "msg_2",
                r#"{"type":"tool","tool":"task","callID":"c2","state":{"status":"running","input":{"description":"dig"},"metadata":{"sessionId":"ses_child"}}}"#,
            ),
            (
                "prt_07",
                "msg_2",
                r#"{"type":"subtask","prompt":"go","description":"d","agent":"general"}"#,
            ),
            ("prt_08", "msg_2", r#"{"type":"compaction","auto":true}"#),
            (
                "prt_09",
                "msg_2",
                r#"{"type":"text","text":"done","time":{"start":3,"end":4}}"#,
            ),
            (
                "prt_10",
                "msg_2",
                r#"{"type":"step-finish","reason":"stop"}"#,
            ),
        ];
        for (id, message, data) in parts {
            conn.execute(
                "INSERT INTO part VALUES (?1, ?2, 'ses_a', 10, 10, ?3)",
                params![id, message, data],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn chat_events_opencode_store_holds_at_a_running_tool() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let writer = fixture(&db);
        let roots = ChatRoots {
            transcripts: Vec::new(),
            opencode_stores: vec![db],
        };
        let key = SessionKey {
            harness: "opencode".into(),
            session_id: "ses_a".into(),
            subagent: None,
        };
        let mut reader = SessionReader::open(&key, &roots, 0).unwrap();
        let first = reader.poll().unwrap();
        let kinds = |events: &[crate::proto::chat_event::ChatEvent]| {
            events
                .iter()
                .map(|e| match &e.body {
                    ChatBody::UserMessage { .. } => "user",
                    ChatBody::TurnStarted { .. } => "turn",
                    ChatBody::TurnEnded { .. } => "turn_end",
                    ChatBody::ThinkingStarted { .. }
                    | ChatBody::ThinkingDelta { .. }
                    | ChatBody::ThinkingEnded { .. } => "think",
                    ChatBody::TextStarted { .. }
                    | ChatBody::TextDelta { .. }
                    | ChatBody::TextEnded { .. } => "text",
                    ChatBody::ToolCalled { .. } => "call",
                    ChatBody::ToolFinished { .. } => "finish",
                    ChatBody::SubagentStarted { .. } => "subagent",
                    ChatBody::Compacted { .. } => "compacted",
                    other => panic!("unexpected {other:?}"),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            kinds(&first),
            [
                "user", "user", "turn", "think", "think", "think", "call", "finish", "call",
                "subagent"
            ]
        );
        assert!(
            reader.poll().unwrap().is_empty(),
            "an unchanged store emits nothing"
        );

        writer
            .execute(
                "UPDATE part SET time_updated = 11, data = ?1 WHERE id = 'prt_06'",
                params![r#"{"type":"tool","tool":"task","callID":"c2","state":{"status":"completed","input":{"description":"dig"},"output":"found","metadata":{"sessionId":"ses_child"}}}"#],
            )
            .unwrap();
        let second = reader.poll().unwrap();
        assert_eq!(
            kinds(&second),
            [
                "finish",
                "user",
                "compacted",
                "text",
                "text",
                "text",
                "turn_end"
            ]
        );
        assert_eq!(second[0].seq, first.len() as u64);

        let replay = SessionReader::open(&key, &roots, 0)
            .unwrap()
            .poll()
            .unwrap();
        let bodies = |events: Vec<crate::proto::chat_event::ChatEvent>| {
            events
                .into_iter()
                .map(|e| (e.seq, e.body))
                .collect::<Vec<_>>()
        };
        assert_eq!(bodies(replay), bodies([first, second].concat()));
    }
}
