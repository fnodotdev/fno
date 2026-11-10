//! Topic extraction over the asks rows: what the user's own
//! messages discuss, clustered by the things they mention. A mention-based
//! net - backlog node ids, PR refs, versions, capitalized words that are
//! not sentence-initial, backticked tokens - kept when it spans at least
//! two distinct asks. Plain uncapitalized unbackticked words are not
//! topics; that is the documented cost of the route (no NER, no graph
//! lookup lives client-side).

use serde_json::Value;
use std::collections::BTreeMap;

/// One topic cluster: the asks that mention it, in the items slice's own
/// order (pending first, then newest first).
#[derive(Debug, PartialEq)]
pub(crate) struct Topic {
    pub name: String,
    /// Indices into the items slice `topics_of` was given.
    pub asks: Vec<usize>,
    pub pending: usize,
    /// The newest mention's ts (RFC3339, so lexicographic order is time
    /// order).
    pub last_ts: String,
}

/// Cluster `items` (the `asks_items()` shape) by mention. A topic must
/// span at least two distinct asks to be listed, counting one mention per
/// ask however often it repeats inside it. Topics sort by last mention
/// descending, then count descending, then name - deterministic.
pub(crate) fn topics_of(items: &[Value]) -> Vec<Topic> {
    // name -> (ask indices in visit order, newest ts)
    let mut map: BTreeMap<String, (Vec<usize>, String)> = BTreeMap::new();
    for (idx, item) in items.iter().enumerate() {
        let ts = item.get("ts").and_then(Value::as_str).unwrap_or("");
        let mut text = item
            .get("typed_text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        // The answer counts too: an ask whose reply discusses 0.4.7
        // belongs to the 0.4.7 cluster.
        let end = item.get("end_reply").and_then(Value::as_str).unwrap_or("");
        let base = item.get("reply_text").and_then(Value::as_str).unwrap_or("");
        text.push('\n');
        text.push_str(if end.trim().is_empty() { base } else { end });
        for key in mentions(&text) {
            let entry = map.entry(key).or_default();
            if !entry.0.contains(&idx) {
                entry.0.push(idx);
            }
            if ts > entry.1.as_str() {
                entry.1 = ts.to_string();
            }
        }
    }
    let mut topics: Vec<Topic> = map
        .into_iter()
        .filter(|(_, (asks, _))| asks.len() >= 2)
        .map(|(name, (asks, last_ts))| {
            let pending = asks
                .iter()
                .filter(|i| {
                    items
                        .get(**i)
                        .and_then(|v| v.get("state"))
                        .and_then(Value::as_str)
                        == Some("pending")
                })
                .count();
            Topic {
                name,
                asks,
                pending,
                last_ts,
            }
        })
        .collect();
    topics.sort_by(|a, b| {
        b.last_ts
            .cmp(&a.last_ts)
            .then(b.asks.len().cmp(&a.asks.len()))
            .then(a.name.cmp(&b.name))
    });
    topics
}

/// Every mention in `text`, left to right, deduplicated per call site by
/// the caller's ask-index set.
#[allow(clippy::indexing_slicing)] // the walk bounds-checks every read (j < b.len() guards each index); the lint cannot see the guard
fn mentions(text: &str) -> Vec<String> {
    let b: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if i > 0 && b[i - 1].is_alphanumeric() {
            i += 1;
            continue;
        }
        match scan(&b, i) {
            Some((tok, next)) => {
                out.push(tok);
                i = next;
            }
            None => i += 1,
        }
    }
    out
}

/// The token starting at `i` (a word start: the char before it is not
/// alphanumeric), if the scanners recognize one, with the index past it.
#[allow(clippy::indexing_slicing)] // the walk bounds-checks every read (j < b.len() guards each index); the lint cannot see the guard
fn scan(b: &[char], i: usize) -> Option<(String, usize)> {
    match b[i] {
        '#' => scan_pr(b, i),
        '`' => scan_backtick(b, i),
        c if c.is_ascii_lowercase() => scan_node_id(b, i),
        c if c.is_ascii_digit() => scan_version(b, i),
        c if c.is_uppercase() => scan_capitalized(b, i),
        _ => None,
    }
}

/// A backlog node id: `x-`/`fno-`/`fnh-`/`d-`/`t-` plus 4-10 lowercase
/// hex, whole word.
#[allow(clippy::indexing_slicing)] // the walk bounds-checks every read (j < b.len() guards each index); the lint cannot see the guard
fn scan_node_id(b: &[char], i: usize) -> Option<(String, usize)> {
    let mut j = i;
    while j < b.len() && b[j].is_ascii_lowercase() {
        j += 1;
    }
    let prefix: String = b[i..j].iter().collect();
    if !matches!(prefix.as_str(), "x" | "fno" | "fnh" | "d" | "t") {
        return None;
    }
    if j >= b.len() || b[j] != '-' {
        return None;
    }
    j += 1;
    let hs = j;
    while j < b.len() && (b[j].is_ascii_digit() || ('a'..='f').contains(&b[j])) {
        j += 1;
    }
    let len = j - hs;
    if !(4..=10).contains(&len) || (j < b.len() && b[j].is_alphanumeric()) {
        return None;
    }
    Some((b[i..j].iter().collect(), j))
}

/// A PR ref: `#` plus two or more digits, whole word.
#[allow(clippy::indexing_slicing)] // the walk bounds-checks every read (j < b.len() guards each index); the lint cannot see the guard
fn scan_pr(b: &[char], i: usize) -> Option<(String, usize)> {
    let mut j = i + 1;
    while j < b.len() && b[j].is_ascii_digit() {
        j += 1;
    }
    let len = j - (i + 1);
    if len < 2 || (j < b.len() && b[j].is_alphanumeric()) {
        return None;
    }
    Some((b[i..j].iter().collect(), j))
}

/// A version: `N(.N)+`, greedy, not opening a dotted chain it sits inside
/// (`1.2.3.4` reads as one token). A trailing dot is allowed so a sentence
/// end keeps the version.
#[allow(clippy::indexing_slicing)] // the walk bounds-checks every read (j < b.len() guards each index); the lint cannot see the guard
fn scan_version(b: &[char], i: usize) -> Option<(String, usize)> {
    if i > 0 && (b[i - 1] == '.' || b[i - 1].is_alphanumeric()) {
        return None;
    }
    let mut j = i;
    let mut parts = 0;
    while j < b.len() {
        let ds = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j == ds {
            break;
        }
        parts += 1;
        if j < b.len() && b[j] == '.' && j + 1 < b.len() && b[j + 1].is_ascii_digit() {
            j += 1;
        } else {
            break;
        }
    }
    if parts < 2 || (j < b.len() && b[j].is_alphanumeric()) {
        return None;
    }
    Some((b[i..j].iter().collect(), j))
}

/// A capitalized word, three letters or more, not sentence-initial: the
/// word Rio in "asked about Rio", never the one opening a sentence. The
/// first char uppercase, the rest lowercase, so acronyms (CI, PR) never
/// match.
#[allow(clippy::indexing_slicing)] // the walk bounds-checks every read (j < b.len() guards each index); the lint cannot see the guard
fn scan_capitalized(b: &[char], i: usize) -> Option<(String, usize)> {
    if !b[i].is_uppercase() {
        return None;
    }
    let mut j = i + 1;
    while j < b.len() && b[j].is_ascii_lowercase() {
        j += 1;
    }
    let len = j - i;
    if len < 3 || (j < b.len() && b[j].is_alphabetic()) || sentence_initial(b, i) {
        return None;
    }
    Some((b[i..j].iter().collect(), j))
}

/// Whether the word at `i` opens a sentence: text start, or the nearest
/// non-space char before it closes one.
#[allow(clippy::indexing_slicing)] // the walk bounds-checks every read (j < b.len() guards each index); the lint cannot see the guard
fn sentence_initial(b: &[char], i: usize) -> bool {
    let mut j = i;
    while j > 0 && (b[j - 1] == ' ' || b[j - 1] == '\t') {
        j -= 1;
    }
    j == 0 || matches!(b[j - 1], '.' | '!' | '?' | ';' | ':' | '\n')
}

/// A backticked token of three or more chars (`fno doctor test`). A lone
/// backtick stops scanning at the close within a bounded window, else it
/// bails.
#[allow(clippy::indexing_slicing)] // the walk bounds-checks every read (j < b.len() guards each index); the lint cannot see the guard
fn scan_backtick(b: &[char], i: usize) -> Option<(String, usize)> {
    let limit = (i + 200).min(b.len());
    let close = b[(i + 1)..limit].iter().position(|c| *c == '`')? + i + 1;
    let inner: String = b[i + 1..close].iter().collect();
    let trimmed = inner.trim();
    if trimmed.len() < 3 || !trimmed.chars().next().is_some_and(|c| c.is_alphanumeric()) {
        return None;
    }
    Some((trimmed.to_string(), close + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ask(ts: &str, state: &str, typed: &str, reply: &str) -> Value {
        json!({"ts": ts, "state": state, "typed_text": typed, "reply_text": reply, "end_reply": ""})
    }

    #[test]
    fn scanners_find_the_five_kinds_and_the_two_ask_floor_holds() {
        let items = vec![
            ask(
                "2026-11-07T09:00:00Z",
                "answered",
                "merge x-a1450 now",
                "shipped in #4621",
            ),
            ask(
                "2026-11-07T10:00:00Z",
                "pending",
                "what blocks x-a1450?",
                "waiting on 0.4.7",
            ),
            ask(
                "2026-11-07T11:00:00Z",
                "answered",
                "is 0.4.7 cut yet",
                "one ask only for `fno doctor test` here",
            ),
            ask(
                "2026-11-07T12:00:00Z",
                "pending",
                "Rio today?",
                "Rio shipped in #4621",
            ),
        ];
        let topics = topics_of(&items);
        let names: Vec<&str> = topics.iter().map(|t| t.name.as_str()).collect();
        // Rio appears in one ask only and stays off the list; the version,
        // node id and PR each span two.
        assert_eq!(names, vec!["#4621", "0.4.7", "x-a1450"]);
        let t0 = topics.first().expect("the PR cluster lists first");
        assert_eq!(t0.asks, vec![0, 3]);
        assert_eq!(t0.pending, 1);
        let t1 = topics.get(1).expect("the version cluster lists second");
        assert_eq!(t1.asks, vec![1, 2]);
        assert_eq!(t1.pending, 1);
        let t2 = topics.get(2).expect("the node-id cluster lists third");
        assert_eq!(t2.asks, vec![0, 1]);
    }

    #[test]
    fn backtick_capitalization_and_sentence_start_rules() {
        let items = vec![
            ask(
                "2026-11-07T08:00:00Z",
                "answered",
                "Watchdog reran overnight",
                "the Watchdog report landed",
            ),
            ask(
                "2026-11-07T09:00:00Z",
                "answered",
                "run `fno doctor test`",
                "Fine. Watchdog shipped today.",
            ),
            ask(
                "2026-11-07T10:00:00Z",
                "answered",
                "asked about `fno doctor test` and the Watchdog page",
                "fine",
            ),
        ];
        let topics = topics_of(&items);
        let names: Vec<&str> = topics.iter().map(|t| t.name.as_str()).collect();
        // Watchdog spans the ask 0 reply and the ask 2 typed mention; the
        // ask 0 typed opener and the ask 1 reply opener open a sentence
        // and are skipped. Without the sentence-start rule the member set
        // would read [0, 1, 2].
        let watchdog = topics
            .iter()
            .find(|t| t.name == "Watchdog")
            .expect("mid-sentence mentions keep Watchdog");
        assert_eq!(watchdog.asks, vec![0, 2]);
        assert_eq!(names, vec!["Watchdog", "fno doctor test"]);
        assert_eq!(
            topics
                .iter()
                .find(|t| t.name == "fno doctor test")
                .expect("backtick token")
                .asks,
            vec![1, 2]
        );
    }
}
