//! The serve HTTP surface for each opencode contract.
//!
//! 1.x and 2.x speak different route sets and envelopes (measured on opencode
//! `origin/dev`, cli 2.2.7):
//!
//! | need            | 1.x                          | 2.x                                        |
//! |-----------------|------------------------------|--------------------------------------------|
//! | health          | `GET /global/health` `{healthy,version}` | `GET /api/info` `{version,pid,...}`, a 200 is the signal |
//! | session         | `GET /session/:id` bare      | `GET /api/session/:id` `{data}`            |
//! | messages        | `GET /session/:id/message` `[{info,parts}]` | `GET /api/session/:id/message` `{data,cursor}`, newest first, `type` not `role` |
//! | archive         | `PATCH {time:{archived:ms}}` | `PATCH {archived:true}` answers 204        |
//! | turn            | `run --attach` writer        | `POST /prompt {text}` or `POST /command {name,text}` |
//!
//! Everything contract-specific lives here, so the lane code above reads one
//! typed shape and never branches on a JSON layout.

use serde::Deserialize;

use crate::opencode_install::OpencodeContract;

/// Why one serve exchange could not be judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeError {
    /// The request never got an answer, or a non-HTTP seam refused.
    Transport(String),
    /// The serve answered a status the route does not accept.
    Http {
        route: String,
        status: u16,
        reason: String,
    },
    /// The body was not JSON.
    Parse(String),
    /// The JSON lacked the envelope or field the contract names.
    Shape(&'static str),
    /// A model id the 2.x ref cannot carry.
    Model(String),
    /// The recorded serve state is missing or unreadable.
    State(String),
}

impl std::fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServeError::Transport(why) | ServeError::State(why) => f.write_str(why),
            ServeError::Http {
                route,
                status,
                reason,
            } => write!(f, "{route} answered {status}: {reason}"),
            ServeError::Parse(why) => write!(f, "body parse: {why}"),
            ServeError::Shape(what) => f.write_str(what),
            ServeError::Model(model) => write!(f, "opencode model {model:?} is not provider/model"),
        }
    }
}

impl From<String> for ServeError {
    fn from(why: String) -> Self {
        ServeError::Transport(why)
    }
}

impl From<ServeError> for String {
    fn from(error: ServeError) -> Self {
        error.to_string()
    }
}

/// One stored message, parsed once from either contract's readback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeMessage {
    pub id: String,
    /// `user`, `assistant`, or the 2.x type of any other stored row.
    pub role: String,
    /// The first text part, if the message carries one.
    pub text: Option<String>,
    /// The model id the serve recorded for an assistant message.
    pub model: Option<String>,
    /// False while a 2.x assistant message still streams (`time.completed`
    /// absent). 1.x rows always read true, the behavior the lane shipped with.
    pub complete: bool,
}

/// The 2.x message page size fno asks for: the route's ceiling. The high-water
/// diff only needs the newest rows, and 2.x answers newest first.
const V2_MESSAGE_PAGE: u32 = 200;

pub(super) fn health_path(contract: OpencodeContract) -> &'static str {
    match contract {
        OpencodeContract::V1 => "/global/health",
        OpencodeContract::V2 => "/api/info",
    }
}

/// The reported version when the health answer is healthy, else `None`. 1.x
/// says so with `healthy: true`. 2.x has no such field: a 200 carrying a
/// string `version` is the healthy answer.
pub(super) fn healthy_version(
    contract: OpencodeContract,
    status: u16,
    body: &str,
) -> Option<String> {
    if status != 200 {
        return None;
    }
    let health: HealthBody = serde_json::from_str(body).ok()?;
    match contract {
        OpencodeContract::V1 => health.healthy.then(|| health.version.unwrap_or_default()),
        OpencodeContract::V2 => health.version,
    }
}

#[derive(Deserialize)]
struct HealthBody {
    #[serde(default)]
    healthy: bool,
    #[serde(default)]
    version: Option<String>,
}

/// The 2.x `{data: ...}` envelope.
#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}

/// The session fields the lane reads, from either contract.
#[derive(Deserialize, Default)]
struct SessionView {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    time: Option<SessionTime>,
    #[serde(default)]
    model: Option<ModelView>,
}

#[derive(Deserialize, Default)]
struct SessionTime {
    #[serde(default)]
    archived: Option<serde_json::Value>,
}

#[derive(Deserialize, Default)]
struct ModelView {
    #[serde(rename = "providerID", default)]
    provider_id: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    variant: Option<String>,
}

fn session_view(session: &serde_json::Value) -> SessionView {
    SessionView::deserialize(session).unwrap_or_default()
}

pub(super) fn session_path(contract: OpencodeContract, session_id: &str) -> String {
    match contract {
        OpencodeContract::V1 => format!("/session/{session_id}"),
        OpencodeContract::V2 => format!("/api/session/{session_id}"),
    }
}

pub(super) fn messages_path(contract: OpencodeContract, session_id: &str) -> String {
    match contract {
        OpencodeContract::V1 => format!("/session/{session_id}/message"),
        OpencodeContract::V2 => {
            format!("/api/session/{session_id}/message?order=desc&limit={V2_MESSAGE_PAGE}")
        }
    }
}

/// The session object from a `GET` body: bare on 1.x, under `data` on 2.x.
pub(super) fn session_from_body(
    contract: OpencodeContract,
    body: &str,
) -> Result<serde_json::Value, ServeError> {
    let parse = |e: serde_json::Error| ServeError::Parse(e.to_string());
    match contract {
        OpencodeContract::V1 => serde_json::from_str(body).map_err(parse),
        OpencodeContract::V2 => serde_json::from_str::<Envelope<serde_json::Value>>(body)
            .map(|envelope| envelope.data)
            .map_err(parse),
    }
}

/// The session's own id, when it carries one.
pub(super) fn session_id(session: &serde_json::Value) -> Option<String> {
    session_view(session).id
}

/// True when the session object carries a non-null `time.archived` (both
/// contracts keep the stamp there).
pub(super) fn is_archived(session: &serde_json::Value) -> bool {
    session_view(session)
        .time
        .is_some_and(|time| time.archived.is_some())
}

pub(super) fn archive_body(contract: OpencodeContract, now_ms: u64) -> serde_json::Value {
    match contract {
        OpencodeContract::V1 => serde_json::json!({"time": {"archived": now_ms}}),
        OpencodeContract::V2 => serde_json::json!({"archived": true}),
    }
}

/// Parse a message readback into oldest-first [`ServeMessage`]s. A row that
/// does not read as a message is skipped, never fatal: one odd row must not
/// hide the turn.
pub(super) fn messages_from_body(
    contract: OpencodeContract,
    body: &str,
) -> Result<Vec<ServeMessage>, ServeError> {
    let parse = |e: serde_json::Error| ServeError::Parse(e.to_string());
    match contract {
        OpencodeContract::V1 => {
            let rows: Vec<serde_json::Value> = serde_json::from_str(body).map_err(parse)?;
            Ok(rows.iter().filter_map(v1_message).collect())
        }
        OpencodeContract::V2 => {
            let page: Envelope<Vec<serde_json::Value>> =
                serde_json::from_str(body).map_err(parse)?;
            // The route answers newest first; the lane reads oldest first.
            Ok(page.data.iter().rev().filter_map(v2_message).collect())
        }
    }
}

#[derive(Deserialize)]
struct TextPart {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

fn first_text(parts: &[serde_json::Value]) -> Option<String> {
    parts
        .iter()
        .filter_map(|part| TextPart::deserialize(part).ok())
        .filter(|part| part.kind == "text")
        .find_map(|part| part.text)
}

#[derive(Deserialize)]
struct V1Row {
    info: V1Info,
    #[serde(default)]
    parts: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct V1Info {
    id: String,
    #[serde(default)]
    role: String,
    #[serde(rename = "modelID", default)]
    model_id: Option<String>,
}

fn v1_message(row: &serde_json::Value) -> Option<ServeMessage> {
    let row = V1Row::deserialize(row).ok()?;
    Some(ServeMessage {
        id: row.info.id,
        role: row.info.role,
        text: first_text(&row.parts),
        model: row.info.model_id,
        complete: true,
    })
}

#[derive(Deserialize)]
struct V2Row {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<serde_json::Value>,
    #[serde(default)]
    content: Vec<serde_json::Value>,
    #[serde(default)]
    model: Option<ModelView>,
    #[serde(default)]
    time: Option<V2Time>,
}

#[derive(Deserialize)]
struct V2Time {
    #[serde(default)]
    completed: Option<serde_json::Value>,
}

fn v2_message(row: &serde_json::Value) -> Option<ServeMessage> {
    let row = V2Row::deserialize(row).ok()?;
    let assistant = row.kind == "assistant";
    let text = if assistant {
        first_text(&row.content)
    } else {
        row.text
            .as_ref()
            .and_then(|t| t.as_str())
            .map(str::to_string)
    };
    Some(ServeMessage {
        complete: !assistant || row.time.is_some_and(|time| time.completed.is_some()),
        id: row.id,
        role: row.kind,
        text,
        model: row.model.and_then(|m| m.id),
    })
}

fn split_model(model: &str) -> Result<(&str, &str), ServeError> {
    model
        .split_once('/')
        .filter(|(p, m)| !p.is_empty() && !m.is_empty())
        .ok_or_else(|| ServeError::Model(model.to_string()))
}

/// The 2.x `Model.Ref` for a `provider/model` id: `{providerID, id,
/// variant?}`. Effort rides as the variant, the same value 1.x persists in
/// its model state file.
pub(super) fn model_ref(
    model: &str,
    variant: Option<&str>,
) -> Result<serde_json::Value, ServeError> {
    let (provider, id) = split_model(model)?;
    Ok(match variant.filter(|v| !v.is_empty()) {
        Some(variant) => serde_json::json!({"providerID": provider, "id": id, "variant": variant}),
        None => serde_json::json!({"providerID": provider, "id": id}),
    })
}

/// The 2.x session-create body: the title names the worker (the plugin claims
/// by title) and `location.directory` binds the worktree.
pub(super) fn create_body(
    title: &str,
    directory: &str,
    model: &serde_json::Value,
    permissions: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "title": title,
        "location": {"directory": directory},
        "model": model,
        "permissions": permissions,
    })
}

/// The 2.x request that starts one turn. A slash seed (`/fno:target x`) rides
/// the native command route with its arguments as `text`; prose rides
/// `/prompt`. 2.x `run` dropped `--command`, so this route is the only one that
/// expands a command template server-side. Only a real command token routes
/// there: a path-led line (`/tmp/x is broken`) is prose, or the serve would
/// answer CommandNotFound.
pub(super) fn turn_request(session_id: &str, message: &str) -> (String, serde_json::Value) {
    if let Some(rest) = message.strip_prefix('/') {
        let (name, args) = rest.split_once(' ').unwrap_or((rest, ""));
        let first = message.split_whitespace().next().unwrap_or("");
        if crate::provider::parse_command_token(first).is_some() {
            return (
                format!("/api/session/{session_id}/command"),
                serde_json::json!({"name": name, "text": args}),
            );
        }
    }
    (
        format!("/api/session/{session_id}/prompt"),
        serde_json::json!({"text": message}),
    )
}

/// The `POST /api/session/:id/model` body that moves a 2.x session onto
/// `model`, or `None` when the session already runs it. The session's own
/// variant carries over, so a steer never resets the effort the launch set.
pub(super) fn model_switch(
    session: &serde_json::Value,
    model: &str,
) -> Result<Option<serde_json::Value>, ServeError> {
    let current = session_view(session).model.unwrap_or_default();
    let (provider, id) = split_model(model)?;
    if current.provider_id.as_deref() == Some(provider) && current.id.as_deref() == Some(id) {
        return Ok(None);
    }
    Ok(Some(
        serde_json::json!({"model": model_ref(model, current.variant.as_deref())?}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use OpencodeContract::{V1, V2};

    #[test]
    fn health_reads_each_contracts_own_signal() {
        assert_eq!(
            healthy_version(V1, 200, r#"{"healthy":true,"version":"1.18.33"}"#).as_deref(),
            Some("1.18.33")
        );
        assert_eq!(healthy_version(V1, 200, r#"{"healthy":false}"#), None);
        // 2.x has no healthy field: a 200 with a version is the answer.
        assert_eq!(
            healthy_version(V2, 200, r#"{"version":"2.2.7","pid":1,"urls":[]}"#).as_deref(),
            Some("2.2.7")
        );
        assert_eq!(
            healthy_version(V2, 401, r#"{"_tag":"UnauthorizedError"}"#),
            None
        );
        assert_eq!(healthy_version(V2, 200, "<html>"), None);
    }

    #[test]
    fn v2_readback_is_reordered_oldest_first_and_waits_for_completion() {
        let body = r#"{"data":[
            {"id":"msg_3","type":"assistant","model":{"id":"glm-5.3","providerID":"zai"},
             "content":[{"type":"reasoning","text":"hm"}],"time":{"created":3}},
            {"id":"msg_2","type":"assistant","model":{"id":"glm-5.3","providerID":"zai"},
             "content":[{"type":"text","text":"journey-ok"}],"time":{"created":2,"completed":2}},
            {"id":"msg_1","type":"user","text":"say it","time":{"created":1}}
        ],"cursor":{}}"#;
        let messages = messages_from_body(V2, body).unwrap();
        let ids: Vec<&str> = messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["msg_1", "msg_2", "msg_3"]);
        let [_, reply, streaming] = messages.as_slice() else {
            panic!("three messages: {messages:?}");
        };
        assert_eq!(reply.text.as_deref(), Some("journey-ok"));
        assert_eq!(reply.model.as_deref(), Some("glm-5.3"));
        assert!(reply.complete);
        assert!(!streaming.complete, "a streaming reply is not an answer");
        assert_eq!(streaming.text, None, "reasoning is not reply text");
    }

    #[test]
    fn turn_request_routes_a_slash_seed_to_the_command_route() {
        let (path, body) = turn_request("ses_a", "/fno:target --no-merge x-1");
        assert_eq!(path, "/api/session/ses_a/command");
        assert_eq!(
            body,
            serde_json::json!({"name": "fno:target", "text": "--no-merge x-1"})
        );
        let (path, body) = turn_request("ses_a", "do the thing");
        assert_eq!(path, "/api/session/ses_a/prompt");
        assert_eq!(body, serde_json::json!({"text": "do the thing"}));
        let (path, _) = turn_request("ses_a", "/tmp/x is broken");
        assert_eq!(path, "/api/session/ses_a/prompt", "a path is prose");
    }

    #[test]
    fn model_switch_keeps_the_variant_and_skips_a_no_op() {
        let session =
            serde_json::json!({"model": {"providerID": "zai", "id": "glm-5.3", "variant": "high"}});
        assert_eq!(model_switch(&session, "zai/glm-5.3").unwrap(), None);
        assert_eq!(
            model_switch(&session, "zai/glm-5.3-flash").unwrap(),
            Some(
                serde_json::json!({"model": {"providerID": "zai", "id": "glm-5.3-flash", "variant": "high"}})
            )
        );
        assert!(model_ref("bare", None).is_err());
    }
}
