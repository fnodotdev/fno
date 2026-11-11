//! One chat event shape for every harness transcript (Claude JSONL, Codex
//! rollouts, the opencode store). A reader in `crate::chat_events` maps each
//! source row to [`ChatBody`] values and stamps the per-session `seq`; the mux
//! serves them on a [`super::ClientMsg::ChatTail`] connection.
//!
//! Text and thinking follow opencode's started, delta, ended split, so a live
//! tail and a replay emit the same events. A replay emits one delta that
//! carries the full text.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

/// Largest decoded image that rides the stream as base64. A bigger image
/// carries its hash and size only.
pub const CHAT_IMAGE_INLINE_MAX: usize = 512 * 1024;

/// One chat stream: a harness session, or one subagent inside it. A subagent
/// is its own stream, so two files never interleave in one `seq`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey {
    pub harness: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<String>,
}

/// Where in the source an event came from: the byte offset of its line, or
/// the store row id.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceRef {
    Line { offset: u64 },
    Row { id: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ChatEvent {
    /// Gap-free from 0 per stream. A pure function of the source bytes, so a
    /// reconnect re-reads the source and skips `seq < from_seq`.
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<i64>,
    pub source: SourceRef,
    #[serde(deserialize_with = "body_or_unknown")]
    pub body: ChatBody,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UserOrigin {
    /// A person typed it.
    Typed,
    /// Harness-injected text: meta rows, hook output, AGENTS.md context.
    Meta,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatBody {
    UserMessage {
        id: String,
        origin: UserOrigin,
        parts: Vec<ContentPart>,
    },
    TextStarted {
        part: String,
    },
    TextDelta {
        part: String,
        text: String,
    },
    TextEnded {
        part: String,
    },
    ThinkingStarted {
        part: String,
        redacted: bool,
    },
    ThinkingDelta {
        part: String,
        text: String,
    },
    ThinkingEnded {
        part: String,
    },
    ToolCalled {
        call_id: String,
        name: String,
        input: ToolInput,
    },
    ToolFinished {
        call_id: String,
        ok: bool,
        output: Vec<ContentPart>,
    },
    QuestionAsked {
        call_id: String,
        prompt: Value,
    },
    QuestionAnswered {
        call_id: String,
        answer: Value,
    },
    SubagentStarted {
        call_id: String,
        child: SessionKey,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<String>,
    },
    Compacted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        redacted: bool,
    },
    TurnStarted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
    },
    TurnEnded {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        outcome: TurnOutcome,
    },
    /// A source row, or a wire body from a newer server, this build has no
    /// variant for. Kept, never dropped, so the UI shows it as unknown.
    Unknown {
        name: String,
        raw: Value,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    Image { image: ImageRef },
    File { path: String },
}

/// Claude and opencode carry a tool input as an object; Codex carries a JSON
/// string. The string is parsed at the edge and kept raw when it is not an
/// object.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum ToolInput {
    Object(Map<String, Value>),
    Raw(String),
}

impl ToolInput {
    pub fn from_value(value: Value) -> Self {
        match value {
            Value::Object(map) => Self::Object(map),
            Value::String(text) => Self::from_json_text(text),
            other => Self::Raw(other.to_string()),
        }
    }

    pub fn from_json_text(text: String) -> Self {
        match serde_json::from_str::<Map<String, Value>>(&text) {
            Ok(map) => Self::Object(map),
            Err(_) => Self::Raw(text),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageRef {
    Inline {
        media_type: String,
        bytes_len: u64,
        sha256: String,
        /// Base64 bytes, set at or under [`CHAT_IMAGE_INLINE_MAX`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
    },
    Path {
        path: String,
    },
    Url {
        url: String,
    },
}

impl ImageRef {
    /// An image from base64 text. Bytes that do not decode still yield an
    /// image: its hash covers the text, and no data rides along.
    pub fn from_base64(media_type: String, data: String) -> Self {
        use sha2::{Digest, Sha256};
        let decoded = decode_base64(&data);
        let bytes = decoded.as_deref().unwrap_or(data.as_bytes());
        let sha256 = Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let inline = decoded.is_some() && bytes.len() <= CHAT_IMAGE_INLINE_MAX;
        Self::Inline {
            media_type,
            bytes_len: bytes.len() as u64,
            sha256,
            data: inline.then_some(data),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnOutcome {
    Done,
    Aborted,
    Error { message: String },
}

/// Standard base64 with optional padding; `None` on any other byte.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    fn sextet(byte: u8) -> Option<u32> {
        Some(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    }
    let bytes = text.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut acc = 0u32;
        for (i, b) in chunk.iter().enumerate() {
            acc |= sextet(*b)? << (18 - 6 * i);
        }
        let take = match chunk.len() {
            4 => 3,
            3 => 2,
            2 => 1,
            _ => return None,
        };
        out.extend_from_slice(&acc.to_be_bytes()[1..1 + take]);
    }
    Some(out)
}

/// A body this build cannot read becomes [`ChatBody::Unknown`], so an older
/// client never fails a batch on a newer server's kind.
fn body_or_unknown<'de, D: Deserializer<'de>>(d: D) -> Result<ChatBody, D::Error> {
    #[derive(Deserialize)]
    struct Tag {
        #[serde(default)]
        kind: String,
    }
    let raw = Value::deserialize(d)?;
    Ok(
        ChatBody::deserialize(&raw).unwrap_or_else(|_| ChatBody::Unknown {
            name: Tag::deserialize(&raw).map(|t| t.kind).unwrap_or_default(),
            raw,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{read_msg_sync, write_msg_sync, ServerMsg};

    fn key() -> SessionKey {
        SessionKey {
            harness: "claude".into(),
            session_id: "11111111-2222-3333-4444-555555555555".into(),
            subagent: None,
        }
    }

    #[test]
    fn chat_event_every_body_round_trips_and_unknown_kind_decodes() {
        let child = SessionKey {
            subagent: Some("a1".into()),
            ..key()
        };
        let image = ImageRef::from_base64("image/png".into(), "aGVsbG8=".into());
        assert!(matches!(
            &image,
            ImageRef::Inline {
                bytes_len: 5,
                data: Some(_),
                ..
            }
        ));
        let bodies = vec![
            ChatBody::UserMessage {
                id: "u1".into(),
                origin: UserOrigin::Typed,
                parts: vec![
                    ContentPart::Text { text: "hi".into() },
                    ContentPart::Image { image },
                    ContentPart::File { path: "/a".into() },
                ],
            },
            ChatBody::TextStarted { part: "p".into() },
            ChatBody::TextDelta {
                part: "p".into(),
                text: "t".into(),
            },
            ChatBody::TextEnded { part: "p".into() },
            ChatBody::ThinkingStarted {
                part: "q".into(),
                redacted: true,
            },
            ChatBody::ThinkingDelta {
                part: "q".into(),
                text: "t".into(),
            },
            ChatBody::ThinkingEnded { part: "q".into() },
            ChatBody::ToolCalled {
                call_id: "c".into(),
                name: "Bash".into(),
                input: ToolInput::from_json_text("{\"cmd\":\"ls\"}".into()),
            },
            ChatBody::ToolCalled {
                call_id: "d".into(),
                name: "apply_patch".into(),
                input: ToolInput::from_json_text("*** Begin Patch".into()),
            },
            ChatBody::ToolFinished {
                call_id: "c".into(),
                ok: false,
                output: vec![],
            },
            ChatBody::QuestionAsked {
                call_id: "q".into(),
                prompt: serde_json::json!({"a": 1}),
            },
            ChatBody::QuestionAnswered {
                call_id: "q".into(),
                answer: serde_json::json!(["x"]),
            },
            ChatBody::SubagentStarted {
                call_id: "s".into(),
                child,
                task: Some("t".into()),
            },
            ChatBody::Compacted {
                summary: None,
                redacted: true,
            },
            ChatBody::TurnStarted {
                turn_id: Some("t".into()),
            },
            ChatBody::TurnEnded {
                turn_id: None,
                outcome: TurnOutcome::Error {
                    message: "x".into(),
                },
            },
            ChatBody::Unknown {
                name: "odd".into(),
                raw: serde_json::json!({"kind": "odd"}),
            },
        ];
        let events: Vec<ChatEvent> = bodies
            .into_iter()
            .enumerate()
            .map(|(i, body)| ChatEvent {
                seq: i as u64,
                at_ms: Some(7),
                source: SourceRef::Line { offset: i as u64 },
                body,
            })
            .collect();
        let msg = ServerMsg::ChatEvents {
            session: key(),
            events,
        };
        let mut buf = Vec::new();
        write_msg_sync(&mut buf, &msg).unwrap();
        let back: ServerMsg = read_msg_sync(&mut buf.as_slice()).unwrap();
        assert_eq!(back, msg);

        let newer =
            r#"{"seq":0,"source":{"type":"row","id":"r"},"body":{"kind":"poll_vote","n":2}}"#;
        let event: ChatEvent = serde_json::from_str(newer).unwrap();
        assert_eq!(
            event.body,
            ChatBody::Unknown {
                name: "poll_vote".into(),
                raw: serde_json::json!({"kind": "poll_vote", "n": 2})
            }
        );
    }
}
