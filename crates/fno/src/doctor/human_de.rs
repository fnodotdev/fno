//! Typed reads for the doctor human renderer.
//!
//! The doctor report used to be walked by string key in every emit arm. The
//! structs here parse the report once, at the top of `emit_human`, and the
//! emit arms read fields. Every field deserializer mirrors one `Value::as_*`
//! accessor, so a mistyped field reads None instead of failing the parse and
//! one bad field cannot blank its neighbors. docs/rust-style.md R1.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// Parse `v` into `T`; any parse failure reads as the all-empty struct.
pub(crate) fn parse<T: DeserializeOwned + Default>(v: &Value) -> T {
    T::deserialize(v).unwrap_or_default()
}

fn raw<'de, D: Deserializer<'de>>(d: D) -> Result<Value, D::Error> {
    Value::deserialize(d)
}

// The `opt_*` deserializers mirror one `Value::as_*` accessor each: null or
// a mistyped value reads None, never an error.
pub(crate) fn opt_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(raw(d)?.as_str().map(str::to_string))
}

pub(crate) fn opt_i64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Ok(raw(d)?.as_i64())
}

pub(crate) fn opt_f64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    Ok(raw(d)?.as_f64())
}

pub(crate) fn opt_bool<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    Ok(raw(d)?.as_bool())
}

/// Present counts, even when null: mirrors `.get(key).is_some()`.
pub(crate) fn opt_value_present<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Value>, D::Error> {
    Ok(Some(raw(d)?))
}

/// `.as_array()` semantics with lenient elements.
pub(crate) fn opt_vec_lenient<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default>(
    d: D,
) -> Result<Option<Vec<T>>, D::Error> {
    let v = raw(d)?;
    Ok(v.as_array().map(|a| {
        a.iter()
            .map(|e| T::deserialize(e).unwrap_or_default())
            .collect()
    }))
}

/// Strings of an array, non-strings dropped: the `filter_map(Value::as_str)`
/// walk.
pub(crate) fn opt_vec_string<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<String>>, D::Error> {
    let v = raw(d)?;
    Ok(v.as_array().map(|a| {
        a.iter()
            .filter_map(|e| e.as_str().map(str::to_string))
            .collect()
    }))
}

/// A nested struct field parsed leniently: null reads None, a scalar or a
/// bad object reads the all-empty struct, never a failed parse.
pub(crate) fn opt_struct<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default>(
    d: D,
) -> Result<Option<T>, D::Error> {
    let v = raw(d)?;
    Ok(if v.is_null() {
        None
    } else {
        Some(T::deserialize(&v).unwrap_or_default())
    })
}

/// A string-keyed map read as ordered pairs: non-string values coerce to
/// "", the `as_str().unwrap_or("")` walk. Pairs, not a map, because the
/// reader iterates the trust table in insertion order.
pub(crate) fn opt_map_string<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<(String, String)>>, D::Error> {
    let v = raw(d)?;
    Ok(v.as_object().map(|m| {
        m.iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
            .collect()
    }))
}

// ---------------------------------------------------------------------------
// report view

/// The doctor report, as the human renderer reads it.
#[derive(Deserialize, Default)]
pub(crate) struct DoctorReport {
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) source_checkout_sync: Option<SourceSync>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) status: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) source_rev: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) installed_rev: Option<String>,
    #[serde(default, deserialize_with = "opt_vec_string")]
    pub(crate) missing_config_keys: Option<Vec<String>>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) marker_stale: Option<bool>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) rust_installed_rev: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) rust_source_rev: Option<String>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) rust_stale: Option<bool>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) rust_binary: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) binary_rev: Option<String>,
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) components: Option<Vec<ComponentRow>>,
    #[serde(default)]
    pub(crate) daemon_drift: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) gh_proxy_shim: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) path_gh: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) gh_proxy: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) mux_front_door: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) mux_binary: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) path_fno: Option<String>,
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) mux_server_stale: Option<Vec<Value>>,
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) running_components: Option<Vec<RunCompRow>>,
    #[serde(default, deserialize_with = "opt_vec_string")]
    pub(crate) orphan_files: Option<Vec<String>>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) fd_limit: Option<FdLimit>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) dead_letter: Option<DeadLetter>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) managed_block: Option<ManagedBlock>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) harness_surface: Option<Surface>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) groom: Option<Groom>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) post_merge_sync: Option<PostMergeSync>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) evals: Option<Evals>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) launch_agents: Option<LaunchAgents>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) silent_switches: Option<SilentSwitches>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) auto_merge_review_gap: Option<ReviewGap>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) merge_gating_optouts: Option<Optouts>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) plugin_cache: Option<PluginCache>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) pre_push_hook: Option<PrePush>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) plugin_hooks: Option<PluginHooks>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) codex_app_server: Option<CodexAppServer>,
}

#[derive(Deserialize, Default)]
pub(crate) struct SourceSync {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) status: Option<String>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) behind: Option<i64>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) source_head: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) detail: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct ComponentRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) status: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) line: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) component: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct RunCompRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) verdict: Option<String>,
    #[serde(default)]
    pub(crate) pid: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) component: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) name: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) evidence: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) on_restart: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) survives: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct PrWatch {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) verdict: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) detail: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) fix: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct CpaSection {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) unknown_reason: Option<String>,
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) red: Option<Vec<ArmRow>>,
    #[serde(default)]
    pub(crate) dead_sinks: Option<Value>,
}

#[derive(Deserialize, Default)]
pub(crate) struct ArmRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) line: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) producer_evidence: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) arm: Option<String>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) age_s: Option<i64>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) interval_s: Option<i64>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) skip_reason: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct FdLimit {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) verdict: Option<String>,
    #[serde(default)]
    pub(crate) soft: Option<Value>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) threshold: Option<i64>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) launchd_soft: Option<Value>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) kern_maxfiles: Option<i64>,
}

#[derive(Deserialize, Default)]
pub(crate) struct DeadLetter {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) drain_hook_wired: Option<bool>,
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) stale_unread: Option<Vec<HandleRow>>,
}

#[derive(Deserialize, Default)]
pub(crate) struct HandleRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) handle: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct ManagedBlock {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) stale: Option<bool>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) file: Option<String>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) stamped: Option<i64>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) current: Option<i64>,
}

#[derive(Deserialize, Default)]
pub(crate) struct Surface {
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) opencode_doctor_lines: Option<Vec<TextRow>>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) codex_plugin: Option<CodexPlugin>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) codex_context_window: Option<CtxWindow>,
    #[serde(default)]
    pub(crate) codex_hooks_dual: Option<Value>,
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) codex_hooks_foreign_json: Option<Vec<Value>>,
}

#[derive(Deserialize, Default)]
pub(crate) struct TextRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) text: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct CodexPlugin {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) status: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) channel: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) cache_version: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) cache_digest: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) issue: Option<String>,
    #[serde(default, deserialize_with = "opt_vec_string")]
    pub(crate) enabled_plugin_ids: Option<Vec<String>>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) remedy: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct CtxWindow {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) overstated: Option<bool>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) leans_on_cached_cap: Option<bool>,
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) percent: Option<f64>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) model_source: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) model: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) window_source: Option<String>,
    #[serde(default)]
    pub(crate) configured: Option<Value>,
    #[serde(default)]
    pub(crate) effective: Option<Value>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) max_context_window: Option<i64>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) context_window: Option<i64>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) cache_fetched_at: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) cache_client_version: Option<String>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) app_server_running: Option<bool>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) base_synthesized: Option<bool>,
}

#[derive(Deserialize, Default)]
pub(crate) struct Groom {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) state: Option<String>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) agent_installed: Option<bool>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) stale: Option<bool>,
    #[serde(default, deserialize_with = "opt_f64")]
    pub(crate) hours: Option<f64>,
}

#[derive(Deserialize, Default)]
pub(crate) struct PostMergeSync {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) stale: Option<bool>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) state: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) detail: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct Evals {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) never_ran: Option<bool>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) stale: Option<bool>,
    #[serde(default, deserialize_with = "opt_vec_string")]
    pub(crate) regressed: Option<Vec<String>>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) window_days: Option<i64>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) age_days: Option<i64>,
}

#[derive(Deserialize, Default)]
pub(crate) struct LaunchAgents {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) applicable: Option<bool>,
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) dead: Option<Vec<DeadEntry>>,
}

#[derive(Deserialize, Default)]
pub(crate) struct DeadEntry {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) label: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) exit: Option<Value>,
}

#[derive(Deserialize, Default)]
pub(crate) struct SilentSwitches {
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) posture: Option<PostureStamp>,
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) findings: Option<Vec<SilentFinding>>,
}

#[derive(Deserialize, Default)]
pub(crate) struct PostureStamp {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) posture: Option<String>,
    #[serde(default)]
    pub(crate) applied_at: Option<Value>,
    #[serde(default)]
    pub(crate) scope: Option<Value>,
}

#[derive(Deserialize, Default)]
pub(crate) struct SilentFinding {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) switch: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) command: Option<String>,
    #[serde(default)]
    pub(crate) count: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) count_label: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) direction: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) cause: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct ReviewGap {
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) manifests: Option<i64>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) breakdown: Option<String>,
    #[serde(default, deserialize_with = "opt_vec_string")]
    pub(crate) keys: Option<Vec<String>>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) remedy: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct Optouts {
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) residue: Option<Vec<ResidueRow>>,
}

#[derive(Deserialize, Default)]
pub(crate) struct ResidueRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) key: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) path: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) claim_state: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) command: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct PluginCache {
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) roots: Option<Vec<RootRow>>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) detail: Option<String>,
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) rename_findings: Option<Vec<RenameRow>>,
}

#[derive(Deserialize, Default)]
pub(crate) struct RootRow {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) live: Option<bool>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) origin: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) path: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) status: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) note: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct RenameRow {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) detail: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) command: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct PrePush {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) status: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct PluginHooks {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) applicable: Option<bool>,
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) manifest_errors: Option<Vec<ManifestError>>,
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) missing_scripts: Option<Vec<MissingScript>>,
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) failures: Option<Vec<HookFailure>>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) reason: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct ManifestError {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) issue: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) path: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) manifest: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct MissingScript {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) script: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) manifest: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) event: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct HookFailure {
    #[serde(default)]
    pub(crate) rc: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) resolved: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) stderr: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) manifest: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) event: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct CodexAppServer {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) present: Option<bool>,
}

/// The `--codex-hooks` arm's result payload.
#[derive(Deserialize, Default)]
pub(crate) struct CodexHooksReport {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) status: Option<String>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) footnote_toml_wired: Option<bool>,
    #[serde(default, deserialize_with = "opt_map_string")]
    pub(crate) footnote_toml_trust: Option<Vec<(String, String)>>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) footnote_toml_trust_verified: Option<bool>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) state: Option<String>,
    #[serde(default, deserialize_with = "opt_vec_lenient")]
    pub(crate) errors: Option<Vec<Value>>,
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) plugin_hooks: Option<PluginHooksTrust>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) duplicate_layers: Option<bool>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) hooks_json_path: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) config_path: Option<String>,
    #[serde(default, deserialize_with = "opt_vec_string")]
    pub(crate) footnote_json_hooks: Option<Vec<String>>,
    #[serde(default, deserialize_with = "opt_vec_string")]
    pub(crate) foreign_json_hooks: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
pub(crate) struct PluginHooksTrust {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) status: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) reason: Option<String>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) trusted: Option<u64>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) not_trusted: Option<u64>,
    #[serde(default, deserialize_with = "opt_u64")]
    pub(crate) checked: Option<u64>,
}

/// The `--codex-bind` arm's result payload.
#[derive(Deserialize, Default)]
pub(crate) struct CodexBind {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) bound: Option<bool>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) codex_version: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) session_id: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) oracle: Option<String>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) elapsed_s: Option<i64>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) error: Option<String>,
}

/// One review journal line, as the invocation scan reads it.
#[derive(Deserialize, Default)]
pub(crate) struct JournalEvent {
    #[serde(default, deserialize_with = "opt_struct")]
    pub(crate) data: Option<InvocationData>,
    #[serde(default, rename = "type", deserialize_with = "opt_string")]
    pub(crate) r#type: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) ts: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct InvocationData {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) invocation_id: Option<String>,
}
