//! The conversation one chat stream folds into: rows the view draws, each
//! with its parsed markdown cached until the row's text grows.

use std::cell::OnceCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{Image, ImageFormat};

use super::markdown::{blocks, Block};
use crate::proto::chat_event::{
    decode_base64, ChatBody, ChatEvent, ContentPart, ImageRef, ToolInput, TurnOutcome, UserOrigin,
};

/// An image a row draws: decoded once, when its event arrives.
#[derive(Debug, Clone)]
pub enum Picture {
    Bytes(Arc<Image>),
    Path(PathBuf),
    Url(String),
    /// Too big to ride the stream, or bytes that did not decode.
    Missing {
        bytes_len: u64,
    },
}

impl PartialEq for Picture {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Bytes(a), Self::Bytes(b)) => Arc::ptr_eq(a, b),
            (Self::Path(a), Self::Path(b)) => a == b,
            (Self::Url(a), Self::Url(b)) => a == b,
            (Self::Missing { bytes_len: a }, Self::Missing { bytes_len: b }) => a == b,
            _ => false,
        }
    }
}

fn picture(image: &ImageRef) -> Picture {
    match image {
        ImageRef::Inline {
            media_type,
            bytes_len,
            data,
            ..
        } => {
            let format = match media_type.as_str() {
                "image/png" => Some(ImageFormat::Png),
                "image/jpeg" | "image/jpg" => Some(ImageFormat::Jpeg),
                "image/webp" => Some(ImageFormat::Webp),
                "image/gif" => Some(ImageFormat::Gif),
                "image/svg+xml" => Some(ImageFormat::Svg),
                _ => None,
            };
            match (format, data.as_deref().and_then(decode_base64)) {
                (Some(f), Some(bytes)) => Picture::Bytes(Arc::new(Image::from_bytes(f, bytes))),
                _ => Picture::Missing {
                    bytes_len: *bytes_len,
                },
            }
        }
        ImageRef::Path { path } => Picture::Path(path.into()),
        ImageRef::Url { url } => Picture::Url(url.clone()),
    }
}

/// Text and images from content parts. A file part reads as its path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Content {
    pub text: String,
    pub pictures: Vec<Picture>,
}

impl Content {
    fn of(parts: &[ContentPart]) -> Self {
        let mut out = Content::default();
        let mut texts = Vec::new();
        for p in parts {
            match p {
                ContentPart::Text { text } => texts.push(text.clone()),
                ContentPart::File { path } => texts.push(format!("[file {path}]")),
                ContentPart::Image { image } => out.pictures.push(picture(image)),
            }
        }
        out.text = texts.join("\n");
        out
    }
}

/// One row of the conversation.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    User {
        content: Content,
        meta: bool,
    },
    Text {
        part: String,
        text: String,
        md: OnceCell<Vec<Block>>,
    },
    Thinking {
        part: String,
        text: String,
        md: OnceCell<Vec<Block>>,
    },
    Tool {
        call_id: String,
        name: String,
        /// The one field that names the call, for its row.
        summary: String,
        /// The whole input, for its sheet.
        input: String,
        result: Option<(bool, Content)>,
    },
    /// A kind with no richer view, an unknown one included: one plain line.
    Line(String),
}

impl Item {
    /// The row's markdown, parsed on first draw and kept until it grows.
    pub fn blocks(&self) -> Option<&[Block]> {
        match self {
            Item::Text { text, md, .. } | Item::Thinking { text, md, .. } => {
                Some(md.get_or_init(|| blocks(text)))
            }
            _ => None,
        }
    }
}

fn text_item(part: String, text: String, thinking: bool) -> Item {
    let md = OnceCell::new();
    if thinking {
        Item::Thinking { part, text, md }
    } else {
        Item::Text { part, text, md }
    }
}

/// The one input field that names the call, else the whole input.
fn summary(input: &ToolInput) -> String {
    match input {
        ToolInput::Raw(text) => text.clone(),
        ToolInput::Object(map) => ["command", "file_path", "path", "pattern", "description"]
            .iter()
            .find_map(|k| map.get(*k).and_then(|v| v.as_str()).map(str::to_string))
            .unwrap_or_else(|| serde_json::Value::Object(map.clone()).to_string()),
    }
}

fn whole(input: &ToolInput) -> String {
    match input {
        ToolInput::Raw(text) => text.clone(),
        ToolInput::Object(map) => serde_json::to_string_pretty(map).unwrap_or_default(),
    }
}

/// One session's rows and the view state that belongs to them.
#[derive(Debug, Default)]
pub struct Conversation {
    pub items: Vec<Item>,
    /// Events folded so far: the stream's next `seq`, so a reopen resumes.
    pub events: u64,
    /// Thinking rows the user opened, by index.
    pub expanded: HashSet<usize>,
}

impl Conversation {
    /// Fold one event. Returns the index of the row it added or changed.
    pub fn push(&mut self, event: ChatEvent) -> Option<usize> {
        self.events = event.seq + 1;
        let item = match event.body {
            ChatBody::UserMessage { origin, parts, .. } => Item::User {
                content: Content::of(&parts),
                meta: origin == UserOrigin::Meta,
            },
            ChatBody::TextStarted { part } => text_item(part, String::new(), false),
            ChatBody::ThinkingStarted { part, .. } => text_item(part, String::new(), true),
            ChatBody::TextDelta { part, text } => return Some(self.append(part, text, false)),
            ChatBody::ThinkingDelta { part, text } => return Some(self.append(part, text, true)),
            ChatBody::TextEnded { .. }
            | ChatBody::ThinkingEnded { .. }
            | ChatBody::TurnStarted { .. }
            | ChatBody::TurnEnded {
                outcome: TurnOutcome::Done,
                ..
            } => return None,
            ChatBody::ToolCalled {
                call_id,
                name,
                input,
            } => Item::Tool {
                call_id,
                name,
                summary: summary(&input),
                input: whole(&input),
                result: None,
            },
            ChatBody::ToolFinished {
                call_id,
                ok,
                output,
            } => {
                let call = self
                    .items
                    .iter_mut()
                    .enumerate()
                    .rev()
                    .find_map(|(i, it)| match it {
                        Item::Tool {
                            call_id: id,
                            result,
                            ..
                        } if *id == call_id => Some((i, result)),
                        _ => None,
                    });
                if let Some((i, result)) = call {
                    *result = Some((ok, Content::of(&output)));
                    return Some(i);
                }
                Item::Line(format!("result of an unseen call {call_id}"))
            }
            ChatBody::QuestionAsked { prompt, .. } => Item::Line(format!("asked: {prompt}")),
            ChatBody::QuestionAnswered { answer, .. } => Item::Line(format!("answered: {answer}")),
            ChatBody::SubagentStarted { task, .. } => Item::Line(format!(
                "subagent started: {}",
                task.unwrap_or_else(|| "no task given".into())
            )),
            ChatBody::Compacted { .. } => Item::Line("context compacted".into()),
            ChatBody::TurnEnded {
                outcome: TurnOutcome::Aborted,
                ..
            } => Item::Line("turn aborted".into()),
            ChatBody::TurnEnded {
                outcome: TurnOutcome::Error { message },
                ..
            } => Item::Line(format!("turn failed: {message}")),
            ChatBody::Unknown { name, raw } => Item::Line(format!("{name}: {raw}")),
        };
        self.items.push(item);
        Some(self.items.len() - 1)
    }

    /// A delta joins the newest row of its part and drops that row's parsed
    /// markdown. A delta with no start (a stream opened mid-part) starts the
    /// row itself.
    fn append(&mut self, part: String, delta: String, thinking: bool) -> usize {
        let open = self
            .items
            .iter_mut()
            .enumerate()
            .rev()
            .find_map(|(i, it)| match it {
                Item::Text { part: p, text, md } if !thinking && *p == part => Some((i, text, md)),
                Item::Thinking { part: p, text, md } if thinking && *p == part => {
                    Some((i, text, md))
                }
                _ => None,
            });
        if let Some((i, text, md)) = open {
            text.push_str(&delta);
            md.take();
            return i;
        }
        self.items.push(text_item(part, delta, thinking));
        self.items.len() - 1
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;
    use serde_json::json;

    #[test]
    fn chat_folds_turns_tools_and_unknown_kinds_in_order() {
        let bodies = [
            json!({"kind": "user_message", "id": "u1", "origin": "typed", "parts": [{"type": "text", "text": "hi"}]}),
            json!({"kind": "text_started", "part": "p1"}),
            json!({"kind": "text_delta", "part": "p1", "text": "**hello** "}),
            json!({"kind": "text_delta", "part": "p1", "text": "there"}),
            json!({"kind": "tool_called", "call_id": "t1", "name": "Bash", "input": {"command": "ls"}}),
            json!({"kind": "from_a_newer_server"}),
            json!({"kind": "tool_finished", "call_id": "t1", "ok": true, "output": [{"type": "text", "text": "a.txt"}]}),
            json!({"kind": "turn_ended", "outcome": {"type": "done"}}),
        ];
        let mut conv = Conversation::default();
        let mut touched = Vec::new();
        for (seq, body) in bodies.into_iter().enumerate() {
            let line = json!({"seq": seq, "source": {"type": "line", "offset": seq}, "body": body});
            touched.push(conv.push(serde_json::from_value(line).unwrap()));
        }
        assert_eq!(conv.events, 8);
        // The second delta and the result change rows in place.
        assert_eq!(
            touched,
            [
                Some(0),
                Some(1),
                Some(1),
                Some(1),
                Some(2),
                Some(3),
                Some(2),
                None
            ]
        );
        let user = Item::User {
            content: Content {
                text: "hi".into(),
                pictures: vec![],
            },
            meta: false,
        };
        assert_eq!(conv.items[0], user);
        assert!(matches!(&conv.items[1], Item::Text { text, .. } if text == "**hello** there"));
        let Item::Tool {
            summary, result, ..
        } = &conv.items[2]
        else {
            panic!("row 2 is {:?}", conv.items[2]);
        };
        assert_eq!(summary, "ls");
        assert_eq!(
            result.as_ref().map(|(ok, c)| (*ok, c.text.as_str())),
            Some((true, "a.txt"))
        );
        assert!(matches!(&conv.items[3], Item::Line(l) if l.starts_with("from_a_newer_server: ")));
        assert_eq!(conv.items.len(), 4);
    }
}
