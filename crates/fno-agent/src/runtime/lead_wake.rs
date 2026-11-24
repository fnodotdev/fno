//! The lead-wake arm: a lead whose journaled check-in beat nears its
//! deadline gets woken by the daemon (default: five minutes early, so the
//! beat still lands on time) (the mail lane; a codex thread
//! receives it as turn/start). No other lead is told: a subordinate cannot
//! run its superior's check-in, so a cross-lead notice only costs the
//! recipient a turn. The operator lane carries a lapse that repeats - the
//! same lead missing a second beat in a row. The arm writes the `lead_wake`
//! tick rows the status table read UNOBSERVED before, and a `lead_wake`
//! journal event receipts each wake, so one lapse gets one wake: every
//! attempt receipts, and a receipt newer than the beat row holds the next
//! plan until a fresh check-in moves the beat.

use crate::harness::Harness;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::paths::AgentsHome;
use crate::territory::Team;

/// The arm's own beat, matching its `KNOWN_ARMS` row.
pub const LEAD_WAKE_INTERVAL_S: u64 = 300;

/// One minute past the missed beat: the operator escalation's second line
/// is `lead.checkin_interval` (default 55m) times two plus this grace.
pub const WAKE_AFTER_SECS: i64 = 60;

/// The wake lands this many seconds BEFORE the beat deadline, so the beat
/// still lands on time. The old wake fired one minute after the deadline:
/// the daemon mailed only after the 55-minute beat had already passed
/// (lead-cf20 retro, w7 05:53:57).
pub const WAKE_BEFORE_SECS: i64 = 300;

/// A parked lead still takes one beat a day: the slow heartbeat that
/// re-stamps the park, drains mail, and bounds every fail-closed read.
pub const PARK_HEARTBEAT_SECS: i64 = 86_400;

/// The park stamp's question-id cap. A scope's open questions number in the
/// tens; the cap keeps a runaway board from ballooning every beat row.
const PARK_IDS_CAP: usize = 64;

/// The held park one team's last beat stamped: the open question ids the
/// beat saw. A park whose set still matches the live pages keeps the lead
/// asleep; a moved set lifts it.
pub(crate) struct ParkState {
    pub ids: Vec<String>,
}

/// The recheck verdict for one parked scope. `Unreadable` keeps sleeping:
/// the recheck runs every arm tick, so a stale attention index must not
/// page the operator through the second-lapse ladder - the daily heartbeat
/// bounds the stall instead.
pub(crate) enum ParkVerdict {
    Held,
    Lifted,
    Unreadable,
}

/// The mail sender name the wake and the operator notice carry.
const SENDER: &str = "fno-lead-wake";

/// How much of a team scope a notice shows before it elides.
const SCOPE_CAP: usize = 48;

/// One overdue lead and whether the operator lane rides this wake.
/// `park_beat` marks the daily heartbeat of a still-parked lead: the wake
/// text carries the held-park marker, so the wake meter leaves the
/// re-park turn out of its machine count.
pub(crate) struct WakePlan<'a> {
    pub lead: &'a Team,
    pub idle_secs: i64,
    pub operator: bool,
    pub park_beat: bool,
}

/// The node ids a scope spelling names: its comma-separated members, trimmed.
/// A project-name scope yields names no graph row carries.
pub(crate) fn scope_members(scope: &str) -> impl Iterator<Item = &str> {
    scope.split(',').map(str::trim).filter(|s| !s.is_empty())
}

/// The overdue set and who hears about it. A lead with no journaled beat
/// has no baseline, so it is skipped, never invented. Only the idle lead
/// itself is planned; nobody one rung away is. One wake per lapse: a
/// receipt newer than the beat row names the same missed beat, so it waits;
/// a fresh check-in moves the beat forward and re-arms the wake for the
/// next miss. A receipt newer than the beat with a whole further beat gone
/// by is the second missed beat in a row: the plan re-arms once, with the
/// operator lane riding it.
///
/// A scope whose last beat stamped a held park skips the cadence wake
/// while its stamped question set still matches the live pages: every row
/// waits on an owner question, so a full check-in beat would read "no
/// change". The park lifts on a moved set - a question answered
/// or a new one asked - or at the daily heartbeat, whichever lands first.
/// A parked scope still rides the same receipt ladder, so one lift wake
/// per lapse holds and a missed lift escalates exactly like a missed beat.
pub(crate) fn plan_wakes<'a>(
    teams: &'a [Team],
    beats: &BTreeMap<String, Option<i64>>,
    wakes: &BTreeMap<String, i64>,
    parks: &BTreeMap<String, ParkState>,
    beat_secs: i64,
    now: i64,
    recheck: &mut dyn FnMut(&str, &ParkState) -> ParkVerdict,
) -> Vec<WakePlan<'a>> {
    // The wake fires BEFORE the beat deadline: five minutes early by
    // default, floored at half the beat so a short beat still gets a real
    // window. The old line fired one minute after the deadline, so the
    // daemon mailed the lead only after the beat had already passed.
    let wake_line = (beat_secs - WAKE_BEFORE_SECS).max(beat_secs / 2);
    let mut out = Vec::new();
    for team in teams {
        let Some(last) = beats.get(&team.scope).copied().flatten() else {
            continue;
        };
        let idle_secs = now - last;
        // The parked gate. Held and unreadable keep sleeping inside the
        // heartbeat window; Lifted and the past-heartbeat case fall through
        // to the receipt ladder with the normal wake text, except the
        // heartbeat itself, whose text carries the held-park marker.
        let park_beat;
        match parks.get(&team.scope) {
            Some(state) if idle_secs < PARK_HEARTBEAT_SECS => match recheck(&team.scope, state) {
                ParkVerdict::Lifted => park_beat = false,
                _ => continue,
            },
            Some(_) => park_beat = true,
            None => {
                if idle_secs < wake_line {
                    continue;
                }
                park_beat = false;
            }
        }
        let second_line = last + 2 * beat_secs + WAKE_AFTER_SECS;
        let receipt = wakes.get(&team.scope).copied();
        let operator;
        if receipt.is_none_or(|w| w <= last) {
            // First lapse of this beat (the wake lands before the deadline): the
            // lead is woken alone. The
            // operator lane never fires on a first lapse, and no other
            // lead hears about it - a subordinate cannot run its
            // superior's check-in.
            operator = false;
        } else if receipt.is_some_and(|w| w < second_line) {
            // Woken once for this beat. The second beat is missed only
            // past its own grace minute; escalate exactly once, and the
            // receipt this pass writes holds every later pass.
            if now < second_line {
                continue;
            }
            operator = true;
        } else {
            // The escalation for this lapse is already out.
            continue;
        }
        out.push(WakePlan {
            lead: team,
            idle_secs,
            operator,
            park_beat,
        });
    }
    out
}

fn mins(secs: i64) -> i64 {
    secs / 60
}

fn short_scope(scope: &str) -> String {
    let mut text: String = scope.chars().take(SCOPE_CAP).collect();
    if scope.chars().count() > SCOPE_CAP {
        text.push_str("...");
    }
    text
}

pub(crate) fn wake_text(idle_secs: i64, beat_secs: i64) -> String {
    format!(
        "Automatic lead-wake from the fno daemon: no check-in beat journaled for {}m \
         and the beat is due at {}m. Run your check-in now: fno agent hq checkin. \
         A second missed beat in a row tells the operator.",
        mins(idle_secs),
        mins(beat_secs),
    )
}

/// The daily heartbeat of a still-parked lead. The literal marker is the
/// wake meter's exclusion key (eval/wake_meter.rs), so the re-park turn
/// stays out of the machine count: a permanently parked lead whose only
/// turns are these heartbeats must not read "wake ratio over 3 to 1".
pub(crate) fn park_beat_text(idle_secs: i64) -> String {
    format!(
        "Automatic lead-wake from the fno daemon (held-park heartbeat, {}m since \
         the last beat): every row in scope still waits on a question, so the \
         cadence wake sleeps. Run your check-in to re-stamp the park or start \
         work: fno agent hq checkin.",
        mins(idle_secs),
    )
}

/// The body of the operator notice, which only rides a second consecutive
/// missed beat: which lead, how idle, and that the daemon already woke it
/// once for the first lapse.
pub(crate) fn operator_text(lead: &Team, idle_secs: i64, beat_secs: i64) -> String {
    format!(
        "Lead wake: {} (L{} {}) journaled no check-in for {}m; the beat is {}m. \
         The daemon woke it on the first missed beat; this is the second in a row.",
        lead.holder,
        lead.level,
        short_scope(&lead.scope),
        mins(idle_secs),
        mins(beat_secs),
    )
}

/// The arm's pass outcome, the same shape every tick arm reports.
pub(crate) struct Outcome {
    pub acted: u64,
    pub skip_reason: Option<String>,
    pub detail: String,
}

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn parse_ts(raw: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|value| value.timestamp())
}

/// The newest `lead_wake` receipt per live team scope: the dedupe memory.
/// Rows for scopes with no live team are ignored. Every attempt counts,
/// `acted` included: the flood of 2026-11-08 re-sent a held lead's wake
/// every tick for ~18 h because zero-acted receipts (the delivery parked in
/// the harness queue) did not suppress the next plan. One attempt per lapse
/// is the contract now; the parked copy drains on its own when the session
/// frees, so re-sending was never the cure.
pub(crate) fn wake_receipts(
    journals: &[std::path::PathBuf],
    teams: &[Team],
) -> BTreeMap<String, i64> {
    let scopes: std::collections::HashSet<&str> = teams.iter().map(|t| t.scope.as_str()).collect();
    let mut out: BTreeMap<String, i64> = BTreeMap::new();
    for journal in journals {
        for line in crate::event_store::journal_text(journal, &["lead_wake"]).lines() {
            let Ok(row) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let (Some(scope), Some(ts)) = (
                row.get("data")
                    .and_then(|d| d.get("scope"))
                    .and_then(Value::as_str),
                row.get("ts").and_then(Value::as_str).and_then(parse_ts),
            ) else {
                continue;
            };
            if !scopes.contains(scope) {
                continue;
            }
            let entry = out.entry(scope.to_string()).or_insert(ts);
            *entry = (*entry).max(ts);
        }
    }
    out
}

/// One delivery over the shared runner: mail first, resume fallback only
/// when the mail lane did not park a durable copy (burn_watch's ladder, so
/// a codex thread gets its turn/start and a parked claude session is
/// revived by the drain, never by a second send - the 2026-11-08 flood
/// doubled every tick through this leg).
fn deliver(sid: &str, text: &str) -> bool {
    let runner: crate::burn_watch::Runner = &mut crate::burn_watch::run_command;
    let (ok, _lane) = crate::burn_watch::wake_with_text(sid, true, text, SENDER, runner);
    ok
}

fn notify_operator(title: &str, body: &str) -> bool {
    crate::operator_notice::notify_operator_confirmed(title, body, Some("fno agent status"))
}

/// The beat's cron act: a codex lead the wake reached gets its
/// resting goal resumed, so the beat is a work beat (check-in, then the
/// board) and not one turn that sleeps again. A claude lead, a stale
/// manifest, or an unreachable root is no signal, never an error; a provider
/// refusal rides the note, and the beat still happened.
fn resume_resting_goal(
    home: &AgentsHome,
    plan: &WakePlan,
    lead_session: &str,
    roots: &BTreeMap<String, String>,
) -> Option<String> {
    let root = plan
        .lead
        .holder_session
        .as_deref()
        .and_then(|sid| roots.get(sid))
        .or_else(|| roots.get(&plan.lead.holder))
        .filter(|r| !r.is_empty())?;
    let manifest_path = crate::paths::space_dir(Path::new(root))
        .join("leads")
        .join(format!("{}.md", plan.lead.scope));
    let manifest = beat_resume_target(lead_session, &manifest_path)?;
    match crate::lead_goal::resume(&manifest, Path::new(root)) {
        Ok(receipt) => {
            let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
            let payload = serde_json::json!({
                "session_id": lead_session,
                "scope": plan.lead.scope,
                "resumed": true,
                "successor": false,
                "reason": "beat",
                "provider_receipt": receipt,
            });
            if let Err(error) = emitter.emit("lead_goal_resumed", &payload) {
                eprintln!("lead-wake: goal resume receipt emit failed: {error}");
            }
            Some("resumed its resting goal".to_string())
        }
        Err(error) => Some(format!("goal resume refused: {error}")),
    }
}

/// The admission gate, no provider call: the piece a test can hold without
/// touching the process env. The caller resolves the manifest path (one
/// space_dir read per pass, not one per waking lead); the gate reads it and
/// admits only a codex manifest naming this exact live session.
fn beat_resume_target(
    lead_session: &str,
    manifest_path: &Path,
) -> Option<crate::lead_termination::LeadManifest> {
    let content = std::fs::read_to_string(manifest_path).ok()?;
    let manifest = crate::lead_termination::parse_lead_manifest(&content)?;
    if manifest.harness.as_deref().map(Harness::parse) != Some(Harness::Codex)
        || manifest.harness_session_id.as_deref() != Some(lead_session)
    {
        return None;
    }
    Some(manifest)
}

/// One wake: deliver to the lead, escalate to the operator on the second
/// consecutive missed beat, receipt the episode. `beat_journal` names where
/// the beat was read, riding the receipt.
fn act_on_plan(
    home: &AgentsHome,
    plan: WakePlan,
    beat_secs: i64,
    beat_journal: Option<&str>,
    roots: &BTreeMap<String, String>,
) -> (u64, String) {
    let mut acted = 0u64;
    let mut notes: Vec<String> = Vec::new();
    let lead_session = plan.lead.holder_session.clone().unwrap_or_default();
    let text = if plan.park_beat {
        park_beat_text(plan.idle_secs)
    } else {
        wake_text(plan.idle_secs, beat_secs)
    };
    if !lead_session.is_empty() && deliver(&lead_session, &text) {
        acted += 1;
        notes.push(format!("woke {}", plan.lead.holder));
        if let Some(note) = resume_resting_goal(home, &plan, &lead_session, roots) {
            notes.push(format!("{} {}", plan.lead.holder, note));
            acted += 1;
        }
    }
    if plan.operator
        && notify_operator(
            "lead beat missed",
            &operator_text(plan.lead, plan.idle_secs, beat_secs),
        )
    {
        acted += 1;
        notes.push("told operator".to_string());
    }
    emit_receipt(
        home,
        plan.lead,
        plan.idle_secs,
        beat_secs,
        beat_journal,
        acted,
    );
    (acted, notes.join(", "))
}

/// The durable record of one wake. `beat_journal` is the journal the beat
/// was read from, so a wake anchored at the shared pair while the team
/// checks in elsewhere is visible in the receipt alone.
fn emit_receipt(
    home: &AgentsHome,
    lead: &Team,
    idle_secs: i64,
    beat_secs: i64,
    beat_journal: Option<&str>,
    acted: u64,
) {
    let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
    let payload = serde_json::json!({
        "scope": lead.scope,
        "level": lead.level,
        "holder_session": lead.holder_session,
        "idle_secs": idle_secs,
        "beat_secs": beat_secs,
        "beat_journal": beat_journal,
        "acted": acted,
    });
    if let Err(error) = emitter.emit("lead_wake", &payload) {
        eprintln!("lead-wake: receipt emit failed: {error}");
    }
}

/// One pass: read the live teams, fold each lead's last beat and wake
/// receipt, wake what is past the line, and report the outcome.
pub(crate) fn run_pass(home: &AgentsHome, config_cwd: &Path) -> Result<Outcome, String> {
    let now = now_epoch();
    let registry = home.registry_json();
    let mut teams =
        crate::territory::live_teams(&registry).map_err(|e| format!("lead_wake: {}", e.0))?;
    let rows = crate::state::load_registry(&registry)
        .map(|loaded| loaded.entries)
        .unwrap_or_default();
    let retired = crate::retired_lead::wake_refusals(home, &rows);
    teams.retain(|team| {
        team.holder_session
            .as_deref()
            .is_none_or(|session| !retired.contains_key(session))
    });
    if teams.is_empty() {
        let mut detail = "no live lead teams".to_string();
        for line in crate::retired_lead::split_rows(rows, &retired).1 {
            detail.push_str("; ");
            detail.push_str(&line);
        }
        return Ok(Outcome {
            acted: 0,
            skip_reason: Some("no_teams".to_string()),
            detail: short(&detail),
        });
    }
    let beat_secs = crate::lead_verdict_inputs::checkin_interval_secs(config_cwd);
    // project -> root, from the workspace config: the durable answer to
    // where a team's check-ins land. A holder row's cwd is the dir the
    // session registered from, and a resume from a stranger dir moves it,
    // which strands the fold at a journal nobody writes beats to. The first
    // scope member keys the map, so a project-keyed scope resolves its own
    // repo and an epic-list scope misses and keeps the holder-cwd fold.
    let workspace_roots = crate::territory::workspace_paths(config_cwd);
    let mut beats: BTreeMap<String, Option<i64>> = BTreeMap::new();
    // scope -> the held park the last beat stamped, when it stamped one.
    let mut parks: BTreeMap<String, ParkState> = BTreeMap::new();
    // scope -> the journal the beat was read from, so the receipt can say
    // where the beat came from and a miskeyed read is visible in the log.
    let mut beat_journals: BTreeMap<String, Option<String>> = BTreeMap::new();
    for team in &teams {
        let workspace_root = scope_members(&team.scope)
            .next()
            .and_then(|key| workspace_roots.get(key))
            .map(|root| Path::new(root.as_str()));
        let beat = crate::lead_history::previous_beat_with_source(
            &team_beat_journals(home, &registry, team, workspace_root),
            &team.scope,
            team.holder_session.as_deref(),
            false,
        )
        .ok();
        let ts = beat
            .as_ref()
            .and_then(|b| b.row.as_ref())
            .and_then(|r| r.get("ts").and_then(Value::as_str))
            .and_then(parse_ts);
        beats.insert(team.scope.clone(), ts);
        if let Some(data) = beat
            .as_ref()
            .and_then(|b| b.row.as_ref())
            .and_then(|r| r.pointer("/data"))
            .and_then(Value::as_object)
        {
            // The row's data deserializes once through the check-in's typed
            // mirror, so the arm reads the stamp through the same shape the
            // beat wrote it with.
            let view = crate::runtime::lead_checkin::de::BeatData::snapshot(data);
            if view.held_park == Some(true) {
                let ids: Vec<String> = view
                    .held_park_qs
                    .unwrap_or_default()
                    .into_iter()
                    .take(PARK_IDS_CAP)
                    .collect();
                parks.insert(team.scope.clone(), ParkState { ids });
            }
        }
        beat_journals.insert(
            team.scope.clone(),
            beat.and_then(|b| b.journal)
                .map(|journal| journal.display().to_string()),
        );
    }
    let wakes = wake_receipts(&crate::tick_ledger::journals(home), &teams);
    // The park recheck reads the same attention pages the beat's held
    // reading read, so the arm's verdict and the stamp share one source. A
    // set moved in either direction - a question answered or a new one
    // asked - lifts the park; an identical set keeps the lead asleep.
    let mut recheck = |scope: &str, park: &ParkState| -> ParkVerdict {
        match crate::lead_answers::held_open_ids(scope) {
            Err(_) => ParkVerdict::Unreadable,
            Ok(ids) => {
                let moved = ids.iter().any(|id| !park.ids.contains(id))
                    || park.ids.iter().any(|id| !ids.contains(id));
                if moved {
                    ParkVerdict::Lifted
                } else {
                    ParkVerdict::Held
                }
            }
        }
    };
    let plans = plan_wakes(&teams, &beats, &wakes, &parks, beat_secs, now, &mut recheck);
    let mut acted = 0u64;
    let mut notes: Vec<String> = Vec::new();
    let mut skip_reason: Option<String> = None;
    if plans.is_empty() {
        skip_reason = Some("none_overdue".to_string());
        notes.push(format!(
            "{} lead(s) observed; none past the {}m line",
            teams.len(),
            mins(beat_secs)
        ));
    } else {
        // holder -> repo root (project_root, else the row's workspace entry,
        // else cwd): where the lead manifest and the provider thread's cwd
        // live, read once per pass, and only when a wake actually plans (the
        // quiet pass never pays the registry parse).
        let roots: BTreeMap<String, String> = crate::state::load_registry(&registry)
            .map(|loaded| {
                loaded
                    .entries
                    .into_iter()
                    .flat_map(|e| {
                        let member = e
                            .role_scope
                            .as_deref()
                            .and_then(|s| scope_members(s).next());
                        let root = journal_root(&e, member, &workspace_roots);
                        // Keyed on the session id (law d-e952ed19); a row
                        // with no id keeps the name key, the legacy shape.
                        let sid = crate::territory::row_session_id(&e).to_string();
                        if sid.is_empty() {
                            return vec![(e.name.clone(), root)];
                        }
                        // A row whose id came from the Rust-only session_id
                        // field keeps its name key too: the beat team's
                        // holder_session reads only harness_session_id, so
                        // the resume lookup falls back to the name.
                        let harness = e.harness_session_id.as_deref().unwrap_or("");
                        if harness.is_empty() {
                            vec![(sid.clone(), root.clone()), (e.name.clone(), root)]
                        } else {
                            vec![(sid, root)]
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        for plan in plans {
            let journal = beat_journals.get(&plan.lead.scope).cloned().flatten();
            let (n, note) = act_on_plan(home, plan, beat_secs, journal.as_deref(), &roots);
            acted += n;
            if !note.is_empty() {
                notes.push(note);
            }
        }
    }
    // The refill-respawn leg: which lead that left its board is woken this
    // beat. One writer of the `lead_wake` row now carries both the
    // overdue-beat wake and the refill respawn.
    let config = crate::lead_wake_triggers::TriggerConfig::read(config_cwd);
    if config.enabled {
        let leg = crate::lead_wake_triggers::run(home, config_cwd, &config);
        acted += leg.acted as u64;
        if leg.acted > 0 || leg.roles > 0 {
            skip_reason = None;
        }
        notes.push(leg.detail_line());
    } else {
        notes.push("respawn off".to_string());
    }
    Ok(Outcome {
        acted,
        skip_reason,
        detail: short(&notes.join("; ")),
    })
}

fn short(text: &str) -> String {
    text.chars().take(160).collect()
}

/// One row's wake-delivery root: the row's `project_root`, else the row's
/// own workspace entry from the config project map, else the row's `cwd`.
/// The workspace step is the fno heal (2026-10-10): the registry-fix
/// restart left the level-1 head's row with an empty `project_root` and a
/// HOME launch `cwd`, so the delivery roots map anchored the wake at a
/// space the led repo does not live in. The beat lookup covers the same
/// drift the other way: it keeps the row's own journal and adds the
/// workspace journal the scope names. `member` is the row's first
/// role-scope member where the caller knows it.
fn journal_root(
    entry: &crate::state::RegistryEntry,
    member: Option<&str>,
    workspace: &HashMap<String, String>,
) -> String {
    if !entry.project_root.is_empty() {
        return entry.project_root.clone();
    }
    if let Some(root) = member.and_then(|m| workspace.get(m)) {
        if !root.is_empty() {
            return root.clone();
        }
    }
    entry.cwd.clone()
}

/// The journals for one team's beat lookup: the shared pair, the holder
/// row's project journal, and the workspace journal the scope names. The
/// check-in emitter journals where its caller points it - the Python lead
/// CLI passes `--emit-path <space>/events.jsonl` - so the shared pair
/// stopped seeing the rows (2026-09-17). The registry row is keyed on the
/// holder session id, never the mutable row name (law d-e952ed19). The
/// holder row's cwd is what the session registered from, so a resume from
/// a stranger dir folds a journal nobody writes beats to; the workspace
/// root names the repo the scope owns and covers that drift. Isolated per
/// team on purpose: `scan_scopes` errors the WHOLE list when one store
/// cannot be opened, so a shared list would turn one corrupt project
/// journal into a blind pass for every team; here only that team's read
/// degrades. An unreadable registry degrades to the shared pair and says
/// so on stderr: the receipt's `beat_journal` records the same fact, but
/// the daemon log is where a broken registry gets noticed.
fn team_beat_journals(
    home: &AgentsHome,
    registry_path: &Path,
    team: &Team,
    workspace_root: Option<&Path>,
) -> Vec<PathBuf> {
    let mut journals = crate::tick_ledger::journals(home);
    match crate::state::load_registry(registry_path) {
        Ok(loaded) => {
            for entry in &loaded.entries {
                if entry.harness_session_id.as_deref() != team.holder_session.as_deref() {
                    continue;
                }
                let root = if entry.project_root.is_empty() {
                    &entry.cwd
                } else {
                    &entry.project_root
                };
                if root.is_empty() {
                    continue;
                }
                let journal = crate::paths::space_dir(Path::new(root)).join("events.jsonl");
                if !journals.contains(&journal) {
                    journals.push(journal);
                }
                break;
            }
        }
        Err(error) => {
            eprintln!(
                "lead-wake: beat lookup for {} degrades to the shared pair: {error}",
                team.scope
            );
        }
    }
    if let Some(root) = workspace_root {
        let journal = crate::paths::space_dir(root).join("events.jsonl");
        if !journals.contains(&journal) {
            journals.push(journal);
        }
    }
    journals
}

/// The arm as the daemon holds it: cadence stamp plus one-in-flight gate.
/// The config cwd rides at construction, so the thresholds
/// (`lead.checkin_interval`) and the territory resolve read the same root
/// the daemon declared.
pub struct Arm {
    config_cwd: PathBuf,
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

impl Arm {
    pub fn new(config_cwd: PathBuf) -> Self {
        Self {
            config_cwd,
            last_tick: Mutex::new(None),
            in_flight: Arc::new(AtomicBool::new(false)),
        }
    }
}

pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    let interval = Duration::from_secs(LEAD_WAKE_INTERVAL_S);
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < interval)
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    let config_cwd = arm.config_cwd.clone();
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        let outcome = match run_pass(&home, &config_cwd) {
            Ok(outcome) => outcome,
            Err(error) => Outcome {
                acted: 0,
                skip_reason: Some("unreadable".to_string()),
                detail: short(&error),
            },
        };
        let journal = crate::loop_runtime::Journal::new_raw(
            home.events_jsonl(),
            crate::daemon::global_events_path(&home),
        );
        crate::tick_ledger::emit_tick(
            &journal,
            "lead_wake",
            crate::tick_ledger::SCHED_DAEMON,
            outcome.acted,
            outcome.skip_reason.as_deref(),
            Some(&outcome.detail),
            LEAD_WAKE_INTERVAL_S,
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team(scope: &str, level: u8, holder: &str, sid: &str) -> Team {
        Team {
            scope: scope.to_string(),
            level,
            holder: holder.to_string(),
            holder_session: Some(sid.to_string()),
        }
    }

    #[test]
    fn the_wake_ladder_and_the_parked_gate_walk_together() {
        let teams = vec![team("x-aaa", 2, "candor", "s-l2a")];
        let beat = 3300;
        let now = 1_000_000_000;
        let last = now - 4000;
        let second_line = last + 2 * beat + 60;
        let beats = BTreeMap::from([("x-aaa".to_string(), Some(last))]);
        let no_parks: BTreeMap<String, ParkState> = BTreeMap::new();
        let mut unplanned =
            |_scope: &str, _park: &ParkState| -> ParkVerdict { panic!("no park to recheck") };
        // First lapse: no receipt yet, the lead is woken alone.
        let plans = plan_wakes(
            &teams,
            &beats,
            &BTreeMap::new(),
            &no_parks,
            beat,
            now,
            &mut unplanned,
        );
        assert_eq!(plans.len(), 1);
        assert!(
            !plans.first().expect("the first lapse plans").operator,
            "a first lapse never rides the operator lane"
        );
        // Woken once, still inside the second beat's grace: suppressed.
        let wakes = BTreeMap::from([("x-aaa".to_string(), last + 400)]);
        let plans = plan_wakes(
            &teams,
            &beats,
            &wakes,
            &no_parks,
            beat,
            last + 4000,
            &mut unplanned,
        );
        assert!(
            plans.is_empty(),
            "the second beat is not missed before its own grace passes"
        );
        // A full further beat with no fresh check-in: the second missed
        // beat in a row. The plan re-arms once and carries the operator.
        let later = second_line + 300;
        let plans = plan_wakes(
            &teams,
            &beats,
            &wakes,
            &no_parks,
            beat,
            later,
            &mut unplanned,
        );
        assert_eq!(
            plans.len(),
            1,
            "the second missed beat in a row re-arms the plan"
        );
        assert!(
            plans.first().expect("the second lapse plans").operator,
            "the operator lane rides the second lapse"
        );
        // The escalation's own receipt holds every later pass.
        let wakes = BTreeMap::from([("x-aaa".to_string(), later)]);
        let plans = plan_wakes(
            &teams,
            &beats,
            &wakes,
            &no_parks,
            beat,
            later + 9000,
            &mut unplanned,
        );
        assert!(
            plans.is_empty(),
            "one operator notice per lapse; a fresh check-in re-arms"
        );
        // The parked gate walks the same map: a held park sleeps inside the
        // heartbeat window, an unreadable recheck also sleeps (fail-closed),
        // a moved set lifts in either direction, the lift's receipt holds
        // the next pass, and the daily heartbeat plans with the marker.
        let parks = BTreeMap::from([(
            "x-aaa".to_string(),
            ParkState {
                ids: vec!["q-1".to_string(), "q-2".to_string()],
            },
        )]);
        let mut held = |_scope: &str, park: &ParkState| -> ParkVerdict {
            assert!(park.ids.contains(&"q-1".to_string()));
            ParkVerdict::Held
        };
        let plans = plan_wakes(
            &teams,
            &beats,
            &BTreeMap::new(),
            &parks,
            beat,
            now,
            &mut held,
        );
        assert!(
            plans.is_empty(),
            "a held park takes no cadence wake while the set stands"
        );
        let mut unreadable =
            |_scope: &str, _park: &ParkState| -> ParkVerdict { ParkVerdict::Unreadable };
        let plans = plan_wakes(
            &teams,
            &beats,
            &BTreeMap::new(),
            &parks,
            beat,
            now,
            &mut unreadable,
        );
        assert!(
            plans.is_empty(),
            "a stale attention read must not page a parked lead every tick"
        );
        let mut lifted = |scope: &str, park: &ParkState| -> ParkVerdict {
            assert_eq!(scope, "x-aaa");
            assert!(park.ids.contains(&"q-2".to_string()));
            ParkVerdict::Lifted
        };
        let plans = plan_wakes(
            &teams,
            &beats,
            &BTreeMap::new(),
            &parks,
            beat,
            now,
            &mut lifted,
        );
        assert_eq!(plans.len(), 1, "the lift wake plans in the same pass");
        assert!(
            !plans.first().expect("the lift plans").park_beat,
            "a lift wake carries the normal wake text, not the heartbeat"
        );
        let last = beats.get("x-aaa").copied().flatten().expect("beat ts");
        let wakes = BTreeMap::from([("x-aaa".to_string(), last + 400)]);
        let plans = plan_wakes(&teams, &beats, &wakes, &parks, beat, now + 900, &mut lifted);
        assert!(
            plans.is_empty(),
            "one lift wake per lapse; the receipt holds the next pass"
        );
        // A set that grew lifts too: the parked premise "nothing changed"
        // no longer holds in either direction.
        let mut grew = |_scope: &str, park: &ParkState| -> ParkVerdict {
            assert_eq!(park.ids.len(), 2);
            ParkVerdict::Lifted
        };
        let plans = plan_wakes(
            &teams,
            &beats,
            &BTreeMap::new(),
            &parks,
            beat,
            now,
            &mut grew,
        );
        assert_eq!(plans.len(), 1, "a moved set lifts the park");
        // Past the heartbeat window the parked lead plans again, and the
        // wake carries the held-park marker the wake meter excludes.
        let later = now + PARK_HEARTBEAT_SECS + 600;
        let plans = plan_wakes(
            &teams,
            &beats,
            &BTreeMap::new(),
            &parks,
            beat,
            later,
            &mut held,
        );
        assert_eq!(plans.len(), 1, "the daily heartbeat wakes a parked lead");
        assert!(
            plans.first().expect("the heartbeat plans").park_beat,
            "the heartbeat wake carries the held-park marker"
        );
        assert!(
            !plans.first().expect("the heartbeat plans").operator,
            "a heartbeat is a first lapse; the operator lane never rides it"
        );
        assert!(park_beat_text(PARK_HEARTBEAT_SECS).contains("held-park heartbeat"));
    }

    fn write_beat_manifest(path: &Path, scope: &str, harness: &str, session: &str) {
        // fno_id is non-empty: the manifest parser refuses a frontmatter
        // block without one, so the fixture must carry a real-shaped id.
        std::fs::write(
            path,
            format!(
                "---\nfno_id: {scope}-fixture\nscope: {scope}\nharness: {harness}\nharness_session_id: {session}\n---\n"
            ),
        )
        .unwrap();
    }

    // The gate reads the manifest path its caller resolved, so the tests
    // need no env pin: the space-dir resolution contract is paths.rs's, and
    // an env pin here raced the suite's unlocked env mutators in CI.
    #[test]
    fn the_beat_resume_gate_admits_only_this_codex_sessions_manifest() {
        let repo = tempfile::tempdir().unwrap();
        let manifest_path = repo.path().join("x-aaa.md");
        // No manifest, a claude manifest, or another session's manifest:
        // all no signal.
        assert!(beat_resume_target("s-l2a", &manifest_path).is_none());
        write_beat_manifest(&manifest_path, "x-aaa", "claude", "s-l2a");
        assert!(beat_resume_target("s-l2a", &manifest_path).is_none());
        write_beat_manifest(&manifest_path, "x-aaa", "codex", "s-other");
        assert!(beat_resume_target("s-l2a", &manifest_path).is_none());
        // The woken codex session's own manifest: admitted.
        write_beat_manifest(&manifest_path, "x-aaa", "codex", "s-l2a");
        let manifest =
            beat_resume_target("s-l2a", &manifest_path).expect("admits the woken codex lead");
        assert_eq!(manifest.scope, "x-aaa");
    }
}
