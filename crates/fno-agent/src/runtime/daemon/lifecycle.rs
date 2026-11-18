//! Identity resolution behind the lifecycle commands (stop/rm). Split from
//! daemon.rs for the file budget.

use serde_json::{json, Value};

/// Resolve the lifecycle target the way the session-connecting commands do:
/// registry first, then a best-effort harness-store heal for session-shaped
/// tokens. `for_stop` tries the
/// rm tombstone FIRST: a session `fno agent rm` just removed resolves from
/// the tombstone the daemon itself stamped (short id, full session id, cwd),
/// never through the adopting heal, whose rm grace window refuses exactly
/// this session and whose adoption would resurrect the removed row. rm and
/// every other caller keep the adopting heal. The caller's pre-heal registry
/// snapshot rides in because the helper may have just adopted a store-only
/// session absent from that snapshot.
pub(crate) async fn entry_for_lifecycle(
    registry: &crate::state::Registry,
    token: &str,
    registry_path: &std::path::Path,
    for_stop: bool,
) -> Result<Option<crate::state::RegistryEntry>, String> {
    let Value::Array(rows) = serde_json::to_value(&registry.entries)
        .map_err(|exc| format!("could not inspect registry identities: {exc}"))?
    else {
        return Err("could not inspect registry identities".to_string());
    };
    let worker_token = token.to_string();
    let path = registry_path.to_path_buf();
    let resolved = tokio::task::spawn_blocking(move || {
        if for_stop {
            if let Some(removed) = crate::rm_tombstone::lookup_removed_beside(&path, &worker_token)
            {
                return Ok(Some(json!({
                    "name": if removed.name.is_empty() {
                        crate::identity::canonical_handle(&removed.session_id)
                    } else {
                        removed.name
                    },
                    "short_id": removed.short,
                    "harness": removed.harness,
                    "harness_session_id": removed.session_id,
                    "cwd": removed.cwd,
                    "host_mode": if removed.host_mode.is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::Value::String(removed.host_mode)
                    },
                    "status": "orphaned",
                    "origin": "adopted",
                    "created_at": crate::daemon::now_rfc3339_like(),
                })));
            }
        }
        crate::client_commands::resolve_entry_with_heal(&rows, &worker_token, &path)
            .map(Some)
            .map_err(|err| err.message())
    })
    .await
    .map_err(|exc| format!("identity resolution task failed: {exc}"))?;
    match resolved {
        Ok(Some(entry)) => {
            let mut entry: crate::state::RegistryEntry = serde_json::from_value(entry)
                .map_err(|exc| format!("resolved identity row is unreadable: {exc}"))?;
            entry.backfill_harness_aliases();
            if let Some(legacy) = entry.backfill_short_id() {
                return Err(format!(
                    "resolved identity row {:?} has conflicting transport ids (legacy={legacy:?})",
                    entry.name
                ));
            }
            Ok(Some(entry))
        }
        Ok(None) => Ok(None),
        Err(message) => Err(message),
    }
}
