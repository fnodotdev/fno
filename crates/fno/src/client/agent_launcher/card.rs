//! The draft card: the composer drawn as sideline rows inside its
//! workspace's group. [`Launcher::card_lines`] is the one source the paint,
//! the editor cursor and the hit test all read, so they cannot disagree.

use super::*;

/// Columns each side of the card text: the frame's edge and one of air.
const CARD_PAD: usize = 2;

/// Greedy word wrap: a line breaks at a space, and only a word wider than
/// the line breaks inside itself.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        while cols(&word) > width {
            let mut head = String::new();
            let mut rest = String::new();
            for c in word.chars() {
                if rest.is_empty() && cols(&head) + cols(&c.to_string()) <= width {
                    head.push(c);
                } else {
                    rest.push(c);
                }
            }
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            out.push(head);
            word = rest;
        }
        if !cur.is_empty() && cols(&cur) + 1 + cols(&word) > width {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(&word);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Display columns, so a wide glyph in a folder name keeps tap spans and the
/// focus band aligned with the cursor column.
fn cols(s: &str) -> usize {
    s.chars()
        .map(|c| usize::from(UnicodeWidthChar::width(c).unwrap_or(0)))
        .sum()
}

/// Columns of air before a picker row's glyph; the selection band starts at
/// the text, not at the margin.
const ROW_INDENT: usize = 3;
/// Wrapped prompt rows the card shows before the window follows the cursor.
const PROMPT_ROWS: usize = 6;
/// Picker rows the card unfolds before the list scrolls with the selection.
const PICKER_ROWS: usize = 10;

/// A weight laid over a run of columns of one card line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mark {
    Dim,
    Bold,
    /// Bold in the accent color: the primary action.
    Accent,
}

/// What a tap on a card line does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardHit {
    Prompt,
    Chip(Focus),
    /// A row of the unfolded picker, as a popup target index.
    Pick(usize),
    More,
    Start,
    Cancel,
    Close,
    /// The will-run line: a tap edits it.
    WillRun,
    /// The x on pill n.
    Pill(usize),
}

/// One painted card line and the columns that answer a tap.
#[derive(Debug, Default)]
pub(crate) struct CardLine {
    pub text: String,
    pub dim: bool,
    /// (first column, one past the last, what it does).
    pub spans: Vec<(usize, usize, CardHit)>,
    /// The focused token's columns, painted as the filled chip.
    pub focused: Option<(usize, usize)>,
    /// The editor cursor's column on this line.
    pub cursor: Option<usize>,
    /// A refusal or an unknown outcome: painted in the attention color.
    pub alert: bool,
    /// The frame's side cells (first and last column) wear the border
    /// color; with `rule` the whole line does.
    pub frame: bool,
    pub rule: bool,
    /// (first column, one past the last, weight) runs inside the line.
    pub marks: Vec<(usize, usize, Mark)>,
}

impl CardLine {
    fn plain(text: String, dim: bool) -> Self {
        Self {
            text,
            dim,
            ..Self::default()
        }
    }

    pub(crate) fn hit_at(&self, col: usize) -> Option<CardHit> {
        self.spans
            .iter()
            .find(|(a, b, _)| col >= *a && col < *b)
            .map(|(_, _, h)| *h)
    }
}

/// One value that flows onto the line: its text, the spans inside it
/// (relative to its first column) and whether it holds the keyboard.
struct Token {
    text: String,
    spans: Vec<(usize, usize, CardHit)>,
    marks: Vec<(usize, usize, Mark)>,
    focused: bool,
}

/// Lay tokens onto lines of at most `w` columns, a new line whenever the
/// next token does not fit. A value is never cut to make a row; only a token
/// wider than the whole row clips.
fn flow(tokens: Vec<Token>, sep: &str, w: usize) -> Vec<CardLine> {
    let mut lines = Vec::new();
    let mut cur = CardLine::default();
    let mut col = 0usize;
    for t in tokens {
        let want = cols(&t.text);
        let sep_w = cols(sep);
        if col > 0 && col + sep_w + want > w {
            lines.push(std::mem::take(&mut cur));
            col = 0;
        } else if col > 0 {
            cur.text.push_str(sep);
            col += sep_w;
        }
        let (text, tw) = if want > w {
            (crate::chrome::clip(&t.text, w), w)
        } else {
            (t.text.clone(), want)
        };
        for (a, b, h) in &t.spans {
            cur.spans.push((col + (*a).min(tw), col + (*b).min(tw), *h));
        }
        for (a, b, m) in &t.marks {
            cur.marks.push((col + (*a).min(tw), col + (*b).min(tw), *m));
        }
        if t.focused {
            cur.focused = Some((col, col + tw));
        }
        cur.text.push_str(&text);
        col += tw;
    }
    if col > 0 || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

fn chip_token(l: &Launcher, f: Focus, catalog: &Option<CatalogOutcome>, labelled: bool) -> Token {
    let value = l.chip_label(f, catalog);
    // The label names the axis in dim type; the value keeps normal weight.
    let label = match f {
        Focus::Harness => "harness",
        Focus::Model => "model",
        Focus::Effort => "effort",
        Focus::Permission => "perms",
        Focus::Project => "folder",
        Focus::Branch => "branch",
        Focus::Where => "where",
        Focus::Worktree => "worktree",
        _ => "",
    };
    let keep = !labelled && value == "default" && !label.is_empty();
    let (text, marks) = if label.is_empty() || (!labelled && !keep) {
        (value, Vec::new())
    } else {
        let dim = vec![(0, cols(label), Mark::Dim)];
        (format!("{label} {value}"), dim)
    };
    let n = cols(&text);
    let open = l.focus == f && l.picker.is_some();
    let mut marks = marks;
    if open {
        marks.push((0, n, Mark::Bold));
    }
    Token {
        spans: vec![(0, n, CardHit::Chip(f))],
        marks,
        focused: l.focus == f && !open,
        text,
    }
}

/// The unfolded picker as card lines: its rows, the selection marked, the
/// list windowed around the selection when it runs long. An entry whose hint
/// does not fit beside its label wraps the hint onto the next line, so the
/// narrow sideline never cuts a hint.
fn picker_lines(pk: &Picker, w: usize) -> Vec<CardLine> {
    let targets = pk.popup.targets();
    let sel = pk.popup.selected();
    let mut lines: Vec<CardLine> = Vec::new();
    let mut sel_line = None;
    // The popup's title carries the axis name and the live filter query.
    let title = &pk.popup.chrome.title;
    if !title.is_empty() {
        lines.push(CardLine::plain(
            crate::chrome::clip(&format!("  {title}"), w),
            true,
        ));
    }
    for (ri, row) in pk.popup.rows.iter().enumerate() {
        let entry = |glyph: &str, label: &str, hint: &str, enabled: bool| -> Vec<CardLine> {
            let target = targets.iter().position(|&(r, c)| r == ri && c == 0);
            let marked = sel == Some((ri, 0));
            // The band is the only selection mark: no arrow, no check. The
            // state bullets share one size.
            let glyph = match glyph {
                "" | "\u{2713}" => String::new(),
                "\u{25cf}" => "\u{2022} ".to_string(),
                g => format!("{g} "),
            };
            let mark = ' ';
            let spans = target
                .filter(|_| enabled)
                .map(|t| vec![(0, w, CardHit::Pick(t))])
                .unwrap_or_default();
            let whole = format!(" {mark} {glyph}{label}  {hint}");
            let mut out = Vec::new();
            if hint.is_empty() || cols(&whole) <= w {
                let text = crate::chrome::clip(whole.trim_end(), w);
                let n = cols(&text);
                out.push(CardLine {
                    text,
                    dim: !enabled,
                    spans,
                    focused: marked.then_some((ROW_INDENT.min(n.saturating_sub(1)), n.max(1))),
                    cursor: None,
                    alert: false,
                    frame: false,
                    rule: false,
                    marks: Vec::new(),
                });
            } else {
                let head = crate::chrome::clip(&format!(" {mark} {glyph}{label}"), w);
                let n = cols(&head);
                out.push(CardLine {
                    text: head,
                    dim: !enabled,
                    spans: spans.clone(),
                    focused: marked.then_some((ROW_INDENT.min(n.saturating_sub(1)), n.max(1))),
                    cursor: None,
                    alert: false,
                    frame: false,
                    rule: false,
                    marks: Vec::new(),
                });
                out.push(CardLine {
                    text: crate::chrome::clip(&format!("     {hint}"), w),
                    dim: true,
                    spans,
                    focused: None,
                    cursor: None,
                    alert: false,
                    frame: false,
                    rule: false,
                    marks: Vec::new(),
                });
            }
            out
        };
        let new: Vec<CardLine> = match row {
            PopupRow::Header(t) | PopupRow::Text(t) => {
                vec![CardLine::plain(
                    crate::chrome::clip(&format!("  {t}"), w),
                    true,
                )]
            }
            PopupRow::Rule => vec![CardLine::plain(
                format!("  {}", "\u{2504}".repeat(w.saturating_sub(4))),
                true,
            )],
            PopupRow::Info { label, value } => vec![CardLine::plain(
                crate::chrome::clip(&format!("  {label}: {value}"), w),
                false,
            )],
            PopupRow::Input { label, text, .. } => vec![CardLine::plain(
                crate::chrome::clip(&format!("  {label}: {text}"), w),
                false,
            )],
            PopupRow::Entry {
                glyph,
                label,
                hint,
                enabled,
            }
            | PopupRow::SwatchEntry {
                glyph,
                label,
                hint,
                enabled,
                ..
            } => entry(glyph, label, hint, *enabled),
            PopupRow::FullWidth(label) => entry("", label, "", true),
            _ => continue,
        };
        if sel_line.is_none() && new.iter().any(|l| l.focused.is_some()) {
            sel_line = Some(lines.len());
        }
        lines.extend(new);
    }
    if lines.len() > PICKER_ROWS {
        let at = sel_line.unwrap_or(0).saturating_sub(PICKER_ROWS / 2);
        let start = at.min(lines.len() - PICKER_ROWS);
        lines = lines.drain(start..start + PICKER_ROWS).collect();
    }
    lines
}

impl Launcher {
    /// The will-run line: the literal command Enter runs. At rest it is one
    /// clipped line; while it holds the keyboard it wraps in full with a
    /// cursor, and an edit that did not parse says why under it. A draft the
    /// card would refuse shows the refusal instead.
    fn will_run_lines(&self, view: &View, w: usize) -> Vec<CardLine> {
        let room = w.saturating_sub(2).max(1);
        if let Some(edit) = &self.will_edit {
            let rows = wrap_message(&edit.text, room);
            let last = rows.len() - 1;
            let mut out: Vec<CardLine> = rows
                .into_iter()
                .enumerate()
                .map(|(k, (_, chunk))| {
                    let cursor = (k == last).then(|| 2 + cols(&chunk));
                    CardLine {
                        text: format!("{}{chunk}", if k == 0 { "$ " } else { "  " }),
                        spans: vec![(0, w, CardHit::WillRun)],
                        cursor,
                        ..CardLine::default()
                    }
                })
                .collect();
            if let Some(error) = &edit.error {
                out.push(CardLine {
                    alert: true,
                    ..CardLine::plain(crate::chrome::clip(&format!("  {error}"), w), false)
                });
            }
            return out;
        }
        match will_run::line(view) {
            Ok(text) => {
                let shown = if cols(&text) > room {
                    format!(
                        "{}\u{2026}",
                        crate::chrome::clip(&text, room.saturating_sub(1))
                    )
                } else {
                    text
                };
                vec![CardLine {
                    spans: vec![(0, w, CardHit::WillRun)],
                    ..CardLine::plain(format!("$ {shown}"), true)
                }]
            }
            Err(reason) => vec![CardLine {
                alert: true,
                ..CardLine::plain(crate::chrome::clip(&format!("  {reason}"), w), false)
            }],
        }
    }

    /// The project's launch facts, for the line under Branch focus.
    fn branch_facts(&self, view: &View) -> String {
        match self
            .draft
            .facts(&view.launcher_catalog)
            .map(|f| (f.policy.as_deref(), f.current.as_deref()))
        {
            Some((Ok("never"), _)) => "policy never: runs in place".to_string(),
            Some((Ok(word), Some(current))) => format!("policy {word} \u{b7} branch {current}"),
            Some((Ok(word), None)) => format!("policy {word} \u{b7} branch ?"),
            Some((Err(e), _)) => format!("policy unread: {e}"),
            None => "reading project facts...".to_string(),
        }
    }

    /// The line under the summary while a project token holds the keyboard:
    /// the full folder, or the launch facts a greyed worktree box explains by.
    fn facts_line(&self, view: &View) -> Option<String> {
        match self.focus {
            Focus::Project => Some(format!("working directory {}", self.draft.cwd())),
            Focus::Branch | Focus::Worktree => Some(self.branch_facts(view)),
            _ => None,
        }
    }

    /// Every line of the card at width `w`, top to bottom: the prompt, the
    /// lifecycle reason, the pills, the three summary lines, the unfolded
    /// picker, the key line and the buttons.
    pub(crate) fn card_lines(&self, view: &View, w: usize) -> Vec<CardLine> {
        let w = w.max(12);
        let inner = w - 2 * CARD_PAD;
        let mut lines = self.card_lines_flush(view, inner);
        for l in &mut lines {
            let fill = inner.saturating_sub(cols(&l.text));
            l.text = format!("\u{2502} {}{} \u{2502}", l.text, " ".repeat(fill));
            for s in &mut l.spans {
                s.0 += CARD_PAD;
                s.1 += CARD_PAD;
            }
            for m in &mut l.marks {
                m.0 += CARD_PAD;
                m.1 += CARD_PAD;
            }
            if let Some(f) = l.focused.as_mut() {
                f.0 += CARD_PAD;
                f.1 += CARD_PAD;
            }
            if let Some(c) = l.cursor.as_mut() {
                *c += CARD_PAD;
            }
            l.frame = true;
        }
        // A titled top rule and a bottom rule close the frame, so the
        // composer reads as its own box between the header above and the
        // roster below.
        let title = " new session ".to_string();
        let title = crate::chrome::clip(&title, w.saturating_sub(4));
        let top_fill = w.saturating_sub(2 + 1 + cols(&title));
        lines.insert(
            0,
            CardLine {
                text: format!(
                    "\u{256d}\u{2500}{title}{}\u{256e}",
                    "\u{2500}".repeat(top_fill)
                ),
                rule: true,
                marks: vec![(3, 2 + cols(&title) - 1, Mark::Bold)],
                ..CardLine::default()
            },
        );
        lines.push(CardLine {
            text: format!("\u{2570}{}\u{256f}", "\u{2500}".repeat(w - 2)),
            rule: true,
            ..CardLine::default()
        });
        lines
    }

    fn card_lines_flush(&self, view: &View, w: usize) -> Vec<CardLine> {
        let w = w.max(8);
        let catalog = &view.launcher_catalog;
        let mut out: Vec<CardLine> = Vec::new();

        let wrap_w = w.saturating_sub(PROMPT_GUTTER).max(1);
        let chunks = wrap_message(&self.draft.message, wrap_w);
        let (cur_row, cur_col) =
            wrapped_cursor(&self.draft.message, self.draft.cursor_chars, wrap_w);
        let start = (cur_row + 1).saturating_sub(PROMPT_ROWS);
        let typing = self.focus == Focus::Message
            && self.sets.naming.is_none()
            && self
                .picker
                .as_ref()
                .is_none_or(|p| matches!(p.mode, PickerMode::Complete { .. }));
        // The completion list's typed token reads in the prompt, though it
        // never lands in the message.
        let ghost = match self.picker.as_ref().map(|p| (&p.mode, &p.filter)) {
            Some((PickerMode::Complete { sigil, .. }, filter)) => Some(format!("{sigil}{filter}")),
            _ => None,
        };
        let last_chunk = chunks.len().saturating_sub(1);
        for (k, (_, chunk)) in chunks.iter().enumerate().skip(start).take(PROMPT_ROWS) {
            let glyph = match (k == 0, self.shell) {
                (true, true) => "! ",
                (true, false) => "\u{276f} ",
                (false, _) => "  ",
            };
            let placeholder = k == 0 && self.draft.message.is_empty();
            let body = if placeholder {
                if self.shell {
                    format!("shell command in {}", self.draft.cwd())
                } else {
                    "prompt \u{b7} +flag \u{b7} :value \u{b7} ! shell".to_string()
                }
            } else {
                match (&ghost, k == last_chunk) {
                    (Some(g), true) if chunk.is_empty() || chunk.ends_with(' ') => {
                        format!("{chunk}{g}")
                    }
                    (Some(g), true) => format!("{chunk} {g}"),
                    _ => chunk.clone(),
                }
            };
            let cursor = (typing && k == cur_row).then(|| {
                PROMPT_GUTTER
                    + if ghost.is_some() && k == last_chunk {
                        cols(&body)
                    } else {
                        chunk
                            .chars()
                            .take(cur_col)
                            .map(|c| usize::from(UnicodeWidthChar::width(c).unwrap_or(0)))
                            .sum::<usize>()
                    }
            });
            out.push(CardLine {
                text: crate::chrome::clip(&format!("{glyph}{body}"), w),
                dim: placeholder,
                spans: vec![(0, w, CardHit::Prompt)],
                focused: None,
                cursor,
                alert: false,
                frame: false,
                rule: false,
                marks: Vec::new(),
            });
        }

        if let Some(name) = &self.sets.naming {
            out.push(CardLine {
                text: format!("  save set as: {name}"),
                cursor: Some(2 + cols("save set as: ") + cols(name)),
                ..CardLine::default()
            });
        }
        if let Some(notice) = &self.sets.notice {
            for line in wrap_words(notice, w.saturating_sub(2).max(1)) {
                out.push(CardLine {
                    alert: true,
                    ..CardLine::plain(format!("  {line}"), false)
                });
            }
        }

        let footer = self.footer();
        if !footer.is_empty() {
            let alert = matches!(self.phase, Phase::Refused { .. } | Phase::Unknown { .. });
            for line in wrap_words(&footer, w.saturating_sub(2).max(1)) {
                out.push(CardLine {
                    alert,
                    ..CardLine::plain(format!("  {line}"), !alert)
                });
            }
        }

        if !self.draft.pills.is_empty() {
            let pills = (0..self.draft.pills.len())
                .map(|i| {
                    let text = format!("{} \u{d7}", self.pill_spell(i));
                    let n = cols(&text);
                    Token {
                        spans: vec![(n - 1, n, CardHit::Pill(i))],
                        marks: Vec::new(),
                        focused: false,
                        text,
                    }
                })
                .collect();
            out.extend(flow(pills, "  ", w));
        }

        out.extend(self.will_run_lines(view, w));

        // At rest the card is the prompt and the command line; Tab (or any
        // chip holding the keyboard) unfolds the chip rows and the key line.
        let unfolded = self.focus != Focus::Message;
        let order = Focus::tab_order(self, catalog);
        let group = |fs: &[Focus], labelled: bool| -> Vec<Token> {
            fs.iter()
                .filter(|f| order.contains(f))
                .map(|f| chip_token(self, *f, catalog, labelled))
                .collect()
        };
        // Where reads as noise while the launch stays local; it appears once
        // a host is chosen, or while Tab holds it.
        let local = self.focus != Focus::Where && self.chip_label(Focus::Where, catalog) == "Local";
        let second: Vec<Focus> = [Focus::Project, Focus::Branch, Focus::Worktree, Focus::Where]
            .into_iter()
            .filter(|f| !(local && *f == Focus::Where))
            .collect();
        let firsts = [
            Focus::Harness,
            Focus::Model,
            Focus::Effort,
            Focus::Permission,
        ];
        if unfolded {
            // A first group that would wrap drops its dim labels so the perms
            // token stays beside the harness.
            let mut first_lines = flow(group(&firsts, true), " \u{b7} ", w);
            if first_lines.len() > 1 {
                first_lines = flow(group(&firsts, false), " \u{b7} ", w);
            }
            let first_empty = group(&firsts, false).is_empty();
            out.extend(first_lines);
            let second = group(&second, true);
            if !first_empty && !second.is_empty() {
                out.push(CardLine::default());
            }
            out.extend(flow(second, " \u{b7} ", w));

            if let Some(facts) = self.facts_line(view) {
                for (_, line) in wrap_message(&facts, w.saturating_sub(2).max(1)) {
                    out.push(CardLine::plain(format!("  {line}"), true));
                }
            }
        }

        if let Some(pk) = &self.picker {
            out.extend(picker_lines(pk, w));
        }

        let pending = matches!(self.phase, Phase::Unknown { .. } | Phase::Submitting { .. });
        let completing = ghost.is_some();
        let bar = if unfolded {
            self.keybar()
        } else if completing {
            "\u{21b5} pick \u{b7} esc cancel".to_string()
        } else {
            String::new()
        };
        let (keybar, esc_at) = keybar_lines(&bar, w);
        for (i, line) in keybar.into_iter().enumerate() {
            let mut cl = CardLine::plain(line, true);
            if let Some((_, x, ew)) = esc_at.filter(|(row, _, _)| *row == i) {
                let hit = if pending {
                    CardHit::Cancel
                } else {
                    CardHit::Close
                };
                cl.spans.push((x, x + ew, hit));
            }
            out.push(cl);
        }

        // [more] sits left and the launch button right-aligned on one row.
        let (go, go_hit) = if pending {
            ("[cancel]", CardHit::Cancel)
        } else {
            ("[start]", CardHit::Start)
        };
        let more = if unfolded { "[less]" } else { "[more]" };
        let gap = w.saturating_sub(more.len() + go.len());
        let esc = if pending { "esc cancel" } else { "esc close" };
        let esc_hit = if pending {
            CardHit::Cancel
        } else {
            CardHit::Close
        };
        let free = gap.saturating_sub(esc.len() + 2);
        if !unfolded && !completing && free >= 2 {
            // At rest the esc word rides the button row, so the card holds
            // three lines.
            let esc_at = more.len() + free / 2;
            let at = w - go.len();
            out.push(CardLine {
                text: format!(
                    "{more}{}{esc}{}{go}",
                    " ".repeat(free / 2),
                    " ".repeat(at - esc_at - esc.len())
                ),
                dim: false,
                spans: vec![
                    (0, more.len(), CardHit::More),
                    (esc_at, esc_at + esc.len(), esc_hit),
                    (at, at + go.len(), go_hit),
                ],
                marks: vec![
                    (esc_at, esc_at + esc.len(), Mark::Dim),
                    (at, at + go.len(), Mark::Accent),
                ],
                ..CardLine::default()
            });
        } else if gap >= 1 {
            let at = more.len() + gap;
            out.push(CardLine {
                text: format!("{more}{}{go}", " ".repeat(gap)),
                spans: vec![(0, more.len(), CardHit::More), (at, at + go.len(), go_hit)],
                marks: vec![(at, at + go.len(), Mark::Accent)],
                ..CardLine::default()
            });
        } else {
            out.push(CardLine {
                text: more.to_string(),
                spans: vec![(0, more.len(), CardHit::More)],
                ..CardLine::default()
            });
            out.push(CardLine {
                text: go.to_string(),
                spans: vec![(0, go.len(), go_hit)],
                marks: vec![(0, go.len(), Mark::Accent)],
                ..CardLine::default()
            });
        }
        out
    }
}

/// Act on a tap on the open card.
pub(crate) async fn card_hit(
    view: &mut View,
    hit: CardHit,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    if view.launcher.is_none() {
        return Ok(());
    }
    refresh_stale_picker(view);
    // A tap off the unfolded list folds it: the list belongs to the token
    // that opened it.
    if !matches!(hit, CardHit::Pick(_)) {
        if let Some(l) = view.launcher.as_mut() {
            l.picker = None;
        }
    }
    match hit {
        CardHit::Prompt => {
            if let Some(l) = view.launcher.as_mut() {
                l.focus = Focus::Message;
            }
        }
        CardHit::Chip(field) => {
            let catalog = view.launcher_catalog.clone();
            let Some(l) = view.launcher.as_mut() else {
                return Ok(());
            };
            l.focus = field;
            // The worktree box toggles; a `never` project's Branch is
            // read-only; every other token unfolds its list.
            if field == Focus::Worktree {
                toggle_worktree(l, &catalog);
            } else if field != Focus::Branch || !l.draft.policy_never(&catalog) {
                open_picker_at(l, &catalog, &view.backlog, INLINE_ANCHOR, field);
            }
        }
        CardHit::Pick(target) => {
            let portal = next_free_portal(view);
            let catalog = view.launcher_catalog.clone();
            let Some(l) = view.launcher.as_mut() else {
                return Ok(());
            };
            let Some(mut picker) = l.picker.take() else {
                return Ok(());
            };
            let field = picker.field;
            picker.popup.select(target);
            let action = picker
                .popup
                .targets()
                .get(target)
                .and_then(|(row, _)| picker.actions.get(*row))
                .cloned()
                .flatten();
            let anchor = picker.anchor;
            l.picker = Some(picker);
            if let Some(action) = action {
                commit_picker_action(l, &catalog, action, portal, field, anchor);
            }
        }
        CardHit::More => {
            let catalog = view.launcher_catalog.clone();
            if let Some(l) = view.launcher.as_mut() {
                if l.focus != Focus::Message {
                    l.focus = Focus::Message;
                    l.picker = None;
                    l.will_edit = None;
                    return Ok(());
                }
                l.focus = Focus::Plus;
                open_picker_at(l, &catalog, &view.backlog, INLINE_ANCHOR, Focus::Plus);
            }
        }
        CardHit::Start => {
            if launcher_can_launch(view) {
                if let Some(l) = view.launcher.as_mut() {
                    l.focus = Focus::Message;
                }
                submit(view, sock_w).await?;
            }
        }
        CardHit::Cancel => {
            if let Some(l) = view.launcher.as_mut() {
                l.phase = Phase::Editing;
                l.armed = None;
            }
        }
        CardHit::WillRun => {
            if let Some(l) = view.launcher.as_mut() {
                l.focus = Focus::WillRun;
            }
            will_run::begin(view);
        }
        CardHit::Close => close(view),
        CardHit::Pill(idx) => {
            if let Some(l) = view.launcher.as_mut() {
                if idx < l.draft.pills.len() {
                    l.draft.pills.remove(idx);
                    l.draft.bump();
                }
            }
        }
    }
    Ok(())
}
