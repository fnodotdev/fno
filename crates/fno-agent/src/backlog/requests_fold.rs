//! The requests read model behind `fno inbox user requests`: each of the
//! operator's asks across sessions, merged when repeated, with a count, its
//! node, the node's state and a rank.
//!
//! The fold reads the asks the You walk already split (mail and machine
//! rows never reach it, so it adds no classifier) and links each to a node
//! in order: the ack ledger's `node:<id>` outcome, then a node id typed in
//! the ask. Text similarity to node titles is deliberately not a link:
//! measured 2026-11-21 over 48 ack-linked asks against 7053 nodes, the acked
//! node scored highest for only 4 of them. Asks then merge by node, by a
//! normalized text key, and by the repeated-asks word overlap, whose 0.6
//! floor sat in an empty gap between true repeats (0.63 and up) and the
//! nearest false pair (0.46) over 1477 asks.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::you_threads::AskTurn;

/// An open node with no movement for this long reads `stalled`.
const STALLED_DAYS: i64 = 3;
/// An ask with no node this long after its newest repeat reads `no_node`.
const NO_NODE_DAYS: i64 = 1;
/// The default reach when `--since` is absent.
pub(crate) const WINDOW_DAYS: i64 = 30;
/// Under this many content words an unlinked ask is a reply ("yes",
/// "continue"), and under the second count when the agent's reply before
/// it ended on a question.
const MIN_WORDS: usize = 4;
const MIN_WORDS_AFTER_QUESTION: usize = 12;
const KEY_CHARS: usize = 200;
const TEXT_CHARS: usize = 200;
const DAY: i64 = 86_400;

/// One ack ledger line; only a `node:<id>` outcome links.
#[derive(Debug, Deserialize)]
struct AckRow {
    #[serde(default)]
    turn_id: Option<String>,
    #[serde(default)]
    outcome: Option<String>,
}

/// The node fields the fold reads, parsed once from a graph row.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct NodeFacts {
    pub id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub touched_at: Option<String>,
    #[serde(default)]
    pub sessions: Vec<SessionSpan>,
    #[serde(default)]
    pub progress_notes: Vec<NoteStamp>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct SessionSpan {
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct NoteStamp {
    #[serde(default)]
    pub ts: Option<String>,
}

/// What the fold left out, by reason. `cross_node` counts node pairs whose
/// asks read alike but stay apart because each has its own node.
#[derive(Debug, Default, Serialize, PartialEq)]
pub(crate) struct Skipped {
    pub reply: u64,
    pub cross_node: u64,
}

/// One agent response inside an ask group: the session that answered,
/// when, the reply excerpt, and the transcript that carries it. The
/// evidence chain a reader walks to check the row's judged outcome.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct ResponseRef {
    pub session: String,
    pub name: String,
    pub ts: String,
    pub reply: String,
    pub transcript: String,
}

/// One merged ask. `key` is `node:<id>` once linked, else `ask:<earliest
/// turn id>`; `aliases` keeps every turn id, so a reader that stored an
/// `ask:` key finds the row after it links.
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct RequestRow {
    pub key: String,
    pub aliases: Vec<String>,
    pub text: String,
    /// Distinct sessions the ask was typed in; `turn_count` counts turns.
    pub ask_count: usize,
    pub turn_count: usize,
    pub first_ts: String,
    pub last_ts: String,
    pub sessions: Vec<String>,
    pub node: Option<String>,
    pub node_link: Option<&'static str>,
    pub node_status: Option<String>,
    pub node_completed_at: Option<String>,
    pub reply_excerpt: String,
    /// Every agent response across the group's asks, oldest first.
    pub responses: Vec<ResponseRef>,
    /// `solved`, `lost`, `forgotten` or `open`: the judged outcome.
    pub outcome: &'static str,
    /// The observed facts the judgment rests on.
    pub outcome_why: String,
    /// `new`, `no_node`, `linked`, `stalled`, `done` or `regression`.
    pub state: &'static str,
    pub days_unaddressed: i64,
    /// `ask_count` x `days_unaddressed`.
    pub rank_score: i64,
}

#[derive(Debug, Default)]
pub(crate) struct Fold {
    pub rows: Vec<RequestRow>,
    pub skipped: Skipped,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RequestsError {
    #[error("no operator capture directory could be resolved")]
    NoCaptureDir,
    #[error("the backlog graph could not be read: {0}")]
    Graph(String),
}

fn epoch(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.timestamp())
}

fn content_words(text: &str) -> usize {
    text.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() >= 3 && !w.bytes().all(|b| b.is_ascii_digit()))
        .count()
}

fn is_reply(turn: &AskTurn) -> bool {
    let words = content_words(&turn.typed_text);
    words < MIN_WORDS || (turn.after_question && words < MIN_WORDS_AFTER_QUESTION)
}

/// The first node id typed in the ask that names a node in the graph.
fn mentioned(text: &str, nodes: &HashMap<&str, &NodeFacts>) -> Option<String> {
    static ID: OnceLock<regex::Regex> = OnceLock::new();
    let re = ID.get_or_init(|| {
        regex::Regex::new(r"\b[a-z][a-z0-9]{0,7}-[0-9a-f]{4,12}\b").expect("valid pattern")
    });
    let lower = text.to_lowercase();
    re.find_iter(&lower)
        .map(|m| m.as_str())
        .find(|id| nodes.contains_key(id))
        .map(str::to_string)
}

fn flat(text: &str, chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = flat.chars().take(chars).collect();
    if flat.chars().count() > chars {
        out.push('\u{2026}');
    }
    out
}

/// The node's newest movement: `touched_at` moves only on curation fields,
/// so a session span or a progress note counts too.
fn movement(node: &NodeFacts) -> Option<i64> {
    let spans = node
        .sessions
        .iter()
        .flat_map(|s| [s.started_at.as_deref(), s.ended_at.as_deref()]);
    let notes = node.progress_notes.iter().map(|n| n.ts.as_deref());
    std::iter::once(node.touched_at.as_deref())
        .chain(spans)
        .chain(notes)
        .flatten()
        .filter_map(epoch)
        .max()
}

/// Union-find over asks; a group holds at most one node.
struct Groups {
    parent: Vec<usize>,
    node: Vec<Option<String>>,
    refused: BTreeSet<(String, String)>,
}

// Every index is a member id in 0..n, the length both vecs are built with.
#[allow(clippy::indexing_slicing)]
impl Groups {
    fn new(nodes: Vec<Option<String>>) -> Self {
        Groups {
            parent: (0..nodes.len()).collect(),
            node: nodes,
            refused: BTreeSet::new(),
        }
    }

    fn node_of(&self, root: usize) -> Option<String> {
        self.node[root].clone()
    }

    fn find(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }

    /// Join two groups unless each carries a different node; a refusal
    /// records the node pair once.
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        if let (Some(x), Some(y)) = (&self.node[ra], &self.node[rb]) {
            if x != y {
                let pair = if x < y {
                    (x.clone(), y.clone())
                } else {
                    (y.clone(), x.clone())
                };
                self.refused.insert(pair);
                return;
            }
        }
        let node = self.node[ra].take().or_else(|| self.node[rb].take());
        self.parent[rb] = ra;
        self.node[ra] = node;
    }
}

/// An ask kept for the fold with its node link (`ack` or `mention`).
type Kept<'a> = (&'a AskTurn, Option<(String, &'static str)>);

/// Link each turn to a node and drop the replies: an ack outcome first,
/// then a node id typed in the ask. A linked turn is kept however short.
fn link_turns<'a>(
    turns: &'a [AskTurn],
    acks: &HashMap<String, String>,
    by_id: &HashMap<&str, &NodeFacts>,
    skipped: &mut Skipped,
) -> Vec<Kept<'a>> {
    let mut kept = Vec::new();
    for turn in turns {
        let link = acks
            .get(&turn.turn_id)
            .map(|id| (id.clone(), "ack"))
            .or_else(|| mentioned(&turn.typed_text, by_id).map(|id| (id, "mention")));
        if link.is_none() && is_reply(turn) {
            skipped.reply += 1;
            continue;
        }
        kept.push((turn, link));
    }
    kept
}

/// Merge the kept asks: same node first, then the same normalized text,
/// then word overlap at the repeated-asks floor.
fn merge(kept: &[Kept<'_>]) -> Groups {
    let mut groups = Groups::new(
        kept.iter()
            .map(|(_, l)| l.as_ref().map(|(id, _)| id.clone()))
            .collect(),
    );
    let mut by_node: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    let mut by_key: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, (turn, link)) in kept.iter().enumerate() {
        if let Some((id, _)) = link {
            by_node.entry(id.as_str()).or_default().push(i);
        }
        by_key
            .entry(flat(&turn.typed_text.to_lowercase(), KEY_CHARS))
            .or_default()
            .push(i);
    }
    for members in by_node.values().chain(by_key.values()) {
        if let Some((&head, rest)) = members.split_first() {
            for &m in rest {
                groups.union(head, m);
            }
        }
    }
    let words: Vec<HashSet<String>> = kept
        .iter()
        .map(|(t, _)| crate::repeated_asks::tokens(&t.typed_text.to_lowercase()))
        .collect();
    for (i, a) in words.iter().enumerate() {
        for (j, b) in words.iter().enumerate().skip(i + 1) {
            // An overlap of 0.6 needs the smaller set at 0.6 of the larger.
            let (small, large) = (a.len().min(b.len()), a.len().max(b.len()));
            if small * 5 < large * 3 || groups.find(i) == groups.find(j) {
                continue;
            }
            if crate::repeated_asks::same_tokens(a, b) {
                groups.union(i, j);
            }
        }
    }
    groups
}

/// Fold ask turns into merged request rows, highest rank first. `acks` maps
/// a turn id to its acked node id; `now` is epoch seconds.
pub(crate) fn fold(
    turns: &[AskTurn],
    acks: &HashMap<String, String>,
    nodes: &[NodeFacts],
    now: i64,
) -> Fold {
    let by_id: HashMap<&str, &NodeFacts> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut skipped = Skipped::default();
    let kept = link_turns(turns, acks, &by_id, &mut skipped);
    let mut groups = merge(&kept);
    let mut members: BTreeMap<usize, Vec<(&AskTurn, Option<&'static str>)>> = BTreeMap::new();
    for (i, (turn, link)) in kept.iter().enumerate() {
        let root = groups.find(i);
        members
            .entry(root)
            .or_default()
            .push((*turn, link.as_ref().map(|(_, how)| *how)));
    }
    let mut rows: Vec<RequestRow> = members
        .into_iter()
        .map(|(root, group)| row_for(&group, groups.node_of(root), &by_id, now))
        .collect();
    rows.sort_by(|a, b| {
        b.rank_score
            .cmp(&a.rank_score)
            .then_with(|| b.last_ts.cmp(&a.last_ts))
    });
    skipped.cross_node = groups.refused.len() as u64;
    Fold { rows, skipped }
}

/// The judged outcome and the facts behind it, from what the row already
/// observed. A closed node reads solved unless the ask came back after the
/// close, or the node went quiet: both read forgotten. A reply the operator
/// asked again past reads forgotten too. Silence with no node reads lost.
/// Everything still in flight reads open. `turns` is oldest first.
fn judge(
    state: &str,
    node: Option<&str>,
    node_status: Option<&str>,
    completed: Option<&str>,
    turns: &[&AskTurn],
    ask_count: usize,
) -> (&'static str, String) {
    let date = |ts: &str| ts.get(0..10).unwrap_or(ts).to_string();
    let id = node.unwrap_or("?");
    let status = node_status.unwrap_or("open");
    let newest = turns.last().copied();
    match state {
        "done" => (
            "solved",
            format!(
                "node {id} closed {}",
                completed
                    .map(|c| date(c))
                    .unwrap_or_else(|| "without a close date".into()),
            ),
        ),
        "regression" => (
            "forgotten",
            format!(
                "node {id} closed {}, asked again {}",
                completed.map(|c| date(c)).unwrap_or_else(|| "?".into()),
                newest.map(|t| date(&t.ts)).unwrap_or_else(|| "?".into()),
            ),
        ),
        "stalled" => (
            "forgotten",
            format!("node {id} {status} took the ask and has not moved since"),
        ),
        "linked" => ("open", format!("node {id} {status} holds the ask")),
        "no_node" => {
            // The reply a repeat forgot is one that came before the newest
            // ask: a reply on the newest turn itself is still fresh, not
            // forgotten evidence, so it reads open.
            let answered = turns
                .split_last()
                .and_then(|(_newest, earlier)| {
                    earlier
                        .iter()
                        .rev()
                        .find(|t| !t.reply_excerpt.is_empty())
                        .map(|t| (t, true))
                })
                .or_else(|| {
                    turns
                        .iter()
                        .rev()
                        .find(|t| !t.reply_excerpt.is_empty())
                        .map(|t| (t, false))
                });
            match answered {
                Some((t, true)) if ask_count > 1 => (
                    "forgotten",
                    format!(
                        "reply {} in {}, asked again {}",
                        date(&t.ts),
                        t.session_id,
                        newest.map(|t| date(&t.ts)).unwrap_or_else(|| "?".into()),
                    ),
                ),
                Some((t, _)) => ("open", format!("reply {} in {}", date(&t.ts), t.session_id)),
                None => (
                    "lost",
                    format!(
                        "no reply and no node since {}",
                        newest.map(|t| date(&t.ts)).unwrap_or_else(|| "?".into()),
                    ),
                ),
            }
        }
        _ => ("open", format!("asked {ask_count}x, awaiting a reply")),
    }
}

/// The session name a response line shows: the registry name, or none. A
/// name that is empty or echoes the session id is the enumerator's fallback,
/// not a name, so it clears and the boards show their unnamed-session label
/// while the id stays in the web title for provenance.
fn display_name(session: &str, name: &str) -> String {
    let id_short = session.get(0..8).unwrap_or(session);
    if name.is_empty() || name == session || name == id_short {
        String::new()
    } else {
        name.to_string()
    }
}

/// One group's row. `group` holds the turns oldest first (the walk sorts
/// them) with each turn's own link kind.
fn row_for(
    group: &[(&AskTurn, Option<&'static str>)],
    node: Option<String>,
    by_id: &HashMap<&str, &NodeFacts>,
    now: i64,
) -> RequestRow {
    let mut turns: Vec<&AskTurn> = group.iter().map(|(t, _)| *t).collect();
    turns.sort_by(|a, b| a.ts.cmp(&b.ts));
    let first = turns.first().copied();
    let newest = turns.last().copied();
    let mut sessions: Vec<String> = Vec::new();
    for t in &turns {
        if !sessions.contains(&t.session_id) {
            sessions.push(t.session_id.clone());
        }
    }
    let node_link = node.as_ref().and(
        group
            .iter()
            .filter_map(|(_, how)| *how)
            .min_by_key(|how| *how != "ack"),
    );
    let facts = node.as_deref().and_then(|id| by_id.get(id).copied());
    let first_epoch = first.and_then(|t| epoch(&t.ts)).unwrap_or(now);
    let last_epoch = newest.and_then(|t| epoch(&t.ts)).unwrap_or(now);
    let since = |from: i64| (now - from).max(0) / DAY;
    let (state, days) = match facts {
        Some(n) if matches!(n.status.as_deref(), Some("done") | Some("superseded")) => {
            let closed = n
                .completed_at
                .as_deref()
                .or(n.touched_at.as_deref())
                .and_then(epoch);
            let reask =
                closed.and_then(|c| turns.iter().filter_map(|t| epoch(&t.ts)).find(|&ts| ts > c));
            match reask {
                Some(ts) => ("regression", since(ts)),
                None => ("done", 0),
            }
        }
        Some(n) => {
            let still = movement(n).is_none_or(|m| now - m > STALLED_DAYS * DAY);
            (if still { "stalled" } else { "linked" }, since(first_epoch))
        }
        None if now - last_epoch > NO_NODE_DAYS * DAY => ("no_node", since(first_epoch)),
        None => ("new", since(first_epoch)),
    };
    let key = match (&node, first) {
        (Some(id), _) => format!("node:{id}"),
        (None, Some(t)) => format!("ask:{}", t.turn_id),
        (None, None) => String::new(),
    };
    let ask_count = sessions.len();
    let responses: Vec<ResponseRef> = turns
        .iter()
        .map(|t| ResponseRef {
            session: t.session_id.clone(),
            name: display_name(&t.session_id, &t.name),
            ts: t.ts.clone(),
            reply: t.reply_excerpt.clone(),
            transcript: t.transcript.clone(),
        })
        .collect();
    let (outcome, outcome_why) = judge(
        state,
        node.as_deref(),
        facts.and_then(|n| n.status.as_deref()),
        facts.and_then(|n| n.completed_at.as_deref()),
        &turns,
        ask_count,
    );
    RequestRow {
        key,
        aliases: turns.iter().map(|t| t.turn_id.clone()).collect(),
        text: newest
            .map(|t| flat(&t.typed_text, TEXT_CHARS))
            .unwrap_or_default(),
        ask_count,
        turn_count: turns.len(),
        first_ts: first.map(|t| t.ts.clone()).unwrap_or_default(),
        last_ts: newest.map(|t| t.ts.clone()).unwrap_or_default(),
        sessions,
        node_status: facts.and_then(|n| n.status.clone()),
        node_completed_at: facts.and_then(|n| n.completed_at.clone()),
        node,
        node_link,
        reply_excerpt: newest.map(|t| t.reply_excerpt.clone()).unwrap_or_default(),
        responses,
        outcome,
        outcome_why,
        state,
        days_unaddressed: days,
        rank_score: days * ask_count as i64,
    }
}

/// One session's ack ledger as turn id -> node id. A missing or unreadable
/// ledger links nothing; a malformed line is skipped.
pub(crate) fn read_ack_links(path: &Path) -> HashMap<String, String> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    raw.lines()
        .filter_map(|line| serde_json::from_str::<AckRow>(line).ok())
        .filter_map(|row| {
            let node = row.outcome?.strip_prefix("node:")?.trim().to_string();
            (!node.is_empty()).then_some((row.turn_id?, node))
        })
        .collect()
}

/// Every read the fold needs, once: the cached reach since `since`, each
/// asking session's ack ledger, and the graph.
pub(crate) fn load(
    home: &crate::paths::AgentsHome,
    since: &str,
    since_epoch: u64,
) -> Result<Fold, RequestsError> {
    let registry = crate::mail_threads::registry_rows();
    let inputs = crate::operator_turns::all_asks_inputs(home, &registry)
        .ok_or(RequestsError::NoCaptureDir)?;
    let turns = crate::you_threads::ask_turns_since(
        &inputs,
        &crate::operator_turns::codex_sessions_dir(),
        &crate::operator_turns::reach_cache_path(home),
        since,
        since_epoch,
    );
    let mut acks: HashMap<String, String> = HashMap::new();
    let asking: BTreeSet<&str> = turns.iter().map(|t| t.session_id.as_str()).collect();
    for sid in asking {
        acks.extend(read_ack_links(&crate::operator_turns::ledger_path(
            &inputs.capture_dir,
            sid,
        )));
    }
    let rows = crate::backlog::decisions_cli::read_graph_entries().map_err(RequestsError::Graph)?;
    let nodes: Vec<NodeFacts> = rows
        .iter()
        .filter_map(|row| NodeFacts::deserialize(row).ok())
        .collect();
    Ok(fold(&turns, &acks, &nodes, chrono::Utc::now().timestamp()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-11-21T00:00:00Z";

    fn now() -> i64 {
        epoch(NOW).unwrap()
    }

    fn turn(session: &str, id: &str, ts: &str, text: &str) -> AskTurn {
        AskTurn {
            session_id: session.into(),
            name: session.into(),
            turn_id: id.into(),
            ts: ts.into(),
            typed_text: text.into(),
            reply_excerpt: String::new(),
            state: "answered",
            project: "p".into(),
            transcript: "t".into(),
            after_question: false,
        }
    }

    fn node(id: &str, status: &str, completed_at: Option<&str>, touched: &str) -> NodeFacts {
        NodeFacts {
            id: id.into(),
            status: Some(status.into()),
            completed_at: completed_at.map(str::to_string),
            touched_at: Some(touched.into()),
            ..Default::default()
        }
    }

    fn acks(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(t, n)| (t.to_string(), n.to_string()))
            .collect()
    }

    /// AC2-HP and the rank order: one ask typed in three sessions, acked to
    /// one node that has not moved, is one row counted three times and
    /// ranked above a younger single ask.
    #[test]
    fn three_sessions_one_node_fold_to_one_row_ranked_first() {
        let turns = [
            turn(
                "s1",
                "t1",
                "2026-11-10T00:00:00Z",
                "make the board show my repeated asks",
            ),
            turn(
                "s2",
                "t2",
                "2026-11-12T00:00:00Z",
                "please the board needs my repeated asks",
            ),
            turn(
                "s2",
                "t3",
                "2026-11-12T01:00:00Z",
                "and a count beside each repeated ask",
            ),
            turn(
                "s3",
                "t4",
                "2026-11-15T00:00:00Z",
                "where is the repeated asks board",
            ),
            turn(
                "s4",
                "t5",
                "2026-11-19T00:00:00Z",
                "rename the fleet pane title bar",
            ),
        ];
        let links = acks(&[
            ("t1", "x-aaaa"),
            ("t2", "x-aaaa"),
            ("t3", "x-aaaa"),
            ("t4", "x-aaaa"),
        ]);
        let nodes = [node("x-aaaa", "ready", None, "2026-11-10T00:00:00Z")];
        let out = fold(&turns, &links, &nodes, now());
        assert_eq!(out.rows.len(), 2, "{:?}", out.rows);
        let top = out.rows.first().unwrap();
        assert_eq!(top.key, "node:x-aaaa");
        assert_eq!((top.ask_count, top.turn_count), (3, 4));
        assert_eq!(top.state, "stalled");
        assert_eq!(top.node_link, Some("ack"));
        assert_eq!(top.days_unaddressed, 11);
        assert_eq!(top.rank_score, 33);
        assert_eq!(top.aliases, ["t1", "t2", "t3", "t4"]);
        let young = out.rows.get(1).unwrap();
        assert_eq!((young.state, young.key.as_str()), ("no_node", "ask:t5"));
    }

    /// AC2-EDGE: a done node asked again after it completed reads
    /// regression, and its days count from the re-ask.
    #[test]
    fn a_reask_after_completion_reads_regression() {
        let turns = [
            turn(
                "s1",
                "t1",
                "2026-11-05T00:00:00Z",
                "ship the x-cccc merge stamp fix",
            ),
            turn(
                "s2",
                "t2",
                "2026-11-11T00:00:00Z",
                "x-cccc is still broken on the stamp",
            ),
        ];
        let nodes = [node(
            "x-cccc",
            "done",
            Some("2026-11-10T00:00:00Z"),
            "2026-11-10T00:00:00Z",
        )];
        let out = fold(&turns, &HashMap::new(), &nodes, now());
        assert_eq!(out.rows.len(), 1);
        let row = out.rows.first().unwrap();
        assert_eq!((row.state, row.node_link), ("regression", Some("mention")));
        assert_eq!((row.days_unaddressed, row.rank_score), (10, 20));
    }

    /// AC2-ERR and AC2-SPLIT: a short typed answer to the agent's question
    /// is no request, and one text acked to two nodes stays two rows.
    #[test]
    fn a_reply_is_skipped_and_two_nodes_never_fuse() {
        let mut yes = turn("s1", "t0", "2026-11-19T00:00:00Z", "yes");
        yes.after_question = true;
        let turns = [
            yes,
            turn(
                "s1",
                "t1",
                "2026-11-19T01:00:00Z",
                "write the user doc for this page",
            ),
            turn(
                "s2",
                "t2",
                "2026-11-19T02:00:00Z",
                "write the user doc for this page",
            ),
        ];
        let links = acks(&[("t1", "x-aaaa"), ("t2", "x-bbbb")]);
        let nodes = [
            node("x-aaaa", "ready", None, "2026-11-20T00:00:00Z"),
            node("x-bbbb", "ready", None, "2026-11-20T00:00:00Z"),
        ];
        let out = fold(&turns, &links, &nodes, now());
        assert_eq!(
            out.skipped,
            Skipped {
                reply: 1,
                cross_node: 1
            }
        );
        let mut keys: Vec<&str> = out.rows.iter().map(|r| r.key.as_str()).collect();
        keys.sort();
        assert_eq!(keys, ["node:x-aaaa", "node:x-bbbb"]);
        assert!(out.rows.iter().all(|r| r.state == "linked"));
    }

    /// The judged outcome table: a closed node with no return reads solved,
    /// a closed node asked again or a stalled one reads forgotten, silence
    /// with no node reads lost, and a reply the operator asked again past
    /// reads forgotten with the reply and the re-ask as its evidence.
    #[test]
    fn judged_outcomes_read_the_observed_facts() {
        let mut solved_ask = turn(
            "s1",
            "t1",
            "2026-11-05T00:00:00Z",
            "fix the x-cccc stamp regression",
        );
        solved_ask.reply_excerpt = "fixed in the merge stamp".into();
        let mut forgotten_ask = turn(
            "s2",
            "t2",
            "2026-11-12T00:00:00Z",
            "the x-dddd pane is still blank",
        );
        forgotten_ask.reply_excerpt = "looking into it".into();
        let mut repeat = turn("s2", "t3", "2026-11-18T00:00:00Z", "x-dddd is still blank");
        repeat.reply_excerpt = "on it now".into();
        let lost_ask = turn(
            "s3",
            "t4",
            "2026-11-15T00:00:00Z",
            "the install doc is missing steps",
        );
        let nodes = [
            node(
                "x-cccc",
                "done",
                Some("2026-11-08T00:00:00Z"),
                "2026-11-08T00:00:00Z",
            ),
            node("x-dddd", "ready", None, "2026-11-13T00:00:00Z"),
        ];
        let out = fold(
            &[solved_ask, forgotten_ask, repeat, lost_ask],
            &HashMap::new(),
            &nodes,
            now(),
        );
        // x-dddd: movement 11-13, now 11-21: 8 days idle -> stalled ->
        // forgotten, and the response chain carries both turns.
        let mut by_key: BTreeMap<&str, &RequestRow> =
            out.rows.iter().map(|r| (r.key.as_str(), r)).collect();
        let solved = by_key.remove("node:x-cccc").unwrap();
        assert_eq!((solved.state, solved.outcome), ("done", "solved"));
        assert!(
            solved.outcome_why.contains("closed 2026-11-08"),
            "{}",
            solved.outcome_why
        );
        let forgotten = by_key.remove("node:x-dddd").unwrap();
        assert_eq!(
            (forgotten.state, forgotten.outcome),
            ("stalled", "forgotten")
        );
        // x-dddd is stalled, so its outcome names the node, not a reply;
        // the no_node pairing below pins which reply a repeat forgets.
        let paired = fold(
            &[
                {
                    let mut a = turn(
                        "s5",
                        "t5",
                        "2026-11-10T00:00:00Z",
                        "add the quota screen today",
                    );
                    a.reply_excerpt = "drafted the screen".into();
                    a
                },
                {
                    let mut b = turn(
                        "s6",
                        "t6",
                        "2026-11-17T00:00:00Z",
                        "add the quota screen tomorrow",
                    );
                    b.reply_excerpt = "rebuilt it today".into();
                    b
                },
            ],
            &HashMap::new(),
            &[],
            now(),
        );
        let row = paired.rows.first().unwrap();
        assert_eq!(row.outcome, "forgotten", "{}", row.outcome_why);
        assert_eq!(row.ask_count, 2, "{}", row.outcome_why);
        assert!(
            row.outcome_why.contains("reply 2026-11-10")
                && row.outcome_why.contains("asked again 2026-11-17"),
            "{}",
            row.outcome_why
        );
        let lost = by_key
            .values()
            .find(|r| r.text.contains("install doc"))
            .unwrap();
        assert_eq!((lost.state, lost.outcome), ("no_node", "lost"));
        assert!(
            lost.outcome_why.contains("no reply"),
            "{}",
            lost.outcome_why
        );
        // t2 (11-12) and t3 (11-18) share node x-dddd -> one row; the
        // response chain carries both turns with their replies.
        assert_eq!(forgotten.responses.len(), 2, "{:?}", forgotten.responses);
        assert_eq!(
            forgotten.responses.first().unwrap().reply,
            "looking into it"
        );
        assert_eq!(
            forgotten.responses.last().unwrap().transcript,
            "t",
            "the transcript path rides each response"
        );
    }

    #[test]
    fn response_names_keep_handles_and_clear_id_fallbacks() {
        assert_eq!(super::display_name("2222bbbb-aaaa", "t-worker"), "t-worker");
        assert_eq!(super::display_name("2222bbbb-aaaa", "lara"), "lara");
        assert_eq!(super::display_name("2222bbbb-aaaa", ""), "");
        assert_eq!(super::display_name("2222bbbb-aaaa", "2222bbbb-aaaa"), "");
        assert_eq!(super::display_name("2222bbbb-aaaa", "2222bbbb"), "");
    }
}
