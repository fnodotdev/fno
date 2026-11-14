//! The first message on a fresh connection: read, decode, and refuse loudly
//! when it cannot be decoded. Every connection kind (`Attach`, the frozen
//! `Query`/`KillServer` pair, `Control`, `ChatTail`) starts here.

use super::ATTACH_TIMEOUT;
use crate::proto::{check_attach_version, err_code, read_msg, write_msg, ClientMsg, ServerMsg};
use tokio::net::UnixStream;

/// The decoded first message, or `None` after a timeout, a read error, or an
/// undecodable message (which gets its refusal written first).
pub(super) async fn read_first(stream: &mut UnixStream) -> Option<ClientMsg> {
    let value = match tokio::time::timeout(ATTACH_TIMEOUT, read_msg::<_, serde_json::Value>(stream))
        .await
    {
        Ok(Ok(value)) => value,
        _ => return None,
    };
    match <ClientMsg as serde::Deserialize>::deserialize(&value) {
        Ok(message) => Some(message),
        Err(error) => {
            if let Some(reply) = unknown_control_refusal(&value) {
                let _ = write_msg(stream, &reply).await;
            } else {
                crate::mux_log!(error; "fno mux: initial client message failed to decode: {error}");
                // The refusal above recovers every envelope shape this
                // build knows. An envelope it cannot interpret at all
                // must not restore the silent close this branch removed:
                // even a hopeless decode gets a loud refusal, so drift
                // surfaces as a client-side error, never a dead socket.
                let _ = write_msg(
                    stream,
                    &ServerMsg::Err {
                        code: err_code::BAD_REQUEST,
                        msg: format!(
                            "undecodable first message; server {} speaks wire v{}",
                            crate::proto::BUILD_VERSION,
                            crate::proto::PROTO_VERSION
                        ),
                    },
                )
                .await;
            }
            None
        }
    }
}

/// Recover enough of an undecodable `Control` envelope to answer a newer
/// floor-compatible client instead of closing silently on an unknown command.
fn unknown_control_refusal(value: &serde_json::Value) -> Option<ServerMsg> {
    let control = value.get("Control")?.as_object()?;
    let proto = u32::try_from(control.get("proto")?.as_u64()?).ok()?;
    let build = control.get("build")?.as_str()?;
    if let Err(reason) = check_attach_version(proto, build) {
        return Some(ServerMsg::Err {
            code: err_code::VERSION_SKEW,
            msg: reason,
        });
    }
    let command = control
        .get("verb")
        .and_then(serde_json::Value::as_object)
        .and_then(|object| object.keys().next())
        .map(String::as_str)
        .unwrap_or("unknown");
    Some(ServerMsg::Err {
        code: err_code::BAD_REQUEST,
        msg: format!(
            "unknown control command {command:?}; server {} speaks wire v{}",
            crate::proto::BUILD_VERSION,
            crate::proto::PROTO_VERSION
        ),
    })
}
