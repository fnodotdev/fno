//! Typed reads for the scoreboard renderers.
//!
//! The render functions used to walk the scoreboard fold by string key in
//! every print arm. The views here parse each fold once, at the renderer's
//! entry, and the arms read fields. Every field deserializer mirrors one
//! `Value::as_*` accessor (crate::lenient_de), so a mistyped field reads
//! None instead of failing the parse and one bad field cannot blank its
//! neighbors. Maps and tables the reader keys dynamically (the calibration
//! verdict table, the per-class cost map, the distribution metrics) stay
//! `Value` with the FIELD typed. docs/rust-style.md R1.

use crate::lenient_de::{opt_bool, opt_f64, opt_i64, opt_string, opt_value_present};
use serde::Deserialize;
use serde_json::Value;

/// The main scoreboard view. The stop-cause map and the delivery-classes
/// map are keyed dynamically; they stay `Value`.
#[derive(Deserialize, Default)]
pub(crate) struct MainView {
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) since_days: Option<i64>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) state: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) coverage: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) event_coverage: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) emission_failures: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) shipped_nodes: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) shipped_by_terminal: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) delivery_classes: Option<Value>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) merged_nodes_without_ledger_row: Option<i64>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) project_scope: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) stop_cause: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) spend: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) autonomy: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) survival: Option<Value>,
}

/// The shared coverage block every scoreboard view carries.
#[derive(Deserialize, Default)]
pub(crate) struct CoverageTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) rows: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) provider: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) model: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) effort: Option<Value>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) termination_reason_pct: Option<i64>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) node_linkage_pct: Option<i64>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) attributed_pct: Option<i64>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) loop_join_pct: Option<i64>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) transcript_pct: Option<i64>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) outcome_tracked_pct: Option<i64>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) ci_unparsed: Option<i64>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) planned_rows: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) joined_pct: Option<Value>,
}

/// One retrospective row of the lanes view.
#[derive(Deserialize, Default)]
pub(crate) struct RetroRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) provider: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) model: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) effort: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) size: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) runs: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) ok_pct: Option<Value>,
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) wall_minutes: Option<f64>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) carveouts_filed: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) sample_state: Option<String>,
}

/// One live lane row of the lanes view. `cap` and `headroom` distinguish a
/// present-null from an absent field (both render n/a; a wrong type renders
/// the string form), so they stay presence-preserving.
#[derive(Deserialize, Default)]
pub(crate) struct LiveRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) provider: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) model: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) effort: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) occupancy: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) cap: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) headroom: Option<Value>,
}

/// The lanes view.
#[derive(Deserialize, Default)]
pub(crate) struct LanesView {
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) since_days: Option<i64>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) coverage: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) retrospective: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) live: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) rate_limited: Option<Value>,
}

/// The journal-integrity block of the main view.
#[derive(Deserialize, Default)]
pub(crate) struct EventCoverageTail {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) complete: Option<bool>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) malformed_lines: Option<i64>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) unreadable_paths: Option<i64>,
}

/// The touch-emission block of the main view.
#[derive(Deserialize, Default)]
pub(crate) struct EmissionTail {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) available: Option<bool>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) count: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) measured_since: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) measured_at: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) reason: Option<String>,
}

/// The project-scope block of the main view.
#[derive(Deserialize, Default)]
pub(crate) struct ScopeTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) project: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) nodes: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) unattributed_rows: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) other_project_rows: Option<Value>,
}

/// The spend split of the main view.
#[derive(Deserialize, Default)]
pub(crate) struct SpendTail {
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) ship_terminal_usd: Option<f64>,
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) wedge_terminal_usd: Option<f64>,
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) other_usd: Option<f64>,
}

/// The autonomy block of the main view.
#[derive(Deserialize, Default)]
pub(crate) struct AutonomyTail {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) available: Option<bool>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) touches_per_shipped_node: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) touches: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) shipped_nodes: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) reason: Option<String>,
}

/// The survival block of the main view.
#[derive(Deserialize, Default)]
pub(crate) struct SurvivalTail {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) available: Option<bool>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) pending: Option<i64>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) rate_pct: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) survived: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) shipped_nodes: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) reason: Option<String>,
}

/// The calibration view. The verdict table and the excluded map are keyed
/// dynamically; they stay `Value`.
#[derive(Deserialize, Default)]
pub(crate) struct CalibrationView {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) excluded: Option<Value>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) unattributed: Option<i64>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) state: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) n: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) need: Option<Value>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) untimed_outcomes: Option<i64>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) table: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) false_positive: Option<Value>,
}

/// The false-positive block of the calibration view.
#[derive(Deserialize, Default)]
pub(crate) struct FalsePositiveTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) count: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) of_pass: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) rate_pct: Option<Value>,
}

/// One by-skill row.
#[derive(Deserialize, Default)]
pub(crate) struct SkillRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) skill: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) version: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) revert_rate_pct: Option<Value>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) runs: Option<i64>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) ship_rate_pct: Option<i64>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) touches_per_run: Option<Value>,
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) cost_per_run: Option<f64>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) method: Option<String>,
}

/// The by-skill view.
#[derive(Deserialize, Default)]
pub(crate) struct BySkillView {
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) since_days: Option<i64>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) state: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) coverage: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) rows: Option<Value>,
}

/// The efficiency view. The per-class and cost maps are keyed dynamically;
/// their VALUES get tails.
#[derive(Deserialize, Default)]
pub(crate) struct EfficiencyView {
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) since_days: Option<i64>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) state: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) coverage: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) per_outcome_class: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) plan_vs_build_cost: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) distribution: Option<Value>,
}

/// One per-outcome-class entry.
#[derive(Deserialize, Default)]
pub(crate) struct ClassTail {
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) n: Option<i64>,
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) spend_usd: Option<f64>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) median_tokens: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) median_fires: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) median_duration_min: Option<Value>,
}

/// One plan-vs-build cost entry.
#[derive(Deserialize, Default)]
pub(crate) struct CostTail {
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) plan_usd: Option<f64>,
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) build_usd: Option<f64>,
}

/// One distribution entry.
#[derive(Deserialize, Default)]
pub(crate) struct DistTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) median: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) p90: Option<Value>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) n: Option<i64>,
}

/// The plan-fidelity view.
#[derive(Deserialize, Default)]
pub(crate) struct PlanFidelityView {
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) since_days: Option<i64>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) state: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) coverage: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) results: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) context_comparison: Option<Value>,
}

/// One plan-fidelity result row.
#[derive(Deserialize, Default)]
pub(crate) struct ResultRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) session_id: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) pr_number: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) status: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) ac_coverage: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) data_model_surprise: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) scope_drift: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) probes: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) context_outcome_trace: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) outcome: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) deviation_load: Option<Value>,
}

/// The AC coverage block of a result row.
#[derive(Deserialize, Default)]
pub(crate) struct AcTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) verified: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) total: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) pct: Option<Value>,
}

/// The scope-drift block of a result row.
#[derive(Deserialize, Default)]
pub(crate) struct ScopeDriftTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) unplanned: Option<Value>,
}

/// The probes block of a result row.
#[derive(Deserialize, Default)]
pub(crate) struct ProbesTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) passed: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) declared: Option<Value>,
}

/// The context-trace block of a result row.
#[derive(Deserialize, Default)]
pub(crate) struct TraceTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) context: Option<Value>,
}

/// The context block of a trace.
#[derive(Deserialize, Default)]
pub(crate) struct CtxTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) bytes: Option<Value>,
}

/// The context-comparison block of the plan-fidelity view.
#[derive(Deserialize, Default)]
pub(crate) struct ComparisonTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) label: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) reason: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) claim: Option<Value>,
}
