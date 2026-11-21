//! The check-in's blueprint reading: which unplanned nodes to blueprint and
//! which to route straight to target. Extracted from lead_checkin.rs under
//! the file budget's shrink rule; the beat's print block reads the JSON this
//! module returns. Each unplanned row routes through the lifecycle command
//! table (`dispatch.blueprint_floor`), so the beat's starts list can never
//! disagree with the routing the dispatch doors answer.
use serde_json::{json, Value};

use std::collections::HashMap;
use std::path::Path;

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
    folded: &Result<Value, String>,
    board: &Result<Value, String>,
) -> Result<Vec<Value>, String> {
    if !level.is_some_and(|l| *l <= 1) {
        return Ok(Vec::new());
    }
    let owned = folded
        .as_ref()
        .map_err(Clone::clone)?
        .get("owned_scopes")
        .and_then(|v| v.as_object())
        .ok_or_else(|| {
            folded
                .as_ref()
                .err()
                .cloned()
                .unwrap_or_else(|| "the fold's owner map is unreadable".to_string())
        })?;
    let by_id: HashMap<&str, &Value> = entries
        .iter()
        .filter_map(|e| Some((e.get("id")?.as_str()?, e)))
        .collect();
    let rows: Vec<Value> = board
        .as_ref()
        .map_err(Clone::clone)?
        .get("queues")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|q| {
            matches!(
                q.get("name").and_then(Value::as_str),
                Some("unplanned") | Some("undispatched")
            )
        })
        .flat_map(|q| {
            q.get("rows")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .collect();
    let row_ids: Vec<&str> = rows
        .iter()
        .filter_map(|r| r.get("id").and_then(Value::as_str))
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
            if id == scope || owned.contains_key(id) {
                break;
            }
            let Some(e) = by_id.get(id) else {
                break;
            };
            let open = !matches!(
                e.get("status").and_then(Value::as_str),
                Some("done") | Some("superseded")
            );
            if e.get("type").and_then(Value::as_str) == Some("epic") && open {
                seat = Some(id);
                break;
            }
            cur = e.get("parent").and_then(Value::as_str);
        }
        if let Some(epic) = seat {
            grouped.entry(epic).or_default().push(row);
        }
    }
    let mut seats: Vec<Value> = Vec::new();
    for (epic, children) in grouped {
        seats.push(json!({"scope": epic, "children": children}));
    }
    Ok(seats)
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
    slots: Result<usize, String>,
    floor: &str,
    other_owned: &Result<HashMap<String, String>, String>,
    idea_children: &Result<usize, String>,
    idea_rows: &[Value],
    seat_leads: &Result<Vec<Value>, String>,
) -> Result<Value, String> {
    let session_id = session_id
        .ok_or_else(|| "no session id; cannot count this lead's blueprint subagents".to_string())?;
    let holders = claims?;
    let other_owned = other_owned.as_ref().map_err(Clone::clone)?;
    let holder = format!("blueprint-session:{session_id}");
    let running = holders.iter().filter(|h| **h == holder).count();
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
    for row in idea_rows {
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
    let seat = seat_leads.as_ref().map_err(Clone::clone)?;
    let seat_pairs: Vec<(String, String)> = seat
        .iter()
        .flat_map(|s| {
            let epic = s
                .get("scope")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            s.get("children")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(move |c| c.as_str().map(|cid| (cid.to_string(), epic.clone())))
        })
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
        .filter(|s| {
            let scope_id = s.get("scope").and_then(Value::as_str).unwrap_or_default();
            seat_pairs.iter().any(|(_, e)| e == scope_id)
        })
        .cloned()
        .map(|mut s| {
            // The journal entry keeps IDEA_LINES children as evidence; the
            // full list already rides the skips, so the seat row stays well
            // under the event cap on a large board.
            if let Some(children) = s.get_mut("children").and_then(Value::as_array_mut) {
                children.truncate(IDEA_LINES);
            }
            s
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

/// Read the ceiling state live. `None` when the session id or the claims
/// listing is unreadable: an unmeasured ceiling never converts an
/// actionable row into a waiting one.
pub(crate) fn stop_check_reading(cwd: &Path) -> Option<BlueprintWait> {
    let (session, _) = crate::claims::resolve_identity();
    let session = session?;
    let rows = crate::claims::list(Some("node:"), None, false).ok()?;
    let running = running_blueprint_ids(&rows, &session);
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
            slots,
            floor,
            Ok(HashMap::new()),
            Ok(0),
            vec![],
        )
    }

    /// The same call with the owner map named, for the deeper-team filter,
    /// and the idea-children count named, for the epic-heavy line.
    fn blueprint_reading_owned(
        dir: &std::path::Path,
        config_toml: &str,
        board: Value,
        session_id: Option<&str>,
        holders: Vec<String>,
        slots: Result<usize, String>,
        floor: &str,
        other_owned: Result<HashMap<String, String>, String>,
        idea_children: Result<usize, String>,
        seat_leads: Vec<Value>,
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
            slots,
            floor,
            &other_owned,
            &idea_children,
            &[],
            &Ok(seat_leads),
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
    #[test]
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
            Ok(4),
            "high",
            Ok(HashMap::new()),
            Ok(0),
            vec![json!({"scope": "x-eSeat", "children": ["x-seat1"]})],
        )
        .unwrap();
        assert_eq!(
            reading["seat_leads"],
            json!([{"scope": "x-eSeat", "children": ["x-seat1"]}]),
            "{reading}"
        );
        assert_eq!(reading["target_ready"], json!(["x-own1"]), "{reading}");
        assert_eq!(
            reading["skips"],
            json!([{"id": "x-seat1",
                "reason": "rides leadless epic x-eSeat: its seat-lead line carries the work"}]),
            "{reading}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The seat walk: a row's parent chain seats under the first open epic
    /// no live team owns; the walk stops at this lead's own scope and at a
    /// live team's node, and an L2+ (or untitled) beat seats nothing.
    #[test]
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
        let folded = Ok(json!({"owned_scopes": {"x-owned1": "x-team", "x-o1": "x-team"}}));
        let seat = seat_leads("x-mine", Some(1), &entries, &folded, &Ok(board)).unwrap();
        assert_eq!(
            seat,
            vec![json!({"scope": "x-e1", "children": ["x-c1", "x-c2"]})]
        );
        assert!(
            seat_leads("x-mine", Some(2), &entries, &folded, &Ok(json!({})))
                .unwrap()
                .is_empty()
        );
        assert!(
            seat_leads("x-mine", None, &entries, &folded, &Ok(json!({})))
                .unwrap()
                .is_empty()
        );
    }

    /// The rendered seat-lead line names the scope, the thread lane and the
    /// lead payload, with no --name: the nameless promote form the grant
    /// gate accepts from a holding session.
    #[test]
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
    /// The row skips naming the team that owns it.
    #[test]
    fn a_child_of_a_deeper_live_team_never_prints_as_start_or_target_ready() {
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
            Ok(4),
            "high",
            Ok(HashMap::from([
                ("x-c1".to_string(), "x-cccc".to_string()),
                ("x-c2".to_string(), "x-cccc".to_string()),
            ])),
            Ok(0),
            vec![],
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
            Ok(4),
            "high",
            Err("a live team's scope does not compile: e-x: unknown".to_string()),
            Ok(0),
            vec![],
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
