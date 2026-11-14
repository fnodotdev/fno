//! Typed reads for the delivery evaluator.
//!
//! The evaluator used to pick required fields off event and fact maps by
//! string key. The views here parse each map once and the validators read
//! fields; every read stays total, so a field of the wrong shape reads
//! None - the same answer the `.get()` walk gave - and the validator's own
//! rejection (required_field_error / the `invalid` closure) fires exactly
//! where it did. Fields stay presence-preserving `Value` where the reader
//! distinguishes present-null from absent or compares whole values;
//! string-only readers take the typed string. docs/rust-style.md R1.

use crate::lenient_de::{opt_string, opt_value_present};
use serde::Deserialize;
use serde_json::Value;

/// The plan frontmatter mapping, as the delivery gate reads it. The
/// validator type-matches `node`/`claims`/`status`/`created`, so those stay
/// presence-preserving `Value`.
#[derive(Deserialize, Default)]
pub(crate) struct MappingTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) completion: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) company_work: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) node: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) claims: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) status: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) created: Option<Value>,
}

/// The company_work declaration block.
#[derive(Deserialize, Default)]
pub(crate) struct CompanyWorkTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) work_order: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) deliverables: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) effects: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) evidence: Option<Value>,
}

/// The `work_order` declaration of company_work.
#[derive(Deserialize, Default)]
pub(crate) struct WorkOrderTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) node_id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) attempt_id: Option<Value>,
}

/// One deliverable declaration.
#[derive(Deserialize, Default)]
pub(crate) struct DeliverableTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) kind: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) work_order_id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) attempt_id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) required_evidence_ids: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) effect_id: Option<Value>,
}

/// One effect declaration.
#[derive(Deserialize, Default)]
pub(crate) struct EffectTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) work_order_id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) attempt_id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) effect_class: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) destination: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) idempotency_key: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) approval_id: Option<Value>,
}

/// One evidence declaration.
#[derive(Deserialize, Default)]
pub(crate) struct EvidenceTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) work_order_id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) attempt_id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) subject_kind: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) subject_id: Option<Value>,
}

/// The `DeliveryEvidenceFact` payload's top-level map. The exact-key-set
/// check stays on the raw map; the field reads come off this view.
#[derive(Deserialize, Default)]
pub(crate) struct FactTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) evidence: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) version: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) observed_at: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) fresh_until: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) producer: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) source_revision: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) fact_revision: Option<Value>,
}

/// The `evidence` block of a fact.
#[derive(Deserialize, Default)]
pub(crate) struct FactEvidenceTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) work_order_id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) attempt_id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) subject_kind: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) subject_id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) result: Option<Value>,
}

/// The event envelope: `type` needs presence fidelity (a present-null type
/// must not fall through like a string), so it stays `Value`.
#[derive(Deserialize, Default)]
pub(crate) struct EventTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) ty: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) ts: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) source: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) data: Option<Value>,
}

/// The `data` map of the producer events and diagnostics, as the evaluator
/// reads it. Whole-value comparisons and raw-nonempty reads stay `Value`.
#[derive(Deserialize, Default)]
pub(crate) struct ProducerDataTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) work_order_id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) attempt_id: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) event_id: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) request_digest: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) effect_id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) idempotency_key: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) evidence: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) producer: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) state: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) previous_state: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) deciding_principal_id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) decision: Option<Value>,
}

/// One requirement row the evaluator composes, as the aggregator reads it.
#[derive(Deserialize, Default)]
pub(crate) struct AggregateRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) result: Option<String>,
}

/// The evaluator's response envelope, as the human renderer reads it.
#[derive(Deserialize, Default)]
pub(crate) struct ResponseTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) verdict: Option<Value>,
}
