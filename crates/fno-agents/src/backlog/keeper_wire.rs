//! The store keeper's wire envelope, typed once for both sides of the
//! socket. A request is `{id, method, params}`; a reply is
//! `{id, ok: true, result}` or `{id, ok: false, error: {kind, message}}`.
//! The keeper (graph_keeper, store_exec) and every client (the fno
//! store_client, plan_doc, script_door) build and read these types, so a
//! misspelled key cannot compile and a refusal keeps its kind.
//!
//! The fno crate cannot depend on fno-agents, so build.rs projects this file
//! into `crates/fno/src/keeper_wire.rs`. It imports only serde and
//! serde_json for that reason. Per-method `params` stay JSON: each handler
//! owns its own params shape.

use serde::de::Deserializer;
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Frame tag of a client request.
pub const TAG_REQUEST: u8 = 1;
/// Frame tag of a keeper reply.
pub const TAG_RESPONSE: u8 = 4;

macro_rules! keeper_methods {
    ($($variant:ident => $wire:literal),+ $(,)?) => {
        /// One store method. `Other` carries a word this build does not
        /// serve, so the keeper answers it with its own refusal text.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum KeeperMethod {
            $($variant,)+
            Other(String),
        }

        impl KeeperMethod {
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $wire,)+
                    Self::Other(word) => word,
                }
            }

            pub fn parse(word: &str) -> Self {
                match word {
                    $($wire => Self::$variant,)+
                    other => Self::Other(other.to_owned()),
                }
            }
        }
    };
}

keeper_methods! {
    Read => "read",
    ReadStrict => "read_strict",
    ReadIds => "read_ids",
    PlanRefs => "plan_refs",
    Begin => "begin",
    Commit => "commit",
    CommitRows => "commit_rows",
    WriteStatus => "write_status",
    ExportStatus => "export_status",
    KeeperScan => "keeper_scan",
    Op => "op",
    Api => "api",
    ReadArchive => "read_archive",
    ReadFile => "read_file",
    Defaults => "defaults",
    Recompute => "recompute",
    Ready => "ready",
    Rules => "rules",
    PlanDocs => "plan_docs",
    Overlay => "overlay",
    SettleEdges => "settle_edges",
    NormalizePlanPath => "normalize_plan_path",
    CanonicalFieldOrder => "canonical_field_order",
    ScoreboardClassify => "scoreboard_classify",
    ScoreboardByProvider => "scoreboard_by_provider",
    ScoreboardPlanFidelity => "scoreboard_plan_fidelity",
    ScoreboardEscalation => "scoreboard_escalation",
    EffectiveCommand => "effective_command",
    EffectiveVerb => "effective_verb",
    PureOp => "pure_op",
}

impl KeeperMethod {
    /// The methods that write the graph. A write carries a `request_id` the
    /// keeper's write ledger records.
    pub fn is_write(&self) -> bool {
        matches!(self, Self::Commit | Self::CommitRows | Self::Op | Self::Api)
    }
}

impl Serialize for KeeperMethod {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// One store request.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KeeperRequest {
    pub id: u64,
    pub method: KeeperMethod,
    pub params: Value,
}

impl KeeperRequest {
    pub fn new(id: u64, method: &str, params: Value) -> Self {
        Self {
            id,
            method: KeeperMethod::parse(method),
            params,
        }
    }
}

/// The request read is lenient field by field, as the keeper has always
/// been: a missing or off-type `id` reads 0, a missing or off-type `method`
/// reads as the empty word the keeper refuses by name, and a missing
/// `params` reads null. Only a payload that is not a JSON object fails.
impl<'de> Deserialize<'de> for KeeperRequest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            id: Value,
            #[serde(default)]
            method: Value,
            #[serde(default)]
            params: Value,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(Self {
            id: raw.id.as_u64().unwrap_or(0),
            method: KeeperMethod::parse(raw.method.as_str().unwrap_or("")),
            params: raw.params,
        })
    }
}

/// Why the keeper refused. `Other` keeps a kind this build does not know, so
/// a newer keeper's refusal still reads as a refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    Corrupt,
    Unreadable,
    MalformedRoot,
    LockTimeout,
    Conflict,
    EmptyFieldUpdate,
    Invalid,
    ClaimsUnavailable,
    Sqlite,
    Io,
    MalformedFrame,
    Internal,
    Busy,
    Other(String),
}

impl ErrorKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Corrupt => "corrupt",
            Self::Unreadable => "unreadable",
            Self::MalformedRoot => "malformed_root",
            Self::LockTimeout => "lock_timeout",
            Self::Conflict => "conflict",
            Self::EmptyFieldUpdate => "empty_field_update",
            Self::Invalid => "invalid",
            Self::ClaimsUnavailable => "claims_unavailable",
            Self::Sqlite => "sqlite",
            Self::Io => "io",
            Self::MalformedFrame => "malformed_frame",
            Self::Internal => "internal",
            Self::Busy => "busy",
            Self::Other(word) => word,
        }
    }

    pub fn parse(word: &str) -> Self {
        match word {
            "corrupt" => Self::Corrupt,
            "unreadable" => Self::Unreadable,
            "malformed_root" => Self::MalformedRoot,
            "lock_timeout" => Self::LockTimeout,
            "conflict" => Self::Conflict,
            "empty_field_update" => Self::EmptyFieldUpdate,
            "invalid" => Self::Invalid,
            "claims_unavailable" => Self::ClaimsUnavailable,
            "sqlite" => Self::Sqlite,
            "io" => Self::Io,
            "malformed_frame" => Self::MalformedFrame,
            "internal" => Self::Internal,
            "busy" => Self::Busy,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl Default for ErrorKind {
    fn default() -> Self {
        Self::Other(String::new())
    }
}

impl Serialize for ErrorKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::parse(&String::deserialize(deserializer)?))
    }
}

/// The refusal a reply carries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeeperError {
    pub kind: ErrorKind,
    pub message: String,
}

/// One keeper reply: the method's result, or the refusal.
#[derive(Debug, Clone, PartialEq)]
pub struct KeeperReply {
    pub id: u64,
    pub outcome: Result<Value, KeeperError>,
}

impl KeeperReply {
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            id,
            outcome: Ok(result),
        }
    }

    pub fn err(id: u64, kind: ErrorKind, message: String) -> Self {
        Self {
            id,
            outcome: Err(KeeperError { kind, message }),
        }
    }

    pub fn is_ok(&self) -> bool {
        self.outcome.is_ok()
    }
}

impl Serialize for KeeperReply {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("id", &self.id)?;
        match &self.outcome {
            Ok(result) => {
                map.serialize_entry("ok", &true)?;
                map.serialize_entry("result", result)?;
            }
            Err(error) => {
                map.serialize_entry("ok", &false)?;
                map.serialize_entry("error", error)?;
            }
        }
        map.end()
    }
}

/// A reply without `ok: true` is a refusal, whatever else it carries; a
/// refusal with no `error` object reads as a default (empty) one.
impl<'de> Deserialize<'de> for KeeperReply {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            id: u64,
            #[serde(default)]
            ok: bool,
            #[serde(default)]
            result: Value,
            #[serde(default)]
            error: Option<KeeperError>,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(Self {
            id: raw.id,
            outcome: if raw.ok {
                Ok(raw.result)
            } else {
                Err(raw.error.unwrap_or_default())
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_reply_shape_serializes_to_the_bytes_it_replaced() {
        let cases = [
            (
                KeeperReply::ok(7, json!({"entries": [{"id": "x-a"}], "version": "v1"})),
                json!({"id": 7, "ok": true, "result": {"entries": [{"id": "x-a"}], "version": "v1"}}),
            ),
            (
                KeeperReply::ok(0, Value::Null),
                json!({"id": 0, "ok": true, "result": null}),
            ),
            (
                KeeperReply::err(
                    3,
                    ErrorKind::Conflict,
                    "version conflict: the graph changed".into(),
                ),
                json!({"id": 3, "ok": false,
                       "error": {"kind": "conflict", "message": "version conflict: the graph changed"}}),
            ),
            (
                KeeperReply::err(0, ErrorKind::MalformedFrame, "request is not JSON".into()),
                json!({"id": 0, "ok": false,
                       "error": {"kind": "malformed_frame", "message": "request is not JSON"}}),
            ),
        ];
        for (reply, legacy) in cases {
            let bytes = serde_json::to_vec(&reply).unwrap();
            assert_eq!(bytes, serde_json::to_vec(&legacy).unwrap());
            let back: KeeperReply = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(back, reply);
        }
    }

    #[test]
    fn an_unknown_error_kind_reads_as_other_not_a_parse_failure() {
        let reply: KeeperReply = serde_json::from_value(
            json!({"id": 1, "ok": false, "error": {"kind": "quota_spent", "message": "m"}}),
        )
        .unwrap();
        assert_eq!(
            reply.outcome,
            Err(KeeperError {
                kind: ErrorKind::Other("quota_spent".into()),
                message: "m".into()
            })
        );
        assert_eq!(
            serde_json::to_value(&reply).unwrap()["error"]["kind"],
            "quota_spent"
        );
    }

    #[test]
    fn every_method_word_round_trips_and_an_unknown_one_is_kept() {
        let req: KeeperRequest =
            serde_json::from_value(json!({"id": 4, "method": "commit_rows", "params": {"a": 1}}))
                .unwrap();
        assert_eq!(req.method, KeeperMethod::CommitRows);
        assert!(req.method.is_write());
        assert_eq!(
            serde_json::to_value(&req).unwrap(),
            json!({"id": 4, "method": "commit_rows", "params": {"a": 1}})
        );
        let odd: KeeperRequest = serde_json::from_value(json!({"method": "raed"})).unwrap();
        assert_eq!(
            (odd.id, odd.method.as_str(), odd.params),
            (0, "raed", Value::Null)
        );
        let off: KeeperRequest = serde_json::from_value(json!({"id": "x", "method": 5})).unwrap();
        assert_eq!(
            (off.id, off.method),
            (0, KeeperMethod::Other(String::new()))
        );
    }
}
