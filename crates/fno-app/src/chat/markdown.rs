//! Markdown to the blocks the chat view draws: text runs with styled byte
//! ranges, code, tables and images. gpui 0.2.2 ships no markdown element.

use std::ops::Range;

use pulldown_cmark::{Event as Md, Options, Parser, Tag, TagEnd};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Bold,
    Italic,
    Code,
    Strike,
    Link(String),
}

/// Text and the styled byte ranges inside it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Runs {
    pub text: String,
    pub spans: Vec<(Range<usize>, Inline)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Para(Runs),
    Heading(u8, Runs),
    /// A list item at its depth (1 for the top level) with its marker: a
    /// bullet, a number, or a task box.
    Item {
        depth: usize,
        marker: String,
        runs: Runs,
    },
    Code(String),
    Quote(Runs),
    Rule,
    Table {
        head: Vec<Runs>,
        rows: Vec<Vec<Runs>>,
    },
    Image {
        url: String,
        alt: String,
    },
}

enum Open {
    Para,
    Heading(u8),
    Item { depth: usize, marker: String },
}

#[derive(Default)]
struct Builder {
    out: Vec<Block>,
    cur: Option<(Open, Runs)>,
    spans: Vec<(Inline, usize)>,
    /// The next number of each open list; `None` for a bullet list.
    lists: Vec<Option<u64>>,
    quote: bool,
    code: Option<String>,
    table: Option<(Vec<Runs>, Vec<Vec<Runs>>)>,
    head: bool,
    cell: Option<Runs>,
    image: Option<(String, String)>,
}

impl Builder {
    fn flush(&mut self) {
        let Some((open, mut runs)) = self.cur.take() else {
            return;
        };
        runs.text.truncate(runs.text.trim_end().len());
        let len = runs.text.len();
        runs.spans.retain_mut(|(r, _)| {
            r.end = r.end.min(len);
            r.start < r.end
        });
        if runs.text.is_empty() {
            return;
        }
        self.out.push(match open {
            Open::Para if self.quote => Block::Quote(runs),
            Open::Para => Block::Para(runs),
            Open::Heading(level) => Block::Heading(level, runs),
            Open::Item { depth, marker } => Block::Item {
                depth,
                marker,
                runs,
            },
        });
    }

    /// Where inline text lands: the open table cell, else the open block.
    fn target(&mut self) -> &mut Runs {
        if let Some(cell) = self.cell.as_mut() {
            return cell;
        }
        &mut self
            .cur
            .get_or_insert_with(|| (Open::Para, Runs::default()))
            .1
    }

    fn text(&mut self, t: &str) {
        if let Some((_, alt)) = self.image.as_mut() {
            alt.push_str(t);
        } else if let Some(code) = self.code.as_mut() {
            code.push_str(t);
        } else {
            self.target().text.push_str(t);
        }
    }

    fn start_span(&mut self, style: Inline) {
        let at = self.target().text.len();
        self.spans.push((style, at));
    }

    fn end_span(&mut self) {
        if let Some((style, from)) = self.spans.pop() {
            let runs = self.target();
            let to = runs.text.len();
            runs.spans.push((from..to, style));
        }
    }

    fn event(&mut self, ev: Md) {
        match ev {
            Md::Start(Tag::Paragraph) => {
                if self.cur.is_none() && self.cell.is_none() {
                    self.cur = Some((Open::Para, Runs::default()));
                }
            }
            Md::End(TagEnd::Paragraph) => match &mut self.cur {
                Some((Open::Item { .. }, runs)) => runs.text.push('\n'),
                _ => self.flush(),
            },
            Md::Start(Tag::Heading { level, .. }) => {
                self.flush();
                self.cur = Some((Open::Heading(level as u8), Runs::default()));
            }
            Md::End(TagEnd::Heading(_)) | Md::End(TagEnd::Item) => self.flush(),
            Md::Start(Tag::BlockQuote(_)) => {
                self.flush();
                self.quote = true;
            }
            Md::End(TagEnd::BlockQuote(_)) => {
                self.flush();
                self.quote = false;
            }
            // The item's own text comes first, so a fence inside it never
            // swallows it.
            Md::Start(Tag::CodeBlock(_)) => {
                self.flush();
                self.code = Some(String::new());
            }
            Md::End(TagEnd::CodeBlock) => {
                if let Some(code) = self.code.take() {
                    self.out.push(Block::Code(code.trim_end().to_string()));
                }
            }
            Md::Start(Tag::List(start)) => {
                self.flush();
                self.lists.push(start);
            }
            Md::End(TagEnd::List(_)) => {
                self.flush();
                self.lists.pop();
            }
            Md::Start(Tag::Item) => {
                self.flush();
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        format!("{}.", *n - 1)
                    }
                    _ => "•".to_string(),
                };
                let depth = self.lists.len();
                self.cur = Some((Open::Item { depth, marker }, Runs::default()));
            }
            Md::TaskListMarker(done) => {
                if let Some((Open::Item { marker, .. }, _)) = &mut self.cur {
                    *marker = if done { "☑" } else { "☐" }.to_string();
                }
            }
            Md::Start(Tag::Emphasis) => self.start_span(Inline::Italic),
            Md::Start(Tag::Strong) => self.start_span(Inline::Bold),
            Md::Start(Tag::Strikethrough) => self.start_span(Inline::Strike),
            Md::Start(Tag::Link { dest_url, .. }) => self.start_span(Inline::Link(dest_url.into())),
            Md::End(TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link) => {
                self.end_span()
            }
            Md::Start(Tag::Image { dest_url, .. }) => {
                self.image = Some((dest_url.into(), String::new()));
            }
            Md::End(TagEnd::Image) => {
                let Some((url, alt)) = self.image.take() else {
                    return;
                };
                if self.cell.is_some() {
                    self.target().text.push_str(&alt);
                } else {
                    self.flush();
                    self.out.push(Block::Image { url, alt });
                }
            }
            Md::Start(Tag::Table(_)) => {
                self.flush();
                self.table = Some((Vec::new(), Vec::new()));
            }
            Md::Start(Tag::TableHead) => self.head = true,
            Md::End(TagEnd::TableHead) => self.head = false,
            Md::Start(Tag::TableRow) => {
                if let Some((_, rows)) = self.table.as_mut() {
                    rows.push(Vec::new());
                }
            }
            Md::Start(Tag::TableCell) => self.cell = Some(Runs::default()),
            Md::End(TagEnd::TableCell) => {
                let (Some(mut cell), Some((head, rows))) = (self.cell.take(), self.table.as_mut())
                else {
                    return;
                };
                cell.text.truncate(cell.text.trim_end().len());
                match (self.head, rows.last_mut()) {
                    (true, _) | (false, None) => head.push(cell),
                    (false, Some(row)) => row.push(cell),
                }
            }
            Md::End(TagEnd::Table) => {
                if let Some((head, rows)) = self.table.take() {
                    self.out.push(Block::Table { head, rows });
                }
            }
            Md::Text(t) | Md::Html(t) | Md::InlineHtml(t) => self.text(&t),
            Md::Code(t) => {
                let runs = self.target();
                let from = runs.text.len();
                runs.text.push_str(&t);
                let to = runs.text.len();
                runs.spans.push((from..to, Inline::Code));
            }
            Md::SoftBreak => self.text(" "),
            Md::HardBreak => self.text("\n"),
            Md::Rule => {
                self.flush();
                self.out.push(Block::Rule);
            }
            _ => {}
        }
    }
}

pub fn blocks(md: &str) -> Vec<Block> {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut b = Builder::default();
    for ev in Parser::new_ext(md, opts) {
        b.event(ev);
    }
    b.flush();
    b.out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;

    fn runs(text: &str, spans: Vec<(Range<usize>, Inline)>) -> Runs {
        Runs {
            text: text.into(),
            spans,
        }
    }

    #[test]
    fn chat_markdown_splits_blocks_tables_lists_and_inline_runs() {
        let md = "# Title\n\nSome **bold**, `code` and [a link](https://x.dev).\n\n\
                  2. two\n3. three\n   ```\n   fn x() {}\n   ```\n\n\
                  | a | b |\n|---|---|\n| 1 | ~~2~~ |\n\n- [x] done\n";
        let got = blocks(md);
        assert_eq!(got[0], Block::Heading(1, runs("Title", vec![])));
        let link = Inline::Link("https://x.dev".into());
        assert_eq!(
            got[1],
            Block::Para(runs(
                "Some bold, code and a link.",
                vec![(5..9, Inline::Bold), (11..15, Inline::Code), (20..26, link)]
            ))
        );
        let item = |marker: &str, text: &str| Block::Item {
            depth: 1,
            marker: marker.into(),
            runs: runs(text, vec![]),
        };
        assert_eq!(got[2], item("2.", "two"));
        // The fence inside item 3 keeps the item's own text.
        assert_eq!(got[3], item("3.", "three"));
        assert_eq!(got[4], Block::Code("fn x() {}".into()));
        assert_eq!(
            got[5],
            Block::Table {
                head: vec![runs("a", vec![]), runs("b", vec![])],
                rows: vec![vec![
                    runs("1", vec![]),
                    runs("2", vec![(0..1, Inline::Strike)])
                ]],
            }
        );
        assert_eq!(got[6], item("☑", "done"));
        assert_eq!(got.len(), 7);
    }
}
