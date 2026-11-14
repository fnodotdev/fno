//! Typed reads for the route-slot payloads.
//!
//! The slot walk, its legs and the audit used to walk their JSON by string
//! key. The views here parse each payload once, at a reader's entry, and the
//! readers read fields. Every field deserializer mirrors one `Value::as_*`
//! accessor (crate::lenient_de), so a mistyped field reads None instead of
//! failing the parse and one bad field cannot blank its neighbors. Maps the
//! reader keys at runtime (capacity by harness, accounts by account) stay
//! `Value`; the FIELD is typed, the runtime keying is not. A payload that is
//! not even an object parses as the all-empty view, the same shape a
//! `.get()` walk reads off a scalar. docs/rust-style.md R1.

use crate::lenient_de::{
    opt_bool, opt_f64, opt_string, opt_struct, opt_u64, opt_value_present, opt_vec_string,
};
use serde::Deserialize;
use serde_json::Value;

/// The slot payload, as the walk and its legs read it. Fields a sibling
/// module consumes as `Value` (profile tables, declared rows, vendor maps)
/// ride as `Value` with a typed presence; their literal-key reads elsewhere
/// get their own tail view.
#[derive(Deserialize, Default)]
pub(crate) struct SlotPayload {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) mode: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) op: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) rung_base: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) capacity: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) inventory: Option<Value>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) node: Option<NodeTail>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) gate_bypassed: Option<bool>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) explicit_lane: Option<bool>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) substrate: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) permission_mode: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) constrain_harness: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) thread_seatable: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) policy: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) lanes_raw: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) work_command: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) slot_by_command: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) explicit_model_value: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) explicit_route_value: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) explicit_vendor_value: Option<String>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) model_occupied: Option<bool>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) declared_rows: Option<Value>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) capacity_refresh: Option<bool>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) capacity_refresh_timeout_secs: Option<u64>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) utc_now_hour: Option<u64>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) account_record_vendors: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) known_harnesses: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) harness_installed: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) effort_ok: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) profile: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) role: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) protected_role: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) protected_role_floor: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) tier: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) provider: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) path: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) event: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) vendor_caps: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) vendor_counts: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) vendor_count_errors: Option<Value>,
    #[serde(default, deserialize_with = "opt_vec_string")]
    pub(crate) argv: Option<Vec<String>>,
}

/// The `node` tail: difficulty, priority and the nested plan_path.
#[derive(Deserialize, Default)]
pub(crate) struct NodeTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) difficulty: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) priority: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) plan_path: Option<String>,
}

/// The `policy` block: the two fields the strict gate and the verdict read.
#[derive(Deserialize, Default)]
pub(crate) struct PolicyTail {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) enforce_inventory: Option<bool>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) operator_access: Option<String>,
}

/// The `profile` tail: the difficulty overlay table. The policy words
/// (`on_exhausted` and friends) the reader keys dynamically, so they stay on
/// the raw profile `Value`.
#[derive(Deserialize, Default)]
pub(crate) struct ProfileTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) by_difficulty: Option<Value>,
}

/// The `inventory` tail of the slot payload.
#[derive(Deserialize, Default)]
pub(crate) struct InventoryTail {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) declared: Option<bool>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) rows: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) objective: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) prefer_harness: Option<String>,
}

/// One harness's capacity detail: the mapping `runtime_capacity` produces.
/// A bare state string is NOT this view (the reader discriminates first).
/// `accounts`, `sources`, `observed_at` and `evidence` are keyed at runtime
/// (by harness and account), so they ride as `Value`.
#[derive(Deserialize, Default)]
pub(crate) struct CapacityDetail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) state: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) window: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) accounts: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) sources: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) observed_at: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) evidence: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) resets: Option<Value>,
}

/// One strict-routing slot entry of `slot_by_command`.
#[derive(Deserialize, Default)]
pub(crate) struct SlotEntry {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) rung_base: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) profile: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) lanes_raw: Option<Value>,
}

/// One `by_difficulty` overlay table entry.
#[derive(Deserialize, Default)]
pub(crate) struct OverlayTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) lanes: Option<Value>,
}

/// The lanes-attribution answer of `route_gather`.
#[derive(Deserialize, Default)]
pub(crate) struct LanesSource {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) from: Option<String>,
    #[serde(default, deserialize_with = "opt_vec_string")]
    pub(crate) overrides: Option<Vec<String>>,
}

/// The walk's decision envelope, as the readout and the journal read it.
#[derive(Deserialize, Default)]
pub(crate) struct WalkOut {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) candidate: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) verdict: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) reason_kind: Option<String>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) refusal_terminal: Option<RefusalTerminal>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) exhausted_payload: Option<ExhaustedPayload>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) chain: Option<Value>,
}

/// The named refusal's terminal line.
#[derive(Deserialize, Default)]
pub(crate) struct RefusalTerminal {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) class: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) text: Option<String>,
}

/// The queue terminal's structured payload.
#[derive(Deserialize, Default)]
pub(crate) struct ExhaustedPayload {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) reason: Option<String>,
}

/// The candidate tail the readout and the states leg read.
#[derive(Deserialize, Default)]
pub(crate) struct CandidateTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) lane_rung: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) lane: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) policy: Option<Value>,
}
/// One declared inventory row, as `inv_row` reads it past the string fields
/// the `row_value` helper keys dynamically.
#[derive(Deserialize, Default)]
pub(crate) struct InvRowTail {
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) percentile: Option<f64>,
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) cost_per_mtok_in: Option<f64>,
}

/// The audit snapshot: config facts plus the sessions inside the window.
#[derive(Deserialize, Default)]
pub(crate) struct AuditSnapshot {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) policy: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) fingerprint: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) sessions: Option<Value>,
}

/// One registry session row of the audit snapshot.
#[derive(Deserialize, Default)]
pub(crate) struct SessionTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) session_id: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) name: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) account: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) receipt_fingerprint: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) model: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) model_basis: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) requested_model: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) harness: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) view_records: Option<Value>,
}

/// One operator-view decision record.
#[derive(Deserialize, Default)]
pub(crate) struct ViewRecordTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) subject: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) lifecycle: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) decision: Option<String>,
}

/// The parsed decision text of an operator-view record.
#[derive(Deserialize, Default)]
pub(crate) struct ViewBodyTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) view: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) fingerprint: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) session_id: Option<String>,
}

/// The config-facts payload the audit snapshot loader receives.
#[derive(Deserialize, Default)]
pub(crate) struct ConfigFacts {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) fingerprint: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) policy: Option<Value>,
}

/// One journalled event row: envelope rows carry `type`, imported flat rows
/// carry `kind`; the rest is the flattened decision/event vocabulary.
#[derive(Deserialize, Default)]
pub(crate) struct EventRow {
    #[serde(default, rename = "type", deserialize_with = "opt_value_present")]
    pub(crate) ty: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) kind: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) ts: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) data: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) _event_type: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) target_decision_id: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) subject: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) supersedes: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) decision: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) decision_id: Option<String>,
}
