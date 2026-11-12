//! The opencode sqlite store as chat events. opencode rewrites a part row in
//! place while it streams, so the reader emits parts in store order and stops
//! at the first part that is not final: a text or reasoning part with no
//! `time.end`, or a tool that is still pending or running. A running tool
//! emits its call once and holds the watermark until it completes, so a live
//! tail and a replay emit the same sequence.
//!
//! opencode 2.x rewrote the store: `session_v2`/`session_message`/
//! `session_part` replace `session`/`message`/`part`, user text moved onto
//! the message row, parts became assistant-content only, and a tool's output
//! moved into its state's content list. The reader picks the family per
//! store (`session_v2` presence wins) and maps both to the same bodies.

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

/// The 2.x store as one ordered row stream: message rows (`kind` 0) carry
/// user text, turn markers, compaction status and the idle outcome; part
/// rows (`kind` 1) carry assistant content under their message. The message
/// `seq` is the durable write order, a part's `ordinal` its slot in the
/// message, so `(seq, kind, ordinal)` replays exactly what the writer saw.
const V2_ROW_SQL: &str = "SELECT 0, m.seq, m.id, m.type, m.time_created, m.data, \
     NULL, NULL, NULL, NULL FROM session_message m WHERE m.session_id = ?1 \
     UNION ALL \
     SELECT 1, msg.seq, p.message_id, msg.type, msg.time_created, msg.data, \
     p.ordinal, p.type, p.data, p.time_created \
     FROM session_part p JOIN session_message msg ON msg.id = p.message_id \
     WHERE p.session_id = ?1 \
     ORDER BY 2, 1, 7 LIMIT -1 OFFSET ?2";

/// Message `time_updated`, then row counts on both tables: any write to
/// either advances the cursor.
const V2_CURSOR_SQL: &str = "SELECT \
     (SELECT max(time_updated) FROM session_message WHERE session_id = ?1), \
     (SELECT count(*) FROM session_message WHERE session_id = ?1), \
     (SELECT count(*) FROM session_part WHERE session_id = ?1)";

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

// --- the 2.x store shapes -------------------------------------------------
// The message `type` lives in a COLUMN, not in the data JSON (the store
// strips it), so each kind parses its own untagged struct. Part data keeps
// its `type` tag.

#[derive(Deserialize)]
struct V2MsgTime {
    #[serde(default)]
    completed: Option<i64>,
}

#[derive(Deserialize)]
struct V2UserData {
    #[serde(default)]
    text: String,
    #[serde(default)]
    files: Option<Value>,
}

#[derive(Deserialize)]
struct V2TextData {
    #[serde(default)]
    text: String,
}

/// The 2.x assistant message: its completion time gates its text, and its
/// content list carries the reasoning/text/tool items. Real stores keep the
/// items inline in the message row; the session_part leg exists for the
/// schema shape.
#[derive(Deserialize)]
struct V2AssistantMsg {
    #[serde(default)]
    time: Option<V2MsgTime>,
    #[serde(default)]
    content: Vec<Value>,
}

#[derive(Deserialize)]
struct V2CompactionData {
    status: String,
    #[serde(default)]
    summary: Option<String>,
}

#[derive(Deserialize)]
struct V2IdleData {
    outcome: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum V2Part {
    Text {
        #[serde(default)]
        text: String,
    },
    Reasoning {
        #[serde(default)]
        text: String,
        #[serde(default)]
        time: Option<V2ReasoningTime>,
    },
    Tool {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
        state: V2ToolState,
    },
}

#[derive(Deserialize)]
struct V2ReasoningTime {
    #[serde(default)]
    completed: Option<i64>,
}

/// One entry of a completed tool's content list, where 2.x moved the output
/// (1.x kept a flat `output` string).
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum V2ToolContent {
    Text {
        #[serde(default)]
        text: String,
    },
    File {
        #[serde(default)]
        name: Option<String>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum V2ToolState {
    /// The input arrives as a partial JSON STRING while streaming.
    Streaming {
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
        content: Vec<V2ToolContent>,
        #[serde(default)]
        metadata: Option<ToolMeta>,
    },
    Error {
        #[serde(default)]
        input: Value,
        #[serde(default)]
        error: Value,
        #[serde(default)]
        content: Vec<V2ToolContent>,
        #[serde(default)]
        metadata: Option<ToolMeta>,
    },
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

/// Which store family a connection reads. The session's own probe decides:
/// `session_v2` first, then the 1.x `session` table, so a migrated store
/// carrying both families serves a session from whichever holds it.
#[derive(Clone, Copy, PartialEq)]
enum StoreFamily {
    V1,
    V2,
}

/// One stream row: a message (`part` columns all `None`) or a part.
struct V2Row {
    message_id: String,
    message_type: String,
    message_data: String,
    message_created: i64,
    part_ordinal: Option<i64>,
    part_type: Option<String>,
    part_data: Option<String>,
    part_created: Option<i64>,
}

/// Map one 2.x stream row to bodies plus finality, mirroring `map_part`'s
/// contract: a row that is not final maps to a prefix of what it maps to
/// once final. `assistant_done` (the parent message's `time.completed`)
/// gates text parts, whose own row never carries a completion.
fn map_v2_row(row: &V2Row) -> (Vec<ChatBody>, bool) {
    if row.part_type.is_none() {
        return map_v2_message(row);
    }
    let (Some(ordinal), Some(_part_type), Some(part_data), Some(_part_created)) = (
        row.part_ordinal,
        row.part_type.as_deref(),
        row.part_data.as_deref(),
        row.part_created,
    ) else {
        return (Vec::new(), true);
    };
    let Ok(part) = serde_json::from_str::<V2Part>(part_data) else {
        return (vec![unknown_part(part_data)], true);
    };
    let parent_done = parent_completed(row);
    map_v2_part(&row.message_id, ordinal, part, parent_done)
}

/// Map one parsed 2.x content item (a session_part row's data, or an entry
/// of an assistant message's inline content list) to bodies plus finality,
/// mirroring `map_part`'s contract: a part that is not final maps to a
/// prefix of what it maps to once final. `parent_done` gates text, whose
/// own row never carries a completion, and closes out a tool the parent
/// outlived: the turn ended, so a tool still streaming or running ended
/// with it, as an error with whatever output it had.
fn map_v2_part(
    message_id: &str,
    ordinal: i64,
    part: V2Part,
    parent_done: bool,
) -> (Vec<ChatBody>, bool) {
    let part = match part {
        V2Part::Tool {
            id,
            name,
            state: V2ToolState::Streaming { input },
        } if parent_done => V2Part::Tool {
            id,
            name,
            state: V2ToolState::Error {
                input,
                error: Value::Null,
                content: Vec::new(),
                metadata: None,
            },
        },
        V2Part::Tool {
            id,
            name,
            state: V2ToolState::Running { input, metadata },
        } if parent_done => V2Part::Tool {
            id,
            name,
            state: V2ToolState::Error {
                input,
                error: Value::Null,
                content: Vec::new(),
                metadata,
            },
        },
        other => other,
    };
    let part_ref = format!("{message_id}:{ordinal}");
    match part {
        V2Part::Text { text } => {
            // A text part has no time of its own; the parent assistant
            // message completing is the write that ends it.
            let bodies = vec![
                ChatBody::TextStarted {
                    part: part_ref.clone(),
                },
                ChatBody::TextDelta {
                    part: part_ref.clone(),
                    text,
                },
                ChatBody::TextEnded { part: part_ref },
            ];
            (if parent_done { bodies } else { Vec::new() }, parent_done)
        }
        V2Part::Reasoning { text, time } => {
            let done = time.as_ref().is_none_or(|t| t.completed.is_some());
            let mut bodies = vec![ChatBody::ThinkingStarted {
                part: part_ref.clone(),
                redacted: text.is_empty(),
            }];
            if !text.is_empty() {
                bodies.push(ChatBody::ThinkingDelta {
                    part: part_ref.clone(),
                    text,
                });
            }
            bodies.push(ChatBody::ThinkingEnded { part: part_ref });
            (if done { bodies } else { Vec::new() }, done)
        }
        V2Part::Tool { id, name, state } => map_v2_tool(&id, &name, state),
    }
}

fn parent_completed(row: &V2Row) -> bool {
    serde_json::from_str::<V2AssistantMsg>(&row.message_data)
        .ok()
        .and_then(|d| d.time)
        .is_some_and(|t| t.completed.is_some())
}

fn v2_content_text(content: &[V2ToolContent]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for item in content {
        match item {
            V2ToolContent::Text { text } => parts.push(text.clone()),
            V2ToolContent::File { name } => {
                if let Some(name) = name {
                    parts.push(format!("[file: {name}]"));
                }
            }
        }
    }
    parts.join("\n")
}

/// Map one 2.x tool part, mirroring `map_tool`'s `question`/`task`
/// special cases: a question's answers and a task's child session ride the
/// state metadata in both stores.
fn map_v2_tool(call_id: &str, name: &str, state: V2ToolState) -> (Vec<ChatBody>, bool) {
    let (input, metadata, end) = match state {
        V2ToolState::Streaming { input }
        | V2ToolState::Running {
            input,
            metadata: None,
        } => (input, ToolMeta::default(), None),
        V2ToolState::Running {
            input,
            metadata: Some(metadata),
        } => (input, metadata, None),
        V2ToolState::Completed {
            input,
            content,
            metadata,
        } => {
            let text = v2_content_text(&content);
            (input, metadata.unwrap_or_default(), Some((true, text)))
        }
        V2ToolState::Error {
            input,
            error,
            content,
            metadata,
        } => {
            let text = if error.is_null() {
                v2_content_text(&content)
            } else {
                value_text(&error)
            };
            (input, metadata.unwrap_or_default(), Some((false, text)))
        }
    };
    let mut bodies: Vec<ChatBody> = Vec::new();
    if name == "question" {
        bodies.push(ChatBody::QuestionAsked {
            call_id: call_id.to_string(),
            prompt: input,
        });
        if let Some((ok, text)) = end {
            bodies.push(if ok {
                ChatBody::QuestionAnswered {
                    call_id: call_id.to_string(),
                    answer: metadata.answers.unwrap_or(Value::String(text)),
                }
            } else {
                ChatBody::ToolFinished {
                    call_id: call_id.to_string(),
                    ok,
                    output: vec![ContentPart::Text { text }],
                }
            });
            return (bodies, true);
        }
        return (bodies, false);
    }
    let task = (name == "task")
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
        call_id: call_id.to_string(),
        name: name.to_string(),
        input: ToolInput::from_value(input),
    });
    if let (true, Some(child)) = (name == "task", metadata.session_id) {
        bodies.push(ChatBody::SubagentStarted {
            call_id: call_id.to_string(),
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
                call_id: call_id.to_string(),
                ok,
                output: vec![ContentPart::Text { text }],
            });
            (bodies, true)
        }
        None => (bodies, false),
    }
}

/// A JSON value as legible text: a string passes through, anything else
/// serializes compact.
fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Map one 2.x MESSAGE row (no part columns) to bodies plus finality.
fn map_v2_message(row: &V2Row) -> (Vec<ChatBody>, bool) {
    match row.message_type.as_str() {
        "user" => {
            let Ok(data) = serde_json::from_str::<V2UserData>(&row.message_data) else {
                return (Vec::new(), true);
            };
            let origin = if injected_text(&data.text) {
                UserOrigin::Meta
            } else {
                UserOrigin::Typed
            };
            let mut parts = vec![ContentPart::Text { text: data.text }];
            if let Some(files) = data.files.as_ref().and_then(Value::as_array) {
                for file in files {
                    let Ok(file) = serde_json::from_value::<V2FileAttachment>(file.clone()) else {
                        continue;
                    };
                    if file.mime.starts_with("image/") && !file.data.is_empty() {
                        let image = ImageRef::from_base64(file.mime, file.data);
                        parts.push(ContentPart::Image { image });
                    } else {
                        parts.push(ContentPart::File {
                            path: file.name.unwrap_or_else(|| file.mime.clone()),
                        });
                    }
                }
            }
            (
                vec![ChatBody::UserMessage {
                    id: row.message_id.clone(),
                    origin,
                    parts,
                }],
                true,
            )
        }
        "synthetic" => {
            let Ok(data) = serde_json::from_str::<V2TextData>(&row.message_data) else {
                return (Vec::new(), true);
            };
            (
                vec![ChatBody::UserMessage {
                    id: row.message_id.clone(),
                    origin: UserOrigin::Meta,
                    parts: vec![ContentPart::Text { text: data.text }],
                }],
                true,
            )
        }
        "assistant" => {
            let Ok(msg) = serde_json::from_str::<V2AssistantMsg>(&row.message_data) else {
                return (
                    vec![ChatBody::TurnStarted {
                        turn_id: Some(row.message_id.clone()),
                    }],
                    true,
                );
            };
            let done = msg.time.as_ref().is_some_and(|t| t.completed.is_some());
            let mut bodies = vec![ChatBody::TurnStarted {
                turn_id: Some(row.message_id.clone()),
            }];
            // The message row carries the whole turn: map each content item
            // in order and hold there while the newest item is still open.
            for (i, item) in msg.content.iter().enumerate() {
                let Ok(part) = serde_json::from_value::<V2Part>(item.clone()) else {
                    continue;
                };
                let (part_bodies, part_done) = map_v2_part(&row.message_id, i as i64, part, done);
                bodies.extend(part_bodies);
                if !part_done {
                    return (bodies, false);
                }
            }
            (bodies, true)
        }
        "compaction" => {
            let Ok(data) = serde_json::from_str::<V2CompactionData>(&row.message_data) else {
                return (Vec::new(), true);
            };
            match data.status.as_str() {
                "running" => (Vec::new(), false),
                "completed" => (
                    vec![ChatBody::Compacted {
                        summary: data.summary,
                        redacted: false,
                    }],
                    true,
                ),
                _ => (Vec::new(), true),
            }
        }
        "idle" => {
            let Ok(data) = serde_json::from_str::<V2IdleData>(&row.message_data) else {
                return (Vec::new(), true);
            };
            let outcome = match data.outcome.as_str() {
                "succeeded" => TurnOutcome::Done,
                "failed" => TurnOutcome::Error {
                    message: String::new(),
                },
                "interrupted" => TurnOutcome::Aborted,
                // An outcome the reader predates still ended the turn.
                _ => TurnOutcome::Done,
            };
            (
                vec![ChatBody::TurnEnded {
                    turn_id: None,
                    outcome,
                }],
                true,
            )
        }
        // system, agent/model/location-switched, skill, shell, and anything
        // unknown: not renderable, never a hold.
        _ => (Vec::new(), true),
    }
}

#[derive(Deserialize)]
struct V2FileAttachment {
    #[serde(default)]
    data: String,
    #[serde(default)]
    mime: String,
    #[serde(default)]
    name: Option<String>,
}

/// The watermark read, per family: v1 reads the part table, v2 reads both
/// tables' shapes.
#[derive(Clone, Copy, PartialEq)]
enum Cursor {
    V1(Option<i64>, i64),
    V2(Option<i64>, i64, i64),
}

pub struct StoreTail {
    path: PathBuf,
    conn: Connection,
    family: StoreFamily,
    session_id: String,
    /// Rows fully emitted, in store order.
    done: usize,
    /// Bodies already emitted for the row at `done`, which is not final.
    held: usize,
    cursor: Option<Cursor>,
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
            // The session names its own family: `session_v2` first, then the
            // 1.x `session` table, so a migrated store carrying both serves
            // a session from whichever family actually holds it.
            let in_v2 = conn
                .query_row(
                    "SELECT 1 FROM session_v2 WHERE id = ?1",
                    params![session_id],
                    |_| Ok(()),
                )
                .optional()
                .map_err(|e| store_error(path, e))?;
            let (found, family) = if in_v2.is_some() {
                (in_v2, StoreFamily::V2)
            } else {
                let in_v1 = conn
                    .query_row(
                        "SELECT 1 FROM session WHERE id = ?1",
                        params![session_id],
                        |_| Ok(()),
                    )
                    .optional()
                    .map_err(|e| store_error(path, e))?;
                (in_v1, StoreFamily::V1)
            };
            if found.is_some() {
                return Ok(Self {
                    path: path.clone(),
                    conn,
                    family,
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
        let cursor = self.read_cursor()?;
        if self.cursor == Some(cursor) {
            return Ok(Vec::new());
        }
        self.cursor = Some(cursor);
        match self.family {
            StoreFamily::V1 => self.poll_v1(),
            StoreFamily::V2 => self.poll_v2(),
        }
    }

    fn read_cursor(&self) -> Result<Cursor, ChatReadError> {
        Ok(match self.family {
            StoreFamily::V1 => {
                let (a, b) = self
                    .conn
                    .query_row(CURSOR_SQL, params![self.session_id], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })
                    .map_err(|e| store_error(&self.path, e))?;
                Cursor::V1(a, b)
            }
            StoreFamily::V2 => {
                let (a, b, c) = self
                    .conn
                    .query_row(V2_CURSOR_SQL, params![self.session_id], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })
                    .map_err(|e| store_error(&self.path, e))?;
                Cursor::V2(a, b, c)
            }
        })
    }

    fn poll_v1(&mut self) -> Result<Vec<(SourceRef, Mapped)>, ChatReadError> {
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

    fn poll_v2(&mut self) -> Result<Vec<(SourceRef, Mapped)>, ChatReadError> {
        let mut stmt = self
            .conn
            .prepare(V2_ROW_SQL)
            .map_err(|e| store_error(&self.path, e))?;
        let rows = stmt
            .query_map(params![self.session_id, self.done as i64], |r| {
                Ok(V2Row {
                    message_id: r.get(2)?,
                    message_type: r.get(3)?,
                    message_created: r.get(4)?,
                    message_data: r.get(5)?,
                    part_ordinal: r.get(6)?,
                    part_type: r.get(7)?,
                    part_data: r.get(8)?,
                    part_created: r.get(9)?,
                })
            })
            .map_err(|e| store_error(&self.path, e))?;
        let mut out = Vec::new();
        for row in rows {
            let row = row.map_err(|e| store_error(&self.path, e))?;
            let (bodies, done) = map_v2_row(&row);
            let created = row.part_created.unwrap_or(row.message_created);
            let id = row
                .part_type
                .is_some()
                .then(|| {
                    format!(
                        "{}:{}",
                        row.message_id,
                        row.part_ordinal.unwrap_or_default()
                    )
                })
                .unwrap_or_else(|| row.message_id.clone());
            let count = bodies.len();
            out.extend(bodies.into_iter().skip(self.held).map(|body| {
                (
                    SourceRef::Row { id: id.clone() },
                    Mapped::new(Some(created), body),
                )
            }));
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

    /// The 2.x positive control: a session_v2 family store must select the
    /// v2 reader, map messages and parts to the same bodies as 1.x, hold at
    /// a streaming text part until its parent message completes, and replay
    /// identically from scratch.
    #[test]
    fn chat_events_opencode_v2_store_holds_at_streaming_text() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE session_v2 (id TEXT PRIMARY KEY, parent_id TEXT);
             CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT, seq INTEGER, time_created INTEGER, time_updated INTEGER, data TEXT);
             CREATE TABLE session_part (message_id TEXT, session_id TEXT, ordinal INTEGER, type TEXT, data TEXT, time_created INTEGER, PRIMARY KEY (message_id, ordinal));",
        )
        .unwrap();
        conn.execute("INSERT INTO session_v2 VALUES ('ses_v2a', NULL)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES \
             ('m1', 'ses_v2a', 'user', 1, 1, 1, '{\"text\":\"fix it\"}')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES \
             ('m2', 'ses_v2a', 'assistant', 2, 2, 2, '{\"time\":{\"created\":2}}')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO session_part (message_id, session_id, ordinal, type, data, time_created) VALUES \
             ('m2', 'ses_v2a', 0, 'reasoning', '{\"type\":\"reasoning\",\"text\":\"think\",\"time\":{\"created\":2,\"completed\":3}}', 2)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO session_part (message_id, session_id, ordinal, type, data, time_created) VALUES \
             ('m2', 'ses_v2a', 1, 'tool', '{\"type\":\"tool\",\"id\":\"c1\",\"name\":\"read\",\"state\":{\"status\":\"completed\",\"input\":{\"filePath\":\"/a\"},\"content\":[{\"type\":\"text\",\"text\":\"body\"}]}}', 2)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO session_part (message_id, session_id, type, data, time_created) VALUES \
             ('m2', 'ses_v2a', 2, 'text', '{\"type\":\"text\",\"text\":\"partial\"}', 2)",
            [],
        )
        .unwrap();
        let roots = ChatRoots {
            transcripts: Vec::new(),
            opencode_stores: vec![db.clone()],
        };
        let key = SessionKey {
            harness: "opencode".into(),
            session_id: "ses_v2a".into(),
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
                    ChatBody::Compacted { .. } => "compacted",
                    other => panic!("unexpected {other:?}"),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            kinds(&first),
            ["user", "turn", "think", "think", "think", "call", "finish"]
        );
        assert!(
            reader.poll().unwrap().is_empty(),
            "an unchanged store emits nothing"
        );
        let writer = Connection::open(&db).unwrap();
        writer
            .execute(
                "UPDATE session_message SET time_updated = 9, data = ?1 WHERE id = 'm2'",
                params![r#"{"time":{"created":2,"completed":8}}"#],
            )
            .unwrap();
        let second = reader.poll().unwrap();
        assert_eq!(kinds(&second), ["text", "text", "text"]);
        writer
            .execute(
                "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES \
                 ('m3', 'ses_v2a', 'idle', 3, 3, 3, '{\"outcome\":\"succeeded\"}')",
                [],
            )
            .unwrap();
        let third = reader.poll().unwrap();
        assert_eq!(kinds(&third), ["turn_end"]);
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
        assert_eq!(
            bodies(replay),
            bodies([first, second, third].concat()),
            "the live tail and a fresh replay agree"
        );
    }

    /// The 2.x inline-content positive control: real stores keep an
    /// assistant turn's reasoning, tool and text items on the message row
    /// itself. The same hold, completion and replay contract as the
    /// session_part control above.
    #[test]
    fn chat_events_opencode_v2_inline_content_holds_until_parent_completes() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE session_v2 (id TEXT PRIMARY KEY, parent_id TEXT);
             CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT, seq INTEGER, time_created INTEGER, time_updated INTEGER, data TEXT);
             CREATE TABLE session_part (message_id TEXT, session_id TEXT, ordinal INTEGER, type TEXT, data TEXT, time_created INTEGER, PRIMARY KEY (message_id, ordinal));",
        )
        .unwrap();
        conn.execute("INSERT INTO session_v2 VALUES ('ses_v2b', NULL)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES \
             ('m1', 'ses_v2b', 'user', 1, 1, 1, '{\"text\":\"run it\"}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES \
             ('m2', 'ses_v2b', 'assistant', 2, 2, 2, ?1)",
            params![
                r#"{"time":{"created":2},"content":[
                     {"type":"reasoning","text":"think","time":{"created":2,"completed":3}},
                     {"type":"tool","id":"c1","name":"read","state":{"status":"completed","input":{"filePath":"/a"},"content":[{"type":"text","text":"body"}]}},
                     {"type":"text","text":"partial"}]}"#
            ],
        )
        .unwrap();
        let roots = ChatRoots {
            transcripts: Vec::new(),
            opencode_stores: vec![db.clone()],
        };
        let key = SessionKey {
            harness: "opencode".into(),
            session_id: "ses_v2b".into(),
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
                    ChatBody::Compacted { .. } => "compacted",
                    other => panic!("unexpected {other:?}"),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            kinds(&first),
            ["user", "turn", "think", "think", "think", "call", "finish"],
            "the finished items emit and the open text holds"
        );
        assert!(
            reader.poll().unwrap().is_empty(),
            "an unchanged store emits nothing"
        );
        let writer = Connection::open(&db).unwrap();
        writer
            .execute(
                "UPDATE session_message SET time_updated = 9, data = ?1 WHERE id = 'm2'",
                params![
                    r#"{"time":{"created":2,"completed":8},"content":[
                         {"type":"reasoning","text":"think","time":{"created":2,"completed":3}},
                         {"type":"tool","id":"c1","name":"read","state":{"status":"completed","input":{"filePath":"/a"},"content":[{"type":"text","text":"body"}]}},
                         {"type":"text","text":"full"}]}"#
                ],
            )
            .unwrap();
        let second = reader.poll().unwrap();
        assert_eq!(kinds(&second), ["text", "text", "text"]);
        writer
            .execute(
                "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES \
                 ('m3', 'ses_v2b', 'idle', 3, 3, 3, '{\"outcome\":\"succeeded\"}')",
                [],
            )
            .unwrap();
        let third = reader.poll().unwrap();
        assert_eq!(kinds(&third), ["turn_end"]);
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
        assert_eq!(
            bodies(replay),
            bodies([first, second, third].concat()),
            "the live tail and a fresh replay agree"
        );
    }
}
