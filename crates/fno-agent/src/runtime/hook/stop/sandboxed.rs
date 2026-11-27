//! The Stop of a sandboxed worker. Inside a sandbox there is no host state
//! to decide from, so the one host decision answers through the relay.

use std::path::PathBuf;

/// The Stop of a sandboxed worker: send loop stop through the pinned socket
/// and pass the host decision's exit and output through unchanged. Any
/// failure to reach the host blocks and names itself; it never allows.
pub(super) fn sandboxed_stop(session_id: &str, message: Option<&str>) -> i32 {
    let sock = std::env::var_os("FNO_SUPERVISOR_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(crate::sandbox::CONTAINER_SOCK));
    let params = serde_json::json!({
        "op": "loop-stop",
        "message": message.unwrap_or_default(),
    });
    let reply = crate::protocol::blocking_response(
        &sock,
        crate::sandbox::ops::OP_METHOD,
        params,
        std::time::Duration::from_secs(900),
    );
    let answer = match reply.map(|r| r.payload) {
        Ok(crate::protocol::ResponsePayload::Ok(v)) => v,
        Ok(crate::protocol::ResponsePayload::Err(e)) => {
            return super::emit_block_for_harness(
                session_id,
                &format!(
                    "target stop-hook: the host refused loop stop for this sandbox: {}. \
                     Fix what it names and stop again.",
                    e.message
                ),
            )
        }
        Err(e) => {
            return super::emit_block_for_harness(
                session_id,
                &format!(
                    "target stop-hook: the sandbox relay did not answer loop stop ({e}); \
                     this stop blocks until the host decides."
                ),
            )
        }
    };
    #[derive(serde::Deserialize)]
    struct Decided {
        exit: i32,
        #[serde(default)]
        stdout: String,
        #[serde(default)]
        stderr: String,
    }
    match serde_json::from_value::<Decided>(answer) {
        Ok(d) => {
            eprint!("{}", d.stderr);
            // Under the claude front a block rides stdout JSON at exit 0: the
            // front skips a nonzero leg, so an exit-2 block would allow.
            if d.exit == 2 && crate::hook::capturing() {
                return super::emit_block_for_harness(session_id, d.stderr.trim());
            }
            let out = d.stdout.trim_end_matches('\n');
            if !out.is_empty() {
                crate::hook::emit_line(out);
            }
            d.exit
        }
        Err(e) => super::emit_block_for_harness(
            session_id,
            &format!("target stop-hook: the host's loop stop answer does not parse ({e})"),
        ),
    }
}
