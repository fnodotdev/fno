//! The activity-feed shell-out leg : a bounded, fail-open shell-out to
//! `fno agent fleet feed --json`, in the same shape as [`crate::org_overlay`]'s
//! fold (: the 800ms cap it first copied was half the projection's
//! measured runtime, so the feed could never render).
//!
//! The projection itself lives in `fno-agent` (`feed.rs`): it joins
//! questions.jsonl and graph.json into ordered rows, so no second
//! implementation of the join can drift from the command's. This module only
//! carries the row shape the client renders and the one bounded call that
//! fetches it - off the UI loop, and a typed [`FeedError`] instead of a bare
//! `None` so the client can say WHICH failure fired.

use serde::Deserialize;
use std::time::Duration;

/// Ten seconds, org_overlay's budget for a comparable multi-store read.
/// The feed joins three stores and serialises 200 rows; its measured runtime
/// is 1.6s against the 800ms this file inherited from needs_overlay's cheap
/// reads, so under the old cap it could not succeed at all. A long budget
/// costs nothing: the fold runs off the UI loop, one at a time, and
/// `kill_on_drop` reaps the child on the timeout.
const SHELLOUT_TIMEOUT: Duration = Duration::from_secs(10);

/// A repeated feed string, shared. Kind, area, node, session, harness and
/// the lead fields repeat across a held history of about 50,000 rows, so
/// every row naming the same value points at one allocation.
#[derive(Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IStr(std::sync::Arc<str>);

impl IStr {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for IStr {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for IStr {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for IStr {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for IStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::fmt::Debug for IStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&*self.0, f)
    }
}

impl From<&str> for IStr {
    fn from(s: &str) -> Self {
        intern(s)
    }
}

impl From<String> for IStr {
    fn from(s: String) -> Self {
        intern(&s)
    }
}

impl PartialEq<str> for IStr {
    fn eq(&self, other: &str) -> bool {
        &*self.0 == other
    }
}

impl PartialEq<&str> for IStr {
    fn eq(&self, other: &&str) -> bool {
        &*self.0 == *other
    }
}

impl<'de> Deserialize<'de> for IStr {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        Ok(intern(&s))
    }
}

/// The one copy of `s` this thread holds. The pool keeps every distinct
/// value it has seen: tens of thousands of nodes and sessions at most, never
/// the per-row titles, which stay owned.
fn intern(s: &str) -> IStr {
    thread_local! {
        static POOL: std::cell::RefCell<std::collections::HashSet<std::sync::Arc<str>>> =
            std::cell::RefCell::new(std::collections::HashSet::new());
    }
    POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        if let Some(hit) = pool.get(s) {
            return IStr(hit.clone());
        }
        let fresh: std::sync::Arc<str> = s.into();
        pool.insert(fresh.clone());
        IStr(fresh)
    })
}

/// An interned optional field as the owned string the search and detail
/// readers take.
pub(crate) fn owned(v: &Option<IStr>) -> Option<String> {
    v.as_deref().map(str::to_string)
}

/// One feed row, as emitted by `fno agent fleet feed --json`. `session_id` is what
/// the deep link resolves through the sideline's own attach path.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FeedItem {
    pub ts: String,
    pub kind: IStr,
    #[serde(default)]
    pub node: Option<IStr>,
    /// The graph node's project directory for node-created launch prefill.
    #[serde(default)]
    pub cwd: Option<IStr>,
    #[serde(default)]
    pub session_id: Option<IStr>,
    #[serde(default)]
    pub harness: Option<IStr>,
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "ref")]
    pub r#ref: Option<String>,
    /// The registry row's worker name, on a removal: the handle the resume
    /// action and the copied command address.
    #[serde(default)]
    pub name: Option<IStr>,
    /// Who acted, when that is a mechanism rather than a session. Provenance,
    /// never an attach target.
    #[serde(default)]
    pub actor: Option<IStr>,
    /// The model observed for that session AT EVENT TIME.
    #[serde(default)]
    pub model: Option<IStr>,
    /// The effort recorded for that session at event time.
    #[serde(default)]
    pub effort: Option<IStr>,
    /// The pipeline phase the session was in.
    #[serde(default)]
    pub phase: Option<IStr>,
    /// A rendered recovery line the panel hands over verbatim, on a
    /// `session_reaped` row.
    #[serde(default)]
    pub detail: Option<String>,
    /// Why the row happened, when the source records one: a removal's
    /// recorded cause, verbatim.
    #[serde(default)]
    pub reason: Option<String>,
    /// `L{level} {scope}` for the team kinds and a teamed removal.
    #[serde(default)]
    pub team: Option<IStr>,
    /// The promoted worker's name on the role kinds; the feed search
    /// answers `l:` through it.
    #[serde(default)]
    pub holder: Option<IStr>,
    /// The lead or epic the row rolls up to; the panel groups on it.
    #[serde(default)]
    pub owner: Option<IStr>,
    /// The session that spawned this row's session, from the birth event.
    #[serde(default)]
    pub parent: Option<IStr>,
    /// The PR URL, on a ship row. The provenance view's PR action opens it.
    #[serde(default)]
    pub url: Option<String>,
    /// The role holder the row rolls up to; `l:` answers through it before
    /// the owner spelling.
    #[serde(default)]
    pub lead: Option<IStr>,
    /// True when `lead` names the LIVE team the projection resolved at
    /// render time; false when it is the event-time holder's fallback. The
    /// now-led-by link lights only on the live name.
    #[serde(default)]
    pub lead_current: bool,
    /// Which slice of the fleet the row is (mail, backlog, ship, agents,
    /// mux, fleet, ci), stamped by the projection.
    #[serde(default)]
    pub area: IStr,
    /// The row's position in the projection's total order. The held history
    /// never pages, so the wire value is skipped: 50,000 cursors cost about
    /// 7 MB, and a row's identity reads from its own fields.
    #[serde(skip)]
    pub cursor: String,
}

/// Why a feed fold failed. Each variant is a different user action - retune a
/// budget, free an admission slot, read the projection's stderr - which is
/// why they stopped collapsing into one `None`.
#[derive(Debug, Clone)]
pub enum FeedError {
    /// The projection outlived [`SHELLOUT_TIMEOUT`].
    Timeout,
    /// The projection never ran: spawn failure or a process-admission
    /// refusal. Carries the io error's own words.
    Spawn(String),
    /// The projection ran and exited non-zero. Carries its stderr.
    Exit(String),
    /// stdout was not the row array. Carries the serde error and stderr.
    Malformed(String),
}

impl std::fmt::Display for FeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeedError::Timeout => {
                write!(f, "feed timed out after {}s", SHELLOUT_TIMEOUT.as_secs())
            }
            FeedError::Spawn(e) => write!(f, "feed could not start: {e}"),
            FeedError::Exit(stderr) => write!(f, "feed exited non-zero: {stderr}"),
            FeedError::Malformed(e) => write!(f, "feed returned malformed JSON: {e}"),
        }
    }
}

/// The projection's stderr, first line, capped for the overlay footer. The
/// ONE shaping rule for stderr in this file: both failure renders go through
/// it, so an Exit line and a Malformed line cannot drift apart.
fn stderr_first_line(stderr: &[u8], none_label: &str) -> String {
    std::str::from_utf8(stderr)
        .ok()
        .and_then(|text| text.lines().next())
        .filter(|line| !line.is_empty())
        .unwrap_or(none_label)
        .chars()
        .take(160)
        .collect()
}

/// The Exit variant's stderr note; a stderr that says nothing (empty, or not
/// text at all) degrades to the exit's own spelling so the line never dangles.
fn stderr_note(stderr: &[u8], status: &std::process::ExitStatus) -> String {
    if std::str::from_utf8(stderr).is_ok() {
        stderr_first_line(stderr, "no stderr")
    } else {
        format!("{status}")
    }
}

/// One fold result, as it travels the client's single-flight channel: the
/// whole history, oldest first.
pub type FoldResult = Result<Vec<FeedItem>, FeedError>;

/// The `--limit` that reads every row: the projection keeps the newest
/// `limit`, and no history comes near this.
pub const FEED_HISTORY_ROWS: usize = 10_000_000;

/// The argv of the panel's one read: the whole history as a JSON array.
pub fn history_argv() -> Vec<String> {
    vec![
        "agent".to_string(),
        "fleet".to_string(),
        "feed".to_string(),
        "--json".to_string(),
        "--limit".to_string(),
        FEED_HISTORY_ROWS.to_string(),
    ]
}

/// Read the whole feed history, `Err` carrying the typed reason on any
/// failure. An empty history is `Ok` with no rows - a real answer.
pub async fn fetch_history() -> FoldResult {
    let mut command = crate::process_admission::tokio_command(crate::server::fno_bin());
    command
        .args(history_argv())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let fut = crate::process_admission::tokio_output(&mut command);
    let output = tokio::time::timeout(SHELLOUT_TIMEOUT, fut)
        .await
        .map_err(|_| FeedError::Timeout)?
        .map_err(|e| FeedError::Spawn(e.to_string()))?;
    if !output.status.success() {
        return Err(FeedError::Exit(stderr_note(&output.stderr, &output.status)));
    }
    parse_feed(&output.stdout, &output.stderr)
}

fn parse_feed(stdout: &[u8], stderr: &[u8]) -> Result<Vec<FeedItem>, FeedError> {
    serde_json::from_slice::<Vec<FeedItem>>(stdout).map_err(|e| {
        FeedError::Malformed(format!(
            "{e}; stderr: {}",
            stderr_first_line(stderr, "none")
        ))
    })
}

/// The context one event row's field map reads: the graph rows' project,
/// parent and open state per node, and the registry's session identities.
pub struct EventCtx {
    /// node id -> (project, parent, open)
    pub nodes: std::collections::HashMap<String, (Option<String>, Option<String>, bool)>,
    pub sessions: crate::search_query::SessionDirectory,
}

impl EventCtx {
    pub fn from_rows(
        rows: &[crate::node_row::NodeRow],
        registry: &[crate::agents_view::RegistryAgent],
    ) -> Self {
        let mut nodes = std::collections::HashMap::new();
        for r in rows {
            let Some(id) = r.id() else {
                continue;
            };
            let open = !r.is_completed();
            nodes.insert(id.to_string(), (r.project.clone(), r.parent.clone(), open));
        }
        EventCtx {
            nodes,
            sessions: crate::search_query::SessionDirectory::from_registry(registry),
        }
    }
}

/// The event row's search field map, the `Surface::Event` leg. The same
/// canonical keys the node leg answers; `s:` refuses here because kind is
/// the event's work state.
pub fn event_fields(item: &FeedItem, ctx: &EventCtx) -> crate::search_query::Fields {
    use crate::search_query::Fields;
    let mut f = Fields::new();
    let push = |f: &mut Fields, key: &str, val: Option<String>| {
        if let Some(v) = val.filter(|v| !v.is_empty()) {
            // Stamps stay as stored: the page's stamp parser reads the
            // RFC3339 markers case-sensitively.
            let v = if key == "ts" { v } else { v.to_lowercase() };
            let slot = f.entry(key.to_string()).or_default();
            if !slot.contains(&v) {
                slot.push(v);
            }
        }
    };
    push(&mut f, "id", owned(&item.node));
    push(&mut f, "kind", Some(item.kind.to_string()));
    if let Some(sid) = &item.session_id {
        push(&mut f, "session", Some(sid.to_string()));
        if let Some(entry) = ctx.sessions.get(sid.as_str()) {
            for id in &entry.ids {
                push(&mut f, "session", Some(id.clone()));
            }
            push(&mut f, "agent", Some(entry.name.clone()));
            push(&mut f, "harness", entry.harness.clone());
            push(&mut f, "model", entry.model.clone());
        }
    }
    push(&mut f, "spawner", owned(&item.parent));
    push(&mut f, "agent", owned(&item.name));
    push(&mut f, "actor", owned(&item.actor));
    push(&mut f, "harness", owned(&item.harness));
    push(&mut f, "model", owned(&item.model));
    push(&mut f, "effort", owned(&item.effort));
    push(&mut f, "phase", owned(&item.phase));
    if let Some(url) = &item.url {
        if let Some(rest) = url.split("/pull/").nth(1) {
            let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            push(&mut f, "pr", (!num.is_empty()).then_some(num));
        }
    }
    let node_ctx = item.node.as_deref().and_then(|n| ctx.nodes.get(n));
    let project = item
        .cwd
        .as_deref()
        .and_then(|cwd| {
            cwd.rsplit('/')
                .find(|seg| !seg.is_empty())
                .map(str::to_string)
        })
        .or_else(|| node_ctx.and_then(|(p, _, _)| p.clone()));
    push(&mut f, "project", project);
    let epic = item
        .owner
        .as_deref()
        .and_then(|o| o.strip_prefix("epic "))
        .and_then(|rest| rest.split_whitespace().next().map(str::to_string));
    push(&mut f, "epic", epic);
    push(&mut f, "epic", node_ctx.and_then(|(_, p, _)| p.clone()));
    // `in`: the node and its ancestors through the context.
    let mut chain: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Some(n) = &item.node {
        chain.push(n.to_string());
        seen.insert(n.to_string());
        let mut cur = node_ctx.and_then(|(_, p, _)| p.clone());
        while let Some(p) = cur {
            if !seen.insert(p.clone()) {
                break;
            }
            chain.push(p.clone());
            cur = ctx.nodes.get(&p).and_then(|(_, parent, _)| parent.clone());
        }
    }
    for id in chain {
        push(&mut f, "in", Some(id));
    }
    let holder = owned(&item.holder).or_else(|| {
        let o = item.owner.as_deref().unwrap_or("");
        if let Some(rest) = o.strip_prefix("lead ") {
            rest.split_once(" L").map(|(h, _)| h.to_string())
        } else {
            o.rsplit_once('(')
                .and_then(|(_, r)| r.strip_suffix(')').map(str::to_string))
        }
    });
    push(&mut f, "lead", owned(&item.lead).or_else(|| holder.clone()));
    push(&mut f, "agent", holder.clone());
    push(&mut f, "ts", Some(item.ts.clone()));
    // One answer for a row's area: the projection's stamp (`area_of`).
    push(
        &mut f,
        "area",
        (!item.area.is_empty()).then(|| item.area.to_string()),
    );
    if node_ctx.is_some_and(|(_, _, open)| *open) {
        push(&mut f, "is", Some("open".to_string()));
    }
    if item
        .session_id
        .as_deref()
        .and_then(|sid| ctx.sessions.get(sid))
        .is_some_and(|entry| !entry.exited)
    {
        push(&mut f, "is", Some("live".to_string()));
    }
    if item.node.is_some() {
        push(&mut f, "has", Some("node".to_string()));
    }
    if item.session_id.is_some() {
        push(&mut f, "has", Some("session".to_string()));
    }
    if item.url.as_deref().is_some_and(|u| u.contains("/pull/")) {
        push(&mut f, "has", Some("pr".to_string()));
    }
    if item.reason.is_some() {
        push(&mut f, "has", Some("reason".to_string()));
    }
    push(&mut f, "title", Some(item.title.clone()));
    push(&mut f, "details", item.reason.clone());
    push(&mut f, "details", item.detail.clone());
    push(&mut f, "text", Some(item.title.clone()));
    push(&mut f, "text", owned(&item.node));
    push(&mut f, "text", owned(&item.name));
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_deserializes() {
        let body = br#"[{"ts":"2026-09-02T17:12:52Z","kind":"node_started","node":"x-aaaa","session_id":"s-do","harness":"claude","title":"port the claim classifier","ref":null},{"ts":"2026-09-02T18:27:06Z","kind":"pr_created","node":"x-aaaa","session_id":"s-ship","title":"PR 1395","ref":"1395"}]"#;
        let items = parse_feed(body, b"").expect("a well-formed body parses");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, "node_started");
        assert_eq!(items[0].session_id.as_deref(), Some("s-do"));
        assert_eq!(items[1].r#ref.as_deref(), Some("1395"));
        // Both rows name node x-aaaa: one shared allocation, not two copies.
        let node_ptr = |i: usize| {
            items
                .get(i)
                .and_then(|it| it.node.as_deref())
                .map(str::as_ptr)
        };
        assert!(
            node_ptr(0).is_some() && node_ptr(0) == node_ptr(1),
            "a repeated node interns once"
        );
    }
    #[test]
    fn garbage_is_malformed_carrying_stderr() {
        let err = parse_feed(b"not json", b"skipped 3 malformed question lines\n")
            .expect_err("garbage is a typed error, not a silent None");
        assert!(matches!(err, FeedError::Malformed(_)));
        let rendered = err.to_string();
        assert!(rendered.contains("malformed JSON"));
        assert!(rendered.contains("skipped 3 malformed question lines"));
    }

    // AC5 and premise 12: the panel's one read spells the current command
    // and reads every row; no cursor rides it, so `--after ''`, which the
    // projection refuses with exit 2, can never be sent.
    #[test]
    fn history_argv_reads_every_row_with_no_cursor() {
        let argv = history_argv();
        assert!(argv.starts_with(&["agent".into(), "fleet".into(), "feed".into()]));
        assert!(argv.ends_with(&["--limit".into(), FEED_HISTORY_ROWS.to_string()]));
        assert!(!argv.iter().any(|a| a == "--after" || a == "--before"));
    }

    // regression: the timeout names itself and its duration, so the
    // rendered line is a budget statement, never the old generic failure.
    #[test]
    fn timeout_renders_the_budget_it_outran() {
        let line = FeedError::Timeout.to_string();
        assert!(line.contains("timed out after 10s"), "got: {line}");
        assert!(!line.contains("failed"));
    }
}
