//! The check-in's blueprint reading: which unplanned nodes to blueprint and
//! which to route straight to target. Extracted from lead_checkin.rs under
//! the file budget's shrink rule; the beat's print block reads the JSON this
//! module returns. Each unplanned row routes through the lifecycle command
//! table (`dispatch.blueprint_floor`), so the beat's starts list can never
//! disagree with the routing the dispatch doors answer.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use std::collections::HashMap;
use std::path::Path;

use crate::lead_wake::scope_members;
use crate::state::RegistryEntry;

/// The unplanned rows this team must not dispatch: node id -> the canonical
/// scope of the live team that owns it, other than this team's own. The
/// fold's owner read is the one instrument; an unread read is the caller's
/// error, never an empty map that would let another lead's node print.
pub(crate) fn other_owned_scopes(
    folded: &Result<Value, String>,
    scope: &str,
) -> Result<HashMap<String, String>, String> {
    let folded = folded.as_ref().map_err(Clone::clone)?;
    let mine = crate::territory::canonical_scope(scope);
    let scopes = folded
        .get("owned_scopes")
        .and_then(|v| v.as_object())
        .ok_or_else(|| {
            folded
                .get("fold")
                .and_then(|f| f.get("owned_reason"))
                .and_then(Value::as_str)
                .unwrap_or("the fold names no owner map")
                .to_string()
        })?;
    Ok(scopes
        .iter()
        .filter_map(|(id, sc)| {
            sc.as_str()
                .filter(|s| *s != mine)
                .map(|s| (id.clone(), s.to_string()))
        })
        .collect())
}

/// The default blueprint-subagent ceiling when no provider budget applies
/// (no new config key: a registry row is barred, so the default is a named
/// Rust constant). Two per lead with one always running: law d-c100e5ec.
pub(crate) const DEFAULT_BLUEPRINT_CEILING: usize = 2;

/// One seat: a leadless epic's scope id and the dispatchable rows riding it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct SeatScope {
    pub(crate) scope: String,
    pub(crate) children: Vec<String>,
}

/// The check-in row shapes the seat walk reads, typed once at the read.
#[derive(Deserialize, Default)]
#[serde(default)]
struct SeatEntry {
    id: Option<String>,
    parent: Option<String>,
    #[serde(rename = "type")]
    node_type: Option<String>,
    status: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SeatFold {
    owned_scopes: HashMap<String, Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SeatBoard {
    queues: Vec<SeatQueue>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SeatQueue {
    name: Option<String>,
    rows: Vec<SeatRow>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SeatRow {
    id: Option<String>,
}

/// The leadless epic sub-scopes an L0/L1 beat seats: epics that hold
/// dispatchable children on this beat and no live team of their own. One
/// entry per epic: its scope id and the dispatchable rows that ride it. An
/// unresolved level seats nothing (an unpromoted session has no grant the
/// spawn gate would accept), and an L2+ lead builds its own org's work
/// rather than seating over a sibling.
pub(crate) fn seat_leads(
    scope: &str,
    level: Option<i64>,
    entries: &[Value],
    folded: Option<&Value>,
    board: Option<&Value>,
) -> Vec<SeatScope> {
    // An unread level, fold or board answers no seats: seating is an offer
    // whose failure mode is "no seat lines print". An unread fold's empty
    // owner map never prints either: the same unread fold fails
    // other_owned_scopes, which fails this reading.
    let Some(level) = level else {
        return Vec::new();
    };
    if level > 1 {
        return Vec::new();
    }
    let fold: SeatFold = folded
        .and_then(|f| serde_json::from_value(f.clone()).ok())
        .unwrap_or_default();
    let owned = &fold.owned_scopes;
    let typed: Vec<SeatEntry> = entries
        .iter()
        .filter_map(|e| serde_json::from_value(e.clone()).ok())
        .collect();
    let by_id: HashMap<&str, &SeatEntry> = typed
        .iter()
        .filter_map(|e| e.id.as_deref().map(|id| (id, e)))
        .collect();
    let b: SeatBoard = board
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let row_ids: Vec<&str> = b
        .queues
        .iter()
        .filter(|q| matches!(q.name.as_deref(), Some("unplanned") | Some("undispatched")))
        .flat_map(|q| q.rows.iter())
        .filter_map(|r| r.id.as_deref())
        .collect();
    // One walk per dispatchable row: the first open epic on its parent chain
    // that no live team owns is the seat scope. The walk stops at this
    // session's own scope and at any live team's node, so a level-1 lead
    // only ever seats a strict descendant of its own scope (the
    // strict-subset grant rule, territory.rs), while a level-0 lead can
    // seat over any leadless epic in the project.
    let mut grouped: std::collections::BTreeMap<&str, Vec<&str>> = Default::default();
    for row in &row_ids {
        let mut cur = Some(*row);
        let mut seat: Option<&str> = None;
        while let Some(id) = cur {
            if scope_members(scope).any(|m| m == id) || owned.contains_key(id) {
                break;
            }
            let Some(e) = by_id.get(id) else {
                break;
            };
            let open = !matches!(e.status.as_deref(), Some("done") | Some("superseded"));
            if e.node_type.as_deref() == Some("epic") && open {
                seat = Some(id);
                break;
            }
            cur = e.parent.as_deref();
        }
        if let Some(epic) = seat {
            grouped.entry(epic).or_default().push(row);
        }
    }
    grouped
        .into_iter()
        .map(|(epic, children)| SeatScope {
            scope: epic.to_string(),
            children: children.into_iter().map(str::to_string).collect(),
        })
        .collect()
}

/// The most adjacent blueprint offers a drained beat names.
const OFFER_LINES: usize = 2;

/// The statuses a node may hold and still be offered for a blueprint: open
/// work that no state parks. `idea` counts: the lifecycle table routes a
/// planless high idea to /blueprint, whose discovery gate grounds it, so a
/// drained scope offers it instead of naming a triage step first.
const OFFERABLE_STATUSES: [&str; 5] = ["idea", "intake", "triage", "ready", "next"];

/// The graph-row fields the offer walk reads, typed once at the read.
#[derive(Deserialize, Default)]
#[serde(default)]
struct OfferMeta {
    tags: Vec<String>,
    archived_at: Option<Value>,
}

/// One printed offer: the node, the lifecycle command that starts it
/// (`/blueprint` or `/target`), how it sits near the scope, its title.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Offer {
    id: String,
    command: String,
    near: String,
    title: String,
}

/// The command that starts one node through the lifecycle command the table
/// answered. The pick and the offer line build theirs here, so they cannot
/// drift. `labelled` puts the `start` word before a blueprint command, the
/// form the pick line prints.
pub(crate) fn start_command(command: &str, id: &str, labelled: bool) -> String {
    match (command == "/target", labelled) {
        (true, _) => format!("fno agent spawn \"/fno:target {id}\""),
        (false, true) => format!("start /fno:blueprint subagent {id}"),
        (false, false) => format!("/fno:blueprint subagent {id}"),
    }
}

/// True when the row is open work no state parks: an offerable status, no
/// lock and no archive stamp. The offer walk and the scope-root routing read
/// the same three fields.
fn open_unparked(row: &Value) -> bool {
    let status = crate::lead_checkin::s_str(row, "status").unwrap_or("");
    let locked = crate::lead_checkin::s_str(row, "locked_by").is_some_and(|l| !l.is_empty());
    let archived = OfferMeta::deserialize(row)
        .unwrap_or_default()
        .archived_at
        .is_some_and(|v| !v.is_null() && v != Value::Bool(false));
    OFFERABLE_STATUSES.contains(&status) && !locked && !archived
}

/// The node and then its ancestors, nearest first. A parent cycle ends the walk.
fn chain(by_id: &HashMap<&str, &Value>, start: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = Some(start.to_string());
    while let Some(id) = cur {
        if out.contains(&id) {
            break;
        }
        cur = by_id
            .get(id.as_str())
            .and_then(|e| crate::lead_checkin::s_str(e, "parent"))
            .map(str::to_string);
        out.push(id);
    }
    out
}

/// The fewest hops between a node and a scope: up from the node to an
/// ancestor, then down to the member that shares it. A node that shares no
/// ancestor with any member is farthest.
fn nearest(path: &[String], members: &[Vec<String>]) -> (usize, String) {
    members
        .iter()
        .flat_map(|member| {
            path.iter().enumerate().filter_map(|(i, a)| {
                member
                    .iter()
                    .position(|m| m == a)
                    .map(|j| (i + j, a.clone()))
            })
        })
        .min()
        .unwrap_or((usize::MAX, String::new()))
}

/// One offer candidate's sort key: hops to a shared ancestor, audience miss,
/// domain miss, priority, the shared ancestor, the node id, the title, then
/// the lifecycle command that starts it.
type OfferKey<'a> = (usize, bool, bool, String, String, &'a str, String, String);

/// Open planless nodes outside this scope that the lifecycle table starts: a
/// node that owes a plan takes a blueprint, an idea that owes none goes
/// straight to a target. A drained scope with free blueprint slots offers these
/// instead of a bare "no unplanned node" beat. Nearness is the hop count to
/// an ancestor shared with any member of the scope; the scope's audience or
/// domain wins a tie, then priority and id. The scope's own nodes (its
/// members, their ancestors and their children), nodes a live team owns, rows
/// this scope already queues, claimed and user-only rows never print. Each
/// offer carries the command that starts it.
pub(crate) fn adjacent_offers(
    scope: &str,
    entries: &[Value],
    other_owned: &HashMap<String, String>,
    queued: &std::collections::HashSet<&str>,
    floor: &str,
    limit: usize,
) -> Vec<Value> {
    use crate::lead_checkin::s_str;
    if limit == 0 {
        return Vec::new();
    }
    let by_id: HashMap<&str, &Value> = entries
        .iter()
        .filter_map(|e| s_str(e, "id").map(|id| (id, e)))
        .collect();
    let audience = |row: &Value| -> Option<String> {
        OfferMeta::deserialize(row)
            .unwrap_or_default()
            .tags
            .into_iter()
            .find(|t| t.starts_with("audience:"))
    };
    // A scope is several node ids, and each member keeps its own chain, so a
    // hop count runs from that member up to the shared ancestor.
    let members: Vec<Vec<String>> = scope_members(scope).map(|m| chain(&by_id, m)).collect();
    let own: std::collections::HashSet<&str> =
        members.iter().flatten().map(String::as_str).collect();
    let is_member = |id: &str| scope_members(scope).any(|m| m == id);
    let me = scope_members(scope).find_map(|m| by_id.get(m)).copied();
    let my_audience = me.and_then(audience);
    let my_domain = me.and_then(|m| s_str(m, "domain"));
    let routed = |row: &Value| {
        crate::backlog_ready::effective_command_with_floor(row, floor)
            .ok()
            .and_then(|(command, _)| command)
    };
    let mut ranked: Vec<OfferKey> = Vec::new();
    for row in entries {
        let Some(id) = s_str(row, "id") else { continue };
        if !open_unparked(row)
            || queued.contains(id)
            || other_owned.contains_key(id)
            || own.contains(id)
        {
            continue;
        }
        // The table decides the command. Only an idea takes a target.
        let idea = s_str(row, "status") == Some("idea");
        let command = routed(row);
        let offerable = command.as_deref() == Some("/blueprint")
            || (idea && command.as_deref() == Some("/target"));
        if !offerable {
            continue;
        }
        let path = chain(&by_id, id);
        // A row whose chain names a scope member is the scope's own child. It
        // rides its epic, so an outside offer never names it.
        if path.iter().any(|a| is_member(a)) {
            continue;
        }
        let (distance, via) = nearest(&path, &members);
        ranked.push((
            distance,
            my_audience.is_none() || audience(row) != my_audience,
            my_domain.is_none() || s_str(row, "domain") != my_domain,
            s_str(row, "priority").unwrap_or("p9").to_string(),
            via,
            id,
            s_str(row, "title").unwrap_or("").to_string(),
            command.unwrap_or_default(),
        ));
    }
    ranked.sort();
    ranked
        .into_iter()
        .take(limit)
        .map(|(distance, _, _, _, via, id, title, command)| {
            let near = if distance == usize::MAX {
                "no shared ancestor".to_string()
            } else {
                format!("shares {via}")
            };
            serde_json::to_value(Offer {
                id: id.to_string(),
                command,
                near,
                title,
            })
            .unwrap_or_default()
        })
        .collect()
}

/// The most idea children a beat routes or names as idle.
const IDEA_LINES: usize = 3;

/// The starts a beat still names when plans ready gate the rest, so a lead
/// with none running always has a blueprint going.
pub(crate) const BLUEPRINT_FLOOR_STARTS: usize = 1;

/// The check-in's blueprint reading: this session's live blueprint
/// subagents against the ceiling, then which unplanned nodes to start and
/// which to skip. Each unplanned row is routed through the lifecycle command
/// table first: a row whose command resolves to /blueprint is a candidate, one
/// that resolves to /target prints as target-ready (no blueprint ceiling
/// applies), and an undecidable row skips with the refusal. `floor` is the
/// operator's `dispatch.blueprint_floor`; the claim list and the session id
/// arrive through the seam (arguments, not ambient reads) so the unit tests
/// need no claims directory. A failed source is this reading's error, never
/// a zero: an unreadable claim list read as `running 0` would name starts
/// past the ceiling. Past the first start, starts wait until plans ready fall
/// below the lead's worker slots - a blueprint nobody can build is the exact
/// spend the wake meter exists to name; a gated beat with none running still
/// names one. `other_owned` is the fold's owner read, the
/// nodes a deeper live team holds: the scope line counts owned active with
/// it, so the starts and target-ready lines answer with the same one and a
/// child of another lead's team never prints. An unread map is this
/// reading's error - an empty read would name starts in foreign territory.
/// `idea_children` counts the scope's idea-status children riding their
/// contained epics: work the selection refuses (a contained child never
/// dispatches alone) that an empty unplanned queue must not read as clean.
/// `idea_rows` are those children as rows. A parent-only child routes through
/// the same command table as an unplanned row (its priority never gates a
/// lead's own epic children), and a contained child prints as `idle`.
pub(crate) fn r_blueprint(
    board: &Result<Value, String>,
    cwd: &Path,
    session_id: Option<String>,
    claims: Result<Vec<String>, String>,
    spawned: usize,
    slots: Result<usize, String>,
    floor: &str,
    other_owned: &Result<HashMap<String, String>, String>,
    idea_children: &Result<usize, String>,
    idea_rows: &[Value],
    seat_leads: &[SeatScope],
    scope: &str,
    entries: &[Value],
) -> Result<Value, String> {
    let session_id = session_id
        .ok_or_else(|| "no session id; cannot count this lead's blueprint subagents".to_string())?;
    let holders = claims?;
    let other_owned = other_owned.as_ref().map_err(Clone::clone)?;
    let holder = format!("blueprint-session:{session_id}");
    let running = holders.iter().filter(|h| **h == holder).count() + spawned;
    let board = board.as_ref()?;
    let rows = crate::lead_checkin::board_queue(board, "unplanned")?
        .get("rows")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let mut candidates: Vec<String> = Vec::new();
    let mut target_ready: Vec<String> = Vec::new();
    let mut skips: Vec<Value> = Vec::new();
    for row in &rows {
        // A row with no string id answers no command: skip it, never dispatch
        // an "unknown" node the way a silent default would invite.
        let id = match row.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => {
                skips.push(json!({"id": null, "reason": "row carries no id"}));
                continue;
            }
        };
        if let Some(owner) = other_owned.get(&id) {
            skips.push(json!({"id": id, "reason": format!("owned by team {owner}")}));
            continue;
        }
        match crate::backlog_ready::effective_command_with_floor(row, floor) {
            Ok((command, _)) if command.as_deref() == Some("/blueprint") => candidates.push(id),
            Ok((command, _)) if command.as_deref() == Some("/target") => target_ready.push(id),
            Ok((_, note)) => skips.push(json!({"id": id, "reason": note})),
            Err(refusal) => skips.push(json!({"id": id, "reason": refusal})),
        }
    }
    let queued: std::collections::HashSet<&str> = rows
        .iter()
        .filter_map(|r| crate::lead_checkin::s_str(r, "id"))
        .collect();
    let mut idle: Vec<Value> = Vec::new();
    let mut idea_routed = 0;
    // The scope's own roots sit above every queue, so an open planless root
    // with no child reaches no list of its own. It routes after the idea
    // children, the board's children-before-epics order. A root the idea rows
    // already carry is no second start.
    let idea_ids: std::collections::HashSet<&str> = idea_rows
        .iter()
        .filter_map(|r| crate::lead_checkin::s_str(r, "id"))
        .collect();
    let roots: Vec<Value> = scope_members(scope)
        .filter_map(|root| {
            entries
                .iter()
                .find(|e| crate::lead_checkin::s_str(e, "id") == Some(root))
        })
        .filter(|row| {
            !idea_ids.contains(crate::lead_checkin::s_str(row, "id").unwrap_or(""))
                && open_unparked(row)
                && crate::lead_checkin::s_str(row, "plan_path").is_none_or(str::is_empty)
        })
        .cloned()
        .collect();
    for row in idea_rows.iter().chain(roots.iter()) {
        let Some(id) = crate::lead_checkin::s_str(row, "id") else {
            continue;
        };
        if queued.contains(id) || other_owned.contains_key(id) {
            continue;
        }
        if let Some(owner) =
            crate::lead_checkin::s_str(row, "contained_in").filter(|o| !o.is_empty())
        {
            if idle.len() < IDEA_LINES {
                idle.push(json!({"id": id, "owner": owner}));
            }
            continue;
        }
        if idea_routed >= IDEA_LINES {
            continue;
        }
        match crate::backlog_ready::effective_command_with_floor(row, floor) {
            Ok((command, _)) if command.as_deref() == Some("/blueprint") => {
                idea_routed += 1;
                candidates.push(id.to_string())
            }
            Ok((command, _)) if command.as_deref() == Some("/target") => {
                idea_routed += 1;
                target_ready.push(id.to_string())
            }
            Ok((_, note)) => skips.push(json!({"id": id, "reason": note})),
            Err(refusal) => skips.push(json!({"id": id, "reason": refusal})),
        }
    }
    // Leadless epics with dispatchable children seat a sub-lead instead of
    // printing their rows as this lead's work: one seat-lead line replaces
    // every start, target-ready and plan-ready line those children would
    // have printed, and seat rows never count toward the start ceiling.
    let seat = seat_leads;
    let seat_pairs: Vec<(String, String)> = seat
        .iter()
        .flat_map(|s| s.children.iter().map(move |c| (c.clone(), s.scope.clone())))
        .collect();
    let is_seat_row = |id: &str| seat_pairs.iter().any(|(r, _)| r == id);
    candidates.retain(|id| !is_seat_row(id));
    target_ready.retain(|id| !is_seat_row(id));
    for (row, epic) in &seat_pairs {
        skips.push(json!(
            {"id": row, "reason": format!("rides leadless epic {epic}: its seat-lead line carries the work")}
        ));
    }
    let seat_scopes: Vec<Value> = seat
        .iter()
        .filter(|s| seat_pairs.iter().any(|(_, e)| e == &s.scope))
        .map(|s| {
            let mut out = s.clone();
            // The journal entry keeps IDEA_LINES children as evidence; the
            // full list already rides the skips, so the seat row stays well
            // under the event cap on a large board.
            out.children.truncate(IDEA_LINES);
            serde_json::to_value(&out).unwrap_or_default()
        })
        .collect();
    let provider = std::env::var("FNO_ROUTE_PROVIDER").unwrap_or_default();
    let provider_cap = crate::spawn_gate_lanes::provider_subagents_cap(cwd, &provider);
    let ceiling = blueprint_ceiling(provider_cap);
    let ceiling_source = if ceiling < DEFAULT_BLUEPRINT_CEILING {
        format!("agents.provider_limits.{provider}.subagents")
    } else {
        "two per lead".to_string()
    };
    let undispatched = crate::lead_checkin::board_queue(board, "undispatched").ok();
    let plans_ready =
        undispatched.and_then(|q| q.get("count").and_then(Value::as_u64).map(|c| c as usize));
    // Planned, unclaimed rows: the plan is done, so the next move is a build
    // worker, not a blueprint. A row a deeper live team owns skips like an
    // unplanned one does; an unreadable queue names none.
    let mut plan_ready: Vec<String> = Vec::new();
    for row in undispatched
        .and_then(|q| q.get("rows").and_then(Value::as_array))
        .into_iter()
        .flatten()
    {
        let Some(id) = row.get("id").and_then(Value::as_str) else {
            skips.push(json!({"id": null, "reason": "row carries no id"}));
            continue;
        };
        match other_owned.get(id) {
            Some(owner) => {
                skips.push(json!({"id": id, "reason": format!("owned by team {owner}")}))
            }
            None => plan_ready.push(id.to_string()),
        }
    }
    plan_ready.retain(|id| !is_seat_row(id));
    let (pr_v, slots_v, gate_reason): (Value, Value, Option<String>) = match (plans_ready, &slots) {
        (Some(pr), Ok(s)) => {
            if pr < *s {
                (json!(pr), json!(s), None)
            } else {
                (
                    json!(pr),
                    json!(s),
                    Some(format!(
                        "plans ready {pr} / slots {s}: build slots are the bottleneck"
                    )),
                )
            }
        }
        (None, Ok(_)) => (
            Value::Null,
            slots.clone().map(|s| json!(s)).unwrap_or(Value::Null),
            Some("plans ready unmeasured: the board names no undispatched count".to_string()),
        ),
        (_, Err(e)) => (
            Value::Null,
            Value::Null,
            Some(format!("plans ready unmeasured: {e}")),
        ),
    };
    let open = if gate_reason.is_some() {
        BLUEPRINT_FLOOR_STARTS.min(ceiling).saturating_sub(running)
    } else {
        ceiling.saturating_sub(running)
    };
    let starts: Vec<String> = candidates.iter().take(open).cloned().collect();
    match &gate_reason {
        Some(reason) => candidates
            .iter()
            .skip(open)
            .for_each(|id| skips.push(json!({"id": id, "reason": reason}))),
        None => candidates.iter().skip(open).for_each(|id| {
            skips.push(json!({"id": id, "reason": format!("at ceiling {running} of {ceiling}")}))
        }),
    };
    if rows.is_empty() && running == 0 && candidates.is_empty() && target_ready.is_empty() {
        let reason = match idea_children {
            Ok(n) if *n > 0 => {
                let ids: Vec<&str> = idle
                    .iter()
                    .filter_map(|v| crate::lead_checkin::s_str(v, "id"))
                    .collect();
                let named = if ids.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", ids.join(", "))
                };
                format!(
                    "no dispatchable unplanned node in scope: {n} idea children ride their contained epics{named}"
                )
            }
            _ => "no unplanned node in scope".to_string(),
        };
        skips.push(json!({ "id": null, "reason": reason }));
    }
    // A drained scope names adjacent work instead of a bare no-change beat:
    // nothing to start, route or build here, and blueprint slots free.
    let drained = candidates.is_empty()
        && target_ready.is_empty()
        && plan_ready.is_empty()
        && seat_scopes.is_empty()
        && running < ceiling;
    let offers = if drained {
        let limit = OFFER_LINES.min(ceiling - running);
        adjacent_offers(scope, entries, other_owned, &queued, floor, limit)
    } else {
        Vec::new()
    };
    if drained && offers.is_empty() {
        skips.push(json!({
            "id": null,
            "reason": "no offer: no open planless node outside this scope routes to a \
                       blueprint or a target start"
        }));
    }
    // Open, planless rows: the unplanned queue and the idea children, read
    // once; the batch lines group the ones that read as one subject.
    let batch = crate::lead_checkin_batch::groups(
        &rows
            .iter()
            .chain(idea_rows.iter().filter(|r| {
                crate::lead_checkin::s_str(r, "id")
                    .is_some_and(|id| !queued.contains(id) && !other_owned.contains_key(id))
            }))
            .filter(|r| !crate::backlog_ready::is_user_only(r))
            .cloned()
            .collect::<Vec<_>>(),
    );
    let idea_children_json = match idea_children {
        Ok(n) => json!(n),
        Err(e) => json!({ "error": e }),
    };
    Ok(json!({
        "running": running,
        "ceiling": ceiling,
        "ceiling_source": ceiling_source,
        "plans_ready": pr_v,
        "slots": slots_v,
        "seat_leads": seat_scopes,
        "starts": starts,
        "offers": offers,
        "target_ready": target_ready,
        "plan_ready": plan_ready,
        "idea_children": idea_children_json,
        "idle": idle,
        "batch": batch,
        "skips": skips,
    }))
}

/// The beat's `blueprint:` lines from an `r_blueprint` reading: the scope
/// line, then start, target-ready, plan-ready, idle, batch and skip lines. The spawn
/// commands are the ones to copy: with no --name, spawn names the worker
/// t-<short slug> and the lane picks its model.
pub(crate) fn render_lines(bp: &Value) -> Vec<String> {
    use crate::lead_checkin::dash;
    let ids = |key: &str| -> Vec<Value> {
        bp.get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut lines = vec![format!(
        "blueprint: running {} of {} ({}); plans ready {} / slots {}",
        dash(bp.get("running")),
        dash(bp.get("ceiling")),
        dash(bp.get("ceiling_source")),
        dash(bp.get("plans_ready")),
        dash(bp.get("slots")),
    )];
    for s in ids("seat_leads") {
        let scope = crate::lead_checkin::s_str(&s, "scope").unwrap_or("-");
        lines.push(format!(
            "  seat-lead: fno agent spawn --as thread --promote {scope} \"/fno:lead\""
        ));
    }
    for id in ids("starts") {
        lines.push(format!(
            "  start /fno:blueprint subagent {}",
            dash(Some(&id))
        ));
    }
    for offer in ids("offers") {
        let o = Offer::deserialize(&offer).unwrap_or_default();
        lines.push(format!(
            "  offer: {} ({}, outside this scope): {}",
            start_command(&o.command, &o.id, false),
            o.near,
            o.title,
        ));
    }
    for id in ids("target_ready") {
        lines.push(format!(
            "  target-ready: fno agent spawn \"/fno:target {}\"",
            dash(Some(&id))
        ));
    }
    for id in ids("plan_ready") {
        lines.push(format!(
            "  plan-ready: fno agent spawn \"/fno:target {}\"",
            dash(Some(&id))
        ));
    }
    for idle in ids("idle") {
        let id = crate::lead_checkin::s_str(&idle, "id").unwrap_or("-");
        let owner = crate::lead_checkin::s_str(&idle, "owner").unwrap_or("-");
        lines.push(format!(
            "  idle: {id} rides {owner}: it ships in {owner}'s PR"
        ));
    }
    lines.extend(crate::lead_checkin_batch::render_lines(&ids("batch")));
    for skip in ids("skips") {
        lines.push(format!(
            "  skip {} : {}",
            dash(skip.get("id")),
            dash(skip.get("reason"))
        ));
    }
    lines
}

/// The stop check's blueprint-ceiling reading: the same ceiling the
/// check-in's `r_blueprint` reads, against the same claims source, so a row
/// the beat skips at ceiling cannot hold the lead's stop open. `running`
/// names the running blueprints by node id; `open` is the count of
/// board-order unplanned rows that may still start. `None` on an unmeasured
/// read: the check-in fails closed so no start is named past the ceiling;
/// the stop check fails inert so no startable row reads as waiting, which
/// would end a lead with work it could have started.
pub(crate) struct BlueprintWait {
    pub(crate) running: Vec<String>,
    pub(crate) running_count: usize,
    pub(crate) ceiling: usize,
    pub(crate) open: usize,
}

/// The ceiling fold the check-in runs: a provider budget can only lower the
/// two-per-lead ceiling, never raise it. One function so the stop check and
/// the check-in cannot drift.
pub(crate) fn blueprint_ceiling(provider_cap: Option<usize>) -> usize {
    match provider_cap {
        Some(cap) if cap < DEFAULT_BLUEPRINT_CEILING => cap,
        _ => DEFAULT_BLUEPRINT_CEILING,
    }
}

/// The node ids behind this session's live `blueprint-session:` claims, read
/// from the same listing the check-in counts holders in.
pub(crate) fn running_blueprint_ids(
    rows: &[crate::claims::ClaimRecord],
    session: &str,
) -> Vec<String> {
    let holder = format!("blueprint-session:{session}");
    rows.iter()
        .filter(|r| r.holder == holder)
        .filter_map(|r| r.key.strip_prefix("node:").map(str::to_string))
        .collect()
}

/// Keys of the live workers `session` spawned whose payload opens with
/// `/fno:blueprint`. A spawned planner joins the spawn's `spawn-handover:`
/// claim, so no `blueprint-session:` holder names its lead; the registry row's
/// owner is the only edge back. The key is the node id the payload names, else
/// the worker's session id, so a plan-path payload still counts once.
pub(crate) fn spawned_blueprint_keys(
    entries: &[RegistryEntry],
    session: &str,
    terminal: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for e in entries {
        // A finished planner can sit idle and unreaped: the same capacity
        // read the slot share uses keeps it from filling a blueprint slot.
        if e.role_level.is_some()
            || !crate::spawn_gate::status_is_liveish(&e.status)
            || !crate::cap_liveness::holds_capacity(e, terminal)
            || e.current_owner_session() != Some(session)
        {
            continue;
        }
        let mut tokens = e.spawn_payload.as_deref().unwrap_or("").split_whitespace();
        let is_blueprint = tokens
            .next()
            .and_then(crate::provider::parse_command_token)
            .is_some_and(|(command, _)| command == "blueprint");
        if !is_blueprint {
            continue;
        }
        let node = tokens.find(|t| !t.starts_with('-') && !t.contains('/') && t.contains('-'));
        let key = node
            .map(str::to_string)
            .or_else(|| e.harness_session_id.clone())
            .unwrap_or_else(|| e.name.clone());
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// `spawned_blueprint_keys` over a loaded registry, with the terminal-run
/// keys the slot share reads. The caller loads the registry, so an unreadable
/// one is its own named error, never a silent 0.
pub(crate) fn spawned_in_registry(
    registry: &crate::state::Registry,
    home: &crate::paths::AgentsHome,
    session: &str,
) -> Vec<String> {
    let terminal = crate::terminal_run::read_keys(&home.terminal_runs_dir());
    spawned_blueprint_keys(&registry.entries, session, &terminal)
}

/// Read the ceiling state live. `None` when the session id, the claims
/// listing or the registry is unreadable: an unmeasured ceiling never
/// converts an actionable row into a waiting one.
pub(crate) fn stop_check_reading(cwd: &Path) -> Option<BlueprintWait> {
    let (session, _) = crate::claims::resolve_identity();
    let session = session?;
    let rows = crate::claims::list(Some("node:"), None, false).ok()?;
    let mut running = running_blueprint_ids(&rows, &session);
    let home = crate::paths::AgentsHome::from_env();
    let registry = crate::state::load_registry(&home.registry_json()).ok()?;
    running.extend(spawned_in_registry(&registry, &home, &session));
    let provider = std::env::var("FNO_ROUTE_PROVIDER").unwrap_or_default();
    let provider_cap = crate::spawn_gate_lanes::provider_subagents_cap(cwd, &provider);
    let ceiling = blueprint_ceiling(provider_cap);
    let running_count = running.len();
    Some(BlueprintWait {
        running,
        running_count,
        ceiling,
        open: ceiling.saturating_sub(running_count),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A board payload naming one `unplanned` queue whose rows carry
    /// `(id, difficulty)` and, when `plans_ready` is Some, one `undispatched`
    /// queue counting it.
    fn unplanned_board(rows: &[(&str, &str)], plans_ready: Option<usize>) -> Value {
        unplanned_board_tagged(
            &rows
                .iter()
                .map(|(id, d)| json!({"id": id, "difficulty": d}))
                .collect::<Vec<_>>(),
            plans_ready,
        )
    }

    fn unplanned_board_tagged(rows: &[Value], plans_ready: Option<usize>) -> Value {
        let mut queues = vec![json!({
            "name": "unplanned", "status": "ok", "count": rows.len(),
            "rows": rows
        })];
        if let Some(pr) = plans_ready {
            queues.push(json!({
                "name": "undispatched", "status": "ok", "count": pr, "rows": []
            }));
        }
        json!({"queues": queues})
    }

    /// Call `r_blueprint` with a fixed provider (so the ambient
    /// FNO_ROUTE_PROVIDER cannot leak in) and a temp config dir.
    fn blueprint_reading(
        dir: &std::path::Path,
        config_toml: &str,
        board: Value,
        session_id: Option<&str>,
        holders: Vec<String>,
        slots: Result<usize, String>,
        floor: &str,
    ) -> Result<Value, String> {
        blueprint_reading_owned(
            dir,
            config_toml,
            board,
            session_id,
            holders,
            0,
            slots,
            floor,
            Ok(HashMap::new()),
            Ok(0),
            vec![],
            &[],
            &[],
            "",
        )
    }

    /// The same call with the owner map named, for the deeper-team filter,
    /// and the idea-children count named, for the epic-heavy line. `scope` is
    /// the scope spelling the reading sees, so a test can name roots.
    fn blueprint_reading_owned(
        dir: &std::path::Path,
        config_toml: &str,
        board: Value,
        session_id: Option<&str>,
        holders: Vec<String>,
        spawned: usize,
        slots: Result<usize, String>,
        floor: &str,
        other_owned: Result<HashMap<String, String>, String>,
        idea_children: Result<usize, String>,
        seat_leads: Vec<SeatScope>,
        idea_rows: &[Value],
        entries: &[Value],
        scope: &str,
    ) -> Result<Value, String> {
        // The FNO_CONFIG pin below is the SOLE config candidate for every
        // concurrent reader, so it must not interleave with another test's
        // config read (the spawn-gate reservation suite reads max_live under
        // this same lock).
        let _env_lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let fnodir = dir.join(".fno");
        std::fs::create_dir_all(&fnodir).unwrap();
        std::fs::write(fnodir.join("config.toml"), config_toml).unwrap();
        // FNO_CONFIG pins the walk to the fixture the way the lanes-cap test
        // does: a repo-root .fno/config.toml must never leak into the read.
        let prior_config = std::env::var_os("FNO_CONFIG");
        std::env::set_var("FNO_CONFIG", fnodir.join("config.toml"));
        let prior_provider = std::env::var_os("FNO_ROUTE_PROVIDER");
        std::env::set_var("FNO_ROUTE_PROVIDER", "zai");
        let reading = r_blueprint(
            &Ok(board),
            dir,
            session_id.map(str::to_string),
            Ok(holders),
            spawned,
            slots,
            floor,
            &other_owned,
            &idea_children,
            idea_rows,
            &seat_leads,
            scope,
            entries,
        );
        match prior_provider {
            Some(v) => std::env::set_var("FNO_ROUTE_PROVIDER", v),
            None => std::env::remove_var("FNO_ROUTE_PROVIDER"),
        }
        match prior_config {
            Some(v) => std::env::set_var("FNO_CONFIG", v),
            None => std::env::remove_var("FNO_CONFIG"),
        }
        reading
    }

    /// An L0 beat seats a leadless epic's dispatchable children: the rows
    /// leave the dispatch lists, print no target-ready line, and ride skips
    /// naming the seat. Seat rows never count toward the start ceiling.
    fn an_l0_beat_seats_a_leadless_epics_children() {
        let dir = std::env::temp_dir().join(format!("fno-bp-seat-{}", std::process::id()));
        let board = json!({
            "queues": [{
                "name": "unplanned", "status": "ok", "count": 2,
                "rows": [
                    {"id": "x-own1", "difficulty": "low"},
                    {"id": "x-seat1", "parent": "x-eSeat", "difficulty": "low"},
                ]
            }]
        });
        let reading = blueprint_reading_owned(
            &dir,
            "",
            board,
            Some("sess-1"),
            vec![],
            0,
            Ok(4),
            "high",
            Ok(HashMap::new()),
            Ok(0),
            vec![SeatScope {
                scope: "x-eSeat".to_string(),
                children: vec!["x-seat1".to_string()],
            }],
            &[],
            &[],
            "",
        )
        .unwrap();
        assert_eq!(
            reading.get("seat_leads"),
            Some(&json!([{"scope": "x-eSeat", "children": ["x-seat1"]}])),
            "{reading}"
        );
        assert_eq!(
            reading.get("target_ready"),
            Some(&json!(["x-own1"])),
            "{reading}"
        );
        assert_eq!(
            reading["skips"],
            json!([{"id": "x-seat1",
                "reason": "rides leadless epic x-eSeat: its seat-lead line carries the work"}]),
            "{reading}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A p2 planned node in scope prints its plan-ready line and counts in
    /// `plans ready`, so the beat names work the board's p2 rows used to hide.
    fn a_p2_plan_in_scope_prints_plan_ready() {
        let dir = std::env::temp_dir().join(format!("fno-bp-p2plan-{}", std::process::id()));
        let board = json!({"queues": [
            {"name": "unplanned", "status": "ok", "count": 0, "rows": []},
            {"name": "undispatched", "status": "ok", "count": 1,
             "rows": [{"id": "x-p2", "priority": "p2", "title": "a p2 plan"}]},
        ]});
        let reading =
            blueprint_reading(&dir, "", board, Some("sess-1"), vec![], Ok(4), "high").unwrap();
        let lines = render_lines(&reading);
        assert!(
            lines.iter().any(|l| l.contains("plans ready 1 /")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("plan-ready") && l.contains("x-p2")),
            "{lines:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A blueprint this lead spawned plans on its own worker session, so the
    /// registry owner edge fills the slot; a plan-path payload counts once by
    /// worker, another lead's planner and a target worker count for nothing.
    fn a_spawned_blueprint_fills_a_slot() {
        let live = |name: &str, payload: &str, owner: &str| RegistryEntry {
            name: name.to_string(),
            status: crate::AgentStatus::Live,
            spawn_payload: Some(payload.to_string()),
            spawned_by_session: Some(owner.to_string()),
            harness_session_id: Some(format!("{name}-sid")),
            ..Default::default()
        };
        let entries = vec![
            live("w1", "/fno:blueprint x-1", "lead"),
            live("w2", "/fno:blueprint subagent plans/p.md", "lead"),
            live("w3", "/fno:blueprint x-1", "lead"),
            live("w4", "/fno:blueprint x-4", "other"),
            live("w5", "/fno:target x-5", "lead"),
        ];
        let terminal = std::collections::HashSet::new();
        let keys = spawned_blueprint_keys(&entries, "lead", &terminal);
        assert_eq!(keys, vec!["x-1".to_string(), "w2-sid".to_string()]);
        let dir = std::env::temp_dir().join(format!("fno-bp-spawned-{}", std::process::id()));
        let reading = blueprint_reading_owned(
            &dir,
            "",
            unplanned_board(&[], Some(0)),
            Some("lead"),
            vec!["blueprint-session:lead".to_string()],
            keys.len(),
            Ok(4),
            "high",
            Ok(HashMap::new()),
            Ok(0),
            vec![],
            &[],
            &[],
            "",
        )
        .unwrap();
        let lines = render_lines(&reading);
        assert!(
            lines.iter().any(|l| l.contains("running 3 of")),
            "{lines:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The seat legs: the L0 integration, the pure walk, and the rendered
    /// line, so the seat surface adds no test declarations to G3.
    fn l0_seat_legs() {
        a_p2_plan_in_scope_prints_plan_ready();
        a_spawned_blueprint_fills_a_slot();
        an_l0_beat_seats_a_leadless_epics_children();
        the_seat_walk_keys_on_leadless_epics_and_the_level();
        the_seat_lead_line_names_scope_lane_and_payload();
    }

    /// The seat walk: a row's parent chain seats under the first open epic
    /// no live team owns; the walk stops at this lead's own scope and at a
    /// live team's node, and an L2+ (or untitled) stand-up seats nothing.
    fn the_seat_walk_keys_on_leadless_epics_and_the_level() {
        let entries = vec![
            json!({"id": "x-mine", "type": "epic", "status": "ready"}),
            json!({"id": "x-e1", "type": "epic", "status": "ready", "parent": "x-mine"}),
            json!({"id": "x-c1", "parent": "x-e1"}),
            json!({"id": "x-c2", "parent": "x-c1"}),
            json!({"id": "x-owned1", "type": "epic", "status": "ready", "parent": "x-mine"}),
            json!({"id": "x-o1", "parent": "x-owned1"}),
            json!({"id": "x-done", "type": "epic", "status": "done", "parent": "x-mine"}),
            json!({"id": "x-d1", "parent": "x-done"}),
        ];
        let board = json!({"queues": [{"name": "unplanned", "status": "ok", "count": 4,
            "rows": [{"id": "x-c1"}, {"id": "x-c2"}, {"id": "x-o1"}, {"id": "x-d1"}]}]});
        let folded = json!({"owned_scopes": {"x-owned1": "x-team", "x-o1": "x-team"}});
        let seat = seat_leads("x-mine", Some(1), &entries, Some(&folded), Some(&board));
        assert_eq!(
            seat,
            vec![SeatScope {
                scope: "x-e1".to_string(),
                children: vec!["x-c1".to_string(), "x-c2".to_string()],
            }]
        );
        // A scope is several ids. A member of it is never seated under.
        let seats = seat_leads(
            "x-mine,x-e1",
            Some(1),
            &entries,
            Some(&folded),
            Some(&board),
        );
        assert!(seats.is_empty(), "{seats:?}");
        assert!(seat_leads("x-mine", Some(2), &entries, Some(&folded), None).is_empty());
        assert!(seat_leads("x-mine", None, &entries, Some(&folded), None).is_empty());
    }

    /// The rendered seat-lead line names the scope, the thread lane and the
    /// lead payload, with no --name: the nameless promote form the grant
    /// gate accepts from a holding session.
    fn the_seat_lead_line_names_scope_lane_and_payload() {
        let lines = render_lines(&json!({
            "seat_leads": [{"scope": "x-eSeat", "children": ["x-seat1"]}],
            "starts": [],
            "target_ready": [],
            "plan_ready": [],
            "idle": [],
            "batch": [],
            "skips": [],
        }));
        let seat_line = lines.iter().find(|l| l.contains("seat-lead:"));
        assert!(seat_line.is_some(), "{lines:?}");
        assert!(
            seat_line.unwrap().contains("--promote x-eSeat"),
            "{lines:?}"
        );
        assert!(seat_line.unwrap().contains("--as thread"), "{lines:?}");
        assert!(seat_line.unwrap().contains("\"/fno:lead\""), "{lines:?}");
    }

    /// The happy path: 4 unplanned candidates, running 0, ceiling capped to
    /// two per lead: the first two ids in board order start, the rest skip at
    /// the ceiling.
    #[test]
    fn r_blueprint_starts_up_to_the_ceiling_in_board_order() {
        let dir = std::env::temp_dir().join(format!("fno-bp-happy-{}", std::process::id()));
        let reading = blueprint_reading(
            &dir,
            "[agents.provider_limits.zai]\nsubagents = 2\n",
            unplanned_board(
                &[
                    ("x-1", "high"),
                    ("x-2", "high"),
                    ("x-3", "high"),
                    ("x-4", "high"),
                ],
                Some(0),
            ),
            Some("sess-1"),
            vec![],
            Ok(4),
            "high",
        )
        .unwrap();
        assert_eq!(reading["running"], 0);
        assert_eq!(reading["ceiling"], 2);
        assert_eq!(reading["ceiling_source"], "two per lead");
        assert_eq!(reading["plans_ready"], 0);
        assert_eq!(reading["slots"], 4);
        assert_eq!(reading["starts"], json!(["x-1", "x-2"]), "{reading}");
        assert_eq!(
            reading["skips"],
            json!([
                {"id": "x-3", "reason": "at ceiling 0 of 2"},
                {"id": "x-4", "reason": "at ceiling 0 of 2"}
            ]),
            "{reading}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        l0_seat_legs();
    }

    /// A missing session id fails the reading: it must never read as
    /// `running 0`, which would name starts past the ceiling.
    #[test]
    fn r_blueprint_fails_without_a_session_id() {
        let dir = std::env::temp_dir().join(format!("fno-bp-nosess-{}", std::process::id()));
        let reading = blueprint_reading(
            &dir,
            "",
            unplanned_board(&[("x-1", "high")], Some(0)),
            None,
            vec![],
            Ok(4),
            "high",
        );
        assert!(reading.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A child of a deeper live team never prints, on either dispatch line:
    /// on 2026-09-28 two p0 children of an L2-held epic leaked as
    /// target-ready past the outer team's beat, and a lead that trusted the
    /// line would have seated a duplicate worker in another lead's territory.
    /// The row skips naming the team that owns it. A drained scope's offer
    /// line honors the same owner map, and also drops claimed, low-difficulty
    /// and already-queued rows, then ranks the rest nearest first. The last
    /// two cases keep their own names as helper functions: the test-audit
    /// scope of this path admits no new declaration.
    #[test]
    fn a_child_of_a_deeper_live_team_never_prints_as_start_target_ready_or_offer() {
        let dir = std::env::temp_dir().join(format!("fno-bp-owned-{}", std::process::id()));
        let reading = blueprint_reading_owned(
            &dir,
            "",
            unplanned_board(
                &[("x-c1", "low"), ("x-c2", "low"), ("x-mine", "low")],
                Some(0),
            ),
            Some("sess-1"),
            vec![],
            0,
            Ok(4),
            "high",
            Ok(HashMap::from([
                ("x-c1".to_string(), "x-cccc".to_string()),
                ("x-c2".to_string(), "x-cccc".to_string()),
            ])),
            Ok(0),
            vec![],
            &[],
            &[],
            "",
        )
        .unwrap();
        assert_eq!(reading["target_ready"], json!(["x-mine"]), "{reading}");
        assert_eq!(reading["starts"], json!([]), "{reading}");
        assert_eq!(
            reading["skips"],
            json!([
                {"id": "x-c1", "reason": "owned by team x-cccc"},
                {"id": "x-c2", "reason": "owned by team x-cccc"},
            ]),
            "{reading}"
        );
        let _ = std::fs::remove_dir_all(&dir);

        let entries = vec![
            json!({"id": "x-top", "type": "epic", "status": "ready"}),
            json!({"id": "x-scope", "parent": "x-top", "type": "epic", "status": "ready"}),
            json!({"id": "x-sib", "parent": "x-top", "status": "ready",
                "difficulty": "high", "title": "sibling work"}),
            json!({"id": "x-far", "status": "idea", "difficulty": "high", "title": "far"}),
            json!({"id": "x-low", "parent": "x-top", "status": "ready", "difficulty": "low"}),
            json!({"id": "x-held", "parent": "x-top", "status": "ready",
                "difficulty": "high", "locked_by": "sess-9"}),
            json!({"id": "x-owned", "parent": "x-top", "status": "ready", "difficulty": "high"}),
            json!({"id": "x-kid", "parent": "x-scope", "status": "ready",
                "difficulty": "high", "title": "inside"}),
        ];
        let owned = HashMap::from([("x-owned".to_string(), "x-team".to_string())]);
        let queued = std::collections::HashSet::from(["x-kid"]);
        let offers = adjacent_offers("x-scope", &entries, &owned, &queued, "high", 2);
        let ids: Vec<&str> = offers.iter().filter_map(|o| o["id"].as_str()).collect();
        assert_eq!(ids, ["x-sib", "x-far"], "{offers:?}");
        let lines = render_lines(&json!({"offers": offers}));
        assert!(
            lines.iter().any(|l| l.starts_with(
                "  offer: /fno:blueprint subagent x-sib (shares x-top, outside this scope)"
            )),
            "{lines:?}"
        );
        assert!(adjacent_offers("x-scope", &entries, &owned, &queued, "high", 0).is_empty());

        // A scope whose only child is a user-only idea row is still drained:
        // the skipped row never counts as a candidate, so the offer prints,
        // and with no offerable node the beat says why instead of going silent.
        let user_only = json!({"id": "x-uo", "status": "idea", "difficulty": "medium",
            "parent": "x-scope", "tags": ["user-only"]});
        let drained = |graph: &[Value]| {
            blueprint_reading_owned(
                &dir,
                "",
                unplanned_board(&[], Some(0)),
                Some("sess-1"),
                vec![],
                0,
                Ok(4),
                "high",
                Ok(owned.clone()),
                Ok(1),
                vec![],
                std::slice::from_ref(&user_only),
                graph,
                "",
            )
            .unwrap()
        };
        let reading = drained(&entries);
        assert!(
            render_lines(&reading)
                .iter()
                .any(|l| l.starts_with("  offer: /fno:blueprint subagent")),
            "{reading}"
        );
        let reading = drained(&[]);
        let lines = render_lines(&reading);
        assert!(
            lines.iter().all(|l| !l.starts_with("  offer:")),
            "{reading}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("  skip - : no offer:")),
            "{reading}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        a_multi_root_scope_never_offers_its_own_subtree();
        an_open_planless_scope_root_routes_like_an_idea_child();
    }

    /// A scope spelled with several members is not one node id. The offer
    /// walk drops the scope's own roots, their ancestors and their children,
    /// counts hops from the member that shares the ancestor, names a sibling
    /// with that ancestor, and names an idea of lower difficulty with the
    /// target that starts it.
    fn a_multi_root_scope_never_offers_its_own_subtree() {
        let entries = vec![
            json!({"id": "x-parent", "status": "idea", "difficulty": "high", "title": "above"}),
            json!({"id": "x-a-2", "parent": "x-parent", "status": "done", "title": "above a"}),
            json!({"id": "x-a-1", "parent": "x-a-2", "status": "done", "title": "above a"}),
            json!({"id": "x-root-a", "parent": "x-a-1", "type": "epic", "status": "idea",
                "difficulty": "high", "title": "own a"}),
            json!({"id": "x-b-parent", "status": "done", "title": "above b"}),
            json!({"id": "x-root-b", "parent": "x-b-parent", "type": "epic", "status": "idea",
                "difficulty": "medium", "title": "own b"}),
            json!({"id": "x-b-sib", "parent": "x-b-parent", "status": "idea", "difficulty": "high",
                "title": "sibling of b"}),
            json!({"id": "x-b-kid", "parent": "x-root-b", "status": "idea",
                "difficulty": "medium", "title": "inside b"}),
            json!({"id": "x-b-grand", "parent": "x-b-kid", "status": "idea",
                "difficulty": "high", "title": "deeper in b"}),
            json!({"id": "x-sib", "parent": "x-parent", "status": "idea", "difficulty": "high",
                "title": "sibling"}),
            json!({"id": "x-out-hi", "status": "idea", "difficulty": "high", "title": "out high"}),
            json!({"id": "x-out-mid", "status": "idea", "difficulty": "medium",
                "title": "out mid"}),
            json!({"id": "x-out-ready", "status": "ready", "difficulty": "medium",
                "title": "out ready"}),
            json!({"id": "x-pq", "status": "idea", "difficulty": "medium",
                "tags": ["premise-question"], "title": "premise open"}),
        ];
        let owned = HashMap::new();
        let walk = |limit: usize| {
            adjacent_offers(
                "x-root-a,x-root-b",
                &entries,
                &owned,
                &std::collections::HashSet::new(),
                "high",
                limit,
            )
        };
        // The first member's chain is long. Hops still count from the member
        // that shares the ancestor, so the sibling of the second member leads.
        let offers = walk(3);
        assert_eq!(
            offers,
            vec![
                json!({"id": "x-b-sib", "command": "/blueprint",
                    "near": "shares x-b-parent", "title": "sibling of b"}),
                json!({"id": "x-sib", "command": "/blueprint",
                    "near": "shares x-parent", "title": "sibling"}),
                json!({"id": "x-out-hi", "command": "/blueprint",
                    "near": "no shared ancestor", "title": "out high"}),
            ],
            "{offers:?}"
        );
        // The table decides the command, not the difficulty band: a medium
        // idea with an open premise question owes a plan.
        let offers = walk(5);
        let last = json!({"id": "x-pq", "command": "/blueprint", "near": "no shared ancestor",
            "title": "premise open"});
        assert_eq!(offers.last(), Some(&last), "{offers:?}");
        let lines = render_lines(&json!({"offers": offers}));
        for line in [
            "  offer: fno agent spawn \"/fno:target x-out-mid\" \
             (no shared ancestor, outside this scope): out mid",
            "  offer: /fno:blueprint subagent x-pq \
             (no shared ancestor, outside this scope): premise open",
        ] {
            assert!(lines.iter().any(|l| l == line), "{lines:?}");
        }
    }

    /// An open planless root of the scope routes like an idea child: high
    /// owes a blueprint, medium goes to a target. A closed root, a root with
    /// a plan and a locked root print no line. A root the idea rows already
    /// carry prints once.
    fn an_open_planless_scope_root_routes_like_an_idea_child() {
        let dir = std::env::temp_dir().join(format!("fno-bp-roots-{}", std::process::id()));
        let entries = vec![
            json!({"id": "x-r-hi", "type": "epic", "status": "idea", "difficulty": "high"}),
            json!({"id": "x-r-mid", "type": "epic", "status": "idea", "difficulty": "medium",
                "parent": "x-r-hi"}),
            json!({"id": "x-r-low", "type": "epic", "status": "idea", "difficulty": "medium"}),
            json!({"id": "x-r-done", "type": "epic", "status": "done", "difficulty": "high"}),
            json!({"id": "x-r-plan", "type": "epic", "status": "idea", "difficulty": "high",
                "plan_path": "/plans/x-r-plan.md"}),
            json!({"id": "x-r-lock", "type": "epic", "status": "idea", "difficulty": "high",
                "locked_by": "sess-9"}),
        ];
        let idea_rows = [
            json!({"id": "x-r-mid", "parent": "x-r-hi", "status": "idea",
            "difficulty": "medium"}),
        ];
        let reading = blueprint_reading_owned(
            &dir,
            "",
            unplanned_board(&[], Some(0)),
            Some("sess-1"),
            vec![],
            0,
            Ok(4),
            "high",
            Ok(HashMap::new()),
            Ok(0),
            vec![],
            &idea_rows,
            &entries,
            "x-r-hi,x-r-mid,x-r-low,x-r-done,x-r-plan,x-r-lock",
        )
        .unwrap();
        let lines = render_lines(&reading);
        for line in [
            "  start /fno:blueprint subagent x-r-hi",
            "  target-ready: fno agent spawn \"/fno:target x-r-mid\"",
            "  target-ready: fno agent spawn \"/fno:target x-r-low\"",
        ] {
            assert!(lines.iter().any(|l| l == line), "{lines:?}");
        }
        assert_eq!(
            lines.iter().filter(|l| l.contains("x-r-mid")).count(),
            1,
            "{lines:?}"
        );
        for parked in ["x-r-done", "x-r-plan", "x-r-lock"] {
            assert!(lines.iter().all(|l| !l.contains(parked)), "{lines:?}");
        }
        assert!(
            lines
                .iter()
                .all(|l| !l.contains("offer:") && !l.contains("skip")),
            "{lines:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unread owner read is the reading's error, never an empty filter:
    /// an empty map would print a foreign team's nodes as dispatchable.
    #[test]
    fn an_unread_owner_map_fails_the_blueprint_reading() {
        let dir = std::env::temp_dir().join(format!("fno-bp-unowned-{}", std::process::id()));
        let reading = blueprint_reading_owned(
            &dir,
            "",
            unplanned_board(&[("x-1", "low")], Some(0)),
            Some("sess-1"),
            vec![],
            0,
            Ok(4),
            "high",
            Err("a live team's scope does not compile: e-x: unknown".to_string()),
            Ok(0),
            vec![],
            &[],
            &[],
            "",
        );
        assert!(reading.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The stop check folds the provider cap into the same one-per-lead
    /// ceiling the check-in reads: never raised, only lowered, and the
    /// default holds with no cap at all.
    #[test]
    fn the_stop_check_ceiling_folds_the_provider_cap_and_never_raises_the_default() {
        assert_eq!(blueprint_ceiling(None), 2);
        assert_eq!(blueprint_ceiling(Some(3)), 2);
        assert_eq!(blueprint_ceiling(Some(1)), 1);
    }
}
