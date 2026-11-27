//! The daemon side of the sandbox seam: the `agent.sandbox-plan` op. Split
//! out of daemon.rs (shrink-only) so sandbox-side additions land here. The
//! plan types and the policy live in `crate::sandbox_launch`, where the
//! spawn client reaches them.
//!
//! The plan names no mount: a sandbox reaches the daemon only through the
//! host relay that owns its exec stream.

use super::*;

/// `agent.sandbox-plan` - the resolved provider, image and env pins for one
/// worktree. A refused policy (an unparsable layer, an unknown value) answers
/// an error that names the layer.
pub(super) fn handle_sandbox_plan(_ctx: &Ctx, req: &Request) -> Response {
    let worktree = match req.params.get("worktree").and_then(|v| v.as_str()) {
        Some(w) => PathBuf::from(w),
        None => {
            return Response::err(req.id, ErrorCode::InvalidParams, "missing `worktree`");
        }
    };
    match crate::sandbox_launch::plan_for(&worktree) {
        Ok(plan) => Response::ok(
            req.id,
            serde_json::to_value(&plan).unwrap_or_else(|_| json!({"provider": "none"})),
        ),
        Err(e) => Response::err(req.id, ErrorCode::InvalidParams, e),
    }
}
