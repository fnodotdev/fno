//! Column 3 for a You thread: the operator's typed turns and the lead's
//! replies, the worker mail between two turns folded into one band, the
//! newest open ask pinned under the title, and a card naming what arrived
//! since the last open. The projection's `you` items are read once into a
//! typed view; the paint never reads a field by name.

use super::*;
use serde::Deserialize;

/// One `you` item as the projection serves it.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub(super) struct YouItemView {
    pub ts: String,
    pub typed_text: String,
    pub reply_text: String,
    pub turn_id: String,
    pub state: String,
    pub asks_you: bool,
    pub gap: Vec<GapView>,
}

/// One band row: a worker's mail or the lead's reply to it.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum GapView {
    Mail {
        #[serde(default)]
        ts: String,
        #[serde(default)]
        sender: String,
        #[serde(default)]
        summary: String,
    },
    LeadUpdate {
        #[serde(default)]
        ts: String,
        #[serde(default)]
        text: String,
    },
    #[serde(other)]
    Other,
}

impl GapView {
    fn ts(&self) -> &str {
        match self {
            GapView::Mail { ts, .. } | GapView::LeadUpdate { ts, .. } => ts,
            GapView::Other => "",
        }
    }
}

/// The state an ask shows. `pending` with a lead update in its band was
/// answered after worker mail cut the turn short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Shown {
    Answered,
    AnsweredInBand,
    Working,
    WaitingOnYou,
}

impl Shown {
    fn open_word(self) -> Option<&'static str> {
        match self {
            Shown::Working => Some("working"),
            Shown::WaitingOnYou => Some("waiting on you"),
            Shown::Answered | Shown::AnsweredInBand => None,
        }
    }
}

impl YouItemView {
    pub(super) fn shown(&self) -> Shown {
        let lead_answered = self
            .gap
            .iter()
            .any(|g| matches!(g, GapView::LeadUpdate { .. }));
        match self.state.as_str() {
            "pending" if lead_answered => Shown::AnsweredInBand,
            "pending" => Shown::Working,
            "answered" if self.asks_you => Shown::WaitingOnYou,
            _ => Shown::Answered,
        }
    }

    /// The key a band's open state survives a refresh under.
    pub(super) fn band_key(&self) -> String {
        if self.turn_id.is_empty() {
            self.ts.clone()
        } else {
            self.turn_id.clone()
        }
    }
}

/// Parse the projection's items once. An item that fails to parse paints
/// as an empty one, so the cursor's item index never shifts.
pub(super) fn item_views(items: &[Value]) -> Vec<YouItemView> {
    items
        .iter()
        .map(|v| YouItemView::deserialize(v).unwrap_or_default())
        .collect()
}

/// The newest stamp the thread shows, gap entries included: the read
/// mark the open writes, so mail already seen never reads as new.
pub(super) fn newest_stamp(items: &[YouItemView]) -> String {
    items
        .iter()
        .flat_map(|i| std::iter::once(i.ts.as_str()).chain(i.gap.iter().map(GapView::ts)))
        .max()
        .unwrap_or("")
        .to_string()
}

/// The band's one closed line: "12 messages from workers: atticus 5,
/// t-board 4, +2 more", then the lead updates.
pub(super) fn band_line(item: &YouItemView) -> String {
    let mut senders: Vec<(String, usize)> = Vec::new();
    let mut updates = 0;
    for g in &item.gap {
        match g {
            GapView::Mail { sender, .. } => match senders.iter_mut().find(|(s, _)| s == sender) {
                Some((_, n)) => *n += 1,
                None => senders.push((sender.clone(), 1)),
            },
            GapView::LeadUpdate { .. } => updates += 1,
            GapView::Other => {}
        }
    }
    let mails: usize = senders.iter().map(|(_, n)| n).sum();
    // Most mail first; a tie keeps arrival order (the sort is stable).
    senders.sort_by(|a, b| b.1.cmp(&a.1));
    let mut parts: Vec<String> = Vec::new();
    if mails > 0 {
        let plural = if mails == 1 { "message" } else { "messages" };
        let named: Vec<String> = senders
            .iter()
            .take(2)
            .map(|(s, n)| format!("{s} {n}"))
            .collect();
        let mut who = named.join(", ");
        if senders.len() > 2 {
            who.push_str(&format!(", +{} more", senders.len() - 2));
        }
        parts.push(format!("{mails} {plural} from workers: {who}"));
    }
    if updates > 0 {
        let plural = if updates == 1 { "update" } else { "updates" };
        parts.push(format!("{updates} lead {plural}"));
    }
    if item.shown() == Shown::AnsweredInBand {
        parts.push("lead answered".to_string());
    }
    parts.join(", ")
}

/// The newest ask still open, with its index and the word for its state.
pub(super) fn pinned(items: &[YouItemView]) -> Option<(usize, &'static str)> {
    items
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, item)| item.shown().open_word().map(|w| (i, w)))
}

/// The card's line, or `None` when nothing arrived after `mark`.
pub(super) fn since_card(items: &[YouItemView], mark: &str) -> Option<String> {
    let mut mails = 0;
    let mut updates = 0;
    let mut replies = 0;
    let mut senders: Vec<&str> = Vec::new();
    for item in items {
        if item.ts.as_str() > mark && !item.reply_text.is_empty() {
            replies += 1;
        }
        for g in item.gap.iter().filter(|g| g.ts() > mark) {
            match g {
                GapView::Mail { sender, .. } => {
                    mails += 1;
                    if !senders.contains(&sender.as_str()) {
                        senders.push(sender);
                    }
                }
                GapView::LeadUpdate { .. } => updates += 1,
                GapView::Other => {}
            }
        }
    }
    if mails + updates + replies == 0 {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    if mails > 0 {
        let from = counted(senders.len(), "sender");
        parts.push(format!("{} from {from}", counted(mails, "worker message")));
    }
    if updates > 0 {
        parts.push(counted(updates, "lead update"));
    }
    if replies > 0 {
        let noun = if replies == 1 { "reply" } else { "replies" };
        parts.push(format!("{replies} {noun} to you"));
    }
    let mut card = format!("Since you last looked: {}", parts.join(", "));
    if let Some((_, word)) = pinned(items) {
        card.push_str(&format!("; your ask is {word}"));
    }
    Some(card)
}

impl MessagesBoard {
    /// The selected You thread's items as typed views.
    pub(super) fn you_item_views(&self, sid: &str) -> Vec<YouItemView> {
        item_views(&self.you_items(sid))
    }

    /// Column 3 for a selected You thread: the typed turn flush right, the
    /// reply on the left, then the item's band. Owners index ITEMS, so the
    /// cursor indexes the item a line belongs to.
    pub(super) fn you_thread_body(&self, w: usize, sid: &str) -> (Vec<TLine>, Vec<Option<usize>>) {
        let mut lines = vec![self.thread_title()];
        let mut owners: Vec<Option<usize>> = vec![None];
        let wrap_w = w.saturating_sub(2).max(1);
        let items = self.you_item_views(sid);
        if items.is_empty() {
            let wrapped = TLine::meta("(no You items)").wrap(wrap_w);
            owners.extend(std::iter::repeat(None).take(wrapped.len()));
            lines.extend(wrapped);
            return (lines, owners);
        }
        if let Some((i, word)) = pinned(&items) {
            let tail = format!(" \u{b7} {word}");
            let room = wrap_w.saturating_sub("Open: ".len() + tail.chars().count());
            let typed = cut_chars(items[i].typed_text.lines().next().unwrap_or(""), room);
            lines.push(TLine::of(&[
                seg("Open: ", TextStyle::Badge),
                seg(typed, TextStyle::Item),
                seg(tail, TextStyle::Meta),
            ]));
            owners.push(None);
        }
        let agent_name = self
            .you_row(sid)
            .map(|t| text_of(t, "name").to_string())
            .unwrap_or_else(|| sid.get(..8).unwrap_or(sid).to_string());
        let mut last: Option<(String, bool)> = None;
        for (item_index, item) in items.iter().enumerate() {
            let selected = item_index == self.cursors[2];
            let (ts, typed, reply) = (&item.ts, &item.typed_text, &item.reply_text);
            if let Some((prev, prev_right)) = &last {
                if Self::ts_gap_minutes(prev, ts).is_none_or(|g| g >= 5) {
                    push_time(&mut lines, &mut owners, prev, *prev_right, w);
                }
            }
            // The reply is the item's last send; with none, the user's turn.
            last = Some((ts.clone(), reply.is_empty()));
            if !typed.is_empty() {
                push_blank(&mut lines, &mut owners);
                let mut block = vec![TLine::of(&[seg("Me", TextStyle::Badge)])];
                block.extend(body_lines(typed, bubble_w(wrap_w), TextStyle::Item));
                flush_right(&mut block, wrap_w);
                // An open ask names its state under the bubble.
                if let Some(word) = item.shown().open_word() {
                    let mut state = vec![TLine::meta(word)];
                    flush_right(&mut state, wrap_w);
                    block.extend(state);
                }
                for mut line in block {
                    line.band = selected;
                    lines.push(line);
                    owners.push(Some(item_index));
                }
            }
            if !reply.is_empty() {
                push_blank(&mut lines, &mut owners);
                let mut label = TLine::of(&[seg(agent_name.clone(), TextStyle::Badge)]);
                label.band = selected;
                lines.push(label);
                owners.push(Some(item_index));
                for mut line in body_lines(reply, wrap_w, TextStyle::Item) {
                    line.band = selected;
                    lines.push(line);
                    owners.push(Some(item_index));
                }
            }
            if item.gap.is_empty() {
                continue;
            }
            let open = self.you_open.contains(&item.band_key());
            push_blank(&mut lines, &mut owners);
            let marker = if open { "v" } else { ">" };
            let mut band = TLine::meta(cut_chars(
                &format!("  {marker} {}", band_line(item)),
                wrap_w,
            ));
            band.band = selected;
            lines.push(band);
            owners.push(Some(item_index));
            if open {
                for g in &item.gap {
                    let row = match g {
                        GapView::Mail {
                            sender, summary, ..
                        } => format!("    {sender} \u{b7} {summary}"),
                        GapView::LeadUpdate { text, .. } => {
                            format!("    lead \u{b7} {}", text.lines().next().unwrap_or(""))
                        }
                        GapView::Other => continue,
                    };
                    let mut line = TLine::meta(cut_chars(&row, wrap_w));
                    line.band = selected;
                    lines.push(line);
                    owners.push(Some(item_index));
                }
            }
        }
        if let Some((prev, prev_right)) = &last {
            push_time(&mut lines, &mut owners, prev, *prev_right, w);
        }
        let card = self
            .you_seen
            .as_ref()
            .filter(|(seen_sid, _)| seen_sid == sid)
            .and_then(|(_, mark)| since_card(&items, mark));
        if let Some(card) = card {
            push_blank(&mut lines, &mut owners);
            let wrapped = TLine::meta(card).wrap(wrap_w);
            owners.extend(std::iter::repeat(None).take(wrapped.len()));
            lines.extend(wrapped);
        }
        (lines, owners)
    }

    /// Enter on a You item: open or close its band. False when the item
    /// carries no band.
    pub(super) fn toggle_you_band(&mut self, sid: &str) -> bool {
        let items = self.you_item_views(sid);
        let Some(item) = items.get(self.cursors[2]).filter(|i| !i.gap.is_empty()) else {
            return false;
        };
        let key = item.band_key();
        if !self.you_open.remove(&key) {
            self.you_open.insert(key);
        }
        true
    }
}

/// `1 lead update`, `2 lead updates`.
fn counted(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// The first `w` chars of `s`: a band or pin row is one line, never wrapped.
fn cut_chars(s: &str, w: usize) -> String {
    s.chars().take(w).collect()
}
