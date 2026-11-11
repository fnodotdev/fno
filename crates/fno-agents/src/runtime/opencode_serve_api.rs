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

use crate::opencode_install::OpencodeContract;

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
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let version = value.get("version").and_then(|v| v.as_str());
    match contract {
        OpencodeContract::V1 => (value.get("healthy") == Some(&serde_json::Value::Bool(true)))
            .then(|| version.unwrap_or_default().to_string()),
        OpencodeContract::V2 => version.map(str::to_string),
    }
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
) -> Result<serde_json::Value, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("session body parse: {e}"))?;
    match contract {
        OpencodeContract::V1 => Ok(value),
        OpencodeContract::V2 => value
            .get("data")
            .cloned()
            .ok_or_else(|| "session body has no data envelope".to_string()),
    }
}

/// True when the session object carries a non-null `time.archived` (both
/// contracts keep the stamp there).
pub(super) fn is_archived(session: &serde_json::Value) -> bool {
    session
        .get("time")
        .and_then(|time| time.get("archived"))
        .is_some_and(|value| !value.is_null())
}

pub(super) fn archive_body(contract: OpencodeContract, now_ms: u64) -> serde_json::Value {
    match contract {
        OpencodeContract::V1 => serde_json::json!({"time": {"archived": now_ms}}),
        OpencodeContract::V2 => serde_json::json!({"archived": true}),
    }
}

/// Parse a message readback into oldest-first [`ServeMessage`]s.
pub(super) fn messages_from_body(
    contract: OpencodeContract,
    body: &str,
) -> Result<Vec<ServeMessage>, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("messages body parse: {e}"))?;
    match contract {
        OpencodeContract::V1 => {
            let rows = value
                .as_array()
                .ok_or_else(|| "1.x message readback is not an array".to_string())?;
            Ok(rows.iter().filter_map(v1_message).collect())
        }
        OpencodeContract::V2 => {
            let rows = value
                .get("data")
                .and_then(|data| data.as_array())
                .ok_or_else(|| "2.x message readback has no data array".to_string())?;
            // The route answers newest first; the lane reads oldest first.
            Ok(rows.iter().rev().filter_map(v2_message).collect())
        }
    }
}

fn first_text(parts: Option<&serde_json::Value>) -> Option<String> {
    parts?
        .as_array()?
        .iter()
        .filter(|part| part.get("type").and_then(|kind| kind.as_str()) == Some("text"))
        .find_map(|part| part.get("text").and_then(|text| text.as_str()))
        .map(str::to_string)
}

fn v1_message(row: &serde_json::Value) -> Option<ServeMessage> {
    let info = row.get("info")?;
    Some(ServeMessage {
        id: info.get("id")?.as_str()?.to_string(),
        role: info
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or_default()
            .to_string(),
        text: first_text(row.get("parts")),
        model: info
            .get("modelID")
            .and_then(|m| m.as_str())
            .map(str::to_string),
        complete: true,
    })
}

fn v2_message(row: &serde_json::Value) -> Option<ServeMessage> {
    let role = row.get("type")?.as_str()?.to_string();
    let text = match role.as_str() {
        "assistant" => first_text(row.get("content")),
        _ => row.get("text").and_then(|t| t.as_str()).map(str::to_string),
    };
    let complete = role != "assistant"
        || row
            .get("time")
            .and_then(|time| time.get("completed"))
            .is_some_and(|value| !value.is_null());
    Some(ServeMessage {
        id: row.get("id")?.as_str()?.to_string(),
        role,
        text,
        model: row
            .get("model")
            .and_then(|m| m.get("id"))
            .and_then(|m| m.as_str())
            .map(str::to_string),
        complete,
    })
}

/// The 2.x `Model.Ref` for a `provider/model` id: `{providerID, id,
/// variant?}`. Effort rides as the variant, the same value 1.x persists in
/// its model state file.
pub(super) fn model_ref(model: &str, variant: Option<&str>) -> Result<serde_json::Value, String> {
    let (provider, id) = model
        .split_once('/')
        .filter(|(p, m)| !p.is_empty() && !m.is_empty())
        .ok_or_else(|| format!("opencode model {model:?} is not provider/model"))?;
    let mut value = serde_json::json!({"providerID": provider, "id": id});
    if let Some(variant) = variant.filter(|v| !v.is_empty()) {
        value["variant"] = serde_json::Value::String(variant.to_string());
    }
    Ok(value)
}

/// The 2.x session-create body: the title names the worker (the plugin claims
/// by title) and `location.directory` binds the worktree.
pub(super) fn create_body(
    title: &str,
    directory: &str,
    model: &serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "title": title,
        "location": {"directory": directory},
        "model": model,
    })
}

/// The 2.x request that starts one turn. A slash seed (`/fno:target x`) rides
/// the native command route with its arguments as `text`; prose rides
/// `/prompt`. 2.x `run` dropped `--command`, so this route is the only one that
/// expands a command template server-side.
/// Only a real command token routes there: a path-led line (`/tmp/x is
/// broken`) is prose, or the serve would answer CommandNotFound.
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
) -> Result<Option<serde_json::Value>, String> {
    let current = session.get("model");
    let variant = current
        .and_then(|m| m.get("variant"))
        .and_then(|v| v.as_str());
    let wanted = model_ref(model, variant)?;
    let same = current.is_some_and(|m| {
        m.get("providerID") == wanted.get("providerID") && m.get("id") == wanted.get("id")
    });
    Ok((!same).then(|| serde_json::json!({"model": wanted})))
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
        assert_eq!(messages[1].text.as_deref(), Some("journey-ok"));
        assert_eq!(messages[1].model.as_deref(), Some("glm-5.3"));
        assert!(messages[1].complete);
        assert!(!messages[2].complete, "a streaming reply is not an answer");
        assert_eq!(messages[2].text, None, "reasoning is not reply text");
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
