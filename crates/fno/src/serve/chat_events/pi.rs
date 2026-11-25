//! pi (`~/.pi/agent/sessions/<cwd>/<ts>_<id>.jsonl`) and oh-my-pi
//! (`~/.omp/agent/sessions/...`) transcripts as chat events. Both write v3
//! JSONL: a `session` header, then typed entries linked by `id`/`parentId`.
//! Only `message` and `compaction` entries emit. The header, omp's title slot
//! and every other entry kind (`model_change`, `label`, ...) emit nothing, so
//! the mapper never indexes a line by position. Rows map in file order: pi
//! replays only the leaf chain of a branched session, but that chain depends
//! on the whole file, and `seq` must stay a function of the bytes read so far.

use super::{image_from_base64, injected_text, iso_ms, Mapped, RowMapper};
use crate::proto::chat_event::{ChatBody, ContentPart, ToolInput, UserOrigin};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;

/// pi's ask-the-user tool. It ships in an installable pi package, not the
/// core, so a session may carry the name without any bundled tool behind it.
const ASK_TOOL: &str = "ask_question";

#[derive(Deserialize)]
struct Row {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    timestamp: Option<String>,
    #[serde(default)]
    message: Value,
    #[serde(default)]
    summary: String,
}

#[derive(Deserialize)]
#[serde(tag = "role")]
enum Message {
    #[serde(rename = "user")]
    User {
        #[serde(default)]
        content: Value,
    },
    #[serde(rename = "assistant")]
    Assistant {
        #[serde(default)]
        content: Vec<Value>,
    },
    #[serde(rename = "toolResult")]
    ToolResult {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(default)]
        content: Vec<Value>,
        #[serde(rename = "isError", default)]
        is_error: bool,
    },
}

#[derive(Deserialize)]
struct Role {
    #[serde(default)]
    role: String,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Block {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        redacted: bool,
    },
    #[serde(rename = "toolCall")]
    ToolCall {
        id: String,
        name: String,
        #[serde(default)]
        arguments: Value,
    },
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

/// The text and image blocks of a user message or a tool result.
fn parts(items: Vec<Value>) -> Vec<ContentPart> {
    items
        .into_iter()
        .filter_map(|raw| match Block::deserialize(&raw) {
            Ok(Block::Text { text }) => Some(ContentPart::Text { text }),
            Ok(Block::Image { data, mime_type }) => Some(ContentPart::Image {
                image: image_from_base64(mime_type, data),
            }),
            _ => None,
        })
        .collect()
}

fn text_of(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Maps both pi and omp rows. Holds the ids of `ask_question` calls so a
/// later `toolResult` becomes the answer instead of a tool output.
#[derive(Default)]
pub struct PiMapper {
    asks: HashSet<String>,
}

impl PiMapper {
    fn message(&mut self, id: &str, raw: Value) -> Vec<ChatBody> {
        match Message::deserialize(&raw) {
            Ok(Message::User { content }) => {
                let parts = match content {
                    Value::String(text) => vec![ContentPart::Text { text }],
                    Value::Array(items) => parts(items),
                    _ => Vec::new(),
                };
                let injected = parts
                    .iter()
                    .any(|p| matches!(p, ContentPart::Text { text } if injected_text(text)));
                vec![ChatBody::UserMessage {
                    id: id.to_string(),
                    origin: if injected {
                        UserOrigin::Meta
                    } else {
                        UserOrigin::Typed
                    },
                    parts,
                }]
            }
            Ok(Message::Assistant { content }) => self.assistant(id, content),
            Ok(Message::ToolResult {
                tool_call_id,
                content,
                is_error,
            }) => {
                let output = parts(content);
                if self.asks.remove(&tool_call_id) {
                    vec![ChatBody::QuestionAnswered {
                        call_id: tool_call_id,
                        answer: Value::String(text_of(&output)),
                    }]
                } else {
                    vec![ChatBody::ToolFinished {
                        call_id: tool_call_id,
                        ok: !is_error,
                        output,
                    }]
                }
            }
            // A role pi adds later (bash executions, custom messages) stays
            // visible as unknown, never dropped.
            Err(_) => {
                let role = Role::deserialize(&raw).map(|r| r.role).unwrap_or_default();
                vec![ChatBody::Unknown {
                    name: format!("message/{role}"),
                    raw,
                }]
            }
        }
    }

    fn assistant(&mut self, id: &str, content: Vec<Value>) -> Vec<ChatBody> {
        let mut out = Vec::new();
        for (index, raw) in content.into_iter().enumerate() {
            let part = format!("{id}:{index}");
            match Block::deserialize(&raw) {
                Ok(Block::Text { text }) => out.extend([
                    ChatBody::TextStarted { part: part.clone() },
                    ChatBody::TextDelta {
                        part: part.clone(),
                        text,
                    },
                    ChatBody::TextEnded { part },
                ]),
                Ok(Block::Thinking { thinking, redacted }) => {
                    out.push(ChatBody::ThinkingStarted {
                        part: part.clone(),
                        redacted: redacted || thinking.is_empty(),
                    });
                    if !thinking.is_empty() {
                        out.push(ChatBody::ThinkingDelta {
                            part: part.clone(),
                            text: thinking,
                        });
                    }
                    out.push(ChatBody::ThinkingEnded { part });
                }
                Ok(Block::ToolCall {
                    id: call_id,
                    name,
                    arguments,
                }) if name == ASK_TOOL => {
                    self.asks.insert(call_id.clone());
                    out.push(ChatBody::QuestionAsked {
                        call_id,
                        prompt: arguments,
                    });
                }
                Ok(Block::ToolCall {
                    id: call_id,
                    name,
                    arguments,
                }) => out.push(ChatBody::ToolCalled {
                    call_id,
                    name,
                    input: ToolInput::from_value(arguments),
                }),
                _ => out.push(ChatBody::Unknown {
                    name: "assistant_block".into(),
                    raw,
                }),
            }
        }
        out
    }
}

impl RowMapper for PiMapper {
    fn map(&mut self, line: &str, _more: bool) -> Option<Vec<Mapped>> {
        // omp's title slot may not be JSON at all; an unreadable row is not a
        // chat event.
        let Ok(row) = serde_json::from_str::<Row>(line) else {
            return Some(Vec::new());
        };
        let at = iso_ms(row.timestamp.as_deref());
        let id = row
            .id
            .unwrap_or_else(|| format!("row:{}", at.unwrap_or_default()));
        let bodies = match row.kind.as_str() {
            "message" => self.message(&id, row.message),
            "compaction" => vec![ChatBody::Compacted {
                redacted: row.summary.is_empty(),
                summary: (!row.summary.is_empty()).then_some(row.summary),
            }],
            _ => Vec::new(),
        };
        Some(bodies.into_iter().map(|b| Mapped::new(at, b)).collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;
    use crate::chat_events::SessionReader;

    const HEADER: &str = r#"{"type":"session","version":3,"id":"0192f3a0-0000-7000-8000-000000000001","timestamp":"2026-11-24T10:00:00.000Z","cwd":"/x"}"#;

    fn rows() -> Vec<&'static str> {
        vec![
            HEADER,
            r#"{"type":"model_change","id":"m0","parentId":null,"timestamp":"2026-11-24T10:00:00.500Z","provider":"anthropic","modelId":"x"}"#,
            r#"{"type":"message","id":"e1","parentId":"m0","timestamp":"2026-11-24T10:00:01.000Z","message":{"role":"user","content":"run ls","timestamp":1}}"#,
            r#"{"type":"message","id":"e2","parentId":"e1","timestamp":"2026-11-24T10:00:01.500Z","message":{"role":"user","content":[{"type":"text","text":"<system-reminder>x</system-reminder>"}],"timestamp":2}}"#,
            r#"{"type":"message","id":"e3","parentId":"e2","timestamp":"2026-11-24T10:00:02.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"ok"},{"type":"toolCall","id":"c1","name":"bash","arguments":{"command":"ls"}}],"timestamp":3}}"#,
            r#"{"type":"message","id":"e4","parentId":"e3","timestamp":"2026-11-24T10:00:03.000Z","message":{"role":"toolResult","toolCallId":"c1","toolName":"bash","content":[{"type":"text","text":"a.txt"}],"isError":false,"timestamp":4}}"#,
            r#"{"type":"message","id":"e5","parentId":"e4","timestamp":"2026-11-24T10:00:04.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"q1","name":"ask_question","arguments":{"question":"which?"}}],"timestamp":5}}"#,
            r#"{"type":"message","id":"e6","parentId":"e5","timestamp":"2026-11-24T10:00:05.000Z","message":{"role":"toolResult","toolCallId":"q1","toolName":"ask_question","content":[{"type":"text","text":"the first"}],"isError":false,"timestamp":6}}"#,
            r#"{"type":"compaction","id":"e7","parentId":"e6","timestamp":"2026-11-24T10:00:06.000Z","summary":"so far","firstKeptEntryId":"e5","tokensBefore":9}"#,
            r#"{"type":"message","id":"e8","parentId":"e7","timestamp":"2026-11-24T10:00:07.000Z","message":{"role":"bashExecution","command":"ls"}}"#,
        ]
    }

    fn read(lines: &[&str]) -> Vec<ChatBody> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        SessionReader::lines(path, Box::new(PiMapper::default()), 0)
            .poll()
            .unwrap()
            .into_iter()
            .map(|e| e.body)
            .collect()
    }

    #[test]
    fn chat_events_pi_fixture_maps_each_kind_in_file_order() {
        let bodies = read(&rows());
        assert_eq!(bodies.len(), 14, "{bodies:#?}");
        assert!(matches!(
            &bodies[0],
            ChatBody::UserMessage { origin: UserOrigin::Typed, id, .. } if id == "e1"
        ));
        assert!(matches!(
            &bodies[1],
            ChatBody::UserMessage {
                origin: UserOrigin::Meta,
                ..
            }
        ));
        assert_eq!(
            bodies[2],
            ChatBody::ThinkingStarted {
                part: "e3:0".into(),
                redacted: false
            }
        );
        assert!(
            matches!(&bodies[6], ChatBody::TextDelta { part, text } if part == "e3:1" && text == "ok")
        );
        assert!(
            matches!(&bodies[8], ChatBody::ToolCalled { call_id, input: ToolInput::Object(m), .. } if call_id == "c1" && m.contains_key("command"))
        );
        assert!(
            matches!(&bodies[9], ChatBody::ToolFinished { call_id, ok: true, .. } if call_id == "c1")
        );
        assert!(matches!(&bodies[10], ChatBody::QuestionAsked { call_id, .. } if call_id == "q1"));
        assert_eq!(
            bodies[11],
            ChatBody::QuestionAnswered {
                call_id: "q1".into(),
                answer: Value::String("the first".into())
            }
        );
        assert_eq!(
            bodies[12],
            ChatBody::Compacted {
                summary: Some("so far".into()),
                redacted: false
            }
        );
        assert!(
            matches!(&bodies[13], ChatBody::Unknown { name, .. } if name == "message/bashExecution")
        );
    }

    #[test]
    fn chat_events_omp_title_slot_before_the_header_maps_the_same() {
        let mut omp = vec![r#"{"title":"fix the thing"}"#, "not json at all"];
        omp.extend(rows());
        assert_eq!(read(&omp), read(&rows()));
    }

    #[test]
    fn chat_events_open_answers_pi_and_omp_from_their_session_dirs() {
        use crate::chat_events::{ChatReadError, ChatRoots};
        use crate::proto::chat_event::SessionKey;
        let id = "0192f3a0-0000-7000-8000-000000000001";
        let root = tempfile::tempdir().unwrap();
        let cwd_dir = root.path().join("--x--");
        std::fs::create_dir_all(&cwd_dir).unwrap();
        std::fs::write(
            cwd_dir.join(format!("2026-11-24T10-00-00-000Z_{id}.jsonl")),
            rows().join("\n") + "\n",
        )
        .unwrap();
        let roots = ChatRoots {
            transcripts: vec![root.path().to_path_buf()],
            opencode_stores: Vec::new(),
        };
        let key = |harness: &str, subagent: Option<&str>| SessionKey {
            harness: harness.into(),
            session_id: id.into(),
            subagent: subagent.map(Into::into),
        };
        for harness in ["pi", "omp"] {
            let events = SessionReader::open(&key(harness, None), &roots, 0)
                .unwrap()
                .poll()
                .unwrap();
            assert_eq!(events.len(), 14, "{harness}");
        }
        assert!(matches!(
            SessionReader::open(&key("pi", Some("child")), &roots, 0),
            Err(ChatReadError::NotFound { .. })
        ));
        assert!(matches!(
            SessionReader::open(&key("agy", None), &roots, 0),
            Err(ChatReadError::UnsupportedHarness(_))
        ));
    }
}
