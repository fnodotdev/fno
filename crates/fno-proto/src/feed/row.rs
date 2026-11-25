//! One activity-feed row, its shared strings and its search field map.
//! The mux client and the fno-agent reader parse the same row and answer
//! the same keys, so neither carries its own copy.

use super::grammar::{Fields, SessionDirectory};
use serde::{Deserialize, Serialize};

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

impl Serialize for IStr {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
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
pub fn owned(v: &Option<IStr>) -> Option<String> {
    v.as_deref().map(str::to_string)
}

/// One feed row, as emitted by `fno agent fleet feed --json`. `session_id` is what
/// the deep link resolves through the sideline's own attach path.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
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

/// The context one event row's field map reads: the graph rows' project,
/// parent and open state per node, and the registry's session identities.
#[derive(Debug, Default)]
pub struct EventCtx {
    /// node id -> (project, parent, open)
    pub nodes: std::collections::HashMap<String, (Option<String>, Option<String>, bool)>,
    pub sessions: SessionDirectory,
}

/// The event row's search field map, the `Surface::Event` leg. The same
/// canonical keys the node leg answers; `s:` refuses here because kind is
/// the event's work state.
pub fn event_fields(item: &FeedItem, ctx: &EventCtx) -> Fields {
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
