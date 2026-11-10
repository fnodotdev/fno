//! The one spawn door.
//!
//! Task 1.1 lands the contract and the owned-caller proof. The door's
//! transaction (`spawn_transaction`), the backend dispatch table, and
//! the producer rewiring build on these modules; the public
//! `spawn()` entry comes with the transaction.

pub mod accounts;
pub mod agents_config;
pub mod agy_ask;
pub mod agy_launch;
pub mod ask_project;
pub(crate) mod bounded_spawn;
pub(crate) mod cap_liveness;
pub mod capability_leaves;
pub mod capacity;
pub mod claude_ask;
pub mod claude_cloud;
pub mod claude_config_tmp;
pub mod claude_login;
pub mod claude_workspace_trust;
pub mod codex_ask;
pub mod codex_daemon_readiness;
pub mod codex_daemon_upgrade;
pub mod codex_hook_trust;
/// Public because `codex_resume` (pub, exercised by the parity test) names
/// [`CodexRoute`] in its signature.
pub mod codex_posture;
pub mod codex_route;
pub mod codex_skills;
pub mod component_update;
pub mod config_route;
pub mod cursor_agent;
pub mod dispatch_credit;
pub mod dispatch_harness;
pub mod effort_surface;
pub mod executor_route;
pub mod fallback_chain;
pub mod fleet_lanes;
pub mod gemini_ask;
pub mod grok_ask;
pub mod harness_capabilities;
pub mod harness_commands;
pub mod harness_matrix;
pub mod harness_roster;
pub mod keeper_spawn;
pub mod lane_heal;
pub mod lanes;
pub mod launch_exec;
pub mod launch_flags;
pub mod launch_record;
pub mod launch_workdir;
pub mod launchd_labels;
pub(crate) mod lifecycle_child;
pub mod loop_dispatch;
pub mod loops_config;
pub mod machine_key;
pub mod model_env_scrub;
pub mod model_family;
pub mod model_price;
pub mod name_draw;
pub mod name_ledger;
pub mod naming;
pub mod native_bg_mint;
pub mod occupancy_classify;
pub mod occupancy_login;
pub mod opencode_ask;
pub mod opencode_config;
pub mod opencode_install;
pub mod pack_profile;
pub mod pane_spawn;
pub mod pi;
pub mod pi_ask;
pub mod plugin_install;
pub mod plugins;
pub mod project_sandbox;
pub mod provider;
pub mod provider_cap;
pub mod provider_cap_commands;
pub mod provider_key;
pub mod provider_route;
pub(crate) mod resolve_tier;
pub mod retask;
pub mod role_cwd;
pub mod role_grant;
pub mod role_migration;
pub mod route_capacity;
pub mod route_gather;
pub mod route_inventory;
pub mod route_node_pin;
pub mod route_recovery;
pub mod route_settings;
pub mod route_slot;
pub mod sandbox_launch;
pub mod sandbox_probe;
pub(crate) mod session_origin;
pub mod session_role;
pub mod spawn_admission;
pub mod spawn_axes;
pub mod spawn_backends;
pub mod spawn_bare;
pub mod spawn_compose;
pub mod spawn_context;
pub mod spawn_contract;
pub mod spawn_cwd;
pub mod spawn_edge;
pub mod spawn_gate;
pub mod spawn_gate_admission;
pub mod spawn_gate_command;
pub mod spawn_gate_forced;
pub mod spawn_gate_lanes;
pub(crate) mod spawn_gate_open_prs;
pub(crate) mod spawn_gate_probe;
pub(crate) mod spawn_gate_queue;
pub mod spawn_gate_reservations;
pub(crate) mod spawn_gate_slots;
pub(crate) mod spawn_gate_spend;
pub mod spawn_guard;
pub mod spawn_lineage;
pub mod spawn_overlay;
pub mod spawn_payload;
pub mod spawn_phase;
pub mod spawn_place;
pub mod spawn_promote;
pub mod spawn_remote;
pub mod spawn_role;
pub mod spawn_route;
pub mod spawn_seam;
pub mod spawn_transaction;
pub(crate) mod spawn_watch;
pub mod stale_lane;
pub mod subprocess_ask;
pub mod team_spawn;
pub mod terminal_route;
pub mod terminal_run;
pub mod terminal_vocab;
pub mod thread_host;
pub mod usage;
pub mod usage_refresh;
pub mod zcode;
pub mod zcode_ask;

pub use crate::spawn_context::{
    resolve_owned_identity_from, resolve_self_identity, session_identity_key, OwnedDisposition,
    OwnedDisposition as _OwnedDispositionAlias, OwnedIdentity,
};
pub use crate::spawn_contract::{
    validate, InvocationRef, NonSessionSource, SessionRef, SpawnError, SpawnHow, SpawnOrigin,
    SpawnOwner, SpawnProvenance, SpawnRequest, SpawnWork, ValidatedSpawn,
};
