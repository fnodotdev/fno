//! The exchange picker (v127): an fzf-style find over the You exchanges.
//!
//! The pairing already exists server-side: `you_threads` pairs each typed
//! turn with its reply in the `mail-threads` projection `you[].items[]`,
//! each item keyed by its text.
//!
//! The picker is a client-side read of that index plus the jump. Rows
//! carry your words, the agent, the age, the state word, and an unread
//! dot when the reply landed while away. Typing filters with the
//! navigator's ranking (nav_score); Enter focuses the session's pane and
//! lands on the row where the prompt echoed, via the server's stamped
//! prompt anchors. When the anchor died (the turn aged out of scrollback
//! or the pane redrew), the fallback is the session transcript, never a
//! wrong-row jump.

use super::*;

/// The picker's state: the typed filter and the cursor.
#[derive(Debug, Default)]
pub(crate) struct ExchangePicker {
    /// Incremental text filter (navigator rules: printable bytes append,
    /// backspace pops, Ctrl-u clears).
    pub(crate) query: String,
    /// Cursor into the currently filtered rows, clamped per key, no wrap.
    pub(crate) cursor: usize,
}

/// One flattened exchange: a typed turn and the reply it got.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ExchangeRow {
    /// The transcript's session id; the jump resolves an agent row by it.
    pub sid: String,
    /// The agent's display name (the projection's name, else the id head).
    pub agent: String,
    /// The turn's stamp as stored (RFC 3339).
    pub ts: String,
    /// The same stamp as epoch ms: the anchor join key.
    pub ts_ms: u64,
    /// First line of what the person typed, the row's visible words.
    pub typed: String,
    /// answered / pending / acked from the projection.
    pub state: String,
    /// The reply landed while away (no read mark yet). Never set on a
    /// pending or acked row.
    pub unread: bool,
    /// The pairing key, also the read mark key's tail.
    pub turn_id: String,
}

impl ExchangeRow {
    /// The row's display label: your words, the agent, the age, and the
    /// state word on the tail (only when not the default answered).
    /// The unread dot prefixes an unseen reply.
    pub(crate) fn label(&self, now_ms: u64) -> String {
        let mut out = String::with_capacity(96);
        if self.unread {
            out.push('\u{25cf}');
            out.push(' ');
        }
        out.push_str(&first_line(&self.typed));
        out.push_str(" \u{b7} ");
        out.push_str(&self.agent);
        out.push_str(" \u{b7} ");
        out.push_str(&age_line(self.ts_ms, now_ms));
        if self.state != "answered" {
            out.push_str(" \u{b7} ");
            out.push_str(&self.state);
        }
        out
    }

    /// The searchable identity: the full typed text (not just the first
    /// line), the agent, the session id.
    pub(crate) fn match_key(&self) -> String {
        format!("{} {} {}", self.typed, self.agent, self.sid).to_lowercase()
    }

    /// The navigator's ranking: lower ranks first, None keeps flatten
    /// order.
    pub(crate) fn score(&self, q: &str) -> Option<u8> {
        nav_rank::nav_score(q, &self.label(0).to_lowercase(), &self.match_key())
    }
}

/// First line of the typed text: the row's visible words.
pub(crate) fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("");
    line.trim().to_string()
}

/// A compact relative age: now, Nm, Nh, Nd past ts_ms. Empty for an
/// unreadable stamp (0).
pub(crate) fn age_line(ts_ms: u64, now_ms: u64) -> String {
    if ts_ms == 0 || now_ms < ts_ms {
        return String::new();
    }
    let s = (now_ms - ts_ms) / 1000;
    if s < 60 {
        "now".into()
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

/// RFC 3339 stamp -> epoch ms; 0 when the stamp does not parse.
pub(crate) fn ts_to_ms(ts: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.timestamp_millis().max(0) as u64)
        .unwrap_or(0)
}

impl MessagesBoard {
    /// Open the picker. The query starts empty: the full index, newest
    /// exchange first.
    pub(crate) fn open_picker(&mut self) {
        if self.picker.is_none() {
            self.picker = Some(ExchangePicker::default());
        }
    }

    /// Every session's exchanges flattened from the projection's you rows,
    /// filtered and ranked by the picker query with the navigator ranking.
    /// An empty query keeps the projection's recency order.
    pub(crate) fn exchange_rows(&self) -> Vec<ExchangeRow> {
        let q = self
            .picker
            .as_ref()
            .map(|p| p.query.to_lowercase())
            .unwrap_or_default();
        let marks = self.read_marks();
        let rows: Vec<ExchangeRow> = self
            .you_threads()
            .iter()
            .flat_map(|t| {
                let sid = text_of(t, "session_id");
                let agent = text_of(t, "name");
                t.get("items")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .map(move |i| (sid, agent, i))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            })
            .filter_map(|(sid, agent, item)| {
                let typed = text_of(item, "typed_text");
                if typed.trim().is_empty() {
                    return None;
                }
                let ts = text_of(item, "ts");
                let ts_ms = ts_to_ms(ts);
                let state = text_of(item, "state").to_string();
                let replied = state == "answered" && !text_of(item, "reply_text").trim().is_empty();
                let turn_id = text_of(item, "turn_id").to_string();
                let key = format!("xchg:{}:{}", sid, turn_id);
                let unread = replied && !marks.contains_key(&key);
                Some(ExchangeRow {
                    sid: sid.to_string(),
                    agent: agent.to_string(),
                    ts: ts.to_string(),
                    ts_ms,
                    typed: typed.to_string(),
                    state,
                    unread,
                    turn_id,
                })
            })
            .collect();
        // Flatten order is session-major and oldest-first within a session;
        // the picker reads newest first, so sort before the rank pass.
        let mut rows = rows;
        rows.sort_by(|a, b| b.ts_ms.cmp(&a.ts_ms));
        let mut scored: Vec<(Option<u8>, ExchangeRow)> = rows
            .into_iter()
            .filter_map(|r| r.score(&q).map(|s| (s, r)))
            .collect();
        scored.sort_by_key(|(s, _)| *s);
        scored.into_iter().map(|(_, r)| r).collect()
    }

    /// The picker takes over the thread column while open: the query line,
    /// the ranked rows, and a one-line footer. Rows paint truncated (fzf
    /// style), the cursor row carries the band.
    pub(crate) fn picker_lines(&self, w: usize) -> (Vec<TLine>, Vec<Option<usize>>) {
        let wrap_w = w.saturating_sub(2).max(1);
        let Some(p) = &self.picker else {
            return (vec![TLine::meta("")], vec![None]);
        };
        let now_ms = board_now() * 1000;
        let rows = self.exchange_rows();
        let head = TLine::of(&[
            seg("find:", TextStyle::Meta),
            seg(p.query.clone(), TextStyle::Item),
            seg("  enter jump / esc close", TextStyle::Meta),
        ]);
        let mut lines = vec![head];
        let mut owners: Vec<Option<usize>> = vec![None];
        lines.push(TLine::meta(""));
        for (i, r) in rows.iter().enumerate() {
            let mut line = TLine::of(&[seg(r.label(now_ms), TextStyle::Item)]);
            line.band = i == p.cursor;
            lines.push(line);
            owners.push(None);
        }
        if rows.is_empty() {
            lines.extend(TLine::meta("(no exchanges match)").wrap(wrap_w));
        }
        (lines, owners)
    }

    /// The exchange was acted on: save the read mark so the unread dot
    /// clears. The key pairs the session and the turn id, the pair that
    /// keys an exchange.
    pub(crate) fn exchange_mark_read(&self, sid: &str, turn_id: &str, ts: &str) {
        self.mark_read(&format!("xchg:{}:{}", sid, turn_id), ts);
    }
}

/// Keys while the exchange picker is open, run before the board's own keys
/// (the detail popup's position in the keys function). Navigator rules:
/// printable bytes edit the query, backspace pops, Ctrl-u clears, arrows
/// and Ctrl-n/Ctrl-p move, Enter jumps, Esc closes.
pub(crate) async fn picker_keys(
    view: &mut View,
    bytes: &[u8],
    sock: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<StdinFlow, String> {
    let Some(b) = view.messages_board.as_mut() else {
        return Ok(StdinFlow::Continue);
    };
    if b.picker.is_none() {
        return Ok(StdinFlow::Continue);
    }
    // A bare Esc byte with no carry decides at once (the board's rule); the
    // fold alone would carry it waiting for the next byte and Esc would lag
    // one keystroke behind.
    let tokens = if bytes == [27] && b.esc.is_empty() {
        vec![ModalKey::Esc]
    } else {
        fold_modal_keys(&mut b.esc, bytes)
    };
    for token in tokens {
        match token {
            ModalKey::Esc => {
                if let Some(b) = view.messages_board.as_mut() {
                    b.picker = None;
                    b.esc.clear();
                }
                return Ok(StdinFlow::Continue);
            }
            ModalKey::Enter => jump_to_exchange(view, sock).await?,
            ModalKey::Up => picker_move(view, -1),
            ModalKey::Down => picker_move(view, 1),
            ModalKey::Byte(b) => match b {
                0x0e => picker_move(view, 1),
                0x10 => picker_move(view, -1),
                0x7f | 0x08 => {
                    if let Some(p) = view.messages_board.as_mut().and_then(|b| b.picker.as_mut()) {
                        p.query.pop();
                        p.cursor = 0;
                    }
                }
                0x15 => {
                    if let Some(p) = view.messages_board.as_mut().and_then(|b| b.picker.as_mut()) {
                        p.query.clear();
                        p.cursor = 0;
                    }
                }
                0x20..=0x7e => {
                    if let Some(p) = view.messages_board.as_mut().and_then(|b| b.picker.as_mut()) {
                        if p.query.len() < 64 {
                            p.query.push(b as char);
                            p.cursor = 0;
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    Ok(StdinFlow::Continue)
}

/// Move the picker cursor one step, clamped into the filtered rows.
fn picker_move(view: &mut View, dir: isize) {
    let (Some(b), len) = (
        view.messages_board.as_mut(),
        view.messages_board
            .as_ref()
            .map(|b| b.exchange_rows().len())
            .unwrap_or(0),
    ) else {
        return;
    };
    let Some(p) = b.picker.as_mut() else {
        return;
    };
    let next = p.cursor as isize + dir;
    p.cursor = next.clamp(0, len.saturating_sub(1) as isize).max(0) as usize;
}

/// Enter on a picker row: focus the session's pane, then land on the row
/// where the prompt echoed (the server's stamped anchors). A refusal keeps
/// the picker open with a notice, navigator-style. On a send, the read
/// mark lands and the picker closes: the list leads into the session.
async fn jump_to_exchange(
    view: &mut View,
    sock: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    let row = {
        let Some(b) = view.messages_board.as_ref() else {
            return Ok(());
        };
        let Some(p) = b.picker.as_ref() else {
            return Ok(());
        };
        b.exchange_rows().into_iter().nth(p.cursor)
    };
    let Some(row) = row else {
        let _ = raw_out(b"\x07");
        return Ok(());
    };
    let target =
        view.layout.agents.iter().find(|a| {
            a.remote.is_none() && a.harness_session_id.as_deref() == Some(row.sid.as_str())
        });
    let Some(a) = target else {
        view.set_notice(format!(
            "no live session here for {} - resume it from the sideline",
            row.agent
        ));
        return Ok(());
    };
    if a.exited {
        view.set_notice(format!(
            "{} has exited - resume it from the sideline",
            row.agent
        ));
        return Ok(());
    }
    let Some(pane) = a.pane_id else {
        view.set_notice(format!(
            "{} holds no pane here - attach it from the sideline",
            row.agent
        ));
        return Ok(());
    };
    // The jump is the read gesture: the dot clears even when the anchor
    // missed, because the transcript fallback shows the same reply.
    view.messages_board
        .as_ref()
        .map(|b| b.exchange_mark_read(&row.sid, &row.turn_id, &row.ts));
    let pane_ref = pane;
    let sid = row.sid.clone();
    let ts_ms = row.ts_ms;
    view.messages_board.as_mut().map(|b| {
        b.picker = None;
        b.pending_jump = Some((sid.clone(), pane_ref, ts_ms));
    });
    let _ = view.set_notice(format!("jump: {}", first_line(&row.typed)));
    write_msg(sock, &ClientMsg::Command(Command::FocusPane(pane)))
        .await
        .map_err(|e| format!("exchange jump send failed: {e}"))?;
    write_msg(sock, &ClientMsg::AnchorJump { pane, ts_ms })
        .await
        .map_err(|e| format!("exchange jump send failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(typed: &str, agent: &str, state: &str, unread: bool) -> ExchangeRow {
        ExchangeRow {
            sid: "sess-1".into(),
            agent: agent.into(),
            ts: "2026-11-11T11:00:00Z".into(),
            ts_ms: 1_000,
            typed: typed.into(),
            state: state.into(),
            unread,
            turn_id: "t-1".into(),
        }
    }

    /// The label: the dot prefixes an unseen reply, the state word only
    /// rides non-answered rows, and the age always paints.
    #[test]
    fn label_dot_state_word_age() {
        let now_ms: u64 = 1_000 + 90_000;
        let r = row("why did the gate refuse?", "kilo", "answered", false);
        let l = r.label(now_ms);
        assert!(l.contains("why did the gate refuse?"), "typed words: {l}");
        assert!(l.contains("kilo"), "agent: {l}");
        assert!(l.contains("1m"), "age: {l}");
        assert!(!l.contains("answered"), "default state unspoken: {l}");
        let unread = row("ship it", "kilo", "answered", true);
        assert!(unread.label(now_ms).starts_with('\u{25cf}'));
        let pending = row("ship it", "kilo", "pending", false);
        assert!(pending.label(now_ms).ends_with("pending"));
    }

    /// The ranking drops non-matches and keeps navigator tiers: a full
    /// substring hit ranks below a name start but stays above a drop.
    #[test]
    fn score_tiers_and_drop() {
        let r = row("why did the gate refuse?", "kilo", "answered", false);
        assert_eq!(r.score(""), Some(0));
        assert_eq!(r.score("gate"), Some(3));
        assert_eq!(r.score("kilo"), Some(2));
        assert_eq!(r.score("zzz"), None);
    }

    /// The age buckets: now, minutes, hours, days; an unreadable stamp (0)
    /// paints nothing.
    #[test]
    fn age_buckets() {
        assert_eq!(age_line(0, 5_000), "");
        assert_eq!(age_line(1_000, 1_000), "now");
        assert_eq!(age_line(1_000, 1_000 + 90_000), "1m");
        assert_eq!(age_line(1_000, 1_000 + 7_200_000), "2h");
        assert_eq!(age_line(1_000, 1_000 + 3 * 86_400_000), "3d");
    }

    /// RFC 3339 in, epoch ms out; garbage reads 0.
    #[test]
    fn stamp_parse() {
        assert_eq!(ts_to_ms("1970-01-01T00:00:01.5Z"), 1_500);
        assert_eq!(ts_to_ms("nope"), 0);
    }
}
