//! The composer's draft: text, a caret or selection, and the IME's marked
//! range. Offsets are bytes on grapheme boundaries; the input handler speaks
//! UTF-16, so the conversions live here too.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

#[derive(Default, Clone)]
pub struct Draft {
    text: String,
    selected: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    /// Bumped on every text change, so a stored layout can tell it is stale.
    revision: u64,
}

impl Draft {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub fn selection(&self) -> Range<usize> {
        self.selected.clone()
    }

    pub fn has_selection(&self) -> bool {
        !self.selected.is_empty()
    }

    /// The end of the selection that moves.
    pub fn caret(&self) -> usize {
        if self.reversed {
            self.selected.start
        } else {
            self.selected.end
        }
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.selected.clone()]
    }

    /// Hand the text over and reset, as a send does.
    pub fn take(&mut self) -> String {
        self.selected = 0..0;
        self.reversed = false;
        self.marked = None;
        self.revision += 1;
        std::mem::take(&mut self.text)
    }

    fn snap(&self, at: usize) -> usize {
        let at = at.min(self.text.len());
        if self.text.is_char_boundary(at) && self.is_grapheme_boundary(at) {
            return at;
        }
        let mut best = self.text.len();
        for (i, _) in self.text.grapheme_indices(true) {
            if i >= at {
                best = i;
                break;
            }
        }
        // Prefer the nearer boundary below when it is closer.
        let below = self.text[..at]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i);
        if at - below <= best - at {
            below
        } else {
            best
        }
    }

    fn is_grapheme_boundary(&self, at: usize) -> bool {
        at == 0 || at == self.text.len() || self.text.grapheme_indices(true).any(|(i, _)| i == at)
    }

    fn set_caret(&mut self, at: usize, extend: bool) {
        let at = self.snap(at);
        if extend {
            let anchor = if self.reversed {
                self.selected.end
            } else {
                self.selected.start
            };
            self.reversed = at < anchor;
            self.selected = at.min(anchor)..at.max(anchor);
        } else {
            self.selected = at..at;
            self.reversed = false;
        }
    }

    pub fn move_to(&mut self, at: usize, extend: bool) {
        self.set_caret(at, extend);
    }

    pub fn select_all(&mut self) {
        self.selected = 0..self.text.len();
        self.reversed = false;
    }

    /// Select the word under `at`, or the run of spaces there.
    pub fn select_word_at(&mut self, at: usize) {
        let at = self.snap(at);
        let mut start = 0;
        let mut end = self.text.len();
        for (i, w) in self.text.split_word_bound_indices() {
            if i <= at && at <= i + w.len() {
                start = i;
                end = i + w.len();
                if at < i + w.len() {
                    break;
                }
            }
        }
        self.selected = start..end;
        self.reversed = false;
    }

    fn prev(&self, at: usize) -> usize {
        self.text[..at]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next(&self, at: usize) -> usize {
        self.text[at..]
            .graphemes(true)
            .next()
            .map_or(self.text.len(), |g| at + g.len())
    }

    pub fn left(&mut self, extend: bool) {
        if !extend && self.has_selection() {
            self.set_caret(self.selected.start, false);
        } else {
            self.set_caret(self.prev(self.caret()), extend);
        }
    }

    pub fn right(&mut self, extend: bool) {
        if !extend && self.has_selection() {
            self.set_caret(self.selected.end, false);
        } else {
            self.set_caret(self.next(self.caret()), extend);
        }
    }

    pub fn word_left(&mut self, extend: bool) {
        let at = self.caret();
        let to = self.text[..at]
            .split_word_bound_indices()
            .rev()
            .find(|(_, w)| w.chars().any(char::is_alphanumeric))
            .map_or(0, |(i, _)| i);
        self.set_caret(to, extend);
    }

    pub fn word_right(&mut self, extend: bool) {
        let at = self.caret();
        let to = self.text[at..]
            .split_word_bound_indices()
            .find(|(_, w)| w.chars().any(char::is_alphanumeric))
            .map_or(self.text.len(), |(i, w)| at + i + w.len());
        self.set_caret(to, extend);
    }

    /// The start of the caret's `\n`-delimited line.
    pub fn home(&mut self, extend: bool) {
        let at = self.caret();
        let to = self.text[..at].rfind('\n').map_or(0, |i| i + 1);
        self.set_caret(to, extend);
    }

    pub fn end(&mut self, extend: bool) {
        let at = self.caret();
        let to = self.text[at..]
            .find('\n')
            .map_or(self.text.len(), |i| at + i);
        self.set_caret(to, extend);
    }

    pub fn doc_start(&mut self, extend: bool) {
        self.set_caret(0, extend);
    }

    pub fn doc_end(&mut self, extend: bool) {
        self.set_caret(self.text.len(), extend);
    }

    /// Replace the selection (or insert at the caret) and put the caret after.
    pub fn replace_selection(&mut self, text: &str) {
        let range = self.selected.clone();
        self.splice(range, text);
        self.marked = None;
    }

    fn splice(&mut self, range: Range<usize>, text: &str) {
        self.text.replace_range(range.clone(), text);
        let at = range.start + text.len();
        self.selected = at..at;
        self.reversed = false;
        self.revision += 1;
    }

    pub fn backspace(&mut self) {
        if !self.has_selection() {
            let at = self.caret();
            self.selected = self.prev(at)..at;
        }
        self.replace_selection("");
    }

    pub fn delete(&mut self) {
        if !self.has_selection() {
            let at = self.caret();
            self.selected = at..self.next(at);
        }
        self.replace_selection("");
    }

    pub fn offset_to_utf16(&self, offset: usize) -> usize {
        self.text[..offset.min(self.text.len())]
            .encode_utf16()
            .count()
    }

    pub fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut units = 0;
        for (i, c) in self.text.char_indices() {
            if units >= offset {
                return self.snap(i);
            }
            units += c.len_utf16();
        }
        self.text.len()
    }

    pub fn range_to_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(r.start)..self.offset_to_utf16(r.end)
    }

    pub fn range_from_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(r.start)..self.offset_from_utf16(r.end)
    }

    /// The input handler's write: replace `range` (UTF-16, default the marked
    /// range, else the selection). `mark` keeps the new text as marked text.
    pub fn replace_utf16(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        mark: bool,
        new_selected: Option<Range<usize>>,
    ) {
        let range = range
            .map(|r| self.range_from_utf16(&r))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selected.clone());
        self.splice(range.clone(), text);
        if mark && !text.is_empty() {
            let end = range.start + text.len();
            self.marked = Some(range.start..end);
            if let Some(sel) = new_selected {
                let base = self.offset_to_utf16(range.start);
                let a = self.offset_from_utf16(base + sel.start);
                let b = self.offset_from_utf16(base + sel.end);
                self.selected = a.min(b)..a.max(b);
            }
        } else {
            self.marked = None;
        }
    }

    pub fn unmark(&mut self) {
        self.marked = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(text: &str, at: usize) -> Draft {
        let mut d = Draft::default();
        d.replace_selection(text);
        d.move_to(at, false);
        d
    }

    #[test]
    fn insert_lands_at_the_caret() {
        let mut d = draft("helo world", 3);
        d.replace_selection("l");
        assert_eq!(d.text(), "hello world");
        assert_eq!(d.caret(), 4);
    }

    #[test]
    fn backspace_removes_one_grapheme_mid_text() {
        let mut d = draft("ae\u{301}b", 4);
        d.backspace();
        assert_eq!(d.text(), "ab");
        assert_eq!(d.caret(), 1);
    }

    #[test]
    fn typing_over_a_selection_replaces_it() {
        let mut d = draft("hello world", 6);
        d.move_to(11, true);
        assert_eq!(d.selected_text(), "world");
        d.replace_selection("there");
        assert_eq!(d.text(), "hello there");
    }

    #[test]
    fn word_moves_skip_spaces() {
        let mut d = draft("one two three", 13);
        d.word_left(false);
        assert_eq!(d.caret(), 8);
        d.word_left(false);
        assert_eq!(d.caret(), 4);
        d.word_right(false);
        assert_eq!(d.caret(), 7);
    }

    #[test]
    fn utf16_offsets_round_trip_and_snap_inside_a_pair() {
        let mut d = draft("a😀日本", 0);
        let end = d.text().len();
        let u = d.offset_to_utf16(end);
        assert_eq!(u, 5);
        assert_eq!(d.offset_from_utf16(u), end);
        // UTF-16 offset 2 is inside the emoji's surrogate pair.
        d.replace_utf16(Some(2..2), "x", false, None);
        assert!(d.text().starts_with('a') && d.text().contains('x'));
    }

    #[test]
    fn marked_text_replaces_then_commits() {
        let mut d = draft("ab", 1);
        d.replace_utf16(None, "に", true, None);
        assert_eq!(d.marked(), Some(1..4));
        d.replace_utf16(None, "日", true, None);
        assert_eq!(d.text(), "a日b");
        d.replace_utf16(None, "日本", false, None);
        assert_eq!(d.text(), "a日本b");
        assert_eq!(d.marked(), None);
    }
}
