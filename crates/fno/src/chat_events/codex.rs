//! Codex rollouts (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`) as chat
//! events. Each `event_msg/item_completed` repeats a `response_item`, so
//! content comes from `response_item` only. `event_msg` adds turn and
//! question facts, and the `compacted` item adds compactions. Session facts
//! (`session_meta`, `turn_context`, `world_state`, `token_count`, ...) emit
//! nothing.

use super::{injected_text, iso_ms, Mapped, RowMapper};
use crate::proto::chat_event::{
    ChatBody, ContentPart, ImageRef, ToolInput, TurnOutcome, UserOrigin,
};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Row {
    #[serde(rename = "type", default)]
    kind: String,
    timestamp: Option<String>,
    #[serde(default)]
    payload: Value,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Item {
    Message {
        #[serde(default)]
        id: Option<String>,
        role: String,
        #[serde(default)]
        content: Vec<Value>,
    },
    Reasoning {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        summary: Vec<Value>,
        #[serde(default)]
        content: Option<Vec<Value>>,
        #[serde(default)]
        encrypted_content: Option<String>,
    },
    FunctionCall {
        name: String,
        #[serde(default)]
        arguments: String,
        call_id: String,
    },
    CustomToolCall {
        name: String,
        #[serde(default)]
        input: String,
        call_id: String,
    },
    FunctionCallOutput {
        call_id: String,
        #[serde(default)]
        output: Value,
    },
    CustomToolCallOutput {
        call_id: String,
        #[serde(default)]
        output: Value,
    },
    LocalShellCall {
        #[serde(default)]
        call_id: Option<String>,
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        action: Value,
    },
    WebSearchCall {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        action: Value,
    },
    /// The encrypted twin of a `compacted` item; the `compacted` item is the
    /// one that emits.
    Compaction {},
    AdditionalTools {},
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    TaskStarted {
        #[serde(default)]
        turn_id: Option<String>,
    },
    TaskComplete {
        #[serde(default)]
        turn_id: Option<String>,
    },
    TurnAborted {
        #[serde(default)]
        turn_id: Option<String>,
    },
    RequestUserInput {
        #[serde(default)]
        call_id: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct Compacted {
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentItem {
    InputText { text: String },
    OutputText { text: String },
    InputImage { image_url: String },
    SummaryText { text: String },
    ReasoningText { text: String },
}

impl ContentItem {
    fn part(self) -> ContentPart {
        match self {
            Self::InputText { text }
            | Self::OutputText { text }
            | Self::SummaryText { text }
            | Self::ReasoningText { text } => ContentPart::Text { text },
            Self::InputImage { image_url } => ContentPart::Image {
                image: image_from_url(image_url),
            },
        }
    }
}

/// A `data:<mime>;base64,<data>` URL becomes an inline image, any other URL
/// stays a URL.
fn image_from_url(url: String) -> ImageRef {
    if let Some((mime, data)) = url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(";base64,"))
    {
        return ImageRef::from_base64(mime.to_string(), data.to_string());
    }
    ImageRef::Url { url }
}

fn parts(items: Vec<Value>) -> Vec<ContentPart> {
    items
        .into_iter()
        .filter_map(|raw| ContentItem::deserialize(&raw).ok().map(ContentItem::part))
        .collect()
}

fn texts(items: Vec<Value>) -> String {
    parts(items)
        .into_iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A tool output is a string or a list of content items.
fn output_parts(output: Value) -> Vec<ContentPart> {
    match output {
        Value::String(text) => vec![ContentPart::Text { text }],
        Value::Array(items) => parts(items),
        Value::Null => Vec::new(),
        other => vec![ContentPart::Text {
            text: other.to_string(),
        }],
    }
}

#[derive(Default)]
pub struct CodexMapper;

impl CodexMapper {
    fn item(&self, item: Item, offset_id: impl Fn() -> String) -> Vec<ChatBody> {
        match item {
            Item::Message { id, role, content } => {
                let id = id.unwrap_or_else(&offset_id);
                match role.as_str() {
                    "user" => {
                        let parts = parts(content);
                        let injected = parts.iter().any(
                            |p| matches!(p, ContentPart::Text { text } if injected_text(text)),
                        );
                        vec![ChatBody::UserMessage {
                            id,
                            origin: if injected {
                                UserOrigin::Meta
                            } else {
                                UserOrigin::Typed
                            },
                            parts,
                        }]
                    }
                    "assistant" => vec![
                        ChatBody::TextStarted { part: id.clone() },
                        ChatBody::TextDelta {
                            part: id.clone(),
                            text: texts(content),
                        },
                        ChatBody::TextEnded { part: id },
                    ],
                    _ => Vec::new(),
                }
            }
            Item::Reasoning {
                id,
                summary,
                content,
                encrypted_content,
            } => {
                let part = id.unwrap_or_else(&offset_id);
                let text = match content.map(texts).filter(|t| !t.is_empty()) {
                    Some(text) => text,
                    None => texts(summary),
                };
                let mut out = vec![ChatBody::ThinkingStarted {
                    part: part.clone(),
                    redacted: text.is_empty() && encrypted_content.is_some(),
                }];
                if !text.is_empty() {
                    out.push(ChatBody::ThinkingDelta {
                        part: part.clone(),
                        text,
                    });
                }
                out.push(ChatBody::ThinkingEnded { part });
                out
            }
            Item::FunctionCall {
                name,
                arguments,
                call_id,
            } => vec![ChatBody::ToolCalled {
                call_id,
                name,
                input: ToolInput::from_json_text(arguments),
            }],
            Item::CustomToolCall {
                name,
                input,
                call_id,
            } => vec![ChatBody::ToolCalled {
                call_id,
                name,
                input: ToolInput::from_json_text(input),
            }],
            Item::FunctionCallOutput { call_id, output }
            | Item::CustomToolCallOutput { call_id, output } => vec![ChatBody::ToolFinished {
                call_id,
                ok: true,
                output: output_parts(output),
            }],
            Item::LocalShellCall {
                call_id,
                id,
                action,
            } => vec![ChatBody::ToolCalled {
                call_id: call_id.or(id).unwrap_or_else(&offset_id),
                name: "local_shell".into(),
                input: ToolInput::from_value(action),
            }],
            Item::WebSearchCall { id, action } => {
                let call_id = id.unwrap_or_else(&offset_id);
                vec![
                    ChatBody::ToolCalled {
                        call_id: call_id.clone(),
                        name: "web_search".into(),
                        input: ToolInput::from_value(action),
                    },
                    ChatBody::ToolFinished {
                        call_id,
                        ok: true,
                        output: Vec::new(),
                    },
                ]
            }
            Item::Compaction {} | Item::AdditionalTools {} => Vec::new(),
        }
    }
}

impl RowMapper for CodexMapper {
    fn map(&mut self, line: &str, _more: bool) -> Option<Vec<Mapped>> {
        let Ok(row) = serde_json::from_str::<Row>(line) else {
            return Some(Vec::new());
        };
        let at = iso_ms(row.timestamp.as_deref());
        let bodies = match row.kind.as_str() {
            "response_item" => match Item::deserialize(&row.payload) {
                Ok(item) => {
                    let fallback = format!("item:{}", at.unwrap_or_default());
                    self.item(item, || fallback.clone())
                }
                Err(_) => {
                    #[derive(Deserialize)]
                    struct Kind {
                        #[serde(rename = "type", default)]
                        kind: String,
                    }
                    let kind = Kind::deserialize(&row.payload)
                        .map(|k| k.kind)
                        .unwrap_or_default();
                    vec![ChatBody::Unknown {
                        name: format!("response_item/{kind}"),
                        raw: row.payload,
                    }]
                }
            },
            "event_msg" => match Event::deserialize(&row.payload) {
                Ok(Event::TaskStarted { turn_id }) => vec![ChatBody::TurnStarted { turn_id }],
                Ok(Event::TaskComplete { turn_id }) => vec![ChatBody::TurnEnded {
                    turn_id,
                    outcome: TurnOutcome::Done,
                }],
                Ok(Event::TurnAborted { turn_id }) => vec![ChatBody::TurnEnded {
                    turn_id,
                    outcome: TurnOutcome::Aborted,
                }],
                Ok(Event::RequestUserInput { call_id }) => vec![ChatBody::QuestionAsked {
                    call_id,
                    prompt: row.payload,
                }],
                Ok(Event::Other) | Err(_) => Vec::new(),
            },
            "compacted" => {
                let summary = Compacted::deserialize(&row.payload)
                    .map(|c| c.message)
                    .unwrap_or_default();
                vec![ChatBody::Compacted {
                    redacted: summary.is_empty(),
                    summary: (!summary.is_empty()).then_some(summary),
                }]
            }
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

    #[test]
    fn chat_events_codex_fixture_reads_each_item_once() {
        let rows = [
            r#"{"timestamp":"2026-11-10T16:01:23.963Z","type":"session_meta","payload":{"id":"s"}}"#,
            r#"{"timestamp":"2026-11-10T16:01:23.963Z","type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}"#,
            r##"{"timestamp":"2026-11-10T16:01:25Z","type":"response_item","payload":{"type":"message","id":"m0","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /x"}]}}"##,
            r#"{"timestamp":"2026-11-10T16:01:25Z","type":"response_item","payload":{"type":"message","id":"m1","role":"user","content":[{"type":"input_text","text":"run ls"}]}}"#,
            r#"{"timestamp":"2026-11-10T16:01:25Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","id":"m1","content":[{"type":"text","text":"run ls"}]}}}"#,
            r#"{"timestamp":"2026-11-10T16:01:26Z","type":"response_item","payload":{"type":"reasoning","id":"r1","summary":[],"encrypted_content":"gAAA"}}"#,
            r#"{"timestamp":"2026-11-10T16:01:26Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"ls\"}","call_id":"c1"}}"#,
            r#"{"timestamp":"2026-11-10T16:01:26Z","type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch","call_id":"c2"}}"#,
            r#"{"timestamp":"2026-11-10T16:01:27Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"a.txt"}}"#,
            r#"{"timestamp":"2026-11-10T16:01:27Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","id":"c1"}}}"#,
            r#"{"timestamp":"2026-11-10T16:01:28Z","type":"response_item","payload":{"type":"message","id":"m2","role":"assistant","content":[{"type":"output_text","text":"done"}]}}"#,
            r#"{"timestamp":"2026-11-10T16:01:28Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","id":"m2"}}}"#,
            r#"{"timestamp":"2026-11-10T16:01:28Z","type":"event_msg","payload":{"type":"token_count","info":{}}}"#,
            r#"{"timestamp":"2026-11-10T16:01:29Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t1"}}"#,
            r#"{"timestamp":"2026-11-10T16:01:30Z","type":"event_msg","payload":{"type":"turn_aborted","turn_id":"t2"}}"#,
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        std::fs::write(&path, rows.join("\n") + "\n").unwrap();
        let events = SessionReader::lines(path, Box::new(CodexMapper), 0)
            .poll()
            .unwrap();
        let bodies: Vec<ChatBody> = events.into_iter().map(|e| e.body).collect();
        let user_count = bodies
            .iter()
            .filter(|b| matches!(b, ChatBody::UserMessage { .. }))
            .count();
        assert_eq!(
            user_count, 2,
            "item_completed must not repeat a message: {bodies:#?}"
        );
        assert!(matches!(
            &bodies[1],
            ChatBody::UserMessage {
                origin: UserOrigin::Meta,
                ..
            }
        ));
        assert!(matches!(
            &bodies[2],
            ChatBody::UserMessage {
                origin: UserOrigin::Typed,
                ..
            }
        ));
        assert_eq!(
            bodies[3],
            ChatBody::ThinkingStarted {
                part: "r1".into(),
                redacted: true
            }
        );
        assert!(
            matches!(&bodies[5], ChatBody::ToolCalled { input: ToolInput::Object(m), .. } if m.contains_key("cmd"))
        );
        assert!(
            matches!(&bodies[6], ChatBody::ToolCalled { input: ToolInput::Raw(r), .. } if r == "*** Begin Patch")
        );
        assert!(
            matches!(&bodies[7], ChatBody::ToolFinished { call_id, ok: true, .. } if call_id == "c1")
        );
        assert_eq!(
            bodies[0],
            ChatBody::TurnStarted {
                turn_id: Some("t1".into())
            }
        );
        assert_eq!(
            bodies[bodies.len() - 2..],
            [
                ChatBody::TurnEnded {
                    turn_id: Some("t1".into()),
                    outcome: TurnOutcome::Done
                },
                ChatBody::TurnEnded {
                    turn_id: Some("t2".into()),
                    outcome: TurnOutcome::Aborted
                },
            ]
        );
        assert_eq!(bodies.len(), 14, "{bodies:#?}");
    }
}
