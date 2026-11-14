//! The app API's version document and chat stream, served on the attention
//! door. `GET /v1` names the API; `GET /v1/chat/{harness}/{session_id}` is a
//! WebSocket over the same readers and the same poll loop the mux socket's
//! `ChatTail` uses, so a phone or browser reads what the desktop app reads.

use crate::attention_api::{authorize, bearer, load_sink_auth, to_response, SinkAuth};
use crate::chat_events::{pump, BatchSink, ChatReadError, ChatRoots, SessionReader, IDLE_POLL};
use crate::proto::chat_event::{ChatEvent, SessionKey};
use crate::proto::{BUILD_VERSION, PROTO_VERSION};
use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// Every `/v1` path the door serves: the list `GET /v1` returns.
const ROUTES: &[&str] = &[
    "/v1",
    "/v1/attention/items",
    "/v1/attention/items/{id}",
    "/v1/attention/items/{id}/answer",
    "/v1/chat/{harness}/{session_id}",
];

/// The WebSocket subprotocol the server selects. A browser also offers
/// `bearer.<token>`, because it cannot set an `Authorization` header.
const SUBPROTOCOL: &str = "fno.v1";
const TOKEN_PREFIX: &str = "bearer.";
const INBOUND_CAP: usize = 64 * 1024;
/// A quiet session still pings, so a proxy on the path keeps the socket.
const PING_EVERY: Duration = Duration::from_secs(30);

pub fn router(roots: ChatRoots) -> axum::Router {
    use axum::routing::get;
    axum::Router::new()
        .route("/v1", get(version))
        .route("/v1/chat/{harness}/{session_id}", get(chat))
        .with_state(roots)
}

async fn version(headers: HeaderMap) -> Response {
    let sinks = load_sink_auth();
    to_response(version_response(&sinks, bearer(&headers).as_deref()))
}

fn version_response(sinks: &[SinkAuth], token: Option<&str>) -> (u16, Value) {
    match authorize(sinks, token) {
        Ok(_) => (
            200,
            json!({"api": 1, "build": BUILD_VERSION, "proto": PROTO_VERSION, "routes": ROUTES}),
        ),
        Err(refusal) => refusal,
    }
}

/// The token a WebSocket client sent: the bearer header (native clients), or
/// the `bearer.<token>` subprotocol (browsers). Never the query string.
fn ws_token(headers: &HeaderMap) -> Option<String> {
    bearer(headers).or_else(|| {
        headers
            .get_all("sec-websocket-protocol")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .find_map(|p| p.trim().strip_prefix(TOKEN_PREFIX))
            .map(str::to_string)
    })
}

#[derive(Deserialize)]
struct ChatQuery {
    since: Option<u64>,
    subagent: Option<String>,
}

async fn chat(
    ws: WebSocketUpgrade,
    Path((harness, session_id)): Path<(String, String)>,
    Query(query): Query<ChatQuery>,
    headers: HeaderMap,
    State(roots): State<ChatRoots>,
) -> Response {
    let token = ws_token(&headers);
    let authorized = authorize(&load_sink_auth(), token.as_deref()).is_ok();
    let ws = ws.protocols([SUBPROTOCOL]).max_message_size(INBOUND_CAP);
    if !authorized {
        // A browser cannot read a failed handshake's status, so a bad token
        // completes the upgrade and closes with 1008, as the web bridge does.
        return ws.on_upgrade(|mut socket| async move {
            socket
                .send(Message::Close(Some(CloseFrame {
                    code: close_code::POLICY,
                    reason: "invalid token".into(),
                })))
                .await
                .ok();
        });
    }
    let key = SessionKey {
        harness,
        session_id,
        subagent: query.subagent,
    };
    let since = query.since.unwrap_or(0);
    ws.on_upgrade(move |socket| stream(socket, roots, key, since))
}

async fn stream(mut socket: WebSocket, roots: ChatRoots, key: SessionKey, since: u64) {
    let open_key = key.clone();
    let opened =
        tokio::task::spawn_blocking(move || SessionReader::open(&open_key, &roots, since)).await;
    let reader = match opened {
        Ok(Ok(reader)) => reader,
        Ok(Err(error)) => return fail(socket, &error).await,
        Err(_) => return,
    };
    let mut sink = WsSink {
        socket: &mut socket,
        key,
        last_ping: Instant::now(),
    };
    if let Err(error) = pump(reader, &mut sink).await {
        fail(socket, &error).await;
    }
}

/// One error frame, then close 1011.
async fn fail(mut socket: WebSocket, error: &ChatReadError) {
    socket
        .send(Message::Text(error_frame(error).into()))
        .await
        .ok();
    socket
        .send(Message::Close(Some(CloseFrame {
            code: close_code::ERROR,
            reason: "chat stream failed".into(),
        })))
        .await
        .ok();
}

struct WsSink<'a> {
    socket: &'a mut WebSocket,
    key: SessionKey,
    last_ping: Instant,
}

impl BatchSink for WsSink<'_> {
    async fn send(&mut self, batch: Vec<ChatEvent>) -> bool {
        let frame = event_frame(&self.key, &batch);
        self.socket.send(Message::Text(frame.into())).await.is_ok()
    }

    async fn idle(&mut self) -> bool {
        tokio::select! {
            _ = tokio::time::sleep(IDLE_POLL) => {
                if self.last_ping.elapsed() < PING_EVERY {
                    return true;
                }
                self.last_ping = Instant::now();
                self.socket.send(Message::Ping(Vec::new().into())).await.is_ok()
            }
            // Read-only: inbound messages are dropped; a close or error ends the stream.
            inbound = self.socket.recv() => {
                matches!(inbound, Some(Ok(m)) if !matches!(m, Message::Close(_)))
            }
        }
    }
}

fn event_frame(session: &SessionKey, events: &[ChatEvent]) -> String {
    json!({"v": 1, "session": session, "events": events}).to_string()
}

fn error_frame(error: &ChatReadError) -> String {
    let kind = match error {
        ChatReadError::UnsupportedHarness(_) => "unsupported_harness",
        ChatReadError::NotFound { .. } => "not_found",
        _ => "io",
    };
    json!({"v": 1, "error": {"kind": kind, "message": error.to_string()}}).to_string()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;
    use crate::chat_events::batches;
    use axum::http::HeaderValue;

    const SID: &str = "0b1c2d3e-4f50-4a6b-8c7d-9e0f1a2b3c4d";

    fn key(harness: &str) -> SessionKey {
        SessionKey {
            harness: harness.into(),
            session_id: SID.into(),
            subagent: None,
        }
    }

    #[test]
    fn version_document_lists_routes_and_refuses_no_token() {
        let sinks = [SinkAuth {
            name: "phone".into(),
            token: "secret".into(),
        }];
        let (status, body) = version_response(&sinks, Some("secret"));
        assert_eq!(status, 200);
        assert_eq!(body["api"], 1);
        let routes: Vec<&str> = body["routes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(routes.contains(&"/v1/chat/{harness}/{session_id}"));
        assert!(routes.contains(&"/v1/attention/items"));
        assert_eq!(version_response(&sinks, None).0, 401);
    }

    #[test]
    fn ws_token_reads_header_then_subprotocol_never_a_query() {
        let mut headers = HeaderMap::new();
        assert_eq!(ws_token(&headers), None);
        headers.insert(
            "sec-websocket-protocol",
            HeaderValue::from_static("fno.v1, bearer.abc123"),
        );
        assert_eq!(ws_token(&headers).as_deref(), Some("abc123"));
        headers.insert("authorization", HeaderValue::from_static("Bearer hdr"));
        assert_eq!(ws_token(&headers).as_deref(), Some("hdr"));
    }

    #[test]
    fn frames_resume_from_since_and_unknown_harness_is_one_error_frame() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        std::fs::create_dir_all(projects.join("-repo")).unwrap();
        std::fs::write(
            projects.join("-repo").join(format!("{SID}.jsonl")),
            concat!(
                r#"{"type":"user","uuid":"u1","message":{"role":"user","content":"hello"}}"#,
                "\n",
                r#"{"type":"assistant","uuid":"a1","message":{"id":"m1","content":[{"type":"text","text":"hi"}]}}"#,
                "\n",
            ),
        )
        .unwrap();
        let roots = ChatRoots {
            transcripts: vec![projects],
            opencode_stores: Vec::new(),
        };
        let seqs = |since: u64| -> Vec<u64> {
            let mut reader = SessionReader::open(&key("claude"), &roots, since)
                .ok()
                .unwrap();
            batches(reader.poll().unwrap())
                .iter()
                .flat_map(|b| {
                    let frame: Value =
                        serde_json::from_str(&event_frame(&key("claude"), b)).unwrap();
                    assert_eq!(frame["v"], 1);
                    frame["events"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|e| e["seq"].as_u64().unwrap())
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        let all = seqs(0);
        assert_eq!(all, (0..all.len() as u64).collect::<Vec<_>>());
        let tail = all.len() as u64 - 2;
        assert_eq!(seqs(tail), vec![tail, tail + 1]);

        let refused = SessionReader::open(&key("pi"), &roots, 0).err().unwrap();
        let frame: Value = serde_json::from_str(&error_frame(&refused)).unwrap();
        assert_eq!(frame["error"]["kind"], "unsupported_harness");
    }
}
