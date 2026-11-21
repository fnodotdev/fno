//! The holding-lead stamp: which lead held a node's scope at `completed_at`.
//!
//! Credit by time, never rewritten. A close stamps the live team record (the
//! holder at that instant IS the holder at `completed_at`); a succession after
//! the close never touches the field. A closed node with no record at all
//! stamps `name: "unknown"`, never a guess.
//!
//! Two producers, one shape. `stamp_at_close` runs on the close path and
//! resolves the LIVE record; the maintain backfill (derive_batch) covers
//! nodes closed before the field existed, from the canonical `lead_checkin`
//! journal rows alone (scope, ts, holder label, `data.holder_session`). The
//! backfill never reads the live record as a credit: the live holder is the
//! holder NOW, and stamping it on a node closed under a predecessor would
//! mis-credit by construction. The stored team title is the one exception:
//! it groups the rollups, and a group label that follows a rename is a
//! display fact, not a credit.
//!
//! A closed epic adds `holders`, every distinct journal holder over
//! `[created_at, completed_at]`. A failed journal walk answers `holders: null`
//! (unmeasured, the repo's null-with-a-reason rule), a walk with no rows in
//! the window answers `[]` (a measured empty).
//!
//! All resolution degrades: a fault stamps unknown and never fails the close.

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use super::workflows::text_at;

/// The node row's field name.
pub(crate) const FIELD: &str = "holding_lead";

/// The name a node stamps when nothing resolves.
pub(crate) const UNKNOWN: &str = "unknown";

fn field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.get(key)
}

fn s_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// The record-side read for the scorecard's per-record fields.
pub(crate) fn read_stamp_of_record(r: &Value) -> (Option<&str>, Option<&str>) {
    (
        s_at(r, "lead_title").filter(|t| !t.is_empty()),
        s_at(r, "lead_name"),
    )
}

/// True when the row already carries the stamp (present and null counts as
/// unstamped; an insert-only close or backfill never overwrites it).
pub(crate) fn is_stamped(row: &Value) -> bool {
    field(row, FIELD).map(|v| !v.is_null()).unwrap_or(false)
}

/// The reader-side read: `(title, name)` from a row's stamp, both None when
/// the field is absent or null. An empty title reads as None (the rollup
/// groups it under unknown).
pub(crate) fn read_stamp(node: &Value) -> (Option<&str>, Option<&str>) {
    match field(node, FIELD) {
        Some(v) if !v.is_null() => (s_at(v, "title").filter(|t| !t.is_empty()), s_at(v, "name")),
        _ => (None, None),
    }
}

/// The stamp object. `name` is the holder's display label or `unknown`;
/// `holder_session` is the credit anchor; `title` groups the rollups.
fn stamp_obj(
    scope: Option<&str>,
    name: Option<&str>,
    title: Option<&str>,
    holder: Option<&str>,
) -> Value {
    let mut obj = Map::new();
    obj.insert(
        "scope".into(),
        scope.map(|s| json!(s)).unwrap_or(Value::Null),
    );
    obj.insert("name".into(), json!(name.unwrap_or(UNKNOWN)));
    obj.insert(
        "title".into(),
        title.map(|t| json!(t)).unwrap_or(Value::Null),
    );
    obj.insert(
        "holder_session".into(),
        holder.map(|h| json!(h)).unwrap_or(Value::Null),
    );
    Value::Object(obj)
}

fn instant(ts: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    ts.parse::<chrono::DateTime<chrono::Utc>>().ok()
}

/// One journal row's holder reading: `(label, holder_session)`. Both may be
/// absent; a row with neither cannot credit anyone.
fn row_holder(event: &Value) -> Option<(Option<String>, Option<String>)> {
    let name = s_at(event, "name").map(str::to_string);
    let holder = field(event, "data")
        .and_then(|d| s_at(d, "holder_session"))
        .map(str::to_string);
    if name.is_none() && holder.is_none() {
        return None;
    }
    Some((name, holder))
}

/// The newest canonical check-in row at or before `before`, as a credit.
/// Events arrive newest-first (scan_scopes sorts them so), so the first
/// qualifying row wins.
fn holder_from_events(events: &[Value], before: &str) -> Option<(Option<String>, Option<String>)> {
    let bound = instant(before)?;
    events
        .iter()
        .filter(|e| s_at(e, "ts").and_then(instant).is_some_and(|t| t <= bound))
        .filter_map(row_holder)
        .next()
}

/// Every distinct holder over the window, oldest first. A session keeps the
/// label and `since` of its first row in the window; re-check-ins by the same
/// session dedupe away. Rows outside the window (unparseable ts included)
/// credit nothing.
fn holders_over_window(events: &[Value], created_at: Option<&str>, before: &str) -> Vec<Value> {
    let floor = created_at.and_then(instant);
    let ceiling = match instant(before) {
        Some(t) => t,
        None => return Vec::new(),
    };
    // Newest-first input; keep the LAST encounter per session, which is the
    // oldest row inside the window.
    let mut picked: BTreeMap<String, (Option<String>, chrono::DateTime<chrono::Utc>)> =
        BTreeMap::new();
    for e in events {
        let Some(ts) = s_at(e, "ts").and_then(instant) else {
            continue;
        };
        if ts > ceiling || floor.is_some_and(|f| ts < f) {
            continue;
        }
        let Some((name, holder)) = row_holder(e) else {
            continue;
        };
        let Some(holder) = holder else {
            // A labeled row with no session id still names a holder, but the
            // credit anchor is the session; label-only rows cannot dedupe and
            // cannot survive a holder rename, so they stay out of the list.
            continue;
        };
        picked.insert(holder, (name, ts));
    }
    let mut out: Vec<Value> = picked
        .into_iter()
        .map(|(holder, (name, ts))| {
            json!({
                "holder_session": holder,
                "name": name.unwrap_or_else(|| UNKNOWN.to_string()),
                "since": ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            })
        })
        .collect();
    // Tenure order, not session-id order: the map keyed by session sorts by
    // id, so the succession history needs its own sort on the since stamp.
    out.sort_by_key(|h| s_at(h, "since").and_then(instant));
    out
}

/// The node's owning team scope through the same owner read the org fold
/// answers from; a fault or a node no live team holds answers None.
fn resolve_scope(node: &Value, rows: &[Value]) -> Option<String> {
    let ctx = derive_ctx(rows);
    ctx.scope_of(node)
}

/// The live record for a scope: display label, stored title, holder session.
fn live_record(scope: &str) -> Option<(String, Option<String>, Option<String>)> {
    let home = crate::paths::AgentsHome::from_env_opt()?;
    let registry = home.registry_json();
    let teams = crate::territory::live_teams(&registry).ok()?;
    let team = teams.iter().find(|t| t.scope == scope)?;
    let title = crate::team_names::live_titles(&home.team_names_json(), &registry)
        .ok()
        .and_then(|t| t.get(scope).cloned());
    Some((
        crate::team_names::display(&team.holder, 1),
        title,
        team.holder_session.clone(),
    ))
}

/// The canonical journal events for a scope, newest first. `None` when the
/// walk fails (the caller answers unmeasured, never an empty history).
fn journal_events(cwd: &Path, scope: &str) -> Option<Vec<Value>> {
    let journals = crate::lead_history::default_journals(cwd);
    let payload = crate::lead_history::scan_scopes(&journals, Some(scope)).ok()?;
    field(&payload, "events").and_then(Value::as_array).cloned()
}

/// One shared resolution pass for a batch of rows: the owner map (one
/// registry read and one scope compile over the whole graph), the stored
/// titles, and one journal scan per distinct scope. A fault in one arm
/// degrades per node; nothing fails the batch.
pub(crate) struct DeriveCtx {
    owners: Option<HashMap<String, String>>,
    titles: Option<BTreeMap<String, String>>,
    /// One scan per distinct scope; `None` records a failed walk, so the
    /// caller answers unmeasured (null) rather than a measured empty.
    journals: BTreeMap<String, Option<Vec<Value>>>,
}

impl DeriveCtx {
    fn scope_of(&self, node: &Value) -> Option<String> {
        let node_id = s_at(node, "id")?;
        self.owners.as_ref()?.get(node_id).cloned()
    }

    /// The stored title for a scope; a fault or an untitled team answers
    /// None (the stamp writes null and the rollup groups it under unknown).
    fn title_of(&self, scope: Option<&str>) -> Option<String> {
        self.titles.as_ref()?.get(scope?).cloned()
    }

    /// The scope's journal events, cached from the batch's one scan per
    /// distinct scope. The walk binds to the CALLER's cwd: the journals a
    /// maintain pass reads are this machine's.
    fn events_for(&mut self, scope: &str) -> Option<&Vec<Value>> {
        if !self.journals.contains_key(scope) {
            let cwd = std::env::current_dir().unwrap_or_default();
            let events = journal_events(&cwd, scope);
            self.journals.insert(scope.to_string(), events);
        }
        self.journals.get(scope).and_then(Option::as_ref)
    }
}

/// Build the batch context: one owner read and one title read for the whole
/// graph; journal scans fill per distinct scope on demand.
pub(crate) fn derive_ctx(rows: &[Value]) -> DeriveCtx {
    let owners = crate::paths::AgentsHome::from_env_opt().and_then(|home| {
        let projects =
            crate::org_board::scope::project_map(&std::env::current_dir().unwrap_or_default());
        crate::territory::owner_read(&home.registry_json(), rows, &projects)
            .ok()
            .map(|(map, _live_scopes)| map)
    });
    let titles = crate::paths::AgentsHome::from_env_opt().and_then(|home| {
        crate::team_names::live_titles(&home.team_names_json(), &home.registry_json()).ok()
    });
    DeriveCtx {
        owners,
        titles,
        journals: BTreeMap::new(),
    }
}

/// The close-time stamp: the live record at this instant, which is the
/// record at `completed_at`. An epic adds the journal-derived holder list.
/// Never fails the close: every fault stamps unknown.
pub(crate) fn stamp_at_close(node: &Value, rows: &[Value]) -> Value {
    let scope = resolve_scope(node, rows);
    let scope_str = scope.as_deref();
    let (name, title, holder) = match scope_str.and_then(live_record) {
        Some((name, title, holder)) => (Some(name), title, holder),
        None => (None, None, None),
    };
    let mut stamp = stamp_obj(
        scope_str,
        name.as_deref(),
        title.as_deref(),
        holder.as_deref(),
    );
    if s_at(node, "type") == Some("epic") {
        let holders = match scope_str {
            Some(scope) => {
                let cwd = text_at(node, "cwd")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                let events = journal_events(&cwd, scope);
                let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                epic_holders_field(events.as_deref(), s_at(node, "created_at"), &now)
            }
            None => Value::Null,
        };
        if let Some(obj) = stamp.as_object_mut() {
            obj.insert("holders".into(), holders);
        }
    }
    stamp
}

/// The epic's `holders` field: the journal holders over the window. A
/// failed walk answers null (unmeasured); a walk with no window rows
/// answers an empty list (measured empty).
fn epic_holders_field(events: Option<&[Value]>, created_at: Option<&str>, done: &str) -> Value {
    match events {
        Some(events) => Value::Array(holders_over_window(events, created_at, done)),
        None => Value::Null,
    }
}

/// The batch backfill: every done row without a stamp gets `(id, stamp)`
/// pairs, the journal alone crediting the holder at `completed_at`. The
/// live record is never a credit here; the stored title only groups.
pub(crate) fn derive_batch(ctx: &mut DeriveCtx, rows: &[Value]) -> Vec<(String, Value)> {
    let candidates: Vec<&Value> = rows
        .iter()
        .filter(|e| s_at(e, "status") == Some("done") && !is_stamped(e))
        .collect();
    let mut out = Vec::with_capacity(candidates.len());
    for node in candidates {
        let Some(id) = s_at(node, "id").map(str::to_string) else {
            continue;
        };
        let scope = ctx.scope_of(node);
        let title = ctx.title_of(scope.as_deref());
        let done_at = s_at(node, "completed_at");
        let credited = match (scope.as_deref(), done_at) {
            (Some(scope), Some(done)) => ctx
                .events_for(scope)
                .and_then(|events| holder_from_events(events, done)),
            _ => None,
        };
        let (name, holder) = match credited {
            Some((name, holder)) => (name, holder),
            None => (None, None),
        };
        let mut stamp = stamp_obj(
            scope.as_deref(),
            name.as_deref(),
            title.as_deref(),
            holder.as_deref(),
        );
        if s_at(node, "type") == Some("epic") {
            let holders = match (scope.as_deref(), done_at) {
                (Some(scope), Some(done)) => epic_holders_field(
                    ctx.events_for(scope).map(|v| v.as_slice()),
                    s_at(node, "created_at"),
                    done,
                ),
                _ => Value::Null,
            };
            if let Some(obj) = stamp.as_object_mut() {
                obj.insert("holders".into(), holders);
            }
        }
        out.push((id, stamp));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(ts: &str, name: Option<&str>, holder: Option<&str>) -> Value {
        let mut event = json!({"scope": "epic-alpha", "ts": ts, "change": "beat", "data": {}});
        let obj = event.as_object_mut().unwrap();
        if let Some(n) = name {
            obj.insert("name".into(), json!(n));
        }
        if let Some(h) = holder {
            if let Some(data) = obj.get_mut("data").and_then(Value::as_object_mut) {
                data.insert("holder_session".into(), json!(h));
            }
        }
        event
    }

    #[test]
    fn holder_at_picks_newest_row_at_or_before_completed() {
        let events = vec![
            row("2026-11-20T12:00:00Z", Some("Avery"), Some("s-b")),
            row("2026-11-18T09:00:00Z", Some("Barnaby"), Some("s-a")),
        ];
        let (name, holder) =
            holder_from_events(&events, "2026-11-19T00:00:00Z").expect("a row inside the bound");
        assert_eq!(name.as_deref(), Some("Barnaby"));
        assert_eq!(holder.as_deref(), Some("s-a"));
        // A succession after the close never steals the credit.
        let (name, holder) =
            holder_from_events(&events, "2026-11-10T00:00:00Z").expect("the older row");
        assert_eq!(holder.as_deref(), Some("s-a"));
        let _ = name;
    }

    #[test]
    fn holder_at_reads_none_without_history_in_the_window() {
        let events = vec![row("2026-11-20T12:00:00Z", Some("Avery"), Some("s-b"))];
        assert!(holder_from_events(&events, "2026-11-01T00:00:00Z").is_none());
        assert!(holder_from_events(&events, "not-a-ts").is_none());
        assert!(holder_from_events(&[], "2026-11-20T12:00:00Z").is_none());
    }

    #[test]
    fn holders_over_window_dedupes_and_orders_oldest_first() {
        // The older tenure carries the alphabetically larger id, so a
        // session-id ordering cannot fake tenure order: s-z (Barnaby, from
        // 11-18) must come before s-a (Avery, from 11-19).
        let events = vec![
            row("2026-11-20T12:00:00Z", Some("Avery"), Some("s-a")),
            row("2026-11-19T12:00:00Z", Some("Avery"), Some("s-a")),
            row("2026-11-18T12:00:00Z", Some("Barnaby"), Some("s-z")),
            row("2026-11-01T12:00:00Z", Some("Early"), Some("s-0")),
        ];
        let holders = holders_over_window(
            &events,
            Some("2026-11-15T00:00:00Z"),
            "2026-11-21T00:00:00Z",
        );
        assert_eq!(
            holders.len(),
            2,
            "the pre-window row and the re-check-in dedupe"
        );
        assert_eq!(s_at(&holders[0], "holder_session"), Some("s-z"));
        assert_eq!(s_at(&holders[0], "name"), Some("Barnaby"));
        assert_eq!(s_at(&holders[1], "holder_session"), Some("s-a"));
        assert_eq!(
            s_at(&holders[1], "name"),
            Some("Avery"),
            "the first label in the window wins"
        );
        assert!(holders_over_window(&events, None, "bad-ts").is_empty());
    }

    #[test]
    fn stamp_reads_unknown_when_nothing_resolves() {
        let node = json!({"id": "x-aaaa", "cwd": "/nonexistent/no-such-dir", "type": "feature"});
        let stamp = stamp_at_close(&node, &[]);
        assert_eq!(s_at(&stamp, "name"), Some(UNKNOWN));
        assert_eq!(s_at(&stamp, "holder_session"), None);
        assert!(!is_stamped(&json!({})));
        assert!(is_stamped(&json!({"holding_lead": {"name": "x"}})));
        assert!(!is_stamped(&json!({"holding_lead": Value::Null})));
    }
}
