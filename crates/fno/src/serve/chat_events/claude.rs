//! Claude Code JSONL (`<projects>/<proj>/<sid>.jsonl`) as chat
//! events. Claude writes one assistant content block per row, so no
//! streamed-versus-complete de-dup is needed. Rows with no `message` object
//! (`attachment`, `api-request`, `custom-title`, and the other session
//! facts) emit nothing.

use super::{iso_ms, Mapped, RowMapper};
use crate::proto::chat_event::{
    ChatBody, ContentPart, ImageRef, SessionKey, ToolInput, UserOrigin,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A subagent's transcript beside its parent:
/// `<sid>/subagents/agent-<id>.jsonl`. `None` when the id is not
/// path-safe or the file is absent.
pub(crate) fn subagent_path(parent: &Path, agent: &str) -> Option<PathBuf> {
    let safe = !agent.is_empty()
        && agent.len() <= 64
        && agent
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    let path = subagents_dir(parent).join(format!("agent-{agent}.jsonl"));
    (safe && path.is_file()).then_some(path)
}

fn subagents_dir(transcript: &Path) -> PathBuf {
    transcript.with_extension("").join("subagents")
}

/// Session-fact rows that still carry a `message` object: the raw request
/// body Claude logs beside the chat, not a chat turn.
const FACT_ROWS_WITH_MESSAGE: &[&str] = &["api-request-blob"];

#[derive(Deserialize)]
struct Head {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    message: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    #[serde(default)]
    uuid: String,
    timestamp: Option<String>,
    #[serde(default)]
    is_meta: bool,
    #[serde(default)]
    is_compact_summary: bool,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Content,
}

/// The fields of a row that pasted images: a sent message, or a message
/// queued while the agent worked, which is an attachment row and nothing else.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PasteRow {
    timestamp: Option<String>,
    #[serde(default)]
    image_paste_ids: Vec<u64>,
    #[serde(default)]
    message: Option<Message>,
    #[serde(default)]
    attachment: Option<QueuedPaste>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueuedPaste {
    #[serde(default)]
    image_paste_ids: Vec<u64>,
    #[serde(default)]
    prompt: Content,
}

/// The session's turns that pasted images. Images follow `imagePasteIds`,
/// which is not always the order of the markers in the text.
pub(super) fn pasted_turns(transcript: &Path) -> std::io::Result<Vec<super::PastedTurn>> {
    use std::io::BufRead;
    let file = std::io::BufReader::new(std::fs::File::open(transcript)?);
    let mut turns: Vec<super::PastedTurn> = Vec::new();
    for line in file.split(b'\n') {
        let line = line?;
        let line = String::from_utf8_lossy(&line);
        // Most rows paste nothing, and some are megabytes long.
        if !line.contains("\"imagePasteIds\"") {
            continue;
        }
        let Ok(row) = serde_json::from_str::<PasteRow>(&line) else {
            continue;
        };
        let (ids, content) = match (row.message, row.attachment) {
            (Some(message), _) => (row.image_paste_ids, message.content),
            (None, Some(queued)) => (queued.image_paste_ids, queued.prompt),
            (None, None) => continue,
        };
        let Some(turn) = pasted_turn(iso_ms(row.timestamp.as_deref()), &ids, content) else {
            continue;
        };
        // A queued message can be written again when it is sent.
        let repeated = turns
            .iter()
            .any(|seen| seen.text == turn.text && seen.images == turn.images);
        if !repeated {
            turns.push(turn);
        }
    }
    Ok(turns)
}

fn pasted_turn(at_ms: Option<i64>, ids: &[u64], content: Content) -> Option<super::PastedTurn> {
    let Content::Blocks(blocks) = content else {
        return None;
    };
    let mut text = String::new();
    let mut data = Vec::new();
    for block in blocks {
        match Block::deserialize(&block) {
            Ok(Block::Text { text: part }) => {
                text.push_str(&part);
                text.push('\n');
            }
            Ok(Block::Image { source }) => data.push(match source {
                ImageSource::Base64 { data, .. } => Some(data),
                ImageSource::Url { .. } => None,
            }),
            _ => {}
        }
    }
    // With unequal counts no image's number is known.
    if ids.is_empty() || ids.len() != data.len() {
        return None;
    }
    let images = ids
        .iter()
        .zip(data)
        .map(|(&number, base64)| super::PastedImage { number, base64 })
        .collect();
    Some(super::PastedTurn {
        at_ms,
        text,
        images,
    })
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<Value>),
}

impl Default for Content {
    fn default() -> Self {
        Self::Blocks(Vec::new())
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Block {
    Text {
        text: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    RedactedThinking {},
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: Option<Content>,
        #[serde(default)]
        is_error: bool,
    },
    Image {
        source: ImageSource,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

impl ImageSource {
    fn into_ref(self) -> ImageRef {
        match self {
            Self::Base64 { media_type, data } => super::image_from_base64(media_type, data),
            Self::Url { url } => ImageRef::Url { url },
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubagentMeta {
    #[serde(default)]
    tool_use_id: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Call {
    Question,
    Other,
}

pub struct ClaudeMapper {
    subagents: PathBuf,
    session_id: String,
    calls: HashMap<String, Call>,
    /// A `compact_boundary` was read; the next row may carry its summary.
    compact_pending: Option<Option<i64>>,
}

impl ClaudeMapper {
    pub fn new(transcript: &Path) -> Self {
        Self {
            subagents: subagents_dir(transcript),
            session_id: transcript
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string(),
            calls: HashMap::new(),
            compact_pending: None,
        }
    }

    /// The subagent an `Agent` call started: the sibling `.meta.json` whose
    /// `toolUseId` names the call.
    fn find_subagent(&self, call_id: &str) -> Option<(String, Option<String>)> {
        for entry in std::fs::read_dir(&self.subagents).ok()?.flatten() {
            let name = entry.file_name();
            let Some(id) = name
                .to_str()
                .and_then(|n| n.strip_prefix("agent-"))
                .and_then(|n| n.strip_suffix(".meta.json"))
            else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            if let Ok(meta) = serde_json::from_str::<SubagentMeta>(&text) {
                if meta.tool_use_id == call_id {
                    return Some((id.to_string(), meta.description));
                }
            }
        }
        None
    }

    fn user(&mut self, row: Row, at: Option<i64>, out: &mut Vec<Mapped>) {
        let content = row.message.map(|m| m.content).unwrap_or_default();
        let mut parts = Vec::new();
        let blocks = match content {
            Content::Text(text) => {
                parts.push(ContentPart::Text { text });
                Vec::new()
            }
            Content::Blocks(blocks) => blocks,
        };
        for raw in blocks {
            match Block::deserialize(&raw) {
                Ok(Block::Text { text }) => parts.push(ContentPart::Text { text }),
                Ok(Block::Image { source }) => parts.push(ContentPart::Image {
                    image: source.into_ref(),
                }),
                Ok(Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                }) => {
                    let body = if self.calls.get(&tool_use_id) == Some(&Call::Question) {
                        ChatBody::QuestionAnswered {
                            call_id: tool_use_id,
                            answer: content.map(content_value).unwrap_or(Value::Null),
                        }
                    } else {
                        ChatBody::ToolFinished {
                            call_id: tool_use_id,
                            ok: !is_error,
                            output: content.map(content_parts).unwrap_or_default(),
                        }
                    };
                    out.push(Mapped::new(at, body));
                }
                _ => out.push(unknown("user_block", raw, at)),
            }
        }
        if parts.is_empty() {
            return;
        }
        if row.is_compact_summary {
            let summary = parts
                .into_iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            out.push(Mapped::new(
                at,
                ChatBody::Compacted {
                    summary: Some(summary),
                    redacted: false,
                },
            ));
            return;
        }
        let injected = parts
            .iter()
            .any(|p| matches!(p, ContentPart::Text { text } if super::injected_text(text)));
        out.push(Mapped::new(
            at,
            ChatBody::UserMessage {
                id: row.uuid,
                origin: if row.is_meta || injected {
                    UserOrigin::Meta
                } else {
                    UserOrigin::Typed
                },
                parts,
            },
        ));
    }

    /// `None` holds the row: an `Agent` call whose subagent meta file is not
    /// written yet, read as the last complete line.
    fn assistant(&mut self, row: Row, at: Option<i64>, more: bool) -> Option<Vec<Mapped>> {
        let blocks = match row.message.map(|m| m.content) {
            Some(Content::Blocks(blocks)) => blocks,
            Some(Content::Text(text)) => vec![serde_json::json!({"type": "text", "text": text})],
            None => Vec::new(),
        };
        let mut out = Vec::new();
        let mut calls = Vec::new();
        for (index, raw) in blocks.into_iter().enumerate() {
            let part = format!("{}:{index}", row.uuid);
            match Block::deserialize(&raw) {
                Ok(Block::Text { text }) => out.extend([
                    ChatBody::TextStarted { part: part.clone() },
                    ChatBody::TextDelta {
                        part: part.clone(),
                        text,
                    },
                    ChatBody::TextEnded { part },
                ]),
                Ok(Block::Thinking { thinking }) => {
                    out.push(ChatBody::ThinkingStarted {
                        part: part.clone(),
                        redacted: thinking.is_empty(),
                    });
                    if !thinking.is_empty() {
                        out.push(ChatBody::ThinkingDelta {
                            part: part.clone(),
                            text: thinking,
                        });
                    }
                    out.push(ChatBody::ThinkingEnded { part });
                }
                Ok(Block::RedactedThinking {}) => out.extend([
                    ChatBody::ThinkingStarted {
                        part: part.clone(),
                        redacted: true,
                    },
                    ChatBody::ThinkingEnded { part },
                ]),
                Ok(Block::ToolUse { id, name, input }) if name == "AskUserQuestion" => {
                    calls.push((id.clone(), Call::Question));
                    out.push(ChatBody::QuestionAsked {
                        call_id: id,
                        prompt: input,
                    });
                }
                Ok(Block::ToolUse { id, name, input }) => {
                    let subagent = matches!(name.as_str(), "Agent" | "Task");
                    calls.push((id.clone(), Call::Other));
                    let found = subagent.then(|| self.find_subagent(&id)).flatten();
                    if subagent && found.is_none() && !more {
                        return None;
                    }
                    let task = SubagentMeta::deserialize(&input)
                        .ok()
                        .and_then(|m| m.description);
                    out.push(ChatBody::ToolCalled {
                        call_id: id.clone(),
                        name,
                        input: ToolInput::from_value(input),
                    });
                    if let Some((child, description)) = found {
                        out.push(ChatBody::SubagentStarted {
                            call_id: id,
                            child: SessionKey {
                                harness: "claude".into(),
                                session_id: self.session_id.clone(),
                                subagent: Some(child),
                            },
                            task: description.or(task),
                        });
                    }
                }
                Ok(Block::Image { source }) => out.push(ChatBody::UserMessage {
                    id: part,
                    origin: UserOrigin::Meta,
                    parts: vec![ContentPart::Image {
                        image: source.into_ref(),
                    }],
                }),
                _ => out.push(unknown("assistant_block", raw, at).body),
            }
        }
        self.calls.extend(calls);
        Some(out.into_iter().map(|b| Mapped::new(at, b)).collect())
    }
}

fn unknown(scope: &str, raw: Value, at: Option<i64>) -> Mapped {
    #[derive(Deserialize)]
    struct Kind {
        #[serde(rename = "type", default)]
        kind: String,
    }
    let kind = Kind::deserialize(&raw).map(|k| k.kind).unwrap_or_default();
    Mapped::new(
        at,
        ChatBody::Unknown {
            name: format!("{scope}/{kind}"),
            raw,
        },
    )
}

fn content_value(content: Content) -> Value {
    match content {
        Content::Text(text) => Value::String(text),
        Content::Blocks(blocks) => Value::Array(blocks),
    }
}

fn content_parts(content: Content) -> Vec<ContentPart> {
    match content {
        Content::Text(text) => vec![ContentPart::Text { text }],
        Content::Blocks(blocks) => blocks
            .into_iter()
            .map(|raw| match Block::deserialize(&raw) {
                Ok(Block::Text { text }) => ContentPart::Text { text },
                Ok(Block::Image { source }) => ContentPart::Image {
                    image: source.into_ref(),
                },
                _ => ContentPart::Text {
                    text: raw.to_string(),
                },
            })
            .collect(),
    }
}

impl RowMapper for ClaudeMapper {
    fn map(&mut self, line: &str, more: bool) -> Option<Vec<Mapped>> {
        let Ok(raw) = serde_json::from_str::<Value>(line) else {
            return Some(Vec::new());
        };
        let Ok(head) = Head::deserialize(&raw) else {
            return Some(Vec::new());
        };
        let is_boundary = head.kind == "system";
        let row = Row::deserialize(&raw).ok();
        let at = row.as_ref().and_then(|r| iso_ms(r.timestamp.as_deref()));
        // A boundary waits for the row after it, which may carry the summary.
        if is_boundary
            && row.as_ref().and_then(|r| r.subtype.as_deref()) == Some("compact_boundary")
        {
            if !more {
                return None;
            }
            self.compact_pending = Some(at);
            return Some(Vec::new());
        }
        let mut out = Vec::new();
        // Read, not taken: a held assistant row below must leave it pending.
        if let Some(pending_at) = self.compact_pending {
            if !row.as_ref().is_some_and(|r| r.is_compact_summary) {
                out.push(Mapped::new(
                    pending_at,
                    ChatBody::Compacted {
                        summary: None,
                        redacted: false,
                    },
                ));
            }
        }
        match (head.kind.as_str(), row) {
            ("user", Some(row)) => self.user(row, at, &mut out),
            ("assistant", Some(row)) => out.extend(self.assistant(row, at, more)?),
            ("system", _) => {}
            (kind, _) if head.message.is_none() || FACT_ROWS_WITH_MESSAGE.contains(&kind) => {}
            (kind, _) => out.push(Mapped::new(
                at,
                ChatBody::Unknown {
                    name: kind.to_string(),
                    raw,
                },
            )),
        }
        self.compact_pending = None;
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;
    use crate::chat_events::SessionReader;

    #[test]
    fn chat_events_claude_fixture_maps_every_kind_in_file_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sid-1.jsonl");
        let subagents = dir.path().join("sid-1").join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::write(
            subagents.join("agent-abc123.meta.json"),
            r#"{"agentType":"x","description":"look around","toolUseId":"toolu_agent"}"#,
        )
        .unwrap();
        let rows = [
            r#"{"type":"user","uuid":"u1","timestamp":"2026-11-11T10:00:00.000Z","message":{"role":"user","content":"fix it"}}"#,
            r#"{"type":"user","uuid":"u2","isMeta":true,"message":{"role":"user","content":[{"type":"text","text":"<system-reminder>x</system-reminder>"}]}}"#,
            r#"{"type":"attachment","uuid":"x1","attachment":{"type":"skill_listing"}}"#,
            r#"{"type":"assistant","uuid":"a1","message":{"id":"m1","content":[{"type":"thinking","thinking":"","signature":"sig"}]}}"#,
            r#"{"type":"assistant","uuid":"a2","message":{"id":"m1","content":[{"type":"thinking","thinking":"plan","signature":"sig"}]}}"#,
            r#"{"type":"assistant","uuid":"a3","message":{"id":"m1","content":[{"type":"text","text":"on it"}]}}"#,
            r#"{"type":"assistant","uuid":"a4","message":{"id":"m1","content":[{"type":"tool_use","id":"toolu_bash","name":"Bash","input":{"command":"ls"}}]}}"#,
            r#"{"type":"user","uuid":"u3","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_bash","content":"a.txt","is_error":false}]}}"#,
            r#"{"type":"assistant","uuid":"a5","message":{"id":"m2","content":[{"type":"tool_use","id":"toolu_q","name":"AskUserQuestion","input":{"questions":[{"question":"Which?"}]}}]}}"#,
            r#"{"type":"user","uuid":"u4","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_q","content":"User answered: A"}]}}"#,
            r#"{"type":"assistant","uuid":"a6","message":{"id":"m3","content":[{"type":"tool_use","id":"toolu_agent","name":"Agent","input":{"description":"look","prompt":"go"}}]}}"#,
            r#"{"type":"user","uuid":"u5","message":{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"aGVsbG8="}}]}}"#,
            r#"{"type":"system","uuid":"s1","subtype":"compact_boundary","timestamp":"2026-11-11T11:00:00Z"}"#,
            r#"{"type":"user","uuid":"u6","isCompactSummary":true,"message":{"role":"user","content":"Summary of the chat"}}"#,
        ];
        std::fs::write(&path, rows.join("\n") + "\n").unwrap();
        let events = SessionReader::lines(path.clone(), Box::new(ClaudeMapper::new(&path)), 0)
            .poll()
            .unwrap();
        let kinds: Vec<&str> = events
            .iter()
            .map(|e| match &e.body {
                ChatBody::UserMessage {
                    origin: UserOrigin::Typed,
                    ..
                } => "user",
                ChatBody::UserMessage {
                    origin: UserOrigin::Meta,
                    ..
                } => "meta",
                ChatBody::ThinkingStarted { redacted: true, .. } => "think_redacted",
                ChatBody::ThinkingStarted { .. } => "think",
                ChatBody::ThinkingDelta { .. } => "think_delta",
                ChatBody::ThinkingEnded { .. } => "think_end",
                ChatBody::TextStarted { .. } => "text",
                ChatBody::TextDelta { .. } => "text_delta",
                ChatBody::TextEnded { .. } => "text_end",
                ChatBody::ToolCalled { .. } => "tool",
                ChatBody::ToolFinished { ok: true, .. } => "tool_ok",
                ChatBody::QuestionAsked { .. } => "ask",
                ChatBody::QuestionAnswered { .. } => "answer",
                ChatBody::SubagentStarted { .. } => "subagent",
                ChatBody::Compacted {
                    summary: Some(_), ..
                } => "compacted",
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "user",
                "meta",
                "think_redacted",
                "think_end",
                "think",
                "think_delta",
                "think_end",
                "text",
                "text_delta",
                "text_end",
                "tool",
                "tool_ok",
                "ask",
                "answer",
                "tool",
                "subagent",
                "user",
                "compacted",
            ]
        );
        assert_eq!(events[0].at_ms, Some(1_794_391_200_000));
        let ChatBody::SubagentStarted { child, task, .. } = &events[15].body else {
            unreachable!()
        };
        assert_eq!(child.subagent.as_deref(), Some("abc123"));
        assert_eq!(task.as_deref(), Some("look around"));
        let ChatBody::UserMessage { parts, .. } = &events[16].body else {
            unreachable!()
        };
        assert!(matches!(
            &parts[0],
            ContentPart::Image { image: ImageRef::Inline { bytes_len: 5, sha256, .. } }
                if sha256.starts_with("2cf24dba")
        ));
        let ChatBody::Compacted { summary, .. } = &events[17].body else {
            unreachable!()
        };
        assert_eq!(summary.as_deref(), Some("Summary of the chat"));

        // A held row changes no state: the pending compaction survives the
        // hold and lands on the retry.
        let mut mapper = ClaudeMapper::new(&dir.path().join("other.jsonl"));
        let boundary = r#"{"type":"system","uuid":"s1","subtype":"compact_boundary"}"#;
        let agent = r#"{"type":"assistant","uuid":"a1","message":{"id":"m","content":[{"type":"tool_use","id":"toolu_none","name":"Agent","input":{}}]}}"#;
        assert_eq!(mapper.map(boundary, true), Some(Vec::new()));
        assert_eq!(mapper.map(agent, false), None);
        let retried = mapper.map(agent, true).unwrap();
        assert!(matches!(
            retried[0].body,
            ChatBody::Compacted { summary: None, .. }
        ));
        assert!(matches!(retried[1].body, ChatBody::ToolCalled { .. }));
    }
}
