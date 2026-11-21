//! The client-side scan behind chat find: pure match ranges over the text
//! each row draws. No server round trip - chat text lives only in the app,
//! and the scan reruns per keystroke so a streaming conversation stays
//! findable.

use std::collections::HashSet;
use std::ops::Range;

use super::markdown::Block;
use super::model::Item;

/// One match: row `item`, its `block` in `Item::blocks()` (0 for rows drawn
/// as one plain text), and the byte range inside that text.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatMatch {
    pub item: usize,
    pub block: usize,
    pub range: Range<usize>,
}

/// Case-insensitive, non-overlapping matches over the drawn text: the
/// markdown block runs and code of Text rows and open Thinking rows,
/// `content.text` for User, `summary` for Tool, the line for Line. Collapsed
/// Thinking rows are never searched: their text is not drawn. Oldest row
/// first, so the newest match is the last.
pub fn chat_matches(items: &[Item], expanded: &HashSet<usize>, query: &str) -> Vec<ChatMatch> {
    let mut out = Vec::new();
    let needle: Vec<char> = query.to_lowercase().chars().collect();
    if needle.is_empty() {
        return out;
    }
    for (ix, item) in items.iter().enumerate() {
        match item {
            Item::Text { .. } | Item::Thinking { .. }
                if match item {
                    Item::Thinking { .. } => expanded.contains(&ix),
                    _ => true,
                } =>
            {
                scan_blocks(item, ix, &needle, &mut out)
            }
            Item::User { content, .. } => scan_text(ix, 0, &content.text, &needle, &mut out),
            Item::Tool { summary, .. } => scan_text(ix, 0, summary, &needle, &mut out),
            Item::Line(text) => scan_text(ix, 0, text, &needle, &mut out),
            _ => {}
        }
    }
    out
}

fn scan_blocks(item: &Item, ix: usize, needle: &[char], out: &mut Vec<ChatMatch>) {
    let blocks = item.blocks().unwrap_or_default();
    for (b, block) in blocks.iter().enumerate() {
        match block {
            Block::Para(runs)
            | Block::Heading(_, runs)
            | Block::Item { runs, .. }
            | Block::Quote(runs) => scan_text(ix, b, &runs.text, needle, out),
            Block::Code(code) => scan_text(ix, b, code, needle, out),
            Block::Table { head, rows } => {
                for runs in head.iter().chain(rows.iter().flatten()) {
                    scan_text(ix, b, &runs.text, needle, out);
                }
            }
            Block::Rule | Block::Image { .. } => {}
        }
    }
}

/// All non-overlapping case-insensitive ranges of `needle` in `text`, as
/// byte ranges of the original (span cuts and run lengths are bytes).
/// Per-char lowercasing reads the first char of a multi-char fold, the same
/// single-pass approximation the server's row scan makes.
fn scan_text(item: usize, block: usize, text: &str, needle: &[char], out: &mut Vec<ChatMatch>) {
    let n = needle.len();
    if n == 0 {
        return;
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < n {
        return;
    }
    let lower: Vec<char> = chars
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    let mut at = 0;
    while at + n <= lower.len() {
        if lower.get(at..at + n) == Some(needle) {
            let range = byte_at(text, at)..byte_at(text, at + n);
            out.push(ChatMatch { item, block, range });
            at += n;
        } else {
            at += 1;
        }
    }
}

/// The byte offset where the `ci`th char starts; one past the end when the
/// index lands on the sentinel.
fn byte_at(text: &str, ci: usize) -> usize {
    text.char_indices().nth(ci).map_or(text.len(), |(b, _)| b)
}

#[cfg(test)]
mod tests {
    use std::cell::OnceCell;

    use super::super::model::Content;
    use super::*;

    fn text_row(text: &str) -> Item {
        Item::Text {
            part: "p".into(),
            text: text.into(),
            md: OnceCell::new(),
        }
    }

    #[test]
    fn two_of_three_rows_hold_the_needle_and_the_newest_is_last() {
        let items = vec![
            text_row("merge main first"),
            text_row("nothing here"),
            text_row("Merge it again"),
        ];
        let hits = chat_matches(&items, &HashSet::new(), "merge");
        assert_eq!(
            hits,
            vec![
                ChatMatch {
                    item: 0,
                    block: 0,
                    range: 0..5
                },
                ChatMatch {
                    item: 2,
                    block: 0,
                    range: 0..5
                },
            ]
        );
    }

    #[test]
    fn a_collapsed_thinking_row_is_never_searched_and_an_open_one_is() {
        let items = vec![Item::Thinking {
            part: "p".into(),
            text: "hidden merge".into(),
            md: OnceCell::new(),
            redacted: false,
        }];
        assert!(chat_matches(&items, &HashSet::new(), "merge").is_empty());
        let mut open = HashSet::new();
        open.insert(0);
        let hits = chat_matches(&items, &open, "merge");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits.first().map(|h| h.range.clone()), Some(7..12));
    }

    #[test]
    fn user_tool_and_line_rows_scan_the_text_they_draw() {
        let items = vec![
            Item::User {
                content: Content {
                    text: "please merge".into(),
                    pictures: vec![],
                },
                meta: false,
            },
            Item::Tool {
                call_id: "c".into(),
                name: "Bash".into(),
                summary: "git merge main".into(),
                input: "{}".into(),
                result: None,
            },
            Item::Line("merged".into()),
        ];
        let hits = chat_matches(&items, &HashSet::new(), "merge");
        assert_eq!(
            hits.iter()
                .map(|h| (h.item, h.range.start))
                .collect::<Vec<_>>(),
            vec![(0, 7), (1, 4), (2, 0)]
        );
    }
}
