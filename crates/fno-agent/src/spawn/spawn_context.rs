//! The spawn door's lineage stamp. The owned-caller proof it consumes lives
//! in [`crate::self_identity`], which fno compiles a copy of; this module
//! re-exports it and keeps the `AgentsHome` form of `resolve_self_identity`
//! every fno-agent caller uses.

use crate::paths::AgentsHome;
pub use crate::self_identity::*;
use serde_json::{json, Value};

/// [`crate::self_identity::resolve_self_identity`] over the registry `home`
/// names.
pub fn resolve_self_identity(
    get: &impl Fn(&str) -> Option<String>,
    collide: Option<CollideHook>,
    witness: Option<WitnessHook>,
    home: &AgentsHome,
) -> OwnedIdentity {
    crate::self_identity::resolve_self_identity(get, collide, witness, &home.registry_json())
}

/// Stamp the caller's proven initiating context onto a daemon-bound spawn
/// request: the client is the earliest boundary that can prove the
/// caller, so the proof is captured ONCE here and serialized across the RPC
/// boundary; the daemon consumes the request values and never recaptures
/// from its own scrubbed environment. When the owned proof resolves to a
/// session (or a cwd-keyed spawn record names one), the request carries a
/// `Session` origin with the proven parent and a `Session` owner; an
/// unproven caller sends no stamp and the daemon keeps today's ambient
/// capture until the door enforces (schema v33 rollout).
/// The ambient parent proof: the spawning session's identity resolved from
/// its own markers and cwd. `None` when unprovable, never a laundered guess.
fn ambient_parent_proof() -> Option<crate::spawn_contract::SessionRef> {
    let get = |k: &str| std::env::var(k).ok();
    let home = AgentsHome::from_env();
    let owned = resolve_self_identity(&get, None, None, &home);
    let session_id = owned.session_id?;
    let harness = owned.harness?;
    let cwd = std::env::var("PWD")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });
    if cwd.is_empty() {
        return None;
    }
    Some(crate::spawn_contract::SessionRef {
        harness: harness.to_string(),
        session_id,
        cwd,
    })
}

pub fn stamp_spawn_lineage(params: &mut serde_json::Map<String, Value>) -> Result<(), String> {
    // Explicit dispatch context outranks ambient capture: a daemon
    // producer (the mission drain) exports FNO_SPAWN_ORIGIN +
    // FNO_SPAWN_OWNER naming its arm and the responsible mission/team, and
    // those ride the request verbatim. A malformed carrier is a producer bug
    // and refuses by name instead of falling back silently.
    let origin_env = std::env::var("FNO_SPAWN_ORIGIN").ok();
    let owner_env = std::env::var("FNO_SPAWN_OWNER").ok();
    let carried = match (origin_env, owner_env) {
        (Some(o), Some(w)) => Some((o, w)),
        (Some(_), None) => {
            return Err("FNO_SPAWN_ORIGIN and FNO_SPAWN_OWNER must be exported together".into())
        }
        // Owner-only carrier: the producer names WHO ANSWERS and lets the
        // door prove WHO CAUSED it from the ambient identity. The composed
        // request validates like any other, so a malformed owner refuses by
        // name instead of riding silently.
        (None, Some(w)) => Some((String::new(), w)),
        (None, None) => None,
    };
    if let Some((origin_raw, owner_raw)) = carried {
        if origin_raw.is_empty() {
            // Owner-only: the origin is the door's ambient proof of who
            // caused the spawn. Unprovable identity keeps today's shape
            // (no stamp; the mint answers), never a half record.
            let Some(parent) = ambient_parent_proof() else {
                return Ok(());
            };
            let owner: crate::spawn_contract::SpawnOwner =
                serde_json::from_str(&owner_raw).map_err(|e| {
                    format!("FNO_SPAWN_OWNER is malformed ({e}); the producer carrier must speak the door's vocabulary")
                })?;
            let request = crate::spawn_contract::SpawnRequest::new(
                crate::spawn_contract::SpawnOrigin::Session {
                    parent: parent.clone(),
                    invocation: None,
                },
                owner,
                crate::spawn_contract::SpawnHow::new("claude", "headless"),
                crate::spawn_contract::SpawnWork::new("carrier", "", "."),
            );
            crate::spawn_contract::validate(&request)
                .map_err(|e| format!("FNO_SPAWN_OWNER refused by the door: {e}"))?;
            params.insert(
                "origin".into(),
                serde_json::to_value(&crate::spawn_contract::SpawnOrigin::Session {
                    parent,
                    invocation: None,
                })
                .unwrap_or(Value::Null),
            );
            let owner_value: Value = serde_json::from_str(&owner_raw).unwrap_or(Value::Null);
            params.insert("owner".into(), owner_value);
            return Ok(());
        }
        let origin: crate::spawn_contract::SpawnOrigin =
            serde_json::from_str(&origin_raw).map_err(|e| {
                format!("FNO_SPAWN_ORIGIN is malformed ({e}); the producer carrier must speak the door's vocabulary")
            })?;
        let owner: crate::spawn_contract::SpawnOwner =
            serde_json::from_str(&owner_raw).map_err(|e| {
                format!("FNO_SPAWN_OWNER is malformed ({e}); the producer carrier must speak the door's vocabulary")
            })?;
        let request = crate::spawn_contract::SpawnRequest::new(
            origin,
            owner,
            crate::spawn_contract::SpawnHow::new("claude", "headless"),
            crate::spawn_contract::SpawnWork::new("carrier", "", "."),
        );
        crate::spawn_contract::validate(&request)
            .map_err(|e| format!("FNO_SPAWN_ORIGIN/FNO_SPAWN_OWNER refused by the door: {e}"))?;
        let origin_value: Value = serde_json::from_str(&origin_raw).unwrap_or(Value::Null);
        let owner_value: Value = serde_json::from_str(&owner_raw).unwrap_or(Value::Null);
        params.insert("origin".into(), origin_value);
        params.insert("owner".into(), owner_value);
        return Ok(());
    }
    let Some(parent) = ambient_parent_proof() else {
        return Ok(());
    };
    params.insert(
        "origin".into(),
        serde_json::to_value(&crate::spawn_contract::SpawnOrigin::Session {
            parent: parent.clone(),
            invocation: None,
        })
        .unwrap_or(Value::Null),
    );
    // Dispatch credit: a --node spawn whose no producer named an owner
    // answers to the live role covering the node (native path). A leadless
    // node keeps the session owner.
    let owner_value = match params
        .get("node")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(node) => {
            let credit = crate::dispatch_credit::covering_role(&json!({"node": node}));
            credit
                .get("owner")
                .cloned()
                .filter(|o| !o.is_null())
                .unwrap_or_else(|| {
                    serde_json::to_value(&crate::spawn_contract::SpawnOwner::Session(
                        parent.clone(),
                    ))
                    .unwrap_or(Value::Null)
                })
        }
        None => serde_json::to_value(&crate::spawn_contract::SpawnOwner::Session(parent.clone()))
            .unwrap_or(Value::Null),
    };
    params.insert("owner".into(), owner_value);
    Ok(())
}
