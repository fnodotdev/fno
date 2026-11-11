//! Chat mode (cmd-1, the mode the app opens in): the focused pane's agent
//! session as a conversation. The events come from the mux's `ChatTail`
//! stream, so the app parses no transcript of its own. The composer types
//! into the same pane the terminal view drives.

use std::collections::HashSet;
use std::ops::Range;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use gpui::{
    div, prelude::*, px, rgb, Context, Div, ElementId, FontStyle, FontWeight, HighlightStyle,
    ScrollHandle, SharedString, StyledText,
};
use pulldown_cmark::{Event as Md, Parser, Tag, TagEnd};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use super::window::{Action, Root};
use crate::chat_events::{tail_sync, ChatReadError};
use crate::proto::chat_event::{
    ChatBody, ChatEvent, ContentPart, SessionKey, ToolInput, TurnOutcome, UserOrigin,
};
use crate::proto::ProtoError;
use std::io::ErrorKind;

/// The longest tool output an expanded call shows.
const OUTPUT_MAX: usize = 4000;

/// One row of the conversation, folded from the stream's events.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    User {
        text: String,
        meta: bool,
    },
    Text {
        part: String,
        text: String,
    },
    Thinking {
        part: String,
        text: String,
    },
    Tool {
        call_id: String,
        name: String,
        input: String,
        /// `(ok, output)` once the call finished.
        result: Option<(bool, String)>,
    },
    /// A kind with no richer view, an unknown one included: one plain line.
    Line(String),
}

#[derive(Debug, Default)]
pub struct Conversation {
    pub items: Vec<Item>,
    pub events: usize,
}

fn parts_text(parts: &[ContentPart]) -> String {
    let each = parts.iter().map(|p| match p {
        ContentPart::Text { text } => text.clone(),
        ContentPart::Image { .. } => "[image]".to_string(),
        ContentPart::File { path } => format!("[file {path}]"),
    });
    each.collect::<Vec<_>>().join("\n")
}

/// The one input field that names the call, else the whole input.
fn input_summary(input: &ToolInput) -> String {
    match input {
        ToolInput::Raw(text) => text.clone(),
        ToolInput::Object(map) => ["command", "file_path", "path", "pattern", "description"]
            .iter()
            .find_map(|k| map.get(*k).and_then(|v| v.as_str()).map(str::to_string))
            .unwrap_or_else(|| serde_json::Value::Object(map.clone()).to_string()),
    }
}

impl Conversation {
    pub fn push(&mut self, event: ChatEvent) {
        self.events += 1;
        let item = match event.body {
            ChatBody::UserMessage { origin, parts, .. } => Item::User {
                text: parts_text(&parts),
                meta: origin == UserOrigin::Meta,
            },
            ChatBody::TextStarted { part } => Item::Text {
                part,
                text: String::new(),
            },
            ChatBody::TextDelta { part, text } => return self.append(part, text, false),
            ChatBody::ThinkingStarted { part, .. } => Item::Thinking {
                part,
                text: String::new(),
            },
            ChatBody::ThinkingDelta { part, text } => return self.append(part, text, true),
            ChatBody::TextEnded { .. }
            | ChatBody::ThinkingEnded { .. }
            | ChatBody::TurnStarted { .. }
            | ChatBody::TurnEnded {
                outcome: TurnOutcome::Done,
                ..
            } => return,
            ChatBody::ToolCalled {
                call_id,
                name,
                input,
            } => Item::Tool {
                call_id,
                name,
                input: input_summary(&input),
                result: None,
            },
            ChatBody::ToolFinished {
                call_id,
                ok,
                output,
            } => {
                let call = self.items.iter_mut().rev().find_map(|i| match i {
                    Item::Tool {
                        call_id: id,
                        result,
                        ..
                    } if *id == call_id => Some(result),
                    _ => None,
                });
                match call {
                    Some(result) => *result = Some((ok, parts_text(&output))),
                    None => self
                        .items
                        .push(Item::Line(format!("result of an unseen call {call_id}"))),
                }
                return;
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
    }

    /// A delta joins the newest item of its part. A delta with no start (a
    /// stream opened mid-part) starts the item itself.
    fn append(&mut self, part: String, delta: String, thinking: bool) {
        let open = self.items.iter_mut().rev().find_map(|i| match i {
            Item::Text { part: p, text } if !thinking && *p == part => Some(text),
            Item::Thinking { part: p, text } if thinking && *p == part => Some(text),
            _ => None,
        });
        match open {
            Some(text) => text.push_str(&delta),
            None if thinking => self.items.push(Item::Thinking { part, text: delta }),
            None => self.items.push(Item::Text { part, text: delta }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inline {
    Bold,
    Italic,
    Code,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Para,
    Heading(u8),
    /// A list item at its nesting depth, 1 for the top level.
    Item(usize),
    Code,
    Quote,
    Rule,
}

/// One markdown block: its text and the styled byte ranges inside it.
/// Tables and images stay as their source text.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub kind: BlockKind,
    pub text: String,
    pub spans: Vec<(Range<usize>, Inline)>,
}

pub fn blocks(md: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut cur: Option<Block> = None;
    let mut open: Vec<(Inline, usize)> = Vec::new();
    let (mut depth, mut quote) = (0usize, false);
    let start = |kind| Block {
        kind,
        text: String::new(),
        spans: Vec::new(),
    };
    let flush = |cur: &mut Option<Block>, out: &mut Vec<Block>| {
        if let Some(mut b) = cur.take().filter(|b| !b.text.trim().is_empty()) {
            if b.kind == BlockKind::Code {
                b.text.truncate(b.text.trim_end().len());
            }
            out.push(b);
        }
    };
    for ev in Parser::new(md) {
        match ev {
            Md::Start(Tag::Heading { level, .. }) => {
                cur = Some(start(BlockKind::Heading(level as u8)))
            }
            Md::Start(Tag::CodeBlock(_)) => cur = Some(start(BlockKind::Code)),
            Md::Start(Tag::BlockQuote(_)) => quote = true,
            Md::End(TagEnd::BlockQuote(_)) => quote = false,
            Md::Start(Tag::List(_)) => {
                flush(&mut cur, &mut out);
                depth += 1;
            }
            Md::End(TagEnd::List(_)) => depth = depth.saturating_sub(1),
            Md::Start(Tag::Item) => cur = Some(start(BlockKind::Item(depth))),
            Md::Start(Tag::Paragraph) if cur.is_none() => {
                cur = Some(start(if quote {
                    BlockKind::Quote
                } else {
                    BlockKind::Para
                }))
            }
            Md::Start(Tag::Emphasis) | Md::Start(Tag::Strong) => {
                let style = if matches!(ev, Md::Start(Tag::Strong)) {
                    Inline::Bold
                } else {
                    Inline::Italic
                };
                let at = cur.as_ref().map_or(0, |b| b.text.len());
                open.push((style, at));
            }
            Md::End(TagEnd::Emphasis) | Md::End(TagEnd::Strong) => {
                if let (Some((style, from)), Some(b)) = (open.pop(), cur.as_mut()) {
                    b.spans.push((from..b.text.len(), style));
                }
            }
            Md::End(TagEnd::Heading(_) | TagEnd::CodeBlock | TagEnd::Item) => {
                flush(&mut cur, &mut out)
            }
            Md::End(TagEnd::Paragraph) => {
                if cur
                    .as_ref()
                    .is_some_and(|b| matches!(b.kind, BlockKind::Para | BlockKind::Quote))
                {
                    flush(&mut cur, &mut out);
                } else if let Some(b) = cur.as_mut() {
                    b.text.push('\n');
                }
            }
            Md::Text(t) | Md::Html(t) | Md::InlineHtml(t) => {
                let b = cur.get_or_insert_with(|| start(BlockKind::Para));
                b.text.push_str(&t);
            }
            Md::Code(t) => {
                let b = cur.get_or_insert_with(|| start(BlockKind::Para));
                let from = b.text.len();
                b.text.push_str(&t);
                b.spans.push((from..b.text.len(), Inline::Code));
            }
            Md::SoftBreak => {
                if let Some(b) = cur.as_mut() {
                    b.text.push(' ');
                }
            }
            Md::HardBreak => {
                if let Some(b) = cur.as_mut() {
                    b.text.push('\n');
                }
            }
            Md::Rule => {
                flush(&mut cur, &mut out);
                out.push(start(BlockKind::Rule));
            }
            _ => {}
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// What the stream thread hands the window.
pub enum Msg {
    Events(Vec<ChatEvent>),
    Closed(String),
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

/// Open `key`'s chat stream on the `session` server, from the first event.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("{0}")]
    Session(String),
    #[error(transparent)]
    Read(#[from] ChatReadError),
    #[error("chat stream: {0}")]
    Io(#[from] std::io::Error),
}

pub fn open(session: &str, key: SessionKey) -> Result<(Stream, UnboundedReceiver<Msg>), OpenError> {
    let socket = crate::proto::socket_path(session).map_err(OpenError::Session)?;
    let tail = tail_sync(&socket, key, 0)?;
    let closer = tail.closer()?;
    let (tx, rx) = unbounded_channel();
    std::thread::Builder::new()
        .name("fno-desktop-chat".into())
        .spawn(move || {
            for batch in tail {
                let msg = match batch {
                    Ok(events) => Msg::Events(events),
                    Err(e) => Msg::Closed(e.to_string()),
                };
                if tx.send(msg).is_err() {
                    return;
                }
            }
            let _ = tx.send(Msg::Closed("the chat stream ended".into()));
        })?;
    Ok((Stream { closer }, rx))
}

/// The focused pane's agent session, when the row names one.
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

/// The chat state the window keeps.
#[derive(Default)]
pub struct Chat {
    /// The session shown, `None` when the focused pane hosts no agent.
    pub key: Option<SessionKey>,
    pub stream: Option<Stream>,
    pub conv: Conversation,
    pub error: Option<String>,
    /// Tool calls and thinking the user expanded, by item index.
    pub expanded: HashSet<usize>,
    pub draft: String,
    pub scroll: ScrollHandle,
}

/// The pause between the pasted text and the Enter that submits it, the
/// same settle `fno mux pane send --submit` uses.
pub const SUBMIT_SETTLE: Duration =
    Duration::from_millis(crate::mux_cli::pane_submit::CR_SETTLE_MS);

fn styled(block: &Block, root: &Root) -> StyledText {
    let code_bg = rgb(root.palette.panel).into();
    let highlights = block.spans.iter().map(move |(range, style)| {
        let h = match style {
            Inline::Bold => HighlightStyle {
                font_weight: Some(FontWeight::BOLD),
                ..Default::default()
            },
            Inline::Italic => HighlightStyle {
                font_style: Some(FontStyle::Italic),
                ..Default::default()
            },
            Inline::Code => HighlightStyle {
                background_color: Some(code_bg),
                ..Default::default()
            },
        };
        (range.clone(), h)
    });
    StyledText::new(SharedString::from(block.text.clone())).with_highlights(highlights)
}

fn markdown(text: &str, root: &Root) -> Div {
    let p = root.palette;
    let mut out = div().flex().flex_col().gap_2();
    for block in blocks(text) {
        let body = div().child(styled(&block, root));
        out = out.child(match block.kind {
            BlockKind::Para => body,
            BlockKind::Heading(1) => body.text_xl().font_weight(FontWeight::BOLD),
            BlockKind::Heading(_) => body.text_lg().font_weight(FontWeight::BOLD),
            BlockKind::Item(depth) => div()
                .flex()
                .flex_row()
                .pl(px(16.0 * depth.saturating_sub(1) as f32))
                .child(div().w(px(16.0)).flex_none().child("•"))
                .child(body.flex_1().min_w_0()),
            BlockKind::Code => body
                .p_2()
                .rounded_md()
                .bg(rgb(p.panel))
                .font_family(root.metrics.font.family.clone())
                .whitespace_normal(),
            BlockKind::Quote => body
                .pl_3()
                .border_l_2()
                .border_color(rgb(p.border))
                .text_color(rgb(p.muted)),
            BlockKind::Rule => div().h(px(1.0)).bg(rgb(p.border)),
        });
    }
    out
}

/// A collapsed row: one line, a click opens it.
fn toggle_row(i: usize, label: String, color: u32, root: &Root, cx: &mut Context<Root>) -> Div {
    let fg = root.palette.fg;
    div().child(
        div()
            .id(ElementId::NamedInteger("chat-item".into(), i as u64))
            .cursor_pointer()
            .w_full()
            .line_clamp(1)
            .text_ellipsis()
            .text_color(rgb(color))
            .hover(move |s| s.text_color(rgb(fg)))
            .on_click(cx.listener(move |root, _, _, cx| root.act(Action::ToggleChat(i), cx)))
            .child(label),
    )
}

fn item(i: usize, it: &Item, root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let open = root.chat.expanded.contains(&i);
    let caret = if open { "▾" } else { "▸" };
    match it {
        Item::User { text, meta: true } => div()
            .text_xs()
            .text_color(rgb(p.muted))
            .line_clamp(1)
            .text_ellipsis()
            .child(text.clone()),
        Item::User { text, .. } => div().flex().flex_row().child(div().flex_1()).child(
            div()
                .max_w(px(640.0))
                .px_3()
                .py_2()
                .rounded_lg()
                .bg(rgb(p.border))
                .child(text.clone()),
        ),
        Item::Text { text, .. } => markdown(text, root),
        Item::Thinking { text, .. } => {
            let row = toggle_row(i, format!("{caret} thinking"), p.muted, root, cx);
            if open {
                row.child(div().pl_4().text_color(rgb(p.muted)).child(text.clone()))
            } else {
                row
            }
        }
        Item::Tool {
            name,
            input,
            result,
            ..
        } => {
            let (mark, color) = match result {
                None => ("…", p.muted),
                Some((true, _)) => ("✓", p.ok),
                Some((false, _)) => ("✗", p.error),
            };
            let first = input.lines().next().unwrap_or_default();
            let label = format!("{caret} {mark} {name}  {first}");
            let row = toggle_row(i, label, color, root, cx);
            if !open {
                return row;
            }
            let mut body = div()
                .ml_4()
                .p_2()
                .rounded_md()
                .bg(rgb(p.panel))
                .font_family(root.metrics.font.family.clone())
                .text_xs()
                .child(input.clone());
            if let Some((_, output)) = result {
                let cut = output
                    .char_indices()
                    .nth(OUTPUT_MAX)
                    .map_or(output.as_str(), |(at, _)| &output[..at]);
                body = body.child(div().pt_2().text_color(rgb(p.muted)).child(cut.to_string()));
            }
            row.child(body)
        }
        Item::Line(text) => div()
            .text_color(rgb(p.muted))
            .line_clamp(1)
            .text_ellipsis()
            .child(text.clone()),
    }
}

fn composer(root: &Root) -> Div {
    let p = root.palette;
    let preedit = root.preedit_text();
    let (text, color) = if root.chat.draft.is_empty() && preedit.is_empty() {
        (
            "Message the agent. Enter sends; Shift-Enter adds a line.".to_string(),
            p.muted,
        )
    } else {
        (format!("{}{preedit}▏", root.chat.draft), p.fg)
    };
    div().flex_none().p_3().child(
        div()
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(rgb(p.focus))
            .bg(rgb(p.panel))
            .text_color(rgb(color))
            .child(text),
    )
}

pub(super) fn render(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let mut list = div()
        .id("chat")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .track_scroll(&root.chat.scroll)
        .flex()
        .flex_col()
        .gap_3()
        .px_6()
        .py_4();
    let status = match (&root.chat.key, &root.chat.error) {
        (_, Some(e)) => Some((e.clone(), p.error)),
        (None, None) => Some((
            "The focused pane hosts no agent session. Pick an agent on the left.".into(),
            p.muted,
        )),
        (Some(_), None) if root.chat.conv.items.is_empty() => {
            Some(("Reading the conversation…".into(), p.muted))
        }
        _ => None,
    };
    for (i, it) in root.chat.conv.items.iter().enumerate() {
        list = list.child(item(i, it, root, cx));
    }
    if let Some((text, color)) = status {
        list = list.child(div().text_color(rgb(color)).child(text));
    }
    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .child(list)
        .child(composer(root))
}

/// The session id of the most recently written claude transcript.
fn newest_claude_session() -> Option<String> {
    let projects = std::fs::read_dir(crate::transcript_tail::claude_projects_dir()).ok()?;
    let files = projects
        .flatten()
        .filter_map(|d| std::fs::read_dir(d.path()).ok())
        .flat_map(|dir| dir.flatten());
    files
        .filter_map(|f| {
            let path = f.path();
            let id = path.file_stem()?.to_str()?.to_string();
            let transcript = path.extension().is_some_and(|e| e == "jsonl")
                && crate::transcript_tail::transcript_uuid_shaped(&id);
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
    let tail = match crate::proto::socket_path(session)
        .map_err(|e| e.to_string())
        .and_then(|socket| tail_sync(&socket, key, 0).map_err(|e| e.to_string()))
    {
        Ok(tail) => tail,
        Err(e) => {
            eprintln!("fno-desktop: {e}");
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
            eprintln!("fno-desktop: {why}");
            1
        }
        None if n == 0 => 1,
        None => 0,
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
        for (seq, body) in bodies.into_iter().enumerate() {
            let line = json!({"seq": seq, "source": {"type": "line", "offset": seq}, "body": body});
            conv.push(serde_json::from_value(line).unwrap());
        }
        assert_eq!(conv.events, 8);
        let tool = Item::Tool {
            call_id: "t1".into(),
            name: "Bash".into(),
            input: "ls".into(),
            result: Some((true, "a.txt".into())),
        };
        let user = Item::User {
            text: "hi".into(),
            meta: false,
        };
        let text = Item::Text {
            part: "p1".into(),
            text: "**hello** there".into(),
        };
        assert_eq!(conv.items[..3], [user, text, tool]);
        assert!(matches!(&conv.items[3], Item::Line(l) if l.starts_with("from_a_newer_server: ")));
        assert_eq!(conv.items.len(), 4);
    }

    #[test]
    fn chat_markdown_splits_blocks_and_styles_inline_runs() {
        let got =
            blocks("# Title\n\nSome **bold** and `code`.\n\n- one\n- two\n\n```\nfn x() {}\n```\n");
        let kinds: Vec<BlockKind> = got.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            vec![
                BlockKind::Heading(1),
                BlockKind::Para,
                BlockKind::Item(1),
                BlockKind::Item(1),
                BlockKind::Code
            ]
        );
        let para = &got[1];
        assert_eq!(para.text, "Some bold and code.");
        assert_eq!(
            para.spans,
            vec![(5..9, Inline::Bold), (14..18, Inline::Code)]
        );
        assert_eq!(got[3].text, "two");
        assert_eq!(got[4].text, "fn x() {}");
    }
}
