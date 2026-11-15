//! Typed reads for the triage health folds.
//!
//! The health folds picked node rows, journal events, config blocks and the
//! evals report apart by string key. The views here parse each row once and
//! the folds read fields; every read stays total, so a field of the wrong
//! shape reads None the way the `.get()` walk did. Fields read whole
//! (passthrough projections, presence/null probes) stay presence-preserving
//! `Value`; string-only readers take the typed string. docs/rust-style.md R1.

use crate::lenient_de::{opt_bool, opt_f64, opt_i64, opt_string, opt_value_present};
use serde::Deserialize;
use serde_json::Value;

/// One backlog node or journal event row. The two shapes share enough
/// fields that one lenient view serves both; a shape that lacks a field
/// reads None, the same answer the `.get()` walk gave.
#[derive(Deserialize, Default)]
pub(crate) struct NodeTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) id: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) title: Option<Value>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) pr_number: Option<i64>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) pr_url: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) additional_prs: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) completed_at: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) data: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) payload: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) node_id: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) _resolved_cwd: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) cwd: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) deferred_at: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) deferred_reason: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) blocked_by: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) blocked_reason: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) status: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) created_at: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) plan_path: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) project: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) cost_sessions: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) ownership_defect: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) orphan_ok: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) parent: Option<String>,
    #[serde(rename = "type", default, deserialize_with = "opt_string")]
    pub(crate) type_: Option<String>,
}

/// One `additional_prs` row.
#[derive(Deserialize, Default)]
pub(crate) struct PrRefTail {
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) pr_number: Option<i64>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) url: Option<String>,
}

/// One `gh pr list --json number,state` row.
#[derive(Deserialize, Default)]
pub(crate) struct GhRowTail {
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) number: Option<i64>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) state: Option<String>,
}

/// One batch-lane event's data block.
#[derive(Deserialize, Default)]
pub(crate) struct RollupDataTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) shipped: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) detail: Option<String>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) member_count: Option<i64>,
}

/// One shipped batch row.
#[derive(Deserialize, Default)]
pub(crate) struct MemberRowTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) members: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) action: Option<String>,
}

/// The evals summary payload row_count gate plus passthrough fields.
#[derive(Deserialize, Default)]
pub(crate) struct PayloadTail {
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) row_count: Option<i64>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) regression_pass_rate: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) flake_count: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) regression_alarm: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) regressed: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) age_days: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) stale: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) never_ran: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) totals: Option<Value>,
}

/// The merged config doc's health-monitor block.
#[derive(Deserialize, Default)]
pub(crate) struct HmDocTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) health_monitor: Option<Value>,
}

/// The health-monitor config block.
#[derive(Deserialize, Default)]
pub(crate) struct HmConfigTail {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) enabled: Option<bool>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) notifications: Option<Value>,
}

/// The notifications block.
#[derive(Deserialize, Default)]
pub(crate) struct NotifTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) surfaces: Option<Value>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) throttle_minutes: Option<i64>,
}

/// The health report the threshold and notify arms read.
#[derive(Deserialize, Default)]
pub(crate) struct ReportTail {
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) orphan_feature_rate: Option<f64>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) evals: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) routing: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) triage_metrics: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) batch_verdict: Option<String>,
}

/// One cost-session row.
#[derive(Deserialize, Default)]
pub(crate) struct CostRowTail {
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) cost_usd: Option<f64>,
}

/// One history line's timestamp.
#[derive(Deserialize, Default)]
pub(crate) struct HistoryTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) timestamp: Option<String>,
}

/// One ownership-defect block.
#[derive(Deserialize, Default)]
pub(crate) struct DefectTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) kind: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) holder: Option<Value>,
}
