//! Typed reads for the mail-threads projection.
//!
//! The projection picked registry rows, bus lines, turns and its own
//! participant rows apart by string key. The views here parse each row once
//! and the readers read fields; every read stays total, so a field of the
//! wrong shape reads None the way the `.get()` walk did. Fields read whole
//! stay presence-preserving `Value`; string-only readers take the typed
//! string. docs/rust-style.md R1.

use crate::lenient_de::{opt_bool, opt_string, opt_u64, opt_value_present};
use serde::Deserialize;
use serde_json::Value;

/// One registry row.
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
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) aliases: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) harness: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) model: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) effort: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) node: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) liveness: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) status: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) role_scope: Option<String>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) role_level: Option<u64>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) created_at: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) exited_at: Option<String>,
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
    pub(crate) expires_at: Option<String>,
}

/// One bus line or projected row.
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
    pub(crate) from_session: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) to: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) to_key: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) from_key: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) ts: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) body: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) subject: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) in_reply_to: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) delivery: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) how: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) reason: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) expires: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) thread: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) chat_id: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) last_ts: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) meta: Option<Value>,
}

/// One typed header turn.
#[derive(Deserialize, Default)]
pub(crate) struct TurnTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) id: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) sender: Option<String>,
}

/// A participant row the projection reads back.
#[derive(Deserialize, Default)]
pub(crate) struct ParticipantTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) key: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) name: Option<String>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) live: Option<bool>,
}

/// The projection document's system block.
#[derive(Deserialize, Default)]
pub(crate) struct SystemTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) system: Option<Value>,
}
