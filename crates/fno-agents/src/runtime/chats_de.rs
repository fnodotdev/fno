//! Typed reads for the chat store.
//!
//! The chat store's bus-line readers picked envelope fields by string key.
//! The views here parse each line once and the readers read fields; every
//! read stays total, so a wrong shape reads None the way the `.get()` walk
//! did. `meta` stays presence-preserving because receipt rows distinguish a
//! present-null meta from an absent one. docs/rust-style.md R1.

use crate::lenient_de::{opt_string, opt_value_present};
use serde::Deserialize;
use serde_json::Value;

/// One registry row, as the participant-key join reads it. The or_else
/// chain falls through on ANY non-string (including a present-null), which
/// the typed string fields mirror exactly.
#[derive(Deserialize, Default)]
pub(crate) struct RegistryRowTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) fno_id: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) harness_session_id: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) session_id: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) name: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) short_id: Option<String>,
}

/// The registry document.
#[derive(Deserialize, Default)]
pub(crate) struct RegistryTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) agents: Option<Value>,
}

/// The `meta` block of a bus line.
#[derive(Deserialize, Default)]
pub(crate) struct MetaTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) scope: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) session: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) reason: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) to_session: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) subject: Option<String>,
}

/// One bus line. `type` rides presence-preserving because the journal
/// filter compares the whole value.
#[derive(Deserialize, Default)]
pub(crate) struct BusLineTail {
    #[serde(rename = "type", default, deserialize_with = "opt_value_present")]
    pub(crate) ty: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) id: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) kind: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) from: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) from_name: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) from_session: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) to: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) to_session: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) to_key: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) ts: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) meta: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) delivery: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) body: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) in_reply_to: Option<String>,
}

/// One delivery (receipt) line, as the thread renderer reads it.
#[derive(Deserialize, Default)]
pub(crate) struct DeliveryTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) id: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) how: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) ts: Option<String>,
}

/// One legacy `<fno_mail>` tag pair, as the envelope rewrite reads it.
#[derive(Deserialize, Default)]
pub(crate) struct LegacyTagTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) from: Option<String>,
}

/// The `stage` envelope fields, in both spellings the record vocabulary
/// accepts (the Python send paths hold msg_id/sender/recipient; the
/// envelope says id/from/to). The or-paired fields stay presence-preserving
/// so a mistyped `id` still short-circuits the `msg_id` fallback the way
/// the raw `.get().or_else()` walk did.
#[derive(Deserialize, Default)]
pub(crate) struct StageTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) msg_id: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) from: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) sender: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) to: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) recipient: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) body: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) to_session: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) to_harness: Option<String>,
}
