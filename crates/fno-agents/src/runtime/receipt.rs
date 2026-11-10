//! Recovery receipts for a registry row that left the registry.
//!
//! Moved out of `daemon.rs` so the write choke point
//! (`state::update_registry`) can stage the same receipt for a row removed
//! through ANY door, not only the reap sweep. `daemon.rs` re-exports the
//! moved items, so existing reap-path references are unchanged.
//!
//! The durability ordering is load-bearing and shared by both callers:
//! receipt FIRST, then the event, so an auditor never sees a removal
//! announced with no recovery path beside it.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::paths::AgentsHome;
use crate::state;

/// The capability-table key for the interactive resume form.
const INTERACTIVE_RESUME_FORM: &str = "interactive_resume";

/// The door that produced a receipt. The vocabulary is closed: a new
/// removal door extends this enum, and the compiler then refuses its
/// writer until it names itself through the required builder parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Writer {
    /// The scheduled retirement sweep and `fno-agents reap --apply`.
    GcSweep,
    /// The roster sweep for harness rows no fno row names.
    RosterReap,
    /// The registry choke point: the surface is whatever argv named.
    RegistryWrite,
}

impl Writer {
    pub fn surface(self) -> String {
        match self {
            Writer::GcSweep => "gc-sweep".to_string(),
            Writer::RosterReap => "roster-reap".to_string(),
            // One command serves a human shell and a daemon loop, so the
            // surface is the whole bounded invocation, not the binary.
            Writer::RegistryWrite => crate::state::invocation_command(),
        }
    }
    /// RegistryWrite reads argv0: the daemon binary is unattended, any
    /// other invocation is a session's call. The stem (not the whole
    /// name) matches, so `fno-agents-daemon.exe` still classifies.
    pub fn trigger(self) -> &'static str {
        match self {
            Writer::RegistryWrite => {
                // current_exe can fail while the process lives (an exotic
                // sandbox, a racing self-update); argv0 is how the process
                // was actually launched and classifies the same way.
                let exe = std::env::current_exe()
                    .ok()
                    .or_else(|| std::env::args_os().next().map(std::path::PathBuf::from))
                    .and_then(|p| p.file_stem().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_default();
                if exe == crate::component_update::AGENTS_DAEMON {
                    "unattended"
                } else {
                    "session"
                }
            }
            _ => "unattended",
        }
    }
}

/// One reaped row's recovery record. Built from the registry row
/// itself - the fields present on every row - plus the harness-DECLARED
/// interactive resume form read from the capability table (the same single
/// source `fno agents history` renders), and enriched from the ledger entry
/// when one exists. Written durably BEFORE the retain drops the row, so a
/// reaped row stays recoverable even when its ledger entry does not exist and
/// never will (leads, blueprint and rescue sessions never open a PR, so no
/// target run ever writes them one).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReapReceipt {
    pub row_name: String,
    pub short_id: String,
    pub harness: String,
    pub harness_session_id: String,
    pub cwd: String,
    pub log_path: Option<String>,
    pub created_at: String,
    pub reaped_at: String,
    /// The resume command, rendered from the capability table's
    /// `interactive_resume` form. Never hardcoded here.
    pub resume: String,
    /// Ledger enrichment (node / pr / plan) when the session resolves there.
    /// The ledger stays the richer source; this is the copy that survives
    /// when the row has no ledger entry at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger: Option<Value>,
    /// The SURFACE that took the row, never the process that ran it:
    /// `gc-sweep`, `roster-reap`, `merge-reaper`, or the argv command for a
    /// removal that came through the registry choke point. Empty only on
    /// a receipt written before this field was required.
    #[serde(default)]
    pub removed_by: String,
    /// `unattended` for a sweep nobody asked for, `session` for a call a
    /// session made. `fno agents rm` is reachable from both, so the writer
    /// name alone cannot separate them. Empty on pre-stamp receipts.
    #[serde(default)]
    pub removal_trigger: String,
    /// Receipt schema version. v1 receipts predate the field and read as
    /// `None` (migration reads old receipts without inventing fields); every
    /// new write stamps `Some(2)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    /// The native identity, store context and team lineage needed after the
    /// registry row leaves the live projection. `None` on a v1 receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<serde_json::Value>,
    /// Where the native history lives (transcript paths, index records), so
    /// resume can find the same session after every active surface is gone.
    /// `None` on a v1 receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_locator: Option<serde_json::Value>,
    /// Known model provenance: what was requested and/or observed for this
    /// session's lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provenance: Option<serde_json::Value>,
    /// The resume command as TOKENS with no shell quoting implied. The v1
    /// `resume` string stays for rendering; tokens are what a launcher runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resume_argv: Vec<String>,
    /// Per-effect retirement progress, appended as effects land so a
    /// crash/retry continues instead of repeating (task 3). Empty on
    /// a v1 receipt and on every receipt until the first effect records.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<EffectRecord>,
    /// Assignment evidence: the node links and completion basis this
    /// retirement was decided on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment: Option<serde_json::Value>,
    /// Set when the retention window expired the expendable detail (ledger
    /// enrichment, per-effect rows) but the identity-critical core was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details_expired_at: Option<String>,
    /// The binary that applied the retirement, stamped at write time as
    /// provenance. Absent on v1 receipts. The pin `reap --verify` audits
    /// against is `retirement_contract`, not this stamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writer_build: Option<String>,
    /// The retirement contract the writer promised
    /// (`gc_verify::retirement_contract`): the pin `reap --verify` audits
    /// against. Absent on receipts written before the stamp existed, and
    /// those skip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retirement_contract: Option<String>,
}

/// One retirement effect's durable record: what ran, what answered, when.
/// The outcome vocabulary is the typed outcome set the lifecycle reports;
/// a partial failure is never an overall success because each effect names
/// its own.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EffectRecord {
    pub op: String,
    /// `confirmed-removed` | `confirmed-already-absent` | `kept` |
    /// `failed` | `not-applicable`
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub at: String,
}

/// Load one receipt from disk. All v2 fields default, so a v1 file reads
/// with them absent rather than invented.
pub fn read_reap_receipt(path: &std::path::Path) -> std::io::Result<ReapReceipt> {
    let raw = std::fs::read(path)?;
    serde_json::from_slice(&raw)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

/// Expire the expendable fields of one aged receipt, keeping the identity
/// core. Returns true when the file still exists (rewritten); false when it
/// was already gone. A receipt with nothing expendable left is left
/// byte-identical.
pub fn expire_receipt_details(receipt: &mut ReapReceipt) -> bool {
    let mut changed = false;
    if receipt.ledger.is_some() {
        receipt.ledger = None;
        changed = true;
    }
    if !receipt.effects.is_empty() {
        receipt.effects.clear();
        changed = true;
    }
    if receipt.log_path.take().is_some() {
        changed = true;
    }
    if changed && receipt.details_expired_at.is_none() {
        receipt.details_expired_at = Some(crate::daemon::now_rfc3339_like());
    }
    changed
}

impl ReapReceipt {
    pub(crate) fn resume_unavailable_reason(&self) -> Option<&str> {
        if !self.resume_argv.is_empty() {
            return None;
        }
        #[derive(serde::Deserialize)]
        struct Identity<'a> {
            resume_unavailable: Option<&'a str>,
        }
        Identity::deserialize(self.identity.as_ref()?)
            .ok()?
            .resume_unavailable
            .filter(|reason| !reason.trim().is_empty())
    }
}

/// Sanitize a receipt filename component: the session id comes from registry
/// rows and is not guaranteed filename-safe across harnesses.
pub fn receipt_filename_part(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Where reap receipts live: `<agents home>/reap-receipts/`, one file per
/// reaped row keyed by its resume identity, or row incarnation when absent.
pub fn reap_receipt_path(home: &AgentsHome, receipt: &ReapReceipt) -> std::path::PathBuf {
    home.root()
        .join("reap-receipts")
        .join(row_receipt_file_name(
            &receipt.harness,
            &receipt.harness_session_id,
            &receipt.row_name,
            &receipt.created_at,
        ))
}

/// The receipt filename for one resume identity: `<harness>-<session id>`
/// in the sanitized alphabet, so every writer lands on the same key.
fn receipt_file_name(harness: &str, session_id: &str) -> String {
    format!(
        "{}-{}.json",
        receipt_filename_part(harness),
        receipt_filename_part(session_id)
    )
}

fn row_receipt_file_name(harness: &str, session_id: &str, name: &str, created_at: &str) -> String {
    if !session_id.trim().is_empty() {
        return receipt_file_name(harness, session_id);
    }
    let key = format!("{harness}\0{name}\0{created_at}");
    format!(
        "unidentified-{}.json",
        blake3::hash(key.as_bytes()).to_hex()
    )
}

/// The receipt path for one resume identity, so a reader without a receipt
/// in hand (the verify gate's event-derived cohort) spells the key once.
pub fn reap_receipt_path_for(
    home: &AgentsHome,
    harness: &str,
    session_id: &str,
) -> std::path::PathBuf {
    home.root()
        .join("reap-receipts")
        .join(receipt_file_name(harness, session_id))
}

/// Persist one receipt durably. 0600 like the rest of the agents tree.
pub fn write_reap_receipt(home: &AgentsHome, receipt: &ReapReceipt) -> std::io::Result<()> {
    let path = reap_receipt_path(home, receipt);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(receipt)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?,
    )?;
    let _ = crate::paths::set_file_mode_0600(&path);
    Ok(())
}

/// The message a receipt's spawn revival carries: the session resumes its
/// own transcript, so the prompt only says to go on.
pub(crate) const RESUME_MESSAGE: &str =
    "continue: this session was reaped and is resumed; pick up where the transcript ends";

/// A bare `claude --resume` lands on the account default, so the recipe asks
/// resume_pin the way the reentry plan does: pin the row's model, or name the
/// fno door that rebuilds a route a copied command cannot carry.
fn claude_resume_recipe(
    e: &state::RegistryEntry,
    sid: &str,
    mut argv: Vec<String>,
    route_provider_of: crate::resume_pin::RouteProviderOf<'_>,
) -> Vec<String> {
    let pins = crate::resume_pin::RowPins::from_entry(e);
    match crate::resume_pin::resolve(Some(pins), None, false, sid, route_provider_of) {
        Ok(pin) => {
            crate::resume_pin::append_axes(
                &mut argv,
                pin.argv_model.as_deref(),
                pin.effort.as_deref(),
            );
            argv
        }
        Err(crate::resume_pin::Unpinned {
            lost_route: Some((provider, model)),
            ..
        }) => {
            // Spawn's own form: the message is the positional, the row name
            // rides `--name` (a revival keeps its name), and the route is one
            // `--route provider/model` token. `-P` is a tombstone that exits 2.
            let route = format!("{provider}/{model}");
            [
                "fno",
                "agents",
                "spawn",
                RESUME_MESSAGE,
                "--name",
                &e.name,
                "--resume",
                sid,
                "--route",
                &route,
            ]
            .map(str::to_string)
            .to_vec()
        }
        Err(_) => argv,
    }
}

/// Build the receipt from the row, or say exactly why it cannot be built.
///
/// Supported identities keep their declared resume command. A missing identity
/// or an explicitly unsupported resume form keeps a terminal record instead;
/// an unreadable capability table or unknown harness still refuses.
pub fn build_reap_receipt(
    e: &state::RegistryEntry,
    ledger: Option<&Value>,
    writer: Writer,
    store_matches: &dyn Fn(&state::RegistryEntry) -> Option<Vec<std::path::PathBuf>>,
) -> Result<ReapReceipt, String> {
    let harness = e.harness_name();
    let sid = e.harness_session_id.as_deref().unwrap_or_default();
    let contract = crate::harness_capabilities::HarnessContract::packaged()
        .map_err(|err| format!("capability table unreadable: {err}"))?;
    let unavailable = if harness.trim().is_empty() {
        Some("missing harness identity")
    } else {
        let caps = contract
            .capabilities(harness)
            .map_err(|err| format!("capability lookup failed: {err}"))?;
        if sid.trim().is_empty() {
            Some("missing harness session identity")
        } else {
            let form = caps
                .resume_strategy
                .forms
                .get(INTERACTIVE_RESUME_FORM)
                .ok_or_else(|| format!("no interactive resume declaration for {harness}"))?;
            (form.kind == "unsupported").then_some("interactive resume is unsupported")
        }
    };
    let argv = if unavailable.is_some() {
        Vec::new()
    } else {
        contract
            .render_session_argv(harness, "interactive_resume", Some(sid))
            .map_err(|err| format!("no interactive resume form declared: {err}"))?
    };
    // A bare `claude --resume` lands on the account default, so a claude
    // argv asks resume_pin first; every other harness keeps its table form.
    let argv = if harness == "claude" && unavailable.is_none() {
        claude_resume_recipe(e, sid, argv, &|m| {
            crate::claude_adopt::provider_from_route_settings(m)
        })
    } else {
        argv
    };
    // The native locator: every transcript candidate the harness's own store
    // holds for this session, supplied by the caller's index. A store read that
    // fails leaves the locator absent rather than inventing a recovery path.
    // The receipt still preserves the recorded identity and its limits.
    let transcripts = store_matches(e).unwrap_or_default();
    let mut identity = serde_json::Map::from_iter([
        ("harness".into(), json!(harness)),
        ("session_id".into(), json!(sid)),
        ("short_id".into(), json!(e.short_id)),
        (
            "store_root".into(),
            json!(store_root_for(harness).map(|p| p.to_string_lossy().to_string())),
        ),
        ("cwd".into(), json!(e.cwd)),
        ("role_level".into(), json!(e.role_level)),
        ("spawned_by_session".into(), json!(e.spawned_by_session)),
    ]);
    if let Some(reason) = unavailable {
        identity.insert("resume_unavailable".into(), json!(reason));
    }
    Ok(ReapReceipt {
        row_name: e.name.clone(),
        short_id: e.short_id.clone(),
        harness: harness.to_string(),
        harness_session_id: sid.to_string(),
        cwd: e.cwd.clone(),
        log_path: e.log_path.clone(),
        created_at: e.created_at.clone(),
        reaped_at: crate::daemon::now_rfc3339_like(),
        // The rendered form a human copies. A claude line is shell-quoted,
        // so a token like `glm-5.3-flash[1m]` survives zsh's glob; harnesses
        // whose table form composes a pre_exec `sh -c` script keep the raw
        // join, because the script token is already shell-quoted content and
        // quoting it again would corrupt it. resume_argv stays raw either way.
        resume: if let Some(reason) = unavailable {
            format!("not resumable: {reason}")
        } else if harness == "claude" {
            argv.iter()
                .map(|t| crate::client_commands::shlex_quote(t))
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            argv.join(" ")
        },
        ledger: ledger.cloned(),
        removed_by: writer.surface(),
        removal_trigger: writer.trigger().to_string(),
        schema_version: Some(2),
        identity: Some(Value::Object(identity)),
        native_locator: Some(serde_json::json!({ "transcripts": transcripts })),
        model_provenance: model_provenance_of(e),
        resume_argv: argv,
        effects: Vec::new(),
        assignment: None,
        details_expired_at: None,
        writer_build: Some(crate::gc_verify::current_build()),
        retirement_contract: Some(crate::gc_verify::retirement_contract()),
    })
}

/// The spawn-axes `reap_receipt` answer: the removal receipt for one row the
/// Python registry choke point dropped, from the builder every Rust door
/// uses. `{"row", "removed_by"}` in; `{"file", "receipt"}` or `{"refused"}`
/// out. The caller writes the file.
pub fn decide_reap_receipt(ask: &Value) -> Value {
    let refused = |reason: String| json!({ "refused": reason });
    let row: state::RegistryEntry = match ask.get("row").map(|v| serde_json::from_value(v.clone()))
    {
        Some(Ok(row)) => row,
        Some(Err(err)) => return refused(format!("row unreadable: {err}")),
        None => return refused("no row in the ask".to_string()),
    };
    let index = std::cell::RefCell::new(crate::gc_inventory::HarnessStoreIndex::default());
    match build_reap_receipt(&row, None, Writer::RegistryWrite, &|e| {
        index.borrow_mut().matches(e)
    }) {
        Ok(mut receipt) => {
            if let Some(by) = ask.get("removed_by").and_then(Value::as_str) {
                if !by.is_empty() {
                    receipt.removed_by = by.to_string();
                }
            }
            json!({
                "file": row_receipt_file_name(&receipt.harness, &receipt.harness_session_id, &receipt.row_name, &receipt.created_at),
                "receipt": receipt,
            })
        }
        Err(err) => refused(err),
    }
}

/// The store root one harness's transcripts live under, for the receipt's
/// identity record. `None` for a harness with no transcript store fno reads.
fn store_root_for(harness: &str) -> Option<std::path::PathBuf> {
    match harness {
        "claude" => std::env::var_os("HOME")
            .map(|h| std::path::PathBuf::from(h).join(".claude").join("projects")),
        "codex" => crate::client_commands::codex_home().map(|h| h.join("sessions")),
        _ => None,
    }
}

/// Requested/observed model provenance from the row's axis fields, in the
/// shape (a bare model is two facts; the basis travels beside it). Present
/// whenever ANY of model, provider or effort is: a row with a provider and no
/// model still proves its vendor, an absent value stays absent.
fn model_provenance_of(e: &state::RegistryEntry) -> Option<serde_json::Value> {
    let model = e.model.as_deref();
    let effort = e.effort.as_deref().or(e.requested_effort.as_deref());
    if model.is_none() && e.provider.is_none() && effort.is_none() {
        return None;
    }
    let mut out = serde_json::Map::new();
    if let Some(model) = model {
        out.insert(
            "model".to_string(),
            serde_json::Value::String(model.to_string()),
        );
    }
    if let Some(provider) = e.provider.as_deref() {
        out.insert(
            "provider".to_string(),
            serde_json::Value::String(provider.to_string()),
        );
    }
    if let Some(effort) = effort {
        out.insert(
            "effort".to_string(),
            serde_json::Value::String(effort.to_string()),
        );
    }
    if let Some(basis) = e.model_basis.as_deref() {
        out.insert(
            "basis".to_string(),
            serde_json::Value::String(basis.to_string()),
        );
    }
    Some(serde_json::Value::Object(out))
}

/// Stage the removal accounting for one row a write path is about to drop:
/// the receipt first, then the `registry_row_removed` event naming
/// the row, the remover and the reason. A receipt that cannot be built or
/// persisted still announces the removal (`receipt_staged: false`, the build
/// error as `reason`, the attempted harness-side outcome as `active_surface`):
/// an unrecoverable removal that is announced is strictly better than a silent
/// one, and refusing the write would turn an audit gap into an outage.
/// Best-effort by contract - an emission failure never fails the write that
/// triggered it.
pub fn stage_removal_accounting(
    home: &AgentsHome,
    entry: &state::RegistryEntry,
    remover: &str,
    emitter: &crate::events::EventEmitter,
    store_matches: &dyn Fn(&state::RegistryEntry) -> Option<Vec<std::path::PathBuf>>,
) {
    // The active-surface outcome outlives the receipt write on purpose. The
    // harness-side removal happens BEFORE that write, so a write that fails
    // leaves the harness row already gone with no receipt on disk - the event
    // is then the only durable record of it, and must name it. `None` means
    // no attempt was made, never "attempted, outcome unknown".
    let mut active_surface: Option<&'static str> = None;
    let already_staged = home
        .root()
        .join("reap-receipts")
        .join(row_receipt_file_name(
            entry.harness_name(),
            entry.harness_session_id.as_deref().unwrap_or_default(),
            &entry.name,
            &entry.created_at,
        ))
        .exists();
    let (receipt_staged, reason) = if already_staged {
        (true, "receipt already staged for this session".to_string())
    } else {
        match build_reap_receipt(entry, None, Writer::RegistryWrite, store_matches) {
            Ok(mut receipt) => {
                let outcome = crate::gc_native::apply_active_surface_removal(entry);
                active_surface = Some(outcome.as_str());
                receipt
                    .effects
                    .push(outcome.effect_record("active-surface"));
                match write_reap_receipt(home, &receipt) {
                    Ok(()) => (
                        true,
                        format!("removed by an update_registry write ({})", outcome.as_str()),
                    ),
                    Err(err) => (false, format!("receipt did not persist: {err}")),
                }
            }
            Err(err) => (false, err),
        }
    };
    let _ = emitter.emit(
        "registry_row_removed",
        &json!({
            "name": entry.name,
            "short_id": entry.short_id,
            "harness": entry.harness_name(),
            "harness_session_id": entry.harness_session_id.clone().unwrap_or_default(),
            "remover": remover,
            "reason": reason,
            "receipt_staged": receipt_staged,
            "active_surface": active_surface,
            "pid": std::process::id(),
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_row(name: &str) -> state::RegistryEntry {
        serde_json::from_str(&format!(
            r#"{{"name":"{name}","short_id":"{name}-id","harness":"claude","harness_session_id":"{name}-session","cwd":"/tmp/x","log_path":"/tmp/x.log","created_at":"2026-09-01T00:00:00Z","status":"live"}}"#
        ))
        .unwrap()
    }

    fn axes_row(name: &str, axes: &str) -> state::RegistryEntry {
        serde_json::from_str(&format!(
            r#"{{"name":"{name}","short_id":"{name}-id","harness":"claude","harness_session_id":"{name}-session","cwd":"/tmp/x","log_path":"/tmp/x.log","created_at":"2026-09-01T00:00:00Z","status":"live"{axes}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn ac1_hp_zai_receipt_resume_string_quotes_the_model() {
        // AC1-HP: the rendered `resume` quotes the [1m] glob; the argv tokens
        // stay raw. An empty route dir is a lookup miss, so the row's own
        // provider names the door - hermetic, no route file needed.
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let route_dir = tempfile::tempdir().unwrap();
        std::env::set_var("FNO_ROUTE_SETTINGS_DIR", route_dir.path());
        let e = axes_row(
            "zairow2",
            r#","provider":"zai","requested_model":"glm-5.3-flash[1m]""#,
        );
        let receipt = build_reap_receipt(&e, None, Writer::GcSweep, &|_| None).unwrap();
        // FNO_ROUTE_SETTINGS_DIR stays set (tempdir drop leaves it dangling):
        // unsetting it process-globally races every lock-free state-resolving
        // test into the paths.rs guard, and a dangling route dir is just a
        // lookup miss, which is what this test wants.
        assert!(
            receipt.resume.ends_with("--route 'zai/glm-5.3-flash[1m]'"),
            "{}",
            receipt.resume
        );
        assert_eq!(
            receipt.resume_argv.last().map(String::as_str),
            Some("zai/glm-5.3-flash[1m]")
        );
    }

    #[test]
    fn decide_reap_receipt_answers_file_and_receipt_for_a_good_row() {
        let mut row = sample_row("goodrow");
        row.provider = Some("anthropic".to_string());
        row.effort = Some("high".to_string());
        let ask = json!({"row": row, "removed_by": "probe"});
        let answer = decide_reap_receipt(&ask);
        assert!(answer.get("refused").is_none(), "{answer}");
        assert_eq!(answer["file"], "claude-goodrow-session.json");
        assert_eq!(answer["receipt"]["removed_by"], "probe");
        assert_eq!(answer["receipt"]["removal_trigger"], "session");
        assert_eq!(
            answer["receipt"]["resume"], "claude --resume goodrow-session",
            "a row with no model keeps the bare form"
        );
        let provenance = &answer["receipt"]["model_provenance"];
        assert_eq!(provenance["provider"], "anthropic");
        assert_eq!(provenance["effort"], "high");
        assert!(
            provenance.get("model").is_none(),
            "an absent model stays absent: {provenance}"
        );

        let plain = decide_reap_receipt(&json!({"row": sample_row("plainrow")}));
        assert!(
            plain["receipt"]["model_provenance"].is_null(),
            "a row with none of the axes gets no provenance object: {plain}"
        );
    }

    #[test]
    fn terminal_receipts_preserve_missing_and_unsupported_resume_identity() {
        let mut missing_sid = axes_row(
            "nosid",
            r#","provider":"zai","requested_model":"glm-5.3-flash[1m]""#,
        );
        missing_sid.harness_session_id = None;
        let mut no_harness = missing_sid.clone();
        no_harness.harness = None;
        let mut unsupported = sample_row("unsupported");
        unsupported.harness = Some("footnote".into());
        for (row, reason) in [
            (missing_sid, "missing harness session identity"),
            (no_harness, "missing harness identity"),
            (unsupported, "interactive resume is unsupported"),
        ] {
            let answer = decide_reap_receipt(&json!({"row": row}));
            assert!(answer.pointer("/refused").is_none(), "{answer}");
            let receipt = answer.pointer("/receipt").unwrap();
            assert!(receipt.pointer("/resume_argv").is_none_or(Value::is_null));
            assert!(receipt
                .pointer("/resume")
                .and_then(Value::as_str)
                .unwrap()
                .starts_with("not resumable:"));
            assert_eq!(
                receipt
                    .pointer("/identity/resume_unavailable")
                    .and_then(Value::as_str),
                Some(reason)
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(dir.path());
        for (name, stamp) in [
            ("same-worker", "00"),
            ("same-worker", "01"),
            ("other-worker", "00"),
        ] {
            let mut row = sample_row(name);
            row.created_at = format!("2026-01-01T00:00:{stamp}Z");
            row.harness_session_id = None;
            let receipt = build_reap_receipt(&row, None, Writer::GcSweep, &|_| None).unwrap();
            write_reap_receipt(&home, &receipt).unwrap();
        }
        assert_eq!(
            std::fs::read_dir(dir.path().join("reap-receipts"))
                .unwrap()
                .count(),
            3,
            "unidentified rows and incarnations must not overwrite each other"
        );
        let hint = crate::resume_receipt::resume_hint(&home, "same-worker").unwrap();
        assert_eq!(
            hint.matches("not resumable: missing harness session identity")
                .count(),
            2,
            "{hint}"
        );
        assert!(!hint.contains("resume by the exact session id"), "{hint}");
        let mut unknown = sample_row("unknown");
        unknown.harness = Some("harness-no-contract".into());
        assert!(decide_reap_receipt(&json!({"row": unknown}))
            .pointer("/refused")
            .is_some_and(Value::is_string));
    }
}
