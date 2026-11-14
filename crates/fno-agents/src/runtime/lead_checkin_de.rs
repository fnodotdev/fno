//! Typed reads for the lead-checkin payloads.
//!
//! The beat used to walk its JSON with `.get("key").and_then(Value::as_str)`
//! chains. Those chains are total: a mistyped value reads None and the beat
//! keeps rendering. The structs here keep that totality. Every field
//! deserializer mirrors one `Value::as_*` accessor, so a field whose value is
//! not the declared type reads None instead of failing the parse, and one bad
//! field cannot blank its neighbors. A payload that is not even an object
//! parses as the all-empty struct, the same shape a `.get()` walk reads off a
//! scalar. docs/rust-style.md R1.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

/// Parse `v` into `T`; any parse failure reads as the all-empty struct.
pub(crate) fn parse<T: DeserializeOwned + Default>(v: &Value) -> T {
    T::deserialize(v).unwrap_or_default()
}

fn raw<'de, D: Deserializer<'de>>(d: D) -> Result<Value, D::Error> {
    Value::deserialize(d)
}

// The `opt_*` deserializers mirror one `Value::as_*` accessor each: null or
// a mistyped value reads None, never an error.
pub(crate) fn opt_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(raw(d)?.as_str().map(str::to_string))
}

pub(crate) fn opt_i64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Ok(raw(d)?.as_i64())
}

pub(crate) fn opt_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Ok(raw(d)?.as_u64())
}

pub(crate) fn opt_f64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    Ok(raw(d)?.as_f64())
}

pub(crate) fn opt_bool<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    Ok(raw(d)?.as_bool())
}

/// Present counts, even when null: mirrors `.get(key).is_some()`. Plain
/// `Option<Value>` maps null to None and cannot tell a null field from an
/// absent one; some reads (`unreadable`) key on presence alone.
pub(crate) fn opt_value_present<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Value>, D::Error> {
    Ok(Some(raw(d)?))
}

/// `.as_array()` semantics: an array reads `Some`, everything else None.
/// Elements that fail their own parse read as the all-empty struct.
pub(crate) fn opt_vec_lenient<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default>(
    d: D,
) -> Result<Option<Vec<T>>, D::Error> {
    let v = raw(d)?;
    Ok(v.as_array().map(|a| {
        a.iter()
            .map(|e| T::deserialize(e).unwrap_or_default())
            .collect()
    }))
}

/// An absent or non-array field reads an empty vec, the `.as_array()` walk.
pub(crate) fn vec_lenient<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default>(
    d: D,
) -> Result<Vec<T>, D::Error> {
    Ok(opt_vec_lenient(d)?.unwrap_or_default())
}

/// Strings of an array, non-strings dropped: the `filter_map(Value::as_str)`
/// walk.
pub(crate) fn opt_vec_string<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<String>>, D::Error> {
    let v = raw(d)?;
    Ok(v.as_array().map(|a| {
        a.iter()
            .filter_map(|e| e.as_str().map(str::to_string))
            .collect()
    }))
}

/// A nested object parsed leniently: null or a scalar reads None, a bad
/// object reads the all-empty struct, never a failed parse.
pub(crate) fn opt_lenient<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default>(
    d: D,
) -> Result<Option<T>, D::Error> {
    let v = raw(d)?;
    Ok(if v.is_null() {
        None
    } else {
        Some(T::deserialize(&v).unwrap_or_default())
    })
}

/// `.as_object()` semantics for a map whose values parse leniently.
pub(crate) fn opt_map_lenient<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default>(
    d: D,
) -> Result<Option<Map<String, T>>, D::Error> {
    let v = raw(d)?;
    Ok(v.as_object().map(|m| {
        m.iter()
            .map(|(k, v)| (k.clone(), T::deserialize(v).unwrap_or_default()))
            .collect()
    }))
}

// ---------------------------------------------------------------------------
// payloads

pub(crate) const UNREADABLE: &str = "unreadable";

/// The tail of one board queue: what the beat reads past the name/status
/// gate in `board_queue`.
#[derive(Deserialize, Default)]
pub(crate) struct QueueTail {
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) count: Option<i64>,
    #[serde(default, deserialize_with = "vec_lenient")]
    pub(crate) rows: Vec<BlockedRow>,
}

/// The scope-fold status gate, as `fetch_fold` reads it.
#[derive(Deserialize, Default)]
pub(crate) struct FoldStatus {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) status: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) reason: Option<String>,
}

/// The org-fold envelope, as `fetch_fold` reads it: the scope's own node
/// list plus the stuck rows the board reading folds in.
#[derive(Deserialize, Default)]
pub(crate) struct FoldEnvelopeInput {
    #[serde(default, deserialize_with = "opt_map_lenient")]
    pub(crate) scope_nodes: Option<Map<String, Value>>,
    #[serde(default)]
    pub(crate) stuck: Option<Value>,
}

/// The stuck rows one fold envelope carries.
#[derive(Deserialize, Default)]
pub(crate) struct StuckView {
    #[serde(default, deserialize_with = "vec_lenient")]
    pub(crate) blocked: Vec<StuckRow>,
}

#[derive(Deserialize, Default)]
pub(crate) struct StuckRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) blocked_by: Option<Value>,
}

/// One `blocked_child` row, fields passed through to the row JSON as-is.
#[derive(Deserialize, Default)]
pub(crate) struct BlockedRow {
    #[serde(default)]
    pub(crate) id: Option<Value>,
    #[serde(default)]
    pub(crate) session: Option<Value>,
    #[serde(default)]
    pub(crate) age_minutes: Option<Value>,
    #[serde(default)]
    pub(crate) reason: Option<Value>,
}

/// The `fold` view of one team scope, as `fetch_fold` mints it.
#[derive(Deserialize, Default)]
pub(crate) struct FoldView {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) name: Option<String>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) total: Option<i64>,
    #[serde(default, deserialize_with = "opt_map_lenient")]
    pub(crate) counts: Option<Map<String, Value>>,
    #[serde(default, deserialize_with = "opt_map_lenient")]
    pub(crate) owned_counts: Option<Map<String, Value>>,
    #[serde(default)]
    pub(crate) owned_reason: Option<Value>,
    #[serde(default)]
    pub(crate) epics: Option<Value>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) epic_cap: Option<u64>,
    #[serde(default, deserialize_with = "vec_lenient")]
    pub(crate) nodes: Vec<FoldNode>,
}

/// One node row of the fold.
#[derive(Deserialize, Default)]
pub(crate) struct FoldNode {
    #[serde(default)]
    pub(crate) owned: Option<Value>,
    #[serde(default)]
    pub(crate) id: Option<Value>,
    #[serde(default)]
    pub(crate) status: Option<Value>,
    #[serde(default)]
    pub(crate) worker: Option<Value>,
    #[serde(default)]
    pub(crate) pr_number: Option<Value>,
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) sessions: Option<Vec<Value>>,
}

/// The org reading `r_org` emits, as the render and `epic_line` read it.
#[derive(Deserialize, Default)]
pub(crate) struct OrgReading {
    #[serde(default)]
    pub(crate) active_nodes: Option<Value>,
    #[serde(default)]
    pub(crate) total_nodes: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) owned_reason: Option<String>,
    #[serde(default)]
    pub(crate) rows: Option<Value>,
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) epics: Option<Vec<EpicRow>>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) epic_cap: Option<u64>,
}

#[derive(Deserialize, Default)]
pub(crate) struct EpicRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) id: Option<String>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) open_children: Option<u64>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) full: Option<bool>,
}

/// The escalations reading.
#[derive(Deserialize, Default)]
pub(crate) struct EscReading {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) unreadable: Option<Value>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) open: Option<i64>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) overdue: Option<i64>,
    #[serde(default, deserialize_with = "vec_lenient")]
    pub(crate) rows: Vec<EscRow>,
}

#[derive(Deserialize, Default)]
pub(crate) struct EscRow {
    #[serde(default)]
    pub(crate) title: Option<Value>,
    #[serde(default)]
    pub(crate) class: Option<Value>,
    #[serde(default)]
    pub(crate) deadline: Option<Value>,
    #[serde(default)]
    pub(crate) state: Option<Value>,
}

/// The `fno doctor footprint --json` payload.
#[derive(Deserialize, Default)]
pub(crate) struct FootprintPayload {
    #[serde(default, deserialize_with = "opt_lenient")]
    pub(crate) admission: Option<Admission>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) unparsed_lines: Option<i64>,
}

#[derive(Deserialize, Default)]
pub(crate) struct Admission {
    #[serde(default)]
    pub(crate) verdict: Option<Value>,
}

/// The `fno agents admit status --json` payload.
#[derive(Deserialize, Default)]
pub(crate) struct GatePayload {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) verdict: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) reason: Option<String>,
    #[serde(default, deserialize_with = "vec_lenient")]
    pub(crate) rows: Vec<GateRow>,
    #[serde(default, deserialize_with = "opt_map_lenient")]
    pub(crate) lanes: Option<Map<String, Lane>>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) cap_clause: Option<String>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) yours: Option<u64>,
}

#[derive(Deserialize, Default)]
pub(crate) struct GateRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) verdict: Option<Value>,
}

#[derive(Deserialize, Default)]
pub(crate) struct Lane {
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) live: Option<u64>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) cap: Option<u64>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) quota: Option<String>,
}

/// The `fno agents hq --json` payload.
#[derive(Deserialize, Default)]
pub(crate) struct HqPayload {
    #[serde(default, deserialize_with = "opt_lenient")]
    pub(crate) summary: Option<HqSummary>,
    #[serde(default, deserialize_with = "vec_lenient")]
    pub(crate) teams: Vec<HqTeam>,
}

#[derive(Deserialize, Default)]
pub(crate) struct HqSummary {
    #[serde(default)]
    pub(crate) total: Option<Value>,
    #[serde(default)]
    pub(crate) splits: Option<Value>,
    #[serde(default)]
    pub(crate) disagreements: Option<Value>,
}

#[derive(Deserialize, Default)]
pub(crate) struct HqTeam {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) status: Option<String>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) agree: Option<bool>,
    #[serde(default)]
    pub(crate) holder: Option<Value>,
    #[serde(default)]
    pub(crate) scope: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) reason: Option<String>,
}

/// One territory row, as the unknown-membership check reads it.
#[derive(Deserialize, Default)]
pub(crate) struct TerritoryRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) membership: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) reason: Option<String>,
}

/// One territory row, as the render reads it.
#[derive(Deserialize, Default)]
pub(crate) struct TerritoryViewRow {
    #[serde(default)]
    pub(crate) scope: Option<Value>,
    #[serde(default)]
    pub(crate) rung: Option<Value>,
    #[serde(default)]
    pub(crate) mission: Option<Value>,
    #[serde(default)]
    pub(crate) live: Option<Value>,
    #[serde(default)]
    pub(crate) cap: Option<Value>,
    #[serde(default)]
    pub(crate) leadless: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) membership: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) reason: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct DrainPayload {
    #[serde(default)]
    pub(crate) undelivered: Option<Value>,
}

#[derive(Deserialize, Default)]
pub(crate) struct IdeaChildren {
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) count: Option<u64>,
}

#[derive(Deserialize, Default)]
pub(crate) struct WorkersTail {
    #[serde(default)]
    pub(crate) rows: Option<Value>,
}

#[derive(Deserialize, Default)]
pub(crate) struct BoardTail {
    #[serde(default, deserialize_with = "opt_vec_string")]
    pub(crate) blocked_on: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
pub(crate) struct PushbackReading {
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) routes: Option<u64>,
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) score: Option<f64>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) overturned: Option<u64>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) answered_self: Option<u64>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) escalated: Option<u64>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) asks: Option<u64>,
}

#[derive(Deserialize, Default)]
pub(crate) struct WakeReading {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) unmeasured: Option<String>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) machine: Option<u64>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) user: Option<u64>,
    #[serde(default)]
    pub(crate) tokens_since: Option<Value>,
    #[serde(default)]
    pub(crate) tokens_session: Option<Value>,
}

#[derive(Deserialize, Default)]
pub(crate) struct RrReading {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) unmeasured: Option<String>,
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) rate: Option<f64>,
    #[serde(default)]
    pub(crate) refused: Option<Value>,
    #[serde(default)]
    pub(crate) total: Option<Value>,
    #[serde(default)]
    pub(crate) window: Option<Value>,
}

#[derive(Deserialize, Default)]
pub(crate) struct RrTail {
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) rate: Option<f64>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) load_discounted: Option<bool>,
}

#[derive(Deserialize, Default)]
pub(crate) struct SelfHoldFields {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) clock_live: Option<bool>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) clock_until: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) delivery_policy: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct DriftReading {
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) undocumented: Option<u64>,
    #[serde(default, deserialize_with = "opt_vec_string")]
    pub(crate) entries: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
pub(crate) struct BootReading {
    #[serde(default, deserialize_with = "vec_lenient")]
    pub(crate) rows: Vec<BootRow>,
}

#[derive(Deserialize, Default)]
pub(crate) struct BootRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) name: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) reason: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct ParkedReading {
    #[serde(default, deserialize_with = "vec_lenient")]
    pub(crate) rows: Vec<ParkedRow>,
}

#[derive(Deserialize, Default)]
pub(crate) struct ParkedRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) key: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) node: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) reason_detail: Option<String>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) age_hours: Option<i64>,
}

#[derive(Deserialize, Default)]
pub(crate) struct SubagentsReading {
    #[serde(default)]
    pub(crate) held_idle: Option<Value>,
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) held: Option<Vec<IdRow>>,
}

#[derive(Deserialize, Default)]
pub(crate) struct IdRow {
    #[serde(default)]
    pub(crate) id: Option<Value>,
}

#[derive(Deserialize, Default)]
pub(crate) struct SkillStale {
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) stale: Option<Vec<StaleRow>>,
}

#[derive(Deserialize, Default)]
pub(crate) struct StaleRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) name: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct AttentionPayload {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) attention: Option<Value>,
}

#[derive(Deserialize, Default)]
pub(crate) struct PromptParkedPayload {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) rows: Option<Value>,
}

#[derive(Deserialize, Default)]
pub(crate) struct PostureReading {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) drifted: Option<Value>,
    #[serde(default)]
    pub(crate) observed: Option<Value>,
}

/// The blueprint reading tail, as `build_data` reads it.
#[derive(Deserialize, Default)]
pub(crate) struct BlueprintTail {
    #[serde(default)]
    pub(crate) plan_ready: Option<Value>,
}

/// The team reading, as the render reads it. `anomalies` rows are a union:
/// a plain string or an object with a `scope` key, so they stay `Value`.
#[derive(Deserialize, Default)]
pub(crate) struct TeamReading {
    #[serde(default)]
    pub(crate) total: Option<Value>,
    #[serde(default)]
    pub(crate) double_ruled: Option<Value>,
    #[serde(default)]
    pub(crate) stale_teamed: Option<Value>,
    #[serde(default)]
    pub(crate) splits: Option<Value>,
    #[serde(default)]
    pub(crate) disagreements: Option<Value>,
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) anomalies: Option<Vec<Value>>,
}

/// The tail of `route_slot::resolve_slot_payload`, as the beat's planned
/// route memo reads it.
#[derive(Deserialize, Default)]
pub(crate) struct RouteTail {
    #[serde(default, deserialize_with = "opt_lenient")]
    pub(crate) candidate: Option<RouteCandidate>,
}

#[derive(Deserialize, Default)]
pub(crate) struct RouteCandidate {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) harness: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) model: Option<String>,
}

/// Parse a top-level array leniently: the element that is not an object
/// reads as the all-empty struct, the way a `.get()` walk reads a scalar
/// row. A non-array reads empty.
pub(crate) fn parse_vec<T: DeserializeOwned + Default>(v: &Value) -> Vec<T> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|e| T::deserialize(e).unwrap_or_default())
                .collect()
        })
        .unwrap_or_default()
}

/// One journalled beat row, as the diff and the render read it.
#[derive(Deserialize, Default)]
pub(crate) struct PrevRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) ts: Option<String>,
    #[serde(default)]
    pub(crate) data: Option<Value>,
}

/// One graph row, as the `--queue` id resolver reads it.
#[derive(Deserialize, Default)]
pub(crate) struct GraphIdRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) id: Option<String>,
}

/// One `blocked_children` row of the beat data, as the render reads it.
#[derive(Deserialize, Default)]
pub(crate) struct BlockedChildRow {
    #[serde(default)]
    pub(crate) node: Option<Value>,
    #[serde(default)]
    pub(crate) session: Option<Value>,
    #[serde(default)]
    pub(crate) age_minutes: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) reason: Option<String>,
}

/// The beat data map, as every in-file reader sees it. Built from the map
/// `build_data` assembles, so the journalled bytes stay exactly as before;
/// only the reads are typed. Fields the file inserts but never reads are not
/// mirrored.
#[derive(Deserialize, Default)]
pub(crate) struct BeatData {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) scope: Option<String>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) open_prs: Option<i64>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) free_claim_no_driver: Option<i64>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) blocked: Option<i64>,
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) blocked_children: Option<Vec<BlockedChildRow>>,
    #[serde(default)]
    pub(crate) owned_active: Option<Value>,
    #[serde(default)]
    pub(crate) live_workers: Option<Value>,
    #[serde(default)]
    pub(crate) oldest_worker_seen: Option<Value>,
    #[serde(default)]
    pub(crate) live_subagents: Option<Value>,
    #[serde(default)]
    pub(crate) capacity_footprint: Option<Value>,
    #[serde(default)]
    pub(crate) capacity_gate: Option<Value>,
    #[serde(default)]
    pub(crate) capacity_gate_cpu: Option<Value>,
    #[serde(default)]
    pub(crate) capacity_lanes: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) capacity_gate_axis: Option<String>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) capacity_disagree: Option<bool>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) unparsed_lines: Option<i64>,
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) refusal_rate: Option<f64>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) load_discounted: Option<bool>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) refusal_rate_rising: Option<bool>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) refusal_rate_trend_unmeasured: Option<bool>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) idle_subagents: Option<u64>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) wake_over: Option<bool>,
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) wake_ratio: Option<f64>,
    #[serde(default)]
    pub(crate) undelivered: Option<Value>,
    #[serde(default)]
    pub(crate) main_ci: Option<Value>,
    #[serde(default, deserialize_with = "opt_vec_string")]
    pub(crate) control_plane_attention: Option<Vec<String>>,
    #[serde(default, deserialize_with = "opt_vec_string")]
    pub(crate) readers_failed: Option<Vec<String>>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) coverage: Option<i64>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) posture_drifted: Option<bool>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) harness: Option<String>,
    #[serde(default)]
    pub(crate) self_hold: Option<Value>,
    #[serde(default, deserialize_with = "opt_vec_string")]
    pub(crate) skill_drift_stale: Option<Vec<String>>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) scope_live_workers: Option<u64>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) scope_idea_children: Option<u64>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) blueprint_slots: Option<u64>,
}

impl BeatData {
    /// The typed mirror of one beat data map. A clone per reader call: the
    /// map is beat-sized, and the reads keep their `as_*` totality.
    pub(crate) fn snapshot(data: &Map<String, Value>) -> BeatData {
        parse(&Value::Object(data.clone()))
    }
}
